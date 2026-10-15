//! Audio filter kernels, as pure DSP over interleaved stereo blocks at the engine rate:
//! [`Gain`], [`NoiseGate`], [`Compressor`] (with an optional sidechain key),
//! [`Limiter`] (look-ahead) and [`Delay`]. ADR-0028 runs filter chains on the mixer
//! thread, so each kernel allocates its fixed-size state in its constructor and nothing
//! after that allocates, locks or blocks (R-RT-001). Each reports its latency in frames
//! through [`AudioFilter::latency_frames`], which the mixer will compensate. Wiring them
//! into the mixer graph, buses and settings comes with the send matrix.
//!
//! Docs: crates/jstream-audio/README.md#filters, docs/adr/0028-audio-model-v2.md

mod compressor;
mod delay;
mod gain;
mod gate;
mod limiter;

pub use compressor::{Compressor, CompressorParams};
pub use delay::{Delay, MAX_DELAY_MS};
pub use gain::Gain;
pub use gate::{GateParams, NoiseGate};
pub use limiter::{Limiter, LimiterParams, MAX_LOOKAHEAD_MS};

use crate::AudioError;

/// A block-based audio filter on the mixer thread.
///
/// Blocks are interleaved stereo `f32` at the rate the filter was built for, processed
/// in place. A trailing half frame is left untouched, and non-finite input samples are
/// treated as 0. A filter never allocates, locks or blocks in [`process`](Self::process),
/// [`process_keyed`](Self::process_keyed) or [`reset`](Self::reset).
pub trait AudioFilter: Send {
    /// Filters one block in place.
    fn process(&mut self, block: &mut [f32]);

    /// Filters one block in place, with `key` as the sidechain input for the same frames
    /// (interleaved stereo, normally `block.len()` samples; frames past its end read as
    /// silence). A filter without a key input ignores `key`.
    fn process_keyed(&mut self, block: &mut [f32], key: &[f32]) {
        let _ = key;
        self.process(block);
    }

    /// Whether the filter reads a key input (ADR-0028's sidechain keys), so the mixer
    /// must process the key's source first.
    fn wants_key(&self) -> bool {
        false
    }

    /// How many frames the output lags the input, for the mixer's latency compensation.
    /// A deliberate shift (the [`Delay`] filter) is not latency and is not counted.
    fn latency_frames(&self) -> usize;

    /// Clears the filter's history (envelopes, look-ahead and delay lines), as for a
    /// new stream. Parameters are kept.
    fn reset(&mut self);
}

/// Lowest and highest engine rates the kernels accept, as the mixer's.
const RATES: std::ops::RangeInclusive<u32> = 8_000..=192_000;

/// Level floor for detectors, dBFS: anything quieter reads as this.
const FLOOR_DB: f32 = -200.0;

fn check_rate(rate: u32) -> Result<(), AudioError> {
    if RATES.contains(&rate) {
        Ok(())
    } else {
        Err(AudioError::InvalidConfig {
            what: "filter rate must be 8000..=192000",
        })
    }
}

/// `Ok` when `v` is in `range` (so never for NaN), else `InvalidConfig { what }`.
fn check(
    v: f32,
    range: std::ops::RangeInclusive<f32>,
    what: &'static str,
) -> Result<(), AudioError> {
    if range.contains(&v) {
        Ok(())
    } else {
        Err(AudioError::InvalidConfig { what })
    }
}

fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

fn lin_to_db(level: f32) -> f32 {
    if level > 0.0 {
        (20.0 * level.log10()).max(FLOOR_DB)
    } else {
        FLOOR_DB
    }
}

/// A duration in milliseconds as whole frames at `rate`, rounded.
fn ms_to_frames(ms: f32, rate: u32) -> usize {
    (f64::from(ms) * f64::from(rate) / 1000.0).round() as usize
}

/// The coefficient of a one-pole smoother with time constant `ms` (it covers `1 − 1/e`
/// of a step in that time); 0, an instant follower, for `ms == 0`.
fn one_pole(ms: f32, rate: u32) -> f32 {
    if ms <= 0.0 {
        0.0
    } else {
        (-1000.0 / (f64::from(ms) * f64::from(rate))).exp() as f32
    }
}

/// A sample, or 0 when it isn't finite.
fn finite(x: f32) -> f32 {
    if x.is_finite() { x } else { 0.0 }
}

/// The stereo-linked level of a frame: the larger absolute sample.
fn frame_level(frame: &[f32]) -> f32 {
    frame.iter().fold(0.0f32, |m, &x| m.max(finite(x).abs()))
}

#[cfg(test)]
pub(crate) mod testsig {
    //! Reference signals for the filter tests.

    /// `frames` of a stereo square wave (both channels equal) of peak `amp` with a period
    /// of `period` frames. Its absolute value is `amp` at every sample, so a peak detector
    /// reads it as a steady level.
    pub(crate) fn square(amp: f32, period: usize, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|n| {
                let s = if (n % period) < period / 2 { amp } else { -amp };
                [s, s]
            })
            .collect()
    }

    /// `frames` of a stereo sine (both channels equal) of peak `amp` at `freq` Hz.
    pub(crate) fn sine(amp: f32, freq: f64, rate: u32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|n| {
                let s = (f64::from(amp)
                    * (std::f64::consts::TAU * freq * n as f64 / f64::from(rate)).sin())
                    as f32;
                [s, s]
            })
            .collect()
    }

    /// The largest absolute sample.
    pub(crate) fn peak(s: &[f32]) -> f32 {
        s.iter().fold(0.0f32, |m, x| m.max(x.abs()))
    }

    /// The level in dB of `got` relative to `reference`.
    pub(crate) fn ratio_db(got: f32, reference: f32) -> f32 {
        20.0 * (got / reference).log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        assert!((db_to_lin(-6.0) - 0.501_187).abs() < 1e-6);
        assert!((lin_to_db(0.5) + 6.020_6).abs() < 1e-4);
        assert_eq!(lin_to_db(0.0), FLOOR_DB);
        assert_eq!(ms_to_frames(10.0, 48_000), 480);
        assert_eq!(one_pole(0.0, 48_000), 0.0);
        // After `ms` worth of frames a step has covered 1 − 1/e.
        let a = one_pole(10.0, 48_000);
        let left = a.powi(480);
        assert!((left - (-1.0f32).exp()).abs() < 1e-4);
    }

    #[test]
    fn rejects_bad_rates() {
        for rate in [0, 7_999, 192_001] {
            assert!(check_rate(rate).is_err());
        }
        assert!(check(f32::NAN, 0.0..=1.0, "x").is_err());
    }
}
