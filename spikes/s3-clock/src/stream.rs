//! One capture stream's per-packet processing, independent of WASAPI.
//!
//! The Windows capture thread converts each `GetBuffer` packet to `f32` and calls
//! [`StreamState::on_packet`]. Everything after that (QPC mapping, the drift fit, glitch
//! handling, the adaptive resampler) is here, so it is unit-tested on any platform.

use std::time::Instant;

use crate::clock::{Continuity, PacketFlags, ResetReason, StreamClock, hns_to_sec};
use crate::drift::DriftFit;
use crate::track::{Quality, Stat, Tracker};

/// A point further than this from the fitted line is an outlier and is not fitted.
const OUTLIER_S: f64 = 0.020;
/// Points the current segment needs before outliers are rejected.
const OUTLIER_MIN_POINTS: u64 = 100;
/// Consecutive outliers that mean the line moved: the segment restarts.
const OUTLIER_RUN_RESET: u32 = 50;

/// Sample formats the spike converts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    F32,
    I16,
    /// 24 valid bits in a 32-bit container, or full 32-bit integers.
    I32,
    /// Packed 24-bit integers.
    I24,
}

impl SampleFormat {
    pub fn bytes(self) -> usize {
        match self {
            Self::F32 | Self::I32 => 4,
            Self::I16 => 2,
            Self::I24 => 3,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I24 => "i24",
        }
    }
}

/// Converts `frames * channels` interleaved samples of `format` from `bytes` into `out`
/// (cleared first). Short input is padded with silence.
pub fn to_f32(bytes: &[u8], format: SampleFormat, samples: usize, out: &mut Vec<f32>) {
    out.clear();
    let width = format.bytes();
    for i in 0..samples {
        let Some(b) = bytes.get(i * width..(i + 1) * width) else {
            out.push(0.0);
            continue;
        };
        let v = match format {
            SampleFormat::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            SampleFormat::I16 => f32::from(i16::from_le_bytes([b[0], b[1]])) / 32_768.0,
            SampleFormat::I32 => {
                (f64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])) / 2_147_483_648.0) as f32
            }
            SampleFormat::I24 => {
                let v = i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8;
                v as f32 / 8_388_608.0
            }
        };
        out.push(if v.is_finite() { v } else { 0.0 });
    }
}

/// What kind of stream this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Mic,
    Loopback,
    ProcessLoopback,
}

impl Role {
    pub fn key(self) -> &'static str {
        match self {
            Self::Mic => "mic",
            Self::Loopback => "loopback",
            Self::ProcessLoopback => "process_loopback",
        }
    }
}

/// A stream's fixed facts.
#[derive(Clone, Debug)]
pub struct StreamInfo {
    pub role: Role,
    pub device_name: String,
    pub device_id: String,
    pub rate: u32,
    pub channels: u16,
    pub format: SampleFormat,
    /// Bits per sample in the container and valid bits.
    pub bits: u16,
    pub valid_bits: u16,
    pub buffer_frames: u32,
    /// The device period WASAPI reports (hns).
    pub period_hns: i64,
}

/// One `GetBuffer` packet, already converted to `f32`.
pub struct Packet<'a> {
    pub device_position: u64,
    pub frames: u32,
    /// QPC time of the first frame, in 100 ns units (`pu64QPCPosition`).
    pub qpc_hns: u64,
    pub flags: u32,
    /// `frames * channels` interleaved samples.
    pub samples: &'a [f32],
}

/// Counters and estimates a stream publishes; the report and CSV read these.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub packets: u64,
    pub frames: u64,
    /// `DATA_DISCONTINUITY` flags, excluding one on the very first packet (WASAPI sets
    /// it there routinely).
    pub discontinuities: u64,
    pub first_packet_discontinuity: bool,
    pub silent_packets: u64,
    pub timestamp_errors: u64,
    pub gaps: u64,
    pub gap_frames: u64,
    pub resets_backwards: u64,
    pub resets_gap_too_large: u64,
    pub resets_gap_time_mismatch: u64,
    pub resets_outliers: u64,
    pub outliers: u64,
    pub fit_points: u64,
    pub fit_segments: u32,
    pub fit_span_s: f64,
    pub drift_ppm: Option<f64>,
    pub drift_stderr_ppm: Option<f64>,
    pub jitter_ms: Option<f64>,
    /// Accumulated device-vs-QPC offset from the fit: drift times the fitted span.
    pub offset_ms: Option<f64>,
    /// The same, measured directly over the current segment (includes jitter).
    pub offset_raw_ms: Option<f64>,
    pub correction_ppm: f64,
    pub residual_ms: Option<f64>,
    pub residual_raw_ms: Option<f64>,
    pub residual: Stat,
    pub residual_raw: Stat,
    pub reanchors: u64,
    /// Seconds (QPC, since the run's origin) of the latest timestamped packet.
    pub last_stamp_s: Option<f64>,
    pub process_us_max: f64,
    pub process_us_mean: f64,
    /// While measuring: the fitted media-clock time of each packet's first frame minus
    /// its raw QPC stamp, ms. How far a smoothed position-to-QPC mapping sits from the
    /// stamps WASAPI reports.
    pub mapping_ms: Stat,
}

impl Snapshot {
    pub fn resets(&self) -> u64 {
        self.resets_backwards
            + self.resets_gap_too_large
            + self.resets_gap_time_mismatch
            + self.resets_outliers
    }

    /// Everything that indicates lost or unreliable audio.
    pub fn glitches(&self) -> u64 {
        self.discontinuities + self.gaps + self.resets() + self.timestamp_errors
    }
}

/// The per-stream state the capture thread owns.
pub struct StreamState {
    rate: u32,
    channels: usize,
    origin_hns: i64,
    settle_s: f64,
    clock: StreamClock,
    fit: DriftFit,
    tracker: Tracker,
    snap: Snapshot,
    pos0: Option<u64>,
    /// First and latest (x, y) of the current segment.
    seg_first: Option<(f64, f64)>,
    seg_last: Option<(f64, f64)>,
    outlier_run: u32,
    process_us_sum: f64,
}

impl StreamState {
    /// `origin_hns` is the run's QPC origin; residual statistics start `settle_s`
    /// seconds after it.
    pub fn new(
        info: &StreamInfo,
        engine_rate: u32,
        origin_hns: i64,
        settle_s: f64,
    ) -> Result<Self, String> {
        let channels = usize::from(info.channels);
        let tracker = Tracker::new(
            info.rate,
            engine_rate,
            channels.clamp(1, 2),
            Quality::ENGINE,
        )?;
        Ok(Self {
            rate: info.rate,
            channels,
            origin_hns,
            settle_s,
            clock: StreamClock::new(info.rate),
            fit: DriftFit::new(),
            tracker,
            snap: Snapshot::default(),
            pos0: None,
            seg_first: None,
            seg_last: None,
            outlier_run: 0,
            process_us_sum: 0.0,
        })
    }

    pub fn on_packet(&mut self, p: &Packet<'_>) -> Result<(), String> {
        let started = Instant::now();
        let flags = PacketFlags::from_bits(p.flags);
        self.snap.packets += 1;
        self.snap.frames += u64::from(p.frames);
        if flags.discontinuity {
            if self.snap.packets == 1 {
                self.snap.first_packet_discontinuity = true;
            } else {
                self.snap.discontinuities += 1;
            }
        }
        if flags.silent {
            self.snap.silent_packets += 1;
        }
        // A zero QPC position is as unusable as a flagged one.
        let stamp_hns = if flags.timestamp_error || p.qpc_hns == 0 {
            self.snap.timestamp_errors += 1;
            None
        } else {
            i64::try_from(p.qpc_hns).ok()
        };

        match self.clock.classify(p.device_position, p.frames, stamp_hns) {
            Continuity::First => self.pos0 = Some(p.device_position),
            Continuity::Continuous => {}
            Continuity::Gap(n) => {
                self.snap.gaps += 1;
                self.snap.gap_frames += n;
                self.tracker.push_silence(n)?;
            }
            Continuity::Reset(reason) => {
                match reason {
                    ResetReason::Backwards => self.snap.resets_backwards += 1,
                    ResetReason::GapTooLarge => self.snap.resets_gap_too_large += 1,
                    ResetReason::GapTimeMismatch => self.snap.resets_gap_time_mismatch += 1,
                }
                self.restart_segment();
            }
        }

        let x = stamp_hns.map(|t| hns_to_sec(t - self.origin_hns));
        if let Some(x) = x {
            self.add_point(x, p.device_position);
            self.tracker.measuring = x >= self.settle_s;
            if self.tracker.measuring
                && self.fit.current_points() >= OUTLIER_MIN_POINTS
                && let Some(mapped) = self.map_position(p.device_position)
            {
                self.snap.mapping_ms.add((mapped - x) * 1e3);
            }
        }

        let want = p.frames as usize * self.channels;
        let samples = p.samples.get(..want).unwrap_or(p.samples);
        self.tracker.push(samples, self.channels, x)?;

        let us = started.elapsed().as_secs_f64() * 1e6;
        self.snap.process_us_max = self.snap.process_us_max.max(us);
        self.process_us_sum += us;
        Ok(())
    }

    fn restart_segment(&mut self) {
        self.fit.new_segment();
        self.tracker.reanchor();
        self.seg_first = None;
        self.seg_last = None;
        self.outlier_run = 0;
    }

    fn add_point(&mut self, x: f64, pos: u64) {
        let pos0 = *self.pos0.get_or_insert(pos);
        let y = (pos as f64 - pos0 as f64) / f64::from(self.rate);
        if self.fit.current_points() >= OUTLIER_MIN_POINTS
            && let Some(pred) = self.fit.predict_y(x)
            && (y - pred).abs() > OUTLIER_S
        {
            self.snap.outliers += 1;
            self.outlier_run += 1;
            if self.outlier_run >= OUTLIER_RUN_RESET {
                self.snap.resets_outliers += 1;
                self.restart_segment();
            }
            return;
        }
        self.outlier_run = 0;
        self.fit.add(x, y);
        self.seg_first.get_or_insert((x, y));
        self.seg_last = Some((x, y));
        self.snap.last_stamp_s = Some(x);
    }

    /// The current counters and estimates.
    pub fn snapshot(&self) -> Snapshot {
        let mut s = self.snap.clone();
        s.fit_points = self.fit.points();
        s.fit_segments = self.fit.segments();
        s.fit_span_s = self.fit.span();
        s.drift_ppm = self.fit.ppm();
        s.drift_stderr_ppm = self.fit.stderr_ppm();
        s.jitter_ms = self.fit.residual_sd().map(|v| v * 1e3);
        s.offset_ms = s.drift_ppm.map(|ppm| ppm * 1e-6 * s.fit_span_s * 1e3);
        s.offset_raw_ms = match (self.seg_first, self.seg_last) {
            (Some((x0, y0)), Some((x1, y1))) => Some(((y1 - y0) - (x1 - x0)) * 1e3),
            _ => None,
        };
        s.correction_ppm = self.tracker.correction_ppm();
        s.residual_ms = self.tracker.residual.map(|v| v * 1e3);
        s.residual_raw_ms = self.tracker.residual_raw.map(|v| v * 1e3);
        s.residual = self.tracker.stat.scaled(1e3);
        s.residual_raw = self.tracker.stat_raw.scaled(1e3);
        s.reanchors = self.tracker.reanchors;
        s.process_us_mean = if s.packets == 0 {
            0.0
        } else {
            self.process_us_sum / s.packets as f64
        };
        s
    }

    /// The fitted media-clock (QPC) time of device position `pos`, in seconds since the
    /// run's origin.
    pub fn map_position(&self, pos: u64) -> Option<f64> {
        let pos0 = self.pos0?;
        self.fit
            .map_to_qpc((pos as f64 - pos0 as f64) / f64::from(self.rate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::HNS_PER_SEC;
    use crate::testutil::Noise;

    fn info(rate: u32, channels: u16) -> StreamInfo {
        StreamInfo {
            role: Role::Mic,
            device_name: "test".into(),
            device_id: "id".into(),
            rate,
            channels,
            format: SampleFormat::F32,
            bits: 32,
            valid_bits: 32,
            buffer_frames: rate / 10,
            period_hns: 100_000,
        }
    }

    #[test]
    fn role_keys() {
        assert_eq!(Role::Mic.key(), "mic");
        assert_eq!(Role::Loopback.key(), "loopback");
        assert_eq!(Role::ProcessLoopback.key(), "process_loopback");
    }

    #[test]
    fn converts_sample_formats() {
        let mut out = Vec::new();
        to_f32(&0.5f32.to_le_bytes(), SampleFormat::F32, 1, &mut out);
        assert_eq!(out, [0.5]);
        to_f32(&(-16_384i16).to_le_bytes(), SampleFormat::I16, 1, &mut out);
        assert_eq!(out, [-0.5]);
        to_f32(&(1i32 << 30).to_le_bytes(), SampleFormat::I32, 1, &mut out);
        assert_eq!(out, [0.5]);
        // -0.5 as packed 24-bit: 0xC00000.
        to_f32(&[0x00, 0x00, 0xC0], SampleFormat::I24, 1, &mut out);
        assert_eq!(out, [-0.5]);
        // Short input pads with silence; NaN becomes silence.
        to_f32(&f32::NAN.to_le_bytes(), SampleFormat::F32, 3, &mut out);
        assert_eq!(out, [0.0, 0.0, 0.0]);
    }

    /// Simulated stream: 10 ms packets, drift `ppm`, timestamp jitter, an occasional
    /// lost packet (with the position advancing, as WASAPI does), and one position
    /// reset that the clock disagrees with.
    #[test]
    fn end_to_end_drift_glitches_and_resampling() {
        let rate = 48_000;
        let origin: i64 = 50 * HNS_PER_SEC;
        let mut s = StreamState::new(&info(rate, 2), 48_000, origin, 60.0).unwrap();
        let ppm = 42.0;
        let true_rate = f64::from(rate) * (1.0 + ppm * 1e-6);
        let mut noise = Noise::new(3);
        let samples = vec![0.25f32; 480 * 2];
        let mut pos: u64 = 0;
        let mut extra: u64 = 0; // position jump the clock never saw
        for i in 0..24_000u64 {
            let t = origin as f64 / 1e7 + 1.0 + (pos - extra) as f64 / true_rate;
            let jitter = noise.next() * 0.3e-3;
            let mut flags = 0;
            if i == 0 {
                flags |= PacketFlags::DATA_DISCONTINUITY;
            }
            if i % 5000 == 4999 {
                // Lost packet: skip it, flag the next one.
                pos += 480;
                continue;
            }
            if i == 12_000 {
                // The position leaps 3 s ahead while time does not.
                pos += 3 * 48_000;
                extra += 3 * 48_000;
            }
            if i % 7000 == 6999 {
                flags |= PacketFlags::TIMESTAMP_ERROR;
            }
            s.on_packet(&Packet {
                device_position: pos,
                frames: 480,
                qpc_hns: ((t + jitter) * 1e7) as u64,
                flags,
                samples: &samples,
            })
            .unwrap();
            pos += 480;
        }
        let snap = s.snapshot();
        assert!(snap.first_packet_discontinuity);
        assert_eq!(snap.discontinuities, 0);
        assert_eq!(snap.gaps, 4);
        assert_eq!(snap.gap_frames, 4 * 480);
        assert_eq!(snap.resets(), 1);
        assert_eq!(snap.resets_gap_too_large, 1);
        assert_eq!(snap.timestamp_errors, 3);
        assert_eq!(snap.fit_segments, 2);
        let drift = snap.drift_ppm.unwrap();
        assert!((drift - ppm).abs() < 1.0, "drift {drift}");
        assert!(snap.jitter_ms.unwrap() < 0.3);
        // 240 s at 42 ppm is about 10 ms.
        let off = snap.offset_ms.unwrap();
        assert!((off - 10.0).abs() < 1.0, "offset {off}");
        assert!(snap.residual.n > 1000);
        assert!(
            snap.residual.max() < 1.0,
            "residual max {} ms",
            snap.residual.max()
        );
        assert!((snap.correction_ppm + ppm).abs() < 5.0);
        // The fitted mapping sits within the ±0.3 ms jitter of the raw stamps.
        assert!(snap.mapping_ms.n > 1000);
        assert!(
            snap.mapping_ms.max() < 0.5,
            "mapping {} ms",
            snap.mapping_ms.max()
        );
        // Mapping a position onto QPC lands on the simulated (jitter-free) time.
        let p = pos - 480;
        let t = 1.0 + (p - extra) as f64 / true_rate;
        assert!((s.map_position(p).unwrap() - t).abs() < 0.5e-3);
    }

    #[test]
    fn outliers_are_rejected_then_reset() {
        let mut s = StreamState::new(&info(48_000, 1), 48_000, 0, 0.0).unwrap();
        let samples = vec![0.0f32; 480];
        let mut pos = 0u64;
        let packet = |s: &mut StreamState, pos: u64, t: f64| {
            s.on_packet(&Packet {
                device_position: pos,
                frames: 480,
                qpc_hns: (t * 1e7) as u64,
                flags: 0,
                samples: &samples,
            })
            .unwrap();
        };
        for _ in 0..200 {
            packet(&mut s, pos, 1.0 + pos as f64 / 48_000.0);
            pos += 480;
        }
        // One timestamp 100 ms off: rejected.
        packet(&mut s, pos, 1.1 + pos as f64 / 48_000.0);
        pos += 480;
        assert_eq!(s.snapshot().outliers, 1);
        // The clock steps by 100 ms for good: after a run of outliers, a new segment.
        for _ in 0..100 {
            packet(&mut s, pos, 1.1 + pos as f64 / 48_000.0);
            pos += 480;
        }
        let snap = s.snapshot();
        assert_eq!(snap.resets_outliers, 1);
        assert_eq!(snap.fit_segments, 2);
        assert!(snap.drift_ppm.unwrap().abs() < 1.0);
    }
}
