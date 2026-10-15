//! A gain filter: a fixed level change, ramped when it changes so a move never clicks.

use super::{AudioFilter, check, check_rate, db_to_lin, finite, ms_to_frames};
use crate::AudioError;

/// How long a gain change ramps for.
const RAMP_MS: f32 = 10.0;
/// Highest gain, dB, as the faders'.
const MAX_GAIN_DB: f32 = 24.0;
/// Lowest finite gain, dB. `-inf` is also accepted and silences.
const MIN_GAIN_DB: f32 = -120.0;

/// A gain stage, `-120` to `+24` dB or `-inf` (silence). A change ramps linearly over
/// 10 ms. Latency 0.
///
/// ```
/// use jstream_audio::filter::{AudioFilter, Gain};
///
/// let mut gain = Gain::new(48_000, -6.0)?;
/// let mut block = vec![1.0f32; 2 * 1024];
/// gain.process(&mut block);
/// assert!((block[0] - 0.501).abs() < 1e-3);
/// # Ok::<(), jstream_audio::AudioError>(())
/// ```
#[derive(Clone, Debug)]
pub struct Gain {
    gain_db: f32,
    current: f32,
    target: f32,
    step: f32,
    remaining: usize,
    ramp_frames: usize,
}

impl Gain {
    /// A gain stage at `gain_db`, starting at that level (no ramp).
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] for a rate outside 8000..=192000 Hz or a gain out
    /// of range.
    pub fn new(rate: u32, gain_db: f32) -> Result<Self, AudioError> {
        check_rate(rate)?;
        check_gain(gain_db)?;
        let level = db_to_lin(gain_db);
        Ok(Self {
            gain_db,
            current: level,
            target: level,
            step: 0.0,
            remaining: 0,
            ramp_frames: ms_to_frames(RAMP_MS, rate).max(1),
        })
    }

    /// The gain set last, dB.
    pub fn gain_db(&self) -> f32 {
        self.gain_db
    }

    /// Sets the gain; the level ramps to it over 10 ms from wherever it is now.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] for a gain out of range; the gain is unchanged.
    pub fn set_gain_db(&mut self, gain_db: f32) -> Result<(), AudioError> {
        check_gain(gain_db)?;
        self.gain_db = gain_db;
        self.target = db_to_lin(gain_db);
        self.remaining = self.ramp_frames;
        self.step = (self.target - self.current) / self.ramp_frames as f32;
        Ok(())
    }
}

fn check_gain(gain_db: f32) -> Result<(), AudioError> {
    if gain_db == f32::NEG_INFINITY {
        return Ok(());
    }
    check(
        gain_db,
        MIN_GAIN_DB..=MAX_GAIN_DB,
        "gain_db must be -120..=24 or -inf",
    )
}

impl AudioFilter for Gain {
    fn process(&mut self, block: &mut [f32]) {
        for frame in block.chunks_exact_mut(2) {
            if self.remaining > 0 {
                self.remaining -= 1;
                self.current = if self.remaining == 0 {
                    self.target
                } else {
                    self.current + self.step
                };
            }
            for s in frame {
                *s = finite(*s) * self.current;
            }
        }
    }

    fn latency_frames(&self) -> usize {
        0
    }

    fn reset(&mut self) {
        self.current = self.target;
        self.remaining = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_the_gain() {
        let mut g = Gain::new(48_000, 6.0).expect("valid");
        let mut b = vec![0.25f32; 64];
        g.process(&mut b);
        assert!(b.iter().all(|&s| (s - 0.25 * db_to_lin(6.0)).abs() < 1e-6));
        let mut mute = Gain::new(48_000, f32::NEG_INFINITY).expect("valid");
        let mut b = vec![0.5f32; 64];
        mute.process(&mut b);
        assert!(b.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn ramps_linearly_over_ten_ms() {
        let mut g = Gain::new(48_000, 0.0).expect("valid");
        g.set_gain_db(f32::NEG_INFINITY).expect("valid");
        let mut b = vec![1.0f32; 2 * 600];
        g.process(&mut b);
        // 480 frames of ramp from 1 to 0, each step 1/480, then silence.
        assert!((b[0] - (1.0 - 1.0 / 480.0)).abs() < 1e-5);
        assert!((b[2 * 239] - 0.5).abs() < 1e-3);
        assert_eq!(b[2 * 479], 0.0);
        assert!(b[2 * 480..].iter().all(|&s| s == 0.0));
        for w in b.chunks_exact(2).collect::<Vec<_>>().windows(2) {
            assert!(w[1][0] <= w[0][0]);
        }
    }

    #[test]
    fn ramp_spans_blocks_and_retargets() {
        let mut g = Gain::new(48_000, 0.0).expect("valid");
        g.set_gain_db(-12.0).expect("valid");
        let mut a = vec![1.0f32; 2 * 100];
        g.process(&mut a);
        // Changing mid-ramp continues from the current level, with no jump.
        g.set_gain_db(0.0).expect("valid");
        let mut b = vec![1.0f32; 2 * 600];
        g.process(&mut b);
        let jump = (b[0] - a[a.len() - 1]).abs();
        assert!(jump < 0.01, "jump {jump}");
        assert!((b[b.len() - 1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rejects_bad_settings_and_sanitizes() {
        assert!(Gain::new(48_000, 25.0).is_err());
        assert!(Gain::new(48_000, f32::NAN).is_err());
        assert!(Gain::new(1_000, 0.0).is_err());
        let mut g = Gain::new(48_000, 0.0).expect("valid");
        assert!(g.set_gain_db(f32::INFINITY).is_err());
        assert_eq!(g.gain_db(), 0.0);
        let mut b = [f32::NAN, f32::INFINITY, 0.5];
        g.process(&mut b);
        assert_eq!(b, [0.0, 0.0, 0.5]); // the half frame is untouched
    }
}
