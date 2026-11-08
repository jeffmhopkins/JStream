//! `quality.probe` (ADR-0132 §3, as ADR-0143 refines it): PSNR and SSIM between the
//! frames an output's encoder session is given and the pictures the platform's decoder
//! makes of the packets it sends.
//!
//! **The reference frames** come from the render thread, through the session's
//! [`ProbeTap`]. While a probe is armed, each frame the thread pushes to the session is
//! also copied to the probe with its `pts`: the RGBA8 frame itself for a session on the
//! CPU path, and for one on the GPU path the compositor's readback of the same draw,
//! which the thread reads back only while a probe is armed. A readback lands a tick or
//! two after its draw with the time it was drawn for, which is the `pts` the GPU frame
//! of that draw went out with, so the two meet by `pts`. Nothing is copied while no
//! probe is armed, and the render thread never blocks or frees a copy: it copies only
//! against credits the probe grants, under a lock it only tries, and the probe disarms
//! the tap before it lets go of the receiving end.
//!
//! **The packets** come from a subscription to the session, which starts at the next
//! keyframe and asks for one. A `jstream-quality-probe` thread decodes them with the
//! platform's decoder (NV12 out, software preferred), matches each picture with its
//! reference by `pts` (in microseconds), and measures the pair as NV12 in BT.709 limited
//! range at the encoded size, the reference converted as the CPU encoders convert it
//! ([`jstream_platform::nv12`]). Pictures and references that find no partner within
//! [`WINDOW`] of each other are let go of, oldest first.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use jstream_encode::{StreamEnd, Subscription};
use jstream_media::{
    ColorInfo, FrameSize, MediaTime, PixelFormat, Rounding, Timebase, VideoFrame, VideoFrameInfo,
};
use jstream_platform::nv12::{RgbOrder, rgb_to_nv12};
use jstream_platform::{
    Codecs, HardwarePreference, PlatformError, VideoDecoder, VideoDecoderConfig, VideoSurface,
};
use jstream_protocol::{
    CallResult, Finite32, FrameQuality, MAX_PSNR_DB, ProtocolError, QualityProbeInfo, QueryResult,
};

use crate::lock;
use crate::snapshot::{SnapshotReply, reply_pair};

/// How long a probe waits for its frames before it answers `action_failed`. An encoder
/// that can't force a keyframe makes it wait for its next regular one, at most a few
/// seconds.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// The most reference frames, or decoded pictures, a probe holds while it waits for
/// their partners.
const WINDOW: usize = 16;
/// The most copies the render thread queues for a probe between two of its reads.
const CREDITS: u32 = 4;
/// How long the probe thread waits for a packet before it looks for references again.
const POLL: Duration = Duration::from_millis(10);

/// A frame the render thread pushed to a probed session, as RGBA8.
pub(crate) struct Reference {
    pts: MediaTime,
    size: FrameSize,
    rgba: Vec<u8>,
}

/// Where the render thread copies a session's frames while a probe is armed. One per
/// encoder session, shared by its render-thread tap and the engine.
#[derive(Debug, Default)]
pub(crate) struct ProbeTap {
    armed: AtomicBool,
    slot: Mutex<Option<Slot>>,
}

#[derive(Debug)]
struct Slot {
    frames: Sender<Reference>,
    credits: u32,
}

impl ProbeTap {
    /// Whether a probe is armed: the render thread reads back a GPU session's frames
    /// only then.
    pub(crate) fn armed(&self) -> bool {
        self.armed.load(Ordering::Acquire)
    }

    /// Copies `rgba`, the `size` frame pushed at `pts`, to the armed probe. Never
    /// blocks: does nothing unless a probe is armed, has credit, and the lock is free.
    /// The copy can't come back to this thread: the probe empties the slot before it
    /// drops the receiver.
    pub(crate) fn offer(&self, size: FrameSize, pts: MediaTime, rgba: &[u8]) {
        if !self.armed() {
            return;
        }
        let Ok(mut slot) = self.slot.try_lock() else {
            return;
        };
        let Some(slot) = slot.as_mut() else {
            return;
        };
        if slot.credits == 0 {
            return;
        }
        slot.credits -= 1;
        let _ = slot.frames.send(Reference {
            pts,
            size,
            rgba: rgba.to_vec(),
        });
    }

    /// Arms the tap for one probe, or `None` while another probe holds it.
    fn arm(self: &Arc<Self>) -> Option<Armed> {
        let mut slot = lock(&self.slot);
        if slot.is_some() {
            return None;
        }
        let (frames, receiver) = mpsc::channel();
        *slot = Some(Slot {
            frames,
            credits: CREDITS,
        });
        self.armed.store(true, Ordering::Release);
        Some(Armed {
            tap: Arc::clone(self),
            receiver: Some(receiver),
        })
    }
}

/// One probe's hold on a [`ProbeTap`]. Dropping it disarms the tap first, and only then
/// drops the receiver, so no copy is ever sent into a closed channel.
struct Armed {
    tap: Arc<ProbeTap>,
    receiver: Option<Receiver<Reference>>,
}

impl Armed {
    /// The references queued since the last call, granting the render thread its
    /// credits again.
    fn take(&self, out: &mut Vec<Reference>) {
        if let Some(receiver) = &self.receiver {
            out.extend(receiver.try_iter());
        }
        if let Some(slot) = lock(&self.tap.slot).as_mut() {
            slot.credits = CREDITS;
        }
    }
}

impl Drop for Armed {
    fn drop(&mut self) {
        self.tap.armed.store(false, Ordering::Release);
        *lock(&self.tap.slot) = None;
        self.receiver = None;
    }
}

/// What a probe of one stream needs, read under the engine's locks.
pub(crate) struct ProbeJob {
    pub(crate) tap: Arc<ProbeTap>,
    pub(crate) packets: Subscription,
    pub(crate) codecs: Arc<dyn Codecs>,
    pub(crate) size: FrameSize,
    pub(crate) encoder: String,
    pub(crate) hardware: bool,
    pub(crate) frames: u32,
}

/// Runs `job` on a `jstream-quality-probe` thread and answers with its measurement.
/// Arms the tap first, so a second probe of the same session waits for nothing: it
/// fails at once.
pub(crate) fn start(job: ProbeJob) -> SnapshotReply {
    let (reply, waiting) = reply_pair();
    let Some(armed) = job.tap.arm() else {
        reply.send(Err(ProtocolError::ActionFailed {
            reason: "another quality probe of this stream is running".into(),
        }));
        return waiting;
    };
    let spawned = thread::Builder::new()
        .name("jstream-quality-probe".into())
        .spawn(move || {
            let result = probe(&job, &armed).map(|probe| CallResult::Query {
                result: QueryResult::QualityProbe { probe },
            });
            drop(armed);
            reply.send(result);
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "could not start a thread for the quality probe");
    }
    waiting
}

fn failed(reason: impl Into<String>) -> ProtocolError {
    ProtocolError::ActionFailed {
        reason: reason.into(),
    }
}

/// The probe itself: decodes the stream's packets until `job.frames` pictures have met
/// their references, or [`PROBE_TIMEOUT`] passes.
fn probe(job: &ProbeJob, armed: &Armed) -> Result<QualityProbeInfo, ProtocolError> {
    let codec = job.packets.info().codec.codec;
    let mut decoder = job
        .codecs
        .video_decoder(&VideoDecoderConfig {
            codec,
            output_format: PixelFormat::Nv12,
            hardware: HardwarePreference::Avoid,
        })
        .map_err(|err| {
            failed(format!(
                "this platform has no decoder for the stream's {codec:?}: {err}"
            ))
        })?;
    // The probe asks for a keyframe of its own: the subscription's request came before
    // the tap was armed.
    job.packets.request_keyframe();
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let want = job.frames as usize;
    let mut matcher = Matcher::default();
    let mut references = Vec::new();
    let mut pictures = Vec::new();
    let mut measured: Vec<FrameQuality> = Vec::new();
    while measured.len() < want {
        if Instant::now() >= deadline {
            return Err(failed(format!(
                "only {} of {want} frames came back from the decoder within {}s",
                measured.len(),
                PROBE_TIMEOUT.as_secs()
            )));
        }
        armed.take(&mut references);
        for reference in references.drain(..) {
            if let Some(pair) = matcher.reference(reference)? {
                measured.push(pair);
            }
        }
        match job.packets.recv_timeout(POLL) {
            Ok(Some(packet)) => feed(decoder.as_mut(), &packet, &mut pictures)?,
            Ok(None) => {}
            Err(StreamEnd::Finished | StreamEnd::Closed) => {
                return Err(failed("the output stopped before the probe ended"));
            }
            Err(StreamEnd::Failed(err)) => {
                return Err(failed(format!("the output's encoder failed: {err}")));
            }
        }
        decoder.poll(&mut pictures).map_err(decode_failed)?;
        for picture in pictures.drain(..) {
            if let Some(pair) = matcher.picture(picture)? {
                measured.push(pair);
            }
        }
    }
    measured.truncate(want);
    measured.sort_by_key(|frame| frame.pts_us);
    let mean = |pick: fn(&FrameQuality) -> f32| {
        let sum: f32 = measured.iter().map(pick).sum();
        finite(sum / measured.len() as f32)
    };
    Ok(QualityProbeInfo {
        encoder: job.encoder.clone(),
        hardware: job.hardware,
        decoder: decoder.info().name.clone(),
        width: job.size.width(),
        height: job.size.height(),
        psnr_y: mean(|f| f.psnr_y.get()),
        psnr: mean(|f| f.psnr.get()),
        ssim_y: mean(|f| f.ssim_y.get()),
        frames: measured,
    })
}

/// Hands `packet` to the decoder, taking its pictures out first while it is busy.
fn feed(
    decoder: &mut dyn VideoDecoder,
    packet: &jstream_media::Packet,
    pictures: &mut Vec<VideoSurface>,
) -> Result<(), ProtocolError> {
    for _ in 0..100 {
        if decoder.can_accept() {
            return decoder.decode(packet).map_err(decode_failed);
        }
        decoder.poll(pictures).map_err(decode_failed)?;
        thread::sleep(Duration::from_millis(1));
    }
    Err(failed("the decoder took no packet for 100 ms"))
}

fn decode_failed(err: PlatformError) -> ProtocolError {
    failed(format!("decoding the stream failed: {err}"))
}

fn finite(value: f32) -> Finite32 {
    Finite32::new(value).unwrap_or_default()
}

/// A frame's time in microseconds, the key a picture and its reference meet on.
fn micros(pts: MediaTime) -> Option<i64> {
    pts.rescale(Timebase::MICROS, Rounding::Nearest)
        .ok()
        .map(MediaTime::ticks)
}

/// The references and pictures still waiting for their partners, by `pts` in
/// microseconds, each at most [`WINDOW`].
#[derive(Default)]
struct Matcher {
    references: BTreeMap<i64, VideoFrame>,
    pictures: BTreeMap<i64, VideoFrame>,
}

impl Matcher {
    /// Takes a reference: its measurement when its picture has come, else it waits.
    fn reference(&mut self, reference: Reference) -> Result<Option<FrameQuality>, ProtocolError> {
        let Some(at) = micros(reference.pts) else {
            return Ok(None);
        };
        let frame = to_nv12(&reference)?;
        if let Some(picture) = self.pictures.remove(&at) {
            return measure(at, &frame, &picture).map(Some);
        }
        keep(&mut self.references, at, frame);
        Ok(None)
    }

    /// Takes a decoded picture: its measurement when its reference has come, else it
    /// waits.
    fn picture(&mut self, picture: VideoSurface) -> Result<Option<FrameQuality>, ProtocolError> {
        let VideoSurface::Cpu(picture) = picture else {
            return Err(failed(
                "the decoder made a GPU picture, which a probe can't read",
            ));
        };
        let Some(at) = micros(picture.info().pts) else {
            return Ok(None);
        };
        if let Some(reference) = self.references.remove(&at) {
            return measure(at, &reference, &picture).map(Some);
        }
        keep(&mut self.pictures, at, picture);
        Ok(None)
    }
}

/// Keeps `frame` at `at`, letting go of the oldest beyond [`WINDOW`].
fn keep(frames: &mut BTreeMap<i64, VideoFrame>, at: i64, frame: VideoFrame) {
    frames.insert(at, frame);
    while frames.len() > WINDOW {
        frames.pop_first();
    }
}

/// A reference frame as NV12 in BT.709 limited range, as the CPU encoders convert it.
fn to_nv12(reference: &Reference) -> Result<VideoFrame, ProtocolError> {
    let info = VideoFrameInfo {
        size: reference.size,
        format: PixelFormat::Nv12,
        color: ColorInfo::default(),
        pts: reference.pts,
    };
    let mut frame = VideoFrame::zeroed(info).map_err(|err| failed(err.to_string()))?;
    let stride = reference.size.width() as usize * 4;
    rgb_to_nv12(&reference.rgba, stride, RgbOrder::Rgba, &mut frame)
        .map_err(|err| failed(format!("converting a reference frame: {err}")))?;
    Ok(frame)
}

/// The PSNR and SSIM of `picture` against `reference`, both NV12 of the same size.
fn measure(
    at: i64,
    reference: &VideoFrame,
    picture: &VideoFrame,
) -> Result<FrameQuality, ProtocolError> {
    let (size, got) = (reference.info().size, picture.info());
    if got.size != size || got.format != PixelFormat::Nv12 {
        return Err(failed(format!(
            "the decoder made a {:?} {}x{} picture of a {}x{} stream",
            got.format,
            got.size.width(),
            got.size.height(),
            size.width(),
            size.height()
        )));
    }
    let (width, height) = (size.width(), size.height());
    let luma = Plane::of(reference, picture, 0, width, height)?;
    let chroma = Plane::of(reference, picture, 1, width, height / 2)?;
    let luma_sse = luma.sse();
    let samples = luma.samples() + chroma.samples();
    Ok(FrameQuality {
        pts_us: at,
        psnr_y: finite(psnr(luma_sse, luma.samples())),
        psnr: finite(psnr(luma_sse + chroma.sse(), samples)),
        ssim_y: finite(luma.ssim()),
    })
}

/// One plane of a reference and its picture, row by row.
struct Plane<'a> {
    a: Vec<&'a [u8]>,
    b: Vec<&'a [u8]>,
    width: usize,
}

impl<'a> Plane<'a> {
    /// Plane `index`, `width` bytes by `rows`, of both frames.
    fn of(
        a: &'a VideoFrame,
        b: &'a VideoFrame,
        index: usize,
        width: u32,
        rows: u32,
    ) -> Result<Self, ProtocolError> {
        let read = |frame: &'a VideoFrame| {
            (0..rows)
                .map(|y| {
                    frame
                        .row(index, y)
                        .and_then(|row| row.get(..width as usize))
                        .ok_or_else(|| failed("a decoded picture is shorter than its size"))
                })
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(Self {
            a: read(a)?,
            b: read(b)?,
            width: width as usize,
        })
    }

    fn samples(&self) -> u64 {
        (self.width * self.a.len()) as u64
    }

    /// The sum of squared differences.
    fn sse(&self) -> u64 {
        self.a
            .iter()
            .zip(&self.b)
            .flat_map(|(a, b)| a.iter().zip(b.iter()))
            .map(|(&a, &b)| {
                let d = u64::from(a.abs_diff(b));
                d * d
            })
            .sum()
    }

    /// The mean SSIM over 8 × 8 windows, 4 samples apart, with the usual constants for
    /// 8-bit samples. A plane smaller than a window is one window of its size.
    fn ssim(&self) -> f32 {
        const C1: f64 = (0.01 * 255.0) * (0.01 * 255.0);
        const C2: f64 = (0.03 * 255.0) * (0.03 * 255.0);
        let (w, h) = (self.width, self.a.len());
        let (win_w, win_h) = (w.min(8), h.min(8));
        if win_w == 0 || win_h == 0 {
            return 1.0;
        }
        let starts = |len: usize, win: usize| (0..=len - win).step_by(4);
        let mut total = 0.0;
        let mut windows = 0u32;
        for y0 in starts(h, win_h) {
            for x0 in starts(w, win_w) {
                let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
                for y in y0..y0 + win_h {
                    for x in x0..x0 + win_w {
                        let (a, b) = (f64::from(self.a[y][x]), f64::from(self.b[y][x]));
                        sa += a;
                        sb += b;
                        saa += a * a;
                        sbb += b * b;
                        sab += a * b;
                    }
                }
                let n = (win_w * win_h) as f64;
                let (ma, mb) = (sa / n, sb / n);
                let va = saa / n - ma * ma;
                let vb = sbb / n - mb * mb;
                let cov = sab / n - ma * mb;
                total += ((2.0 * ma * mb + C1) * (2.0 * cov + C2))
                    / ((ma * ma + mb * mb + C1) * (va + vb + C2));
                windows += 1;
            }
        }
        (total / f64::from(windows)) as f32
    }
}

/// PSNR in dB for 8-bit samples, at most [`MAX_PSNR_DB`] (identical samples).
fn psnr(sse: u64, samples: u64) -> f32 {
    if sse == 0 || samples == 0 {
        return MAX_PSNR_DB;
    }
    let mse = sse as f64 / samples as f64;
    let db = 10.0 * (255.0 * 255.0 / mse).log10();
    (db as f32).min(MAX_PSNR_DB)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(w: u32, h: u32) -> FrameSize {
        FrameSize::new(w, h).unwrap()
    }

    fn reference(w: u32, h: u32, at: i64, shade: impl Fn(u32, u32) -> u8) -> Reference {
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let v = shade(x, y);
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        Reference {
            pts: MediaTime::new(at, Timebase::MICROS),
            size: size(w, h),
            rgba,
        }
    }

    #[test]
    fn identical_pictures_measure_the_ceiling() {
        let r = reference(32, 16, 0, |x, y| ((x * 7 + y * 3) % 256) as u8);
        let frame = to_nv12(&r).unwrap();
        let q = measure(0, &frame, &frame).unwrap();
        assert_eq!(q.psnr_y.get(), MAX_PSNR_DB);
        assert_eq!(q.psnr.get(), MAX_PSNR_DB);
        assert!((q.ssim_y.get() - 1.0).abs() < 1e-6, "{q:?}");
    }

    #[test]
    fn a_uniform_error_gives_the_textbook_psnr() {
        let a = to_nv12(&reference(16, 16, 0, |_, _| 100)).unwrap();
        let mut b = a.clone();
        for y in 0..16 {
            for v in b.row_mut(0, y).unwrap() {
                *v += 5;
            }
        }
        let q = measure(0, &a, &b).unwrap();
        // MSE 25 on luma: 10·log10(255²/25) ≈ 34.15 dB.
        assert!((q.psnr_y.get() - 34.151).abs() < 0.01, "{q:?}");
        // The chroma is untouched, a third of the samples: MSE 25·2/3.
        assert!((q.psnr.get() - 35.912).abs() < 0.01, "{q:?}");
        assert!(q.ssim_y.get() < 1.0 && q.ssim_y.get() > 0.9, "{q:?}");
    }

    #[test]
    fn noise_lowers_ssim_more_than_a_flat_shift() {
        let a = to_nv12(&reference(32, 32, 0, |x, y| ((x * 8) ^ (y * 8)) as u8)).unwrap();
        let mut noisy = a.clone();
        for y in 0..32 {
            for (x, v) in noisy.row_mut(0, y).unwrap().iter_mut().enumerate() {
                *v = if (x + y as usize).is_multiple_of(2) {
                    v.saturating_add(12)
                } else {
                    v.saturating_sub(12)
                };
            }
        }
        let q = measure(0, &a, &noisy).unwrap();
        assert!(q.ssim_y.get() < 0.95, "{q:?}");
    }

    #[test]
    fn a_picture_meets_its_reference_whichever_comes_first() {
        let mut matcher = Matcher::default();
        let r = reference(16, 8, 40_000, |_, _| 128);
        let picture = to_nv12(&r).unwrap();
        // The picture first, then its reference.
        assert!(
            matcher
                .picture(VideoSurface::Cpu(picture.clone()))
                .unwrap()
                .is_none()
        );
        let q = matcher.reference(r).unwrap().expect("matched");
        assert_eq!(q.pts_us, 40_000);
        // The reference first, then its picture.
        let r = reference(16, 8, 80_000, |_, _| 128);
        let mut late = to_nv12(&r).unwrap();
        late.set_pts(MediaTime::new(80_000, Timebase::MICROS));
        assert!(matcher.reference(r).unwrap().is_none());
        let q = matcher
            .picture(VideoSurface::Cpu(late))
            .unwrap()
            .expect("matched");
        assert_eq!(q.pts_us, 80_000);
        assert!(matcher.references.is_empty() && matcher.pictures.is_empty());
    }

    #[test]
    fn unmatched_frames_are_let_go_oldest_first() {
        let mut matcher = Matcher::default();
        for n in 0..(WINDOW as i64 + 4) {
            let r = reference(4, 4, n * 1000, |_, _| 0);
            assert!(matcher.reference(r).unwrap().is_none());
        }
        assert_eq!(matcher.references.len(), WINDOW);
        assert_eq!(matcher.references.keys().next(), Some(&4000));
    }

    #[test]
    fn a_picture_of_the_wrong_size_fails_the_probe() {
        let a = to_nv12(&reference(16, 16, 0, |_, _| 0)).unwrap();
        let b = to_nv12(&reference(8, 8, 0, |_, _| 0)).unwrap();
        let err = measure(0, &a, &b).unwrap_err();
        assert!(matches!(err, ProtocolError::ActionFailed { .. }), "{err:?}");
    }

    #[test]
    fn the_tap_copies_only_while_armed_and_against_credit() {
        let tap = Arc::new(ProbeTap::default());
        let at = MediaTime::new(0, Timebase::MICROS);
        // Disarmed: nothing is copied.
        tap.offer(size(2, 2), at, &[0; 16]);
        let armed = tap.arm().expect("free");
        assert!(tap.arm().is_none(), "one probe at a time");
        assert!(tap.armed());
        for _ in 0..CREDITS + 3 {
            tap.offer(size(2, 2), at, &[0; 16]);
        }
        let mut got = Vec::new();
        armed.take(&mut got);
        assert_eq!(
            got.len(),
            CREDITS as usize,
            "copies stop when credit runs out"
        );
        tap.offer(size(2, 2), at, &[0; 16]);
        armed.take(&mut got);
        assert_eq!(
            got.len(),
            CREDITS as usize + 1,
            "taking grants credit again"
        );
        drop(armed);
        assert!(!tap.armed());
        tap.offer(size(2, 2), at, &[0; 16]);
        assert!(tap.arm().is_some(), "free again once the probe ends");
    }
}
