//! One input's processing on its audio thread: continuity, drift tracking, the
//! timing-error loop and resampling into the input's ring.
//!
//! For every device packet ([`InputProducer::push_packet`]):
//!
//! 1. **Continuity** ([`continuity`](super::continuity)): a gap the clock agrees with is
//!    filled with silence; a backwards position, a gap longer than 2 s or one the clock
//!    disagrees with restarts the mapping. So does a `discontinuity` flag (except on the
//!    first packet, where WASAPI sets it routinely).
//! 2. **Drift fit** ([`drift`](super::drift)): the packet's stamp is fitted against its
//!    device position. A stamp more than 20 ms off the line (once the segment has 100
//!    points) is an outlier: it is not fitted and not used for timing. A run of 50
//!    outliers means the line moved, so the segment restarts with the 50th. The first
//!    stamp after a `discontinuity` flag (on that packet or, if it had none, a later one)
//!    is never an outlier: when it is off the line, the segment restarts with it at once.
//! 3. **Anchoring.** An input is placed on the engine time line from a stamp: its next
//!    output frame `n = ceil(t · rate_out)` goes at the first engine frame at or after the
//!    stamp's time `t`, and the resampler's position is set so that this frame holds input
//!    time `n / rate_out` exactly. A restart re-anchors at the next accepted stamp: the
//!    ring jumps forward over the gap (which reads as silence), or, when the new
//!    position is behind what was already written, the overlap is skipped (counted as
//!    late). Until the first stamp, packets are dropped and counted.
//! 4. **Timing error and PI loop** ([`control`](super::control)): after each packet,
//!    `e = n_next / rate_out − T_in(p_next)`, where `T_in(p)` extrapolates the input
//!    position `p` from the last accepted stamp at the nominal rate. It is smoothed
//!    (0.5 s) and fed to the PI controller (0.02 Hz, damping 0.707, correction clamped to
//!    ±5000 ppm) that sets the resampler's step. An error beyond 20 ms, which the loop
//!    would take seconds to remove, re-anchors instead.
//! 5. **Resampling** ([`sinc`](super::sinc)) of the downmixed stereo
//!    ([`downmix`](super::downmix)) into the ring ([`ring`](super::ring)), through a
//!    fixed staging buffer.
//!
//! Unlike the spike, whose error was relative to its first measurement, the error here is
//! absolute: every input is steered onto the same engine time line, so inputs line up
//! with each other (and with video stamped on the same clock), not just with themselves.
//!
//! Nothing here allocates, locks, blocks or logs after construction (R-RT-001): state is
//! fixed-size, and counters go out through atomics.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jstream_media::AudioFormat;
use jstream_platform::{AudioInputHandler, AudioPacket, PlatformError};

use super::bus::Sends;
use super::continuity::{Continuity, PositionTracker};
use super::control::{Ema, Pi};
use super::downmix::Downmix;
use super::drift::DriftFit;
use super::retire::Retirer;
use super::ring::{FrameRing, RingWriter, WriteReport};
use super::shared::{AtomicF32, AtomicF64, Counter, GainParam, Meter};
use super::sinc::{Output, Resampler};

/// Closed-loop natural frequency of the timing controller (S3's tuning).
pub(crate) const LOOP_BANDWIDTH_HZ: f64 = 0.02;
/// Closed-loop damping.
pub(crate) const LOOP_DAMPING: f64 = 0.707;
/// Largest step correction (±0.5 %, 5000 ppm).
pub(crate) const MAX_CORRECTION: f64 = 0.005;
/// Time constant of the error smoother, in seconds.
pub(crate) const ERROR_SMOOTHING_S: f64 = 0.5;
/// A stamp further than this from the fitted line is an outlier, in seconds.
pub(crate) const OUTLIER_S: f64 = 0.020;
/// Points the current segment needs before outliers are rejected.
pub(crate) const OUTLIER_MIN_POINTS: u64 = 100;
/// Consecutive outliers that mean the line moved: the segment restarts.
pub(crate) const OUTLIER_RUN_RESET: u32 = 50;
/// A timing error beyond this re-anchors instead of being steered out, in seconds.
pub(crate) const REANCHOR_ERROR_S: f64 = 0.020;
/// Stamps at or beyond this many seconds from the clock's zero are rejected.
pub(crate) const MAX_STAMP_S: f64 = 1e13;
/// Frames staged between resampler and ring.
const STAGE_FRAMES: usize = 256;

/// What the audio thread and the control plane share for one input.
#[derive(Debug)]
pub(crate) struct InputShared {
    /// The input's fader and mute, applied before its sends.
    pub(crate) gain: GainParam,
    /// The activity gain, 0..=1, applied after the fader and meter and before the sends
    /// (ADR-0028's audio that follows the program).
    pub(crate) activity: AtomicF32,
    /// Whether the privacy screen mutes this input: while the mixer's privacy screen is
    /// on, it silences the input before its chain and applies an activity gain of 0
    /// (docs/adr/0027-program-master-and-output-chains.md).
    pub(crate) privacy_muted: AtomicBool,
    /// The input's row of the send matrix (post-fader).
    pub(crate) sends: Sends,
    pub(crate) meter: Meter,
    pub(crate) packets: Counter,
    pub(crate) discontinuities: Counter,
    pub(crate) gap_frames: Counter,
    pub(crate) resets: Counter,
    pub(crate) reanchors: Counter,
    pub(crate) outliers: Counter,
    pub(crate) timestamps_missing: Counter,
    pub(crate) unanchored_frames: Counter,
    pub(crate) late_frames: Counter,
    pub(crate) overrun_frames: Counter,
    pub(crate) underrun_frames: Counter,
    /// NaN until the fit has a slope.
    pub(crate) drift_ppm: AtomicF64,
    /// Timestamp jitter around the fitted line, seconds; NaN until measurable.
    pub(crate) jitter: AtomicF64,
    pub(crate) correction_ppm: AtomicF64,
    /// The smoothed timing error in seconds; NaN until measured.
    pub(crate) timing_error: AtomicF64,
    pub(crate) failed: AtomicBool,
}

impl InputShared {
    pub(crate) fn new() -> Self {
        Self {
            gain: GainParam::new(),
            activity: AtomicF32::new(1.0),
            privacy_muted: AtomicBool::new(false),
            sends: Sends::new(),
            meter: Meter::default(),
            packets: Counter::default(),
            discontinuities: Counter::default(),
            gap_frames: Counter::default(),
            resets: Counter::default(),
            reanchors: Counter::default(),
            outliers: Counter::default(),
            timestamps_missing: Counter::default(),
            unanchored_frames: Counter::default(),
            late_frames: Counter::default(),
            overrun_frames: Counter::default(),
            underrun_frames: Counter::default(),
            drift_ppm: AtomicF64::new(f64::NAN),
            jitter: AtomicF64::new(f64::NAN),
            correction_ppm: AtomicF64::new(0.0),
            timing_error: AtomicF64::new(f64::NAN),
            failed: AtomicBool::new(false),
        }
    }
}

/// The resampler's output side: a staging buffer in front of the ring writer, and the
/// engine frame of the next output.
#[derive(Debug)]
struct Sink {
    writer: Option<RingWriter>,
    stage: Box<[[f32; 2]]>,
    staged: usize,
    /// The engine frame the next output lands on.
    next_out: i64,
    report: WriteReport,
}

impl Sink {
    fn accept(&mut self, out: Output) {
        match out {
            Output::Frame(f) => {
                self.stage[self.staged] = f;
                self.staged += 1;
                self.next_out = self.next_out.saturating_add(1);
                if self.staged == self.stage.len() {
                    self.flush();
                }
            }
            Output::Silence(n) => {
                self.flush();
                let r = match self.writer.as_mut() {
                    Some(writer) => writer.fill_silence(n),
                    None => unreachable!("writer used after drop"),
                };
                self.add(r);
                self.next_out = self
                    .next_out
                    .saturating_add(i64::try_from(n).unwrap_or(i64::MAX));
            }
        }
    }

    fn flush(&mut self) {
        let n = self.staged;
        if n == 0 {
            return;
        }
        let r = match self.writer.as_mut() {
            Some(writer) => writer.write(&self.stage[..n]),
            None => unreachable!("writer used after drop"),
        };
        self.add(r);
        self.staged = 0;
    }

    /// Moves the next output to engine frame `n` (forward only, after the first time).
    fn seek(&mut self, n: i64) {
        self.flush();
        let (r, pos) = match self.writer.as_mut() {
            Some(writer) => (writer.seek(n), writer.position()),
            None => unreachable!("writer used after drop"),
        };
        self.add(r);
        self.next_out = pos.unwrap_or(n);
    }

    fn add(&mut self, r: WriteReport) {
        self.report.late += r.late;
        self.report.overwritten += r.overwritten;
    }
}

/// The audio-thread end of one mixer input. It implements the platform's
/// [`AudioInputHandler`], so it is handed straight to
/// [`AudioIo::open_input`](jstream_platform::AudioIo::open_input).
///
/// It must be built for the stream's actual format ([`Mixer::add_input`]): the
/// device's rate and channel layout, interleaved `f32`.
///
/// [`Mixer::add_input`]: crate::Mixer::add_input
#[derive(Debug)]
pub struct InputProducer {
    rate_in: f64,
    rate_out: f64,
    channels: usize,
    downmix: Downmix,
    resampler: Resampler,
    sink: Sink,
    positions: PositionTracker,
    fit: DriftFit,
    pos0: Option<u64>,
    outlier_run: u32,
    /// A `discontinuity` flag whose stamp has not been fitted yet: the next stamp
    /// restarts the segment instead of being rejected as an outlier.
    trust_next_stamp: bool,
    /// The last accepted stamp: (resampler input index, clock seconds).
    stamp: Option<(f64, f64)>,
    anchored: bool,
    need_anchor: bool,
    seen_packet: bool,
    pi: Pi,
    smooth: Ema,
    shared: Option<Arc<InputShared>>,
    /// Where `drop` hands the ring and the counters, so the audio thread frees neither.
    retirer: Retirer,
}

impl InputProducer {
    /// The caller ([`crate::Mixer::add_input`]) has checked the format and rates.
    pub(crate) fn new(
        format: AudioFormat,
        engine_rate: u32,
        ring: Arc<FrameRing>,
        shared: Arc<InputShared>,
        retirer: Retirer,
    ) -> Self {
        let rate = format.sample_rate();
        Self {
            rate_in: f64::from(rate),
            rate_out: f64::from(engine_rate),
            channels: format.channels(),
            downmix: Downmix::for_layout(format.layout()),
            resampler: Resampler::new(rate, engine_rate),
            sink: Sink {
                writer: Some(RingWriter::new(ring)),
                stage: vec![[0.0; 2]; STAGE_FRAMES].into_boxed_slice(),
                staged: 0,
                next_out: 0,
                report: WriteReport::default(),
            },
            positions: PositionTracker::new(rate),
            fit: DriftFit::default(),
            pos0: None,
            outlier_run: 0,
            trust_next_stamp: false,
            stamp: None,
            anchored: false,
            need_anchor: false,
            seen_packet: false,
            pi: Pi::new(LOOP_BANDWIDTH_HZ, LOOP_DAMPING, MAX_CORRECTION),
            smooth: Ema::new(ERROR_SMOOTHING_S),
            shared: Some(shared),
            retirer,
        }
    }

    fn shared(&self) -> &InputShared {
        match self.shared.as_ref() {
            Some(shared) => shared,
            // Drop takes the handle and does not call back in.
            None => unreachable!("shared used after drop"),
        }
    }

    /// The ring this producer writes. Tests attach a drop hook to see which thread
    /// frees it.
    #[cfg(test)]
    pub(crate) fn ring(&self) -> &FrameRing {
        match self.sink.writer.as_ref() {
            Some(writer) => writer.ring(),
            None => unreachable!("ring used after drop"),
        }
    }

    /// Processes one device packet. Called on the audio thread; does not allocate,
    /// lock, block or log.
    pub fn push_packet(&mut self, packet: &AudioPacket<'_>) {
        self.shared().packets.add(1);
        let frames = packet.samples.len() / self.channels;
        if frames == 0 {
            return;
        }
        let first = !self.seen_packet;
        self.seen_packet = true;
        // A stamp beyond 10^13 s (300,000 years) is garbage, and could not be placed
        // on the engine time line without overflowing it.
        let time = packet
            .time
            .map(|t| t.as_secs_f64())
            .filter(|t| t.is_finite() && t.abs() < MAX_STAMP_S);
        if time.is_none() {
            self.shared().timestamps_missing.add(1);
        }
        if packet.flags.discontinuity && !first {
            self.shared().discontinuities.add(1);
            self.need_anchor = true;
            self.trust_next_stamp = true;
        }

        let pos = packet.device_position;
        let mut processed = frames as u64;
        match self.positions.classify(pos, frames as u64, time) {
            Continuity::First => self.pos0 = Some(pos),
            Continuity::Continuous => {}
            Continuity::Gap(n) => {
                self.shared().gap_frames.add(n);
                if self.anchored {
                    let sink = &mut self.sink;
                    self.resampler.push_silence(n, &mut |o| sink.accept(o));
                    processed += n;
                }
            }
            Continuity::Reset(_) => {
                self.shared().resets.add(1);
                self.restart_segment();
            }
        }

        let accepted = time.filter(|&x| self.add_point(x, pos));
        if self.need_anchor || !self.anchored {
            // Without a usable stamp, an anchored input keeps its old mapping until one
            // arrives; an input that was never anchored drops the packet.
            if let Some(t) = accepted {
                self.anchor(t);
            }
            if !self.anchored {
                self.shared().unanchored_frames.add(frames as u64);
                self.publish();
                return;
            }
        }
        if let Some(t) = accepted {
            self.stamp = Some((self.resampler.input_count() as f64, t));
        }

        let sink = &mut self.sink;
        let samples = &packet.samples[..frames * self.channels];
        for frame in samples.chunks_exact(self.channels) {
            let st = if packet.flags.silent {
                [0.0; 2]
            } else {
                self.downmix.apply(frame)
            };
            self.resampler.push(st, &mut |o| sink.accept(o));
        }
        self.sink.flush();
        self.measure(processed);
        self.publish();
    }

    /// Restarts the drift fit's segment and asks for a re-anchor.
    fn restart_segment(&mut self) {
        self.fit.new_segment();
        self.outlier_run = 0;
        self.need_anchor = true;
    }

    /// Fits the stamp `x` of the packet at device position `pos`; returns whether it was
    /// accepted as timing information.
    fn add_point(&mut self, x: f64, pos: u64) -> bool {
        let pos0 = *self.pos0.get_or_insert(pos);
        let y = (pos as f64 - pos0 as f64) / self.rate_in;
        let trusted = std::mem::take(&mut self.trust_next_stamp);
        if self.fit.current_points() >= OUTLIER_MIN_POINTS
            && let Some(pred) = self.fit.predict_y(x)
            && (y - pred).abs() > OUTLIER_S
        {
            if trusted {
                // The device said the stream is discontinuous: the line moved here, so
                // this stamp starts a new segment without waiting for an outlier run.
                self.shared().resets.add(1);
                self.restart_segment();
                self.fit.add(x, y);
                return true;
            }
            self.shared().outliers.add(1);
            self.outlier_run += 1;
            if self.outlier_run < OUTLIER_RUN_RESET {
                return false;
            }
            // The line moved: this stamp starts a new segment.
            self.shared().resets.add(1);
            self.restart_segment();
        }
        self.outlier_run = 0;
        self.fit.add(x, y);
        true
    }

    /// Places the packet starting now (input index 0 after the reset) at clock time `t`
    /// onto the engine time line.
    fn anchor(&mut self, t: f64) {
        // |t| < MAX_STAMP_S and the engine rate is at most 192 kHz, so this is below
        // 2^61 and the conversion is exact in range.
        let target = (t * self.rate_out).ceil() as i64;
        self.sink.flush();
        let n = match self.sink.writer.as_ref().and_then(RingWriter::position) {
            Some(cur) if cur > target => {
                self.shared().late_frames.add((cur - target).unsigned_abs());
                cur
            }
            _ => target,
        };
        self.sink.seek(n);
        self.resampler
            .reset((n as f64 / self.rate_out - t) * self.rate_in);
        self.stamp = Some((0.0, t));
        self.smooth.reset();
        if self.anchored {
            self.shared().reanchors.add(1);
        }
        self.anchored = true;
        self.need_anchor = false;
    }

    /// Updates the timing error and the controller after `frames` input frames.
    fn measure(&mut self, frames: u64) {
        let Some((k, t)) = self.stamp else {
            return;
        };
        let t_in = t + (self.resampler.position() - k) / self.rate_in;
        let t_out = self.sink.next_out as f64 / self.rate_out;
        let e = t_out - t_in;
        if !e.is_finite() {
            return;
        }
        if e.abs() > REANCHOR_ERROR_S {
            self.need_anchor = true;
            return;
        }
        let dt = frames as f64 / self.rate_in;
        if let Some(smoothed) = self.smooth.update(e, dt) {
            let u = self.pi.update(smoothed, dt);
            self.resampler.set_correction(u);
            self.shared().timing_error.store(smoothed);
            self.shared().correction_ppm.store(u * 1e6);
        }
    }

    /// Publishes the counters that accumulate locally.
    fn publish(&mut self) {
        let r = std::mem::take(&mut self.sink.report);
        self.shared().late_frames.add(r.late);
        self.shared().overrun_frames.add(r.overwritten);
        if let Some(ppm) = self.fit.ppm() {
            self.shared().drift_ppm.store(ppm);
        }
        if let Some(sd) = self.fit.residual_sd() {
            self.shared().jitter.store(sd);
        }
    }
}

impl Drop for InputProducer {
    /// Hands this thread's references to the collector. The audio thread must not free
    /// the ring, including when it holds the last one (the mixer was already dropped).
    fn drop(&mut self) {
        if let Some(writer) = self.sink.writer.take() {
            self.retirer.ring(writer.into_ring());
        }
        if let Some(shared) = self.shared.take() {
            self.retirer.shared(shared);
        }
    }
}

impl AudioInputHandler for InputProducer {
    fn on_packet(&mut self, packet: &AudioPacket<'_>) {
        self.push_packet(packet);
    }

    fn on_error(&mut self, _error: &PlatformError) {
        // The control plane reads the flag; logging is not allowed here (R-RS-005).
        self.shared().failed.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use jstream_media::{ChannelLayout, MediaTime, SampleFormat, Timebase};
    use jstream_platform::AudioPacketFlags;

    use super::*;

    const RATE: u32 = 48_000;

    fn producer() -> (InputProducer, Arc<FrameRing>, Arc<InputShared>) {
        let format =
            AudioFormat::new(RATE, ChannelLayout::Stereo, SampleFormat::F32Interleaved).unwrap();
        let ring = Arc::new(FrameRing::new(1 << 16));
        let shared = Arc::new(InputShared::new());
        let p = InputProducer::new(
            format,
            RATE,
            Arc::clone(&ring),
            Arc::clone(&shared),
            Retirer::spawn().unwrap(),
        );
        (p, ring, shared)
    }

    /// Packet `i` of 480 frames at position `i * 480`, stamped `t0 + i * 10 ms` (or not).
    fn packet(samples: &[f32], i: u64, t0: f64, stamped: bool) -> AudioPacket<'_> {
        let tb = Timebase::per_second(1_000_000).unwrap();
        let t = t0 + i as f64 * 0.01;
        AudioPacket {
            samples,
            device_position: i * 480,
            time: stamped.then(|| MediaTime::new((t * 1e6).round() as i64, tb)),
            flags: AudioPacketFlags::default(),
        }
    }

    /// The first stamp places the input: input frame 0 of packet 1, captured at 2.5 s,
    /// becomes engine frame 120000. Packets before any stamp are dropped and counted.
    #[test]
    fn the_first_stamp_anchors_the_input() {
        let (mut p, ring, shared) = producer();
        let ramp: Vec<f32> = (0..480).flat_map(|i| [i as f32 / 480.0; 2]).collect();
        p.push_packet(&packet(&ramp, 0, 2.49, false));
        assert_eq!(shared.unanchored_frames.get(), 480);
        assert_eq!(shared.timestamps_missing.get(), 1);
        for i in 1..20 {
            p.push_packet(&packet(&ramp, i, 2.49, true));
        }
        // Frames from 120000 on hold packet 1's ramp; nothing before it was written.
        let mut out = [[0.0f32; 2]; 4];
        assert_eq!(ring.read(120_000, &mut out).valid, 4);
        let mut mid = [[0.0f32; 2]; 1];
        ring.read(120_240, &mut mid);
        assert!((mid[0][0] - 0.5).abs() < 0.01, "{mid:?}");
        let mut before = [[1.0f32; 2]; 4];
        assert_eq!(ring.read(119_996, &mut before).valid, 0);
        assert_eq!(shared.reanchors.get(), 0);
        assert!(shared.timing_error.load().abs() < 1e-9);
    }

    #[test]
    fn silent_packets_are_silence_and_errors_are_flagged() {
        let (mut p, ring, shared) = producer();
        let loud = vec![0.5f32; 960];
        for i in 0..10 {
            let mut pk = packet(&loud, i, 1.0, true);
            pk.flags.silent = true;
            p.push_packet(&pk);
        }
        let mut out = [[1.0f32; 2]; 256];
        assert_eq!(ring.read(48_000, &mut out).valid, 256);
        assert!(out.iter().all(|f| *f == [0.0; 2]));
        // An empty packet is counted and ignored.
        p.push_packet(&packet(&[], 10, 1.0, true));
        assert_eq!(shared.packets.get(), 11);
        p.on_error(&PlatformError::DeviceLost {
            what: "test".into(),
            reason: "unplugged".into(),
        });
        assert!(shared.failed.load(Ordering::Relaxed));
    }

    /// The device's stamps step 50 ms for good: the first 49 are rejected as outliers,
    /// the 50th restarts the segment and re-anchors the input 50 ms later on the time
    /// line; the jump reads as silence.
    #[test]
    fn a_run_of_outliers_re_anchors() {
        let (mut p, ring, shared) = producer();
        let s = vec![0.25f32; 960];
        for i in 0..300 {
            let t0 = if i < 200 { 1.0 } else { 1.05 };
            p.push_packet(&packet(&s, i, t0, true));
        }
        assert_eq!(shared.outliers.get(), 50);
        assert_eq!(shared.resets.get(), 1);
        assert_eq!(shared.reanchors.get(), 1);
        // Packets 200..249 kept the old mapping (up to engine frame 48000 + 249 * 480,
        // less the look-ahead); packet 249, stamped 1.05 + 2.49 s, starts the new one.
        let old_end = 48_000 + 249 * 480 - 64;
        let new_start = (3.54 * 48_000.0) as i64;
        let mut out = [[1.0f32; 2]; 8];
        ring.read(old_end + 100, &mut out);
        assert!(out.iter().all(|f| *f == [0.0; 2]), "the jump is silence");
        ring.read(new_start + 100, &mut out);
        assert!(out.iter().all(|f| (f[0] - 0.25).abs() < 1e-4), "{out:?}");
        assert!(new_start - old_end > 2_000);
    }

    /// A stamp no engine frame can hold is treated as missing, never anchored on.
    #[test]
    fn absurd_stamps_are_ignored() {
        let (mut p, ring, shared) = producer();
        let s = vec![0.25f32; 960];
        let mut pk = packet(&s, 0, 0.0, true);
        pk.time = Some(MediaTime::new(i64::MAX, Timebase::per_second(1).unwrap()));
        p.push_packet(&pk);
        assert_eq!(shared.timestamps_missing.get(), 1);
        assert_eq!(shared.unanchored_frames.get(), 480);
        let mut out = [[0.0f32; 2]; 1];
        assert_eq!(
            ring.read(0, &mut out),
            super::super::ring::ReadReport::default()
        );
    }

    /// A discontinuity flag re-anchors (and is counted) even when positions continue.
    #[test]
    fn a_discontinuity_re_anchors() {
        let (mut p, _ring, shared) = producer();
        let s = vec![0.25f32; 960];
        for i in 0..20 {
            let mut pk = packet(&s, i, 1.0, true);
            pk.flags.discontinuity = i == 0 || i == 10;
            p.push_packet(&pk);
        }
        assert_eq!(shared.discontinuities.get(), 1);
        assert_eq!(shared.reanchors.get(), 1);
        assert_eq!(shared.resets.get(), 0);
    }

    /// A discontinuity whose stamp is 50 ms off the fitted line re-anchors at that stamp
    /// at once (#449): the segment restarts with it, nothing counts as an outlier, and
    /// its audio lands at its own time, with silence over the jump. Before the fix it was
    /// rejected and the old mapping held until the 50-outlier run.
    #[test]
    fn a_discontinuity_off_the_line_re_anchors_at_once() {
        let (mut p, ring, shared) = producer();
        let before = vec![0.25f32; 960];
        let after = vec![0.5f32; 960];
        for i in 0..220 {
            let (s, t0) = if i < 200 {
                (&before, 1.0)
            } else {
                (&after, 1.05)
            };
            let mut pk = packet(s, i, t0, true);
            pk.flags.discontinuity = i == 200;
            p.push_packet(&pk);
        }
        assert_eq!(shared.discontinuities.get(), 1);
        assert_eq!(shared.outliers.get(), 0);
        assert_eq!(shared.resets.get(), 1);
        assert_eq!(shared.reanchors.get(), 1);
        // The old mapping would put packet 200 at engine frame 48000 + 200 * 480 =
        // 144000; its stamp, 1.05 + 2.00 s, puts it at 146400. The 50 ms between is
        // silence, and packet 200's audio starts at 146400.
        let mut out = [[1.0f32; 2]; 8];
        ring.read(145_000, &mut out);
        assert!(
            out.iter().all(|f| *f == [0.0; 2]),
            "the jump is silence: {out:?}"
        );
        ring.read(146_400 + 100, &mut out);
        assert!(out.iter().all(|f| (f[0] - 0.5).abs() < 1e-4), "{out:?}");
        // The new segment's own line holds: later stamps on it are not outliers.
        assert!(shared.timing_error.load().abs() < 1e-3);
    }

    /// A flagged packet without a stamp hands the trust to the next stamp, and the trust
    /// is spent by that stamp: an unflagged outlier later is rejected as usual.
    #[test]
    fn only_the_stamp_after_a_discontinuity_skips_outlier_rejection() {
        let (mut p, _ring, shared) = producer();
        let s = vec![0.25f32; 960];
        for i in 0..280 {
            let t0 = match i {
                0..150 => 1.0,
                150..200 => 1.05,
                // A single unflagged 30 ms spike: jitter, not a move of the line.
                260 => 1.08,
                _ => 1.05,
            };
            let mut pk = packet(&s, i, t0, i != 150);
            pk.flags.discontinuity = i == 150;
            p.push_packet(&pk);
        }
        // Packet 151, the first stamp after the flag, restarted the segment; packet 260,
        // once the new segment has 109 points, was only rejected.
        assert_eq!(shared.resets.get(), 1);
        assert_eq!(shared.reanchors.get(), 1);
        assert_eq!(shared.outliers.get(), 1);
    }
}
