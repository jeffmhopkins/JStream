//! The audio the `waveform` and `spectrum` sources draw (ADR-0031's audio → render data
//! path): [`AudioScope`], a lock-free ring of the latest samples of one mix, and what
//! both kinds share.
//!
//! The mixer thread pushes each block it mixes. A source reads the latest window when it
//! draws a frame, on whichever thread asks for the frame, so the analysis (the FFT
//! included) never runs on the mixer thread, and the push neither locks nor allocates
//! ([R-RT-003](../../../../docs/RULES.md#R-RT-003)).

mod spectrum;
mod waveform;

pub(crate) use spectrum::SpectrumKind;
pub(crate) use waveform::WaveformKind;

use crate::color::{parse_color, rgba_frame};
use crate::source::{at_millis, millis, setting};
use crate::{Limits, SourceError};
use jstream_media::{FrameSize, MediaTime, Timebase, VideoFrame};
use jstream_protocol::Settings;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

/// Samples the ring holds: 341 ms at 48 kHz, so a reader that copies the latest window
/// (at most [`WINDOW`]) has many blocks' headroom before the mixer overwrites it.
const CAPACITY: usize = 1 << 14;
/// The window both kinds draw: 2048 samples, 43 ms at 48 kHz.
pub(crate) const WINDOW: usize = 2048;
/// How often a scope source draws a new frame.
const FPS: u64 = 30;
/// How long the ring may stand still before a source draws silence: when no output runs,
/// the program mix stops and the ring keeps its last samples.
const STILL_MS: u64 = 250;

/// The latest audio of one mix, as mono (the mean of left and right), for the waveform
/// and spectrum sources. One thread pushes ([`push`](Self::push)); any number read
/// ([`latest`](Self::latest)), up to [`MAX_WINDOW`](Self::MAX_WINDOW) samples at a time.
/// Neither locks nor allocates.
pub struct AudioScope {
    /// Samples as `f32` bits; sample `n` (counted from the first push) is at
    /// `n % CAPACITY`.
    ring: Box<[AtomicU32]>,
    /// Samples pushed and complete.
    written: AtomicU64,
    /// Samples a push has claimed: set before the push overwrites slots, so a reader can
    /// tell whether the slots it copied changed under it.
    claimed: AtomicU64,
    /// The sample rate of the last push, in hertz; 0 before the first.
    rate: AtomicU32,
}

impl AudioScope {
    /// The longest window [`latest`](Self::latest) copies: 2048 samples, 43 ms at
    /// 48 kHz.
    pub const MAX_WINDOW: usize = WINDOW;

    /// An empty scope: it reads as silence until the first push.
    pub fn new() -> Self {
        Self {
            ring: (0..CAPACITY).map(|_| AtomicU32::new(0)).collect(),
            written: AtomicU64::new(0),
            claimed: AtomicU64::new(0),
            rate: AtomicU32::new(0),
        }
    }

    /// Appends a block of interleaved stereo samples at `rate` hertz. Call it from one
    /// thread at a time: two concurrent pushes garble each other's samples (never
    /// memory). A trailing half frame is ignored.
    pub fn push(&self, rate: u32, interleaved: &[f32]) {
        let frames = interleaved.chunks_exact(2);
        let start = self.written.load(Ordering::Relaxed);
        let end = start + frames.len() as u64;
        // A seqlock's write side: claim, then write, then publish.
        self.claimed.store(end, Ordering::Relaxed);
        fence(Ordering::Release);
        for (n, frame) in (start..end).zip(frames) {
            let mono = (frame[0] + frame[1]) * 0.5;
            self.ring[slot(n)].store(mono.to_bits(), Ordering::Relaxed);
        }
        self.rate.store(rate, Ordering::Relaxed);
        self.written.store(end, Ordering::Release);
    }

    /// Copies the latest `out.len()` samples into `out`, oldest first, and returns how
    /// many samples have been pushed in all (to tell a moving ring from a still one) and
    /// the sample rate (0 before the first push). Samples from before the first push
    /// read as 0. When a push overwrote part of the window while it was copied, the copy
    /// is retried; if pushes keep overtaking it, the window reads as silence.
    ///
    /// # Panics
    /// When `out` is longer than [`MAX_WINDOW`](Self::MAX_WINDOW): a longer window
    /// would leave too little headroom.
    pub fn latest(&self, out: &mut [f32]) -> (u64, u32) {
        assert!(
            out.len() <= WINDOW,
            "an audio scope window is at most {WINDOW}"
        );
        for _ in 0..4 {
            let end = self.written.load(Ordering::Acquire);
            let rate = self.rate.load(Ordering::Relaxed);
            let start = end.saturating_sub(out.len() as u64);
            let missing = out.len() - (end - start) as usize;
            let (silent, copied) = out.split_at_mut(missing);
            silent.fill(0.0);
            for (n, sample) in (start..end).zip(copied) {
                *sample = f32::from_bits(self.ring[slot(n)].load(Ordering::Relaxed));
            }
            // A seqlock's read side: the copy holds unless a later push claimed one of
            // the slots it read.
            fence(Ordering::Acquire);
            if self.claimed.load(Ordering::Relaxed) <= start + CAPACITY as u64 {
                return (end, rate);
            }
        }
        out.fill(0.0);
        (self.written.load(Ordering::Acquire), 0)
    }
}

impl Default for AudioScope {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for AudioScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioScope")
            .field("written", &self.written.load(Ordering::Relaxed))
            .field("rate", &self.rate.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// The ring slot of sample `n`.
fn slot(n: u64) -> usize {
    // CAPACITY is a power of two well inside `usize`.
    (n % CAPACITY as u64) as usize
}

/// Where a scope source reads its audio: the latest window, or silence when there is no
/// scope or its ring has stood still for [`STILL_MS`].
#[derive(Debug)]
struct Feed {
    scope: Option<Arc<AudioScope>>,
    /// The ring's count when it last moved, and when that was (ms since the source
    /// started).
    seen: u64,
    moved_at: u64,
}

impl Feed {
    fn new(scope: Option<Arc<AudioScope>>) -> Self {
        Self {
            scope,
            seen: 0,
            moved_at: 0,
        }
    }

    /// Fills `out` with the latest window at `ms`; returns the sample rate, or `None`
    /// (with `out` silent) when there is nothing to draw.
    fn read(&mut self, ms: u64, out: &mut [f32]) -> Option<u32> {
        let Some(scope) = &self.scope else {
            out.fill(0.0);
            return None;
        };
        let (written, rate) = scope.latest(out);
        if written != self.seen {
            self.seen = written;
            self.moved_at = ms;
        }
        if rate == 0 || ms.saturating_sub(self.moved_at) >= STILL_MS {
            out.fill(0.0);
            return None;
        }
        Some(rate)
    }
}

/// The frame tick `at` falls in, at [`FPS`].
fn tick(at: MediaTime) -> u64 {
    millis(at) * FPS / 1000
}

/// When frame tick `tick` starts.
fn tick_start(tick: u64) -> MediaTime {
    at_millis((tick * 1000).div_ceil(FPS))
}

/// The next frame tick's start after `at`: when a scope source next draws.
fn next_tick(at: MediaTime) -> MediaTime {
    tick_start(tick(at) + 1)
}

/// The frames a scope source draws into. A frame handed out stays valid, so a new one is
/// drawn into a frame no one else holds any more, and only when all are held is another
/// made.
#[derive(Debug)]
struct Frames {
    size: FrameSize,
    pool: Vec<Arc<VideoFrame>>,
    /// The frame handed out last.
    last: usize,
}

/// Frames kept for reuse. The program mix holds the frame it shows and the render
/// snapshot one more, so three are free most of the time.
const POOL: usize = 4;

impl Frames {
    fn new(size: FrameSize) -> Result<Self, SourceError> {
        Ok(Self {
            size,
            pool: vec![Arc::new(blank(size)?)],
            last: 0,
        })
    }

    /// The frame shown last.
    fn current(&self) -> Arc<VideoFrame> {
        Arc::clone(&self.pool[self.last])
    }

    /// Draws a new frame with `draw` and hands it out.
    fn draw(
        &mut self,
        pts: MediaTime,
        draw: impl FnOnce(&mut Canvas<'_>),
    ) -> Result<Arc<VideoFrame>, SourceError> {
        let free = (0..self.pool.len())
            .filter(|&i| i != self.last)
            .find(|&i| Arc::get_mut(&mut self.pool[i]).is_some());
        let index = match free {
            Some(i) => i,
            None if self.pool.len() < POOL => {
                self.pool.push(Arc::new(blank(self.size)?));
                self.pool.len() - 1
            }
            // Every frame is held: replace the oldest. Those holding it keep it.
            None => {
                let oldest = (self.last + 1) % self.pool.len();
                self.pool[oldest] = Arc::new(blank(self.size)?);
                oldest
            }
        };
        // Just found free, or just made.
        if let Some(frame) = Arc::get_mut(&mut self.pool[index]) {
            frame.set_pts(pts);
            draw(&mut Canvas(frame));
            self.last = index;
        }
        Ok(self.current())
    }
}

/// A frame being drawn.
struct Canvas<'f>(&'f mut VideoFrame);

impl Canvas<'_> {
    fn width(&self) -> u32 {
        self.0.info().size.width()
    }

    fn height(&self) -> u32 {
        self.0.info().size.height()
    }

    /// Fills every pixel with `color`.
    fn fill(&mut self, color: [u8; 4]) {
        for y in 0..self.height() {
            if let Some(row) = self.0.row_mut(0, y) {
                for pixel in row.chunks_exact_mut(4) {
                    pixel.copy_from_slice(&color);
                }
            }
        }
    }

    /// Fills rows `top..bottom` of column `x` with `color`.
    fn column(&mut self, x: u32, top: u32, bottom: u32, color: [u8; 4]) {
        let at = x as usize * 4;
        for y in top..bottom.min(self.height()) {
            if let Some(pixel) = self.0.row_mut(0, y).and_then(|row| row.get_mut(at..at + 4)) {
                pixel.copy_from_slice(&color);
            }
        }
    }
}

/// A transparent frame of `size`.
fn blank(size: FrameSize) -> Result<VideoFrame, SourceError> {
    let len =
        usize::try_from(u64::from(size.width()) * u64::from(size.height()) * 4).map_err(|_| {
            jstream_media::MediaError::Overflow {
                what: "a scope frame",
            }
        })?;
    Ok(rgba_frame(
        size,
        vec![0; len],
        MediaTime::zero(Timebase::MILLIS),
    )?)
}

/// The `width` and `height` settings, within `limits`.
fn frame_size(settings: &Settings, limits: &Limits) -> Result<FrameSize, SourceError> {
    let side = |id: &str| {
        setting(settings, id)?
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .filter(|&v| (1..=limits.max_side).contains(&v))
            .ok_or_else(|| {
                bad(
                    id,
                    &format!("must be a whole number from 1 to {}", limits.max_side),
                )
            })
    };
    let (width, height) = (side("width")?, side("height")?);
    // The pool may hold POOL frames.
    let bytes = u64::from(width) * u64::from(height) * 4 * POOL as u64;
    if bytes > limits.max_decoded_bytes {
        return Err(bad(
            if width >= height { "width" } else { "height" },
            &format!(
                "{POOL} frames of {width}x{height} are over {} bytes",
                limits.max_decoded_bytes
            ),
        ));
    }
    Ok(FrameSize::new(width, height)?)
}

fn bad(param: &str, reason: &str) -> SourceError {
    SourceError::Setting {
        param: param.to_owned(),
        reason: reason.to_owned(),
    }
}

/// A colour setting as straight sRGB RGBA bytes.
fn color(settings: &Settings, id: &str) -> Result<[u8; 4], SourceError> {
    setting(settings, id)?
        .as_str()
        .and_then(parse_color)
        .ok_or_else(|| bad(id, "must be a colour as `#rrggbb` or `#rrggbbaa`"))
}

/// The `smoothing` setting: how much of the last frame each new one keeps, 0 to 0.99.
fn smoothing(settings: &Settings) -> Result<f32, SourceError> {
    setting(settings, "smoothing")?
        .as_f64()
        .filter(|s| (0.0..=MAX_SMOOTHING).contains(s))
        .map(|s| s as f32)
        .ok_or_else(|| {
            bad(
                "smoothing",
                &format!("must be a number from 0 to {MAX_SMOOTHING}"),
            )
        })
}

/// The most smoothing: at 1 the picture would never change.
const MAX_SMOOTHING: f64 = 0.99;

/// A string setting.
fn string<'s>(settings: &'s Settings, id: &str) -> Result<&'s str, SourceError> {
    setting(settings, id)?
        .as_str()
        .ok_or_else(|| bad(id, "must be a string"))
}

/// Eases `old` toward `new` over `ticks` frame ticks (at least one): a rise shows at
/// once, and a fall keeps `smoothing` of the gap each tick.
fn ease(old: f32, new: f32, smoothing: f32, ticks: u64) -> f32 {
    if new >= old {
        return new;
    }
    let keep = smoothing.powi(i32::try_from(ticks).unwrap_or(i32::MAX));
    new + (old - new) * keep
}

#[cfg(test)]
mod tests;
