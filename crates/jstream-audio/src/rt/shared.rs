//! State shared between a real-time thread and the control plane: floats in atomics,
//! counters, and meters. Every access is a single atomic load, store or read-modify-write,
//! so neither side ever waits for the other (docs/RULES.md#R-RT-003).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// An `f32` stored as its bits in an [`AtomicU32`].
#[derive(Debug)]
pub(crate) struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub(crate) const fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }

    pub(crate) fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    pub(crate) fn store(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
}

/// An `f64` stored as its bits in an [`AtomicU64`].
#[derive(Debug)]
pub(crate) struct AtomicF64(AtomicU64);

impl AtomicF64 {
    pub(crate) fn new(v: f64) -> Self {
        Self(AtomicU64::new(v.to_bits()))
    }

    pub(crate) fn load(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }

    pub(crate) fn store(&self, v: f64) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
}

/// A monotonically increasing event counter.
#[derive(Debug, Default)]
pub(crate) struct Counter(AtomicU64);

impl Counter {
    pub(crate) fn add(&self, n: u64) {
        if n > 0 {
            self.0.fetch_add(n, Ordering::Relaxed);
        }
    }

    pub(crate) fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Peak and RMS levels of a stereo signal, linear full scale (1.0 is 0 dBFS).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeterReading {
    /// The largest absolute sample per channel (left, right) since the previous
    /// [`take`](crate::InputControl::take_meter) of this meter.
    pub peak: [f32; 2],
    /// The RMS level per channel over the most recent mixed block.
    pub rms: [f32; 2],
}

impl MeterReading {
    /// A linear level in dBFS; `-inf` for silence.
    pub fn to_dbfs(level: f32) -> f32 {
        20.0 * level.log10()
    }
}

/// A stereo meter the mixer writes once per block.
///
/// The peak is held until the control plane takes it, so a reader polling slower than
/// the block rate still sees every peak. Non-negative finite `f32` values order the same
/// way as their bit patterns, so the held peak is a plain `fetch_max` on the bits.
#[derive(Debug, Default)]
pub(crate) struct Meter {
    peak: [AtomicU32; 2],
    rms: [AtomicU32; 2],
}

impl Meter {
    /// Publishes one block's levels. Levels must be finite and non-negative.
    pub(crate) fn publish(&self, peak: [f32; 2], rms: [f32; 2]) {
        for c in 0..2 {
            self.peak[c].fetch_max(sanitize_level(peak[c]).to_bits(), Ordering::Relaxed);
            self.rms[c].store(sanitize_level(rms[c]).to_bits(), Ordering::Relaxed);
        }
    }

    /// Reads the levels and restarts the held peak.
    pub(crate) fn take(&self) -> MeterReading {
        let mut r = MeterReading::default();
        for c in 0..2 {
            r.peak[c] = f32::from_bits(self.peak[c].swap(0, Ordering::Relaxed));
            r.rms[c] = f32::from_bits(self.rms[c].load(Ordering::Relaxed));
        }
        r
    }
}

/// Maps NaN and negative values to 0 and infinity to `f32::MAX`, so the bit order trick
/// in [`Meter`] holds.
fn sanitize_level(v: f32) -> f32 {
    if v.is_nan() || v <= 0.0 {
        0.0
    } else {
        v.min(f32::MAX)
    }
}

/// Peak and RMS of a block of stereo frames.
pub(crate) fn levels(frames: &[[f32; 2]]) -> ([f32; 2], [f32; 2]) {
    let mut peak = [0.0f32; 2];
    let mut sum_sq = [0.0f64; 2];
    for f in frames {
        for c in 0..2 {
            let v = f[c];
            peak[c] = peak[c].max(v.abs());
            sum_sq[c] += f64::from(v) * f64::from(v);
        }
    }
    let n = frames.len().max(1) as f64;
    let rms = [(sum_sq[0] / n).sqrt() as f32, (sum_sq[1] / n).sqrt() as f32];
    (peak, rms)
}

/// A gain the control plane sets and the mixer reads: linear, with a mute flag.
#[derive(Debug)]
pub(crate) struct GainParam {
    gain: AtomicF32,
    muted: AtomicBool,
}

/// The largest gain accepted: +24 dB.
pub(crate) const MAX_GAIN: f32 = 15.848_932;

impl GainParam {
    pub(crate) fn new() -> Self {
        Self {
            gain: AtomicF32::new(1.0),
            muted: AtomicBool::new(false),
        }
    }

    /// Sets the linear gain, clamped to `0..=MAX_GAIN`; NaN is ignored.
    pub(crate) fn set(&self, gain: f32) {
        if !gain.is_nan() {
            self.gain.store(gain.clamp(0.0, MAX_GAIN));
        }
    }

    pub(crate) fn get(&self) -> f32 {
        self.gain.load()
    }

    pub(crate) fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub(crate) fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    /// The gain to apply now: 0 while muted.
    pub(crate) fn effective(&self) -> f32 {
        if self.muted() { 0.0 } else { self.get() }
    }
}

/// Converts decibels to a linear gain; `-inf` dB is 0.
pub(crate) fn db_to_gain(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_holds_the_peak_until_taken() {
        let m = Meter::default();
        m.publish([0.5, 0.25], [0.1, 0.1]);
        m.publish([0.2, 0.75], [0.05, 0.2]);
        let r = m.take();
        assert_eq!(r.peak, [0.5, 0.75]);
        assert_eq!(r.rms, [0.05, 0.2]);
        // Taking restarts the peak; RMS stays the latest block's.
        let r = m.take();
        assert_eq!(r.peak, [0.0, 0.0]);
        assert_eq!(r.rms, [0.05, 0.2]);
        m.publish([f32::NAN, f32::INFINITY], [-1.0, 0.0]);
        let r = m.take();
        assert_eq!(r.peak, [0.0, f32::MAX]);
        assert_eq!(r.rms, [0.0, 0.0]);
    }

    #[test]
    fn levels_of_a_square_and_a_constant() {
        let frames: Vec<[f32; 2]> = (0..100)
            .map(|i| [if i % 2 == 0 { 0.5 } else { -0.5 }, 0.25])
            .collect();
        let (peak, rms) = levels(&frames);
        assert_eq!(peak, [0.5, 0.25]);
        assert!((rms[0] - 0.5).abs() < 1e-6 && (rms[1] - 0.25).abs() < 1e-6);
        assert_eq!(levels(&[]), ([0.0, 0.0], [0.0, 0.0]));
    }

    #[test]
    fn gain_is_clamped_and_muted() {
        let g = GainParam::new();
        assert_eq!(g.effective(), 1.0);
        g.set(f32::NAN);
        assert_eq!(g.get(), 1.0);
        g.set(-2.0);
        assert_eq!(g.get(), 0.0);
        g.set(1e9);
        assert_eq!(g.get(), MAX_GAIN);
        g.set(0.5);
        g.set_muted(true);
        assert_eq!(g.effective(), 0.0);
        g.set_muted(false);
        assert_eq!(g.effective(), 0.5);
        assert!((db_to_gain(-6.0) - 0.501_187).abs() < 1e-6);
        assert!((db_to_gain(24.0) - MAX_GAIN).abs() < 1e-4);
        assert_eq!(db_to_gain(f32::NEG_INFINITY), 0.0);
        assert_eq!(MeterReading::to_dbfs(1.0), 0.0);
    }
}
