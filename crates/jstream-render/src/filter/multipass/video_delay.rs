//! `video_delay`: holds the picture back by `delay_ms`, to line video up with audio.
//!
//! The pass keeps a ring of GPU frames between calls, each a copy of an input stamped
//! with its media-clock time. It outputs the newest held frame at or before
//! `now - delay`, or the oldest held while the ring fills.
//!
//! **Memory cap.** The ring holds at most `min(MAX_FRAMES, max_memory / frame bytes)`
//! frames, an `Rgba16Float` frame being `width * height * 8` bytes, and never fewer than
//! two. To keep the requested delay inside that many frames, it stores a frame only once
//! `delay / (capacity - 1)` has passed since the last one stored: under the cap that is
//! every frame; past it the held frames are spaced more coarsely, so the delay holds and
//! the delayed picture's frame rate drops. Frames older than the one being shown are
//! recycled for new copies, so a steady ring allocates nothing.
//!
//! The ring empties when the input's size changes and when the clock goes backwards (a
//! seek or loop), and is released when the delay is 0, which passes the input through.

use std::collections::VecDeque;

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use super::copy;
use crate::error::RenderError;
use crate::filter::params::{Read, filter, int};
use crate::filter::{Ctx, Pass};
use crate::image::Image;

pub(super) const KIND: &str = "video_delay";

/// The most frames the ring holds, whatever the memory cap: 5 s at 60 fps.
pub(super) const MAX_FRAMES: usize = 300;
/// Bytes per texel of a held frame (`Rgba16Float`).
const TEXEL_BYTES: u64 = 8;
/// Clock readings this close count as equal.
const EPSILON: f64 = 1e-6;

const DEFAULT_MEMORY_MB: i64 = 1024;

pub(super) fn descriptor() -> KindDescriptor {
    filter(
        KIND,
        "Video delay",
        vec![
            int("delay_ms", "Delay", 0, 5000, 0, "ms"),
            ParamDescriptor {
                description: Some(
                    "The most GPU memory the held frames take. Past it, frames are held \
                     farther apart: the delay holds and the delayed picture's frame rate \
                     drops."
                        .into(),
                ),
                advanced: true,
                ..int(
                    "max_memory_mb",
                    "Memory limit",
                    64,
                    16384,
                    DEFAULT_MEMORY_MB,
                    "MB",
                )
            },
        ],
    )
}

/// How many `width` x `height` frames fit in `max_bytes`, within `2..=MAX_FRAMES`.
pub(super) fn capacity(width: u32, height: u32, max_bytes: u64) -> usize {
    let frame = u64::from(width) * u64::from(height) * TEXEL_BYTES;
    let fit = max_bytes / frame.max(1);
    usize::try_from(fit)
        .unwrap_or(usize::MAX)
        .clamp(2, MAX_FRAMES)
}

/// Held frames, oldest first, each with its clock time, and spare ones to reuse. The
/// payload is generic so the selection is tested without a GPU.
#[derive(Debug)]
pub(super) struct Ring<T> {
    held: VecDeque<(f64, T)>,
    spare: Vec<T>,
    size: (u32, u32),
}

impl<T> Default for Ring<T> {
    fn default() -> Self {
        Self {
            held: VecDeque::new(),
            spare: Vec::new(),
            size: (0, 0),
        }
    }
}

impl<T> Ring<T> {
    /// Drops every frame, held or spare.
    pub(super) fn clear(&mut self) {
        self.held.clear();
        self.spare.clear();
    }

    /// Readies the ring for a frame of `size` at `now`, keeping at most `capacity`
    /// frames: a new size empties it, an earlier time recycles what it holds.
    pub(super) fn prepare(&mut self, size: (u32, u32), now: f64, capacity: usize) {
        if size != self.size {
            self.clear();
            self.size = size;
        }
        if self.held.back().is_some_and(|(t, _)| now < *t - EPSILON) {
            self.spare.extend(self.held.drain(..).map(|(_, f)| f));
        }
        while self.held.len() + self.spare.len() > capacity {
            if self.spare.pop().is_none() {
                self.held.pop_front();
            }
        }
    }

    /// Whether a frame at `now` should be stored, `spacing` seconds after the last.
    pub(super) fn wants(&self, now: f64, spacing: f64) -> bool {
        self.held
            .back()
            .is_none_or(|(t, _)| now - t >= spacing - EPSILON)
    }

    /// A frame to overwrite for a new copy, making room in a ring of `capacity`: a
    /// spare one, or the oldest held one when the ring is full. `None` means allocate.
    pub(super) fn reuse(&mut self, capacity: usize) -> Option<T> {
        if self.held.len() >= capacity
            && let Some((_, frame)) = self.held.pop_front()
        {
            return Some(frame);
        }
        self.spare.pop()
    }

    /// Holds `frame`, copied at `now`.
    pub(super) fn push(&mut self, now: f64, frame: T) {
        self.held.push_back((now, frame));
    }

    /// The newest held frame at or before `target`, or the oldest held. Older frames
    /// will not be shown again (the clock only moves forward between resets), so they
    /// become spares.
    pub(super) fn select(&mut self, target: f64) -> Option<&T> {
        let newer = self
            .held
            .iter()
            .position(|(t, _)| *t > target + EPSILON)
            .unwrap_or(self.held.len());
        let keep = newer.saturating_sub(1);
        self.spare
            .extend(self.held.drain(..keep).map(|(_, frame)| frame));
        self.held.front().map(|(_, frame)| frame)
    }

    /// Held frames' times, oldest first.
    #[cfg(test)]
    fn times(&self) -> Vec<f64> {
        self.held.iter().map(|(t, _)| *t).collect()
    }

    #[cfg(test)]
    fn total(&self) -> usize {
        self.held.len() + self.spare.len()
    }
}

pub(super) struct VideoDelay {
    /// Seconds.
    delay: f64,
    max_bytes: u64,
    ring: Ring<Image>,
}

impl VideoDelay {
    pub(super) fn new(settings: &Settings) -> Self {
        let mut pass = Self {
            delay: 0.0,
            max_bytes: 0,
            ring: Ring::default(),
        };
        pass.update(settings);
        pass
    }
}

impl Pass for VideoDelay {
    fn update(&mut self, settings: &Settings) {
        let read = Read(settings);
        self.delay = read.i64("delay_ms", 0).clamp(0, 5000) as f64 / 1000.0;
        let mb = read.i64("max_memory_mb", DEFAULT_MEMORY_MB).max(1) as u64;
        self.max_bytes = mb.saturating_mul(1 << 20);
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        if self.delay <= 0.0 {
            self.ring.clear();
            return copy(ctx, input, output);
        }
        let (w, h) = (input.width(), input.height());
        let now = ctx.clock().seconds;
        let capacity = capacity(w, h, self.max_bytes);
        self.ring.prepare((w, h), now, capacity);
        if self.ring.wants(now, self.delay / (capacity - 1) as f64) {
            let frame = match self.ring.reuse(capacity) {
                Some(frame) => frame,
                None => ctx.layer(w, h)?,
            };
            copy(ctx, input, &frame)?;
            self.ring.push(now, frame);
        }
        match self.ring.select(now - self.delay) {
            Some(frame) => copy(ctx, frame, output),
            None => copy(ctx, input, output),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_FRAMES, Ring, capacity};
    use crate::filter::testing::{filter, run};
    use crate::tests::{near, open, solid};
    use serde_json::json;

    /// Feeds frames `0, 1, 2, ...` at `period` seconds apart through a ring of
    /// `capacity` with `delay`, returning the frame shown each time.
    fn feed(capacity: usize, delay: f64, period: f64, count: usize) -> (Vec<u32>, Ring<u32>) {
        let mut ring = Ring::default();
        let mut shown = Vec::new();
        for i in 0..count {
            let now = i as f64 * period;
            ring.prepare((8, 8), now, capacity);
            if ring.wants(now, delay / (capacity - 1) as f64) {
                let _ = ring.reuse(capacity);
                ring.push(now, i as u32);
            }
            shown.push(*ring.select(now - delay).expect("a frame is held"));
            assert!(ring.total() <= capacity);
        }
        (shown, ring)
    }

    #[test]
    fn capacity_follows_the_memory_cap_within_bounds() {
        // 1920x1080 is 16.6 MB a frame: 1 GiB holds 64.
        assert_eq!(capacity(1920, 1080, 1 << 30), 64);
        assert_eq!(capacity(8, 8, 1 << 30), MAX_FRAMES);
        assert_eq!(capacity(1920, 1080, 1), 2);
        assert_eq!(capacity(8, 8, 512 * 5), 5);
    }

    #[test]
    fn under_the_cap_every_frame_is_held_and_the_output_lags_by_the_delay() {
        let (shown, ring) = feed(100, 0.3, 0.1, 12);
        // Filling: the oldest frame; then the frame 3 periods back.
        assert_eq!(shown, [0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8]);
        // Only the shown frame and newer are held.
        assert_eq!(ring.times().len(), 4);
    }

    #[test]
    fn past_the_cap_frames_are_spaced_out_and_the_delay_holds() {
        // Four frames for a 0.3 s delay: one every 0.1 s, while frames come every 0.05.
        let (shown, ring) = feed(4, 0.3, 0.05, 20);
        let times = ring.times();
        assert!(
            times.windows(2).all(|w| w[1] - w[0] >= 0.1 - 1e-9),
            "{times:?}"
        );
        // Every shown frame is at least the delay old once the ring has filled.
        for (i, frame) in shown.iter().enumerate().skip(6) {
            let lag = (i - *frame as usize) as f64 * 0.05;
            assert!(
                (0.3 - 1e-9..0.4).contains(&lag),
                "frame {i}: showed {frame}"
            );
        }
    }

    #[test]
    fn a_new_size_empties_the_ring_and_a_rewind_recycles_it() {
        let mut ring: Ring<u32> = Ring::default();
        ring.prepare((8, 8), 0.0, 10);
        ring.push(0.0, 1);
        ring.prepare((8, 8), 1.0, 10);
        ring.push(1.0, 2);
        ring.prepare((8, 8), 0.5, 10);
        assert!(ring.times().is_empty());
        assert_eq!(ring.total(), 2, "kept as spares");
        assert_eq!(ring.reuse(10), Some(2));
        ring.prepare((16, 8), 0.6, 10);
        assert_eq!(ring.total(), 0);
        // A smaller cap drops spares first.
        for i in 0..4 {
            ring.push(f64::from(i), i);
        }
        let _ = ring.select(2.0);
        assert_eq!((ring.times().len(), ring.total()), (2, 4));
        ring.prepare((16, 8), 4.0, 2);
        assert_eq!((ring.times().len(), ring.total()), (2, 2));
    }

    #[test]
    fn the_gpu_output_lags_the_clock_by_the_delay() {
        let Some(mut opened) = open() else {
            return;
        };
        let colour = |i: usize| [(i * 20) as u8, 255 - (i * 20) as u8, 0, 255];
        let frames: Vec<_> = (0..10)
            .map(|i| solid(&opened.compositor, 8, 8, colour(i)))
            .collect();
        let mut delay = filter("video_delay", json!({ "delay_ms": 300 }));
        for (i, frame) in frames.iter().enumerate() {
            let out = run(&mut opened, &mut delay, frame, i as f64 * 0.1);
            near(&out, 4, 4, colour(i.saturating_sub(3)), 1);
        }
        // Back to 0 ms: the input passes straight through.
        delay.set_settings(&crate::filter::testing::settings(
            "video_delay",
            json!({ "delay_ms": 0 }),
        ));
        let out = run(&mut opened, &mut delay, &frames[9], 1.0);
        near(&out, 4, 4, colour(9), 1);
        // A new size starts over: the first frame shows at once.
        let mut delay = filter("video_delay", json!({ "delay_ms": 300 }));
        let _ = run(&mut opened, &mut delay, &frames[0], 0.0);
        let small = solid(&opened.compositor, 4, 4, colour(5));
        let out = run(&mut opened, &mut delay, &small, 0.1);
        assert_eq!((out.width(), out.height()), (4, 4));
        near(&out, 2, 2, colour(5), 1);
    }
}
