//! The adaptive resampler under test: one input stream resampled onto the engine's
//! QPC-based time line (ADR-0014), steered by the PI controller in `control`.
//!
//! The resampler (rubato's asynchronous sinc resampler) consumes the input in fixed
//! chunks as it arrives. After each chunk it has consumed `I` input frames and written
//! `J` output frames. The input frame `I` was captured at QPC time `T_in(I)`, known
//! from the packet timestamps. The output frame `J` sits at engine time
//! `J / engine_rate`. The **timing error** is
//!
//! `e = J / engine_rate - T_in(I) - e0`
//!
//! where `e0` is the first measurement (the resampler's own delay and the start-up
//! offset are constant). `e > 0` means the output has run ahead of real time: the
//! device clock is fast relative to QPC and the ratio must come down. A consumer that
//! pulls `engine_rate` frames per QPC second from the output would see its buffer fill
//! drift by exactly `e`, so this is the buffer-fill error measured without the packet
//! quantisation of a real buffer.
//!
//! The controller sees `e` smoothed by a short low-pass filter (the raw value carries
//! the packets' timestamp jitter). Both are reported: `residual` is the smoothed value
//! and `residual_raw` the unsmoothed one.

use std::collections::VecDeque;

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Adjustable, Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

use crate::control::{Ema, Pi};

/// Closed-loop natural frequency of the ratio controller.
pub const LOOP_BANDWIDTH_HZ: f64 = 0.02;
/// Closed-loop damping.
pub const LOOP_DAMPING: f64 = 0.707;
/// Largest ratio correction the controller may apply (±0.5 %, 5000 ppm).
pub const MAX_CORRECTION: f64 = 0.005;
/// Time constant of the error smoother, in seconds.
pub const ERROR_SMOOTHING_S: f64 = 0.5;

/// Which interpolator to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    /// Band-limited sinc interpolation, `sinc_len` taps: the quality an engine needs.
    Sinc { sinc_len: usize },
}

impl Quality {
    /// The setting the spike runs with on real devices.
    pub const ENGINE: Self = Self::Sinc { sinc_len: 128 };
}

/// Running statistics of a signed value.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stat {
    pub n: u64,
    sum_sq: f64,
    pub max_abs: f64,
}

impl Stat {
    pub fn add(&mut self, v: f64) {
        if v.is_finite() {
            self.n += 1;
            self.sum_sq += v * v;
            self.max_abs = self.max_abs.max(v.abs());
        }
    }

    pub fn rms(&self) -> f64 {
        if self.n == 0 {
            f64::NAN
        } else {
            (self.sum_sq / self.n as f64).sqrt()
        }
    }

    pub fn max(&self) -> f64 {
        if self.n == 0 { f64::NAN } else { self.max_abs }
    }

    /// The same statistics with every value multiplied by `k` (e.g. seconds to ms).
    pub fn scaled(self, k: f64) -> Self {
        Self {
            n: self.n,
            sum_sq: self.sum_sq * k * k,
            max_abs: self.max_abs * k.abs(),
        }
    }
}

/// One stream's resampler, controller and residual statistics.
pub struct Tracker {
    rate_in: f64,
    rate_out: f64,
    channels: usize,
    chunk: usize,
    resampler: Async<f32>,
    /// Interleaved input waiting for a full chunk.
    stage: Vec<f32>,
    staged: usize,
    out: Vec<f32>,
    /// Input frames pushed, consumed by the resampler, and output frames written.
    pushed: u64,
    consumed: u64,
    produced: u64,
    /// Packet timestamps: (input frame index, QPC seconds) of each packet's first frame.
    stamps: VecDeque<(u64, f64)>,
    e0: Option<f64>,
    pi: Pi,
    smooth: Ema,
    /// Latest smoothed and raw error, seconds.
    pub residual: Option<f64>,
    pub residual_raw: Option<f64>,
    /// While true, residuals are added to the statistics (after the settling time).
    pub measuring: bool,
    /// Smoothed and raw residual, seconds, while measuring.
    pub stat: Stat,
    pub stat_raw: Stat,
    /// Times the error reference was re-taken after a mapping reset.
    pub reanchors: u64,
}

impl Tracker {
    /// A tracker resampling `channels` channels from `rate_in` to `rate_out` Hz.
    pub fn new(
        rate_in: u32,
        rate_out: u32,
        channels: usize,
        quality: Quality,
    ) -> Result<Self, String> {
        if rate_in == 0 || rate_out == 0 || channels == 0 {
            return Err(format!(
                "bad resampler setup: {rate_in} Hz -> {rate_out} Hz, {channels} channels"
            ));
        }
        // 10 ms chunks, as WASAPI's default shared-mode period.
        let chunk = (rate_in as usize / 100).max(64);
        let ratio = f64::from(rate_out) / f64::from(rate_in);
        let Quality::Sinc { sinc_len } = quality;
        let params = SincInterpolationParameters {
            sinc_len,
            f_cutoff: None,
            oversampling_factor: 128,
            interpolation: SincInterpolationType::Cubic,
            window: WindowFunction::BlackmanHarris2,
        };
        let resampler = Async::<f32>::new_sinc(
            ratio,
            1.0 + 2.0 * MAX_CORRECTION,
            &params,
            chunk,
            channels,
            FixedAsync::Input,
        )
        .map_err(|e| format!("rubato: {e}"))?;
        let out_max = resampler.output_frames_max();
        Ok(Self {
            rate_in: f64::from(rate_in),
            rate_out: f64::from(rate_out),
            channels,
            chunk,
            resampler,
            stage: vec![0.0; chunk * channels],
            staged: 0,
            out: vec![0.0; out_max * channels],
            pushed: 0,
            consumed: 0,
            produced: 0,
            stamps: VecDeque::with_capacity(64),
            e0: None,
            pi: Pi::new(LOOP_BANDWIDTH_HZ, LOOP_DAMPING, MAX_CORRECTION),
            smooth: Ema::new(ERROR_SMOOTHING_S),
            residual: None,
            residual_raw: None,
            measuring: false,
            stat: Stat::default(),
            stat_raw: Stat::default(),
            reanchors: 0,
        })
    }

    /// The current ratio correction in ppm (negative: fewer output frames than nominal).
    pub fn correction_ppm(&self) -> f64 {
        self.pi.output() * 1e6
    }

    /// Output frames written so far.
    #[cfg(test)]
    pub fn produced(&self) -> u64 {
        self.produced
    }

    /// Appends a packet: `frames` interleaved frames of `ch_in` channels (only the first
    /// [`Tracker::channels`] are resampled; missing ones are silent). `stamp` is the QPC
    /// time, in seconds, of the packet's first frame, if it is reliable.
    pub fn push(
        &mut self,
        samples: &[f32],
        ch_in: usize,
        stamp: Option<f64>,
    ) -> Result<(), String> {
        if ch_in == 0 {
            return Err("packet with zero channels".into());
        }
        if let Some(t) = stamp {
            self.stamps.push_back((self.pushed, t));
        }
        for frame in samples.chunks_exact(ch_in) {
            let base = self.staged * self.channels;
            for c in 0..self.channels {
                self.stage[base + c] = frame.get(c).copied().unwrap_or(0.0);
            }
            self.staged += 1;
            self.pushed += 1;
            if self.staged == self.chunk {
                self.process_chunk()?;
            }
        }
        Ok(())
    }

    /// Appends `frames` frames of silence (frames the device lost).
    pub fn push_silence(&mut self, frames: u64) -> Result<(), String> {
        for _ in 0..frames {
            let base = self.staged * self.channels;
            self.stage[base..base + self.channels].fill(0.0);
            self.staged += 1;
            self.pushed += 1;
            if self.staged == self.chunk {
                self.process_chunk()?;
            }
        }
        Ok(())
    }

    /// Re-takes the error reference at the next measurement (after the stream's position
    /// mapping restarted). The controller keeps its integrator, i.e. its drift estimate.
    pub fn reanchor(&mut self) {
        self.e0 = None;
        self.smooth.reset();
        self.reanchors += 1;
    }

    fn process_chunk(&mut self) -> Result<(), String> {
        let input = InterleavedSlice::new(&self.stage[..], self.channels, self.chunk)
            .map_err(|e| format!("input buffer: {e}"))?;
        let out_frames = self.out.len() / self.channels;
        let mut output = InterleavedSlice::new_mut(&mut self.out[..], self.channels, out_frames)
            .map_err(|e| format!("output buffer: {e}"))?;
        let (used, written) = self
            .resampler
            .process_into_buffer(&input, &mut output, None)
            .map_err(|e| format!("resample: {e}"))?;
        self.staged = 0;
        self.consumed += used as u64;
        self.produced += written as u64;
        self.measure();
        Ok(())
    }

    /// The QPC time of input frame `index`, from the latest stamp at or before it.
    fn input_time(&mut self, index: u64) -> Option<f64> {
        while self.stamps.len() > 1 && self.stamps[1].0 <= index {
            self.stamps.pop_front();
        }
        let &(i0, t0) = self.stamps.front()?;
        if i0 > index {
            return None;
        }
        Some(t0 + (index - i0) as f64 / self.rate_in)
    }

    fn measure(&mut self) {
        let Some(t_in) = self.input_time(self.consumed) else {
            return;
        };
        let raw = self.produced as f64 / self.rate_out - t_in;
        let e0 = *self.e0.get_or_insert(raw);
        let e = raw - e0;
        let dt = self.chunk as f64 / self.rate_in;
        let smoothed = self.smooth.update(e, dt);
        let u = self.pi.update(smoothed, dt);
        // The bound is checked when constructing; an error here would be a bug, and the
        // previous ratio staying in force is the safe outcome.
        let _ = self.resampler.set_resample_ratio_relative(1.0 + u, true);
        self.residual = Some(smoothed);
        self.residual_raw = Some(e);
        if self.measuring {
            self.stat.add(smoothed);
            self.stat_raw.add(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Noise;

    /// Feeds `secs` of a 1 kHz tone from a device whose clock is off by `ppm`, in
    /// 10 ms packets stamped with `jitter_ms` of uniform jitter; returns the tracker.
    fn run(rate_in: u32, ppm: f64, jitter_ms: f64, secs: f64, settle: f64) -> Tracker {
        let mut t = Tracker::new(rate_in, 48_000, 2, Quality::ENGINE).unwrap();
        let mut noise = Noise::new(42);
        let per_packet = (rate_in / 100) as usize;
        let true_rate = f64::from(rate_in) * (1.0 + ppm * 1e-6);
        let packets = (secs * 100.0) as usize;
        let mut buf = vec![0.0f32; per_packet * 2];
        let mut pos = 0u64;
        for _ in 0..packets {
            for (i, f) in buf.chunks_exact_mut(2).enumerate() {
                let n = (pos + i as u64) as f64;
                let s = (2.0 * std::f64::consts::PI * 1000.0 * n / f64::from(rate_in)).sin() as f32;
                f[0] = s;
                f[1] = -s;
            }
            let qpc = 3.0 + pos as f64 / true_rate + noise.next() * jitter_ms * 1e-3;
            t.measuring = qpc >= settle;
            t.push(&buf, 2, Some(qpc)).unwrap();
            pos += per_packet as u64;
        }
        t
    }

    #[test]
    fn tracks_a_fast_device_within_a_millisecond() {
        let t = run(48_000, 120.0, 0.5, 300.0, 120.0);
        assert!(t.stat.n > 10_000);
        assert!(t.stat.max() < 1e-3, "max residual {} s", t.stat.max());
        assert!(t.stat.rms() < 0.2e-3, "rms residual {} s", t.stat.rms());
        // The controller found the drift: fewer output frames per input frame.
        assert!(
            (t.correction_ppm() + 120.0).abs() < 5.0,
            "correction {} ppm",
            t.correction_ppm()
        );
    }

    #[test]
    fn tracks_a_slow_44k1_device() {
        let t = run(44_100, -250.0, 0.2, 300.0, 120.0);
        assert!(t.stat.max() < 1e-3, "max residual {} s", t.stat.max());
        assert!(
            (t.correction_ppm() - 250.0).abs() < 5.0,
            "correction {} ppm",
            t.correction_ppm()
        );
        // Output count matches 48 kHz on the QPC time line (within the residual).
        let secs = 300.0 * (1.0 + 250e-6); // the device's 300 s of samples took longer
        let expected = secs * 48_000.0;
        assert!((t.produced() as f64 - expected).abs() < 48.0 + 1024.0);
    }

    #[test]
    fn silence_fill_and_reanchor_keep_tracking() {
        let mut t = Tracker::new(48_000, 48_000, 1, Quality::ENGINE).unwrap();
        let buf = vec![0.0f32; 480];
        let rate = 48_000.0 * (1.0 + 60e-6);
        let mut pos = 0u64;
        for i in 0..30_000u64 {
            // Every 1000th packet is lost; the gap is filled with silence.
            if i % 1000 == 999 {
                t.push_silence(480).unwrap();
            } else {
                t.push(&buf, 1, Some(pos as f64 / rate)).unwrap();
            }
            pos += 480;
            t.measuring = i > 12_000;
        }
        assert!(t.stat.max() < 1e-3, "max residual {} s", t.stat.max());
        let before = t.correction_ppm();
        t.reanchor();
        // After a reanchor the error restarts from zero and the drift estimate stays.
        t.push(&buf, 1, Some(pos as f64 / rate + 5.0)).unwrap();
        assert_eq!(t.reanchors, 1);
        assert!((t.correction_ppm() - before).abs() < 1.0);
        assert!(t.residual.unwrap().abs() < 1e-6);
    }

    #[test]
    fn rejects_bad_setup() {
        assert!(Tracker::new(0, 48_000, 2, Quality::ENGINE).is_err());
        assert!(Tracker::new(48_000, 48_000, 0, Quality::ENGINE).is_err());
        let mut t = Tracker::new(48_000, 48_000, 2, Quality::ENGINE).unwrap();
        assert!(t.push(&[0.0; 4], 0, None).is_err());
    }

    #[test]
    fn stat_rms_and_max() {
        let mut s = Stat::default();
        assert!(s.rms().is_nan() && s.max().is_nan());
        s.add(3.0);
        s.add(-4.0);
        s.add(f64::NAN);
        assert_eq!(s.n, 2);
        assert_eq!(s.max(), 4.0);
        assert!((s.rms() - (12.5f64).sqrt()).abs() < 1e-12);
    }
}
