//! A noise gate with hysteresis, hold, and linear attack and release ramps.

use super::{AudioFilter, check, check_rate, db_to_lin, frame_level, ms_to_frames, one_pole};
use crate::AudioError;

/// The level detector's release: a peak follower with instant attack that decays with
/// this time constant, so the level between a low tone's peaks doesn't drop below the
/// close threshold.
const DETECTOR_MS: f32 = 10.0;

/// A [`NoiseGate`]'s settings. The defaults are OBS's noise gate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GateParams {
    /// The level that opens the gate, dBFS, −96 to 0.
    pub open_threshold_db: f32,
    /// The level below which an open gate starts to close, dBFS, −96 up to
    /// `open_threshold_db`. The gap between the two is the hysteresis.
    pub close_threshold_db: f32,
    /// How long the gate takes to open fully, ms, 0 to 1000.
    pub attack_ms: f32,
    /// How long the level must stay below the close threshold before the gate starts
    /// to close, ms, 0 to 5000.
    pub hold_ms: f32,
    /// How long the gate takes to close fully, ms, 0 to 5000.
    pub release_ms: f32,
    /// The gain when closed, dB, `-inf` (silence) or −96 to 0.
    pub range_db: f32,
}

impl Default for GateParams {
    fn default() -> Self {
        Self {
            open_threshold_db: -26.0,
            close_threshold_db: -32.0,
            attack_ms: 25.0,
            hold_ms: 200.0,
            release_ms: 150.0,
            range_db: f32::NEG_INFINITY,
        }
    }
}

impl GateParams {
    /// Checks every field's range.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] naming the first field out of range.
    pub fn validate(&self) -> Result<(), AudioError> {
        check(
            self.open_threshold_db,
            -96.0..=0.0,
            "open_threshold_db must be -96..=0",
        )?;
        check(
            self.close_threshold_db,
            -96.0..=self.open_threshold_db,
            "close_threshold_db must be -96..=open_threshold_db",
        )?;
        check(self.attack_ms, 0.0..=1000.0, "attack_ms must be 0..=1000")?;
        check(self.hold_ms, 0.0..=5000.0, "hold_ms must be 0..=5000")?;
        check(self.release_ms, 0.0..=5000.0, "release_ms must be 0..=5000")?;
        if self.range_db != f32::NEG_INFINITY {
            check(
                self.range_db,
                -96.0..=0.0,
                "range_db must be -96..=0 or -inf",
            )?;
        }
        Ok(())
    }
}

/// A stereo-linked noise gate. Latency 0.
///
/// The detector follows the larger channel's absolute level (instant attack, 10 ms
/// decay). The gate opens when it reaches the open threshold, and stays open while it
/// stays at or above the close threshold; after it has been below that for the hold
/// time, the gate closes. Opening ramps the gain linearly from the range to 1 over the
/// attack time, closing back down over the release time.
#[derive(Clone, Debug)]
pub struct NoiseGate {
    rate: u32,
    params: GateParams,
    open_level: f32,
    close_level: f32,
    floor: f32,
    attack_step: f32,
    release_step: f32,
    hold_frames: usize,
    detector_coef: f32,
    // State.
    env: f32,
    open: bool,
    hold_left: usize,
    gain: f32,
}

impl NoiseGate {
    /// A closed gate.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] for a rate outside 8000..=192000 Hz or invalid
    /// `params`.
    pub fn new(rate: u32, params: GateParams) -> Result<Self, AudioError> {
        check_rate(rate)?;
        let mut gate = Self {
            rate,
            params,
            open_level: 0.0,
            close_level: 0.0,
            floor: 0.0,
            attack_step: 0.0,
            release_step: 0.0,
            hold_frames: 0,
            detector_coef: one_pole(DETECTOR_MS, rate),
            env: 0.0,
            open: false,
            hold_left: 0,
            gain: 0.0,
        };
        gate.set_params(params)?;
        gate.gain = gate.floor;
        Ok(gate)
    }

    /// The current settings.
    pub fn params(&self) -> GateParams {
        self.params
    }

    /// Changes the settings. The gate's state (open or closed, gain) carries over.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] when `params` don't [validate](GateParams::validate);
    /// the settings are unchanged.
    pub fn set_params(&mut self, params: GateParams) -> Result<(), AudioError> {
        params.validate()?;
        self.params = params;
        self.open_level = db_to_lin(params.open_threshold_db);
        self.close_level = db_to_lin(params.close_threshold_db);
        self.floor = db_to_lin(params.range_db);
        let span = 1.0 - self.floor;
        self.attack_step = span / ms_to_frames(params.attack_ms, self.rate).max(1) as f32;
        self.release_step = span / ms_to_frames(params.release_ms, self.rate).max(1) as f32;
        self.hold_frames = ms_to_frames(params.hold_ms, self.rate);
        Ok(())
    }

    /// Whether the gate is open (opening, open or holding).
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The gate's gain at the end of the last block, linear.
    pub fn gain(&self) -> f32 {
        self.gain
    }
}

impl AudioFilter for NoiseGate {
    fn process(&mut self, block: &mut [f32]) {
        for frame in block.chunks_exact_mut(2) {
            let level = frame_level(frame);
            self.env = level.max(self.env * self.detector_coef);
            if self.env >= self.open_level {
                self.open = true;
                self.hold_left = self.hold_frames;
            } else if self.open {
                if self.env >= self.close_level {
                    self.hold_left = self.hold_frames;
                } else if self.hold_left > 0 {
                    self.hold_left -= 1;
                } else {
                    self.open = false;
                }
            }
            self.gain = if self.open {
                (self.gain + self.attack_step).min(1.0)
            } else {
                (self.gain - self.release_step).max(self.floor)
            };
            for s in frame {
                *s = super::finite(*s) * self.gain;
            }
        }
    }

    fn latency_frames(&self) -> usize {
        0
    }

    fn reset(&mut self) {
        self.env = 0.0;
        self.open = false;
        self.hold_left = 0;
        self.gain = self.floor;
    }
}

#[cfg(test)]
mod tests {
    use super::super::testsig::{peak, ratio_db, sine, square};
    use super::*;

    const RATE: u32 = 48_000;

    fn params() -> GateParams {
        GateParams {
            open_threshold_db: -20.0,
            close_threshold_db: -30.0,
            attack_ms: 1.0,
            hold_ms: 10.0,
            release_ms: 5.0,
            range_db: f32::NEG_INFINITY,
        }
    }

    #[test]
    fn mutes_below_and_passes_above() {
        let mut g = NoiseGate::new(RATE, params()).expect("valid");
        // −40 dBFS: below both thresholds, silenced.
        let mut quiet = sine(0.01, 1000.0, RATE, 4800);
        g.process(&mut quiet);
        assert_eq!(peak(&quiet), 0.0);
        assert!(!g.is_open());
        // −6 dBFS: opens within the 1 ms attack (48 frames), then passes unchanged.
        let loud = sine(0.5, 1000.0, RATE, 4800);
        let mut out = loud.clone();
        g.process(&mut out);
        assert!(g.is_open());
        assert_eq!(&out[2 * 100..], &loud[2 * 100..]);
    }

    #[test]
    fn hysteresis_hold_and_release() {
        let mut g = NoiseGate::new(RATE, params()).expect("valid");
        let mut open = square(0.5, 48, 4800);
        g.process(&mut open);
        // −26 dBFS is between the thresholds: an open gate stays open indefinitely.
        let between = square(0.05, 48, 48_000);
        let mut out = between.clone();
        g.process(&mut out);
        assert!(g.is_open());
        assert_eq!(out, between);
        // −40 dBFS: the detector decays from −26 to −30 dBFS in 220 frames, the 10 ms hold
        // keeps the gate fully open for 480 more, then it closes over the 5 ms release.
        let mut out = square(0.01, 48, 4800);
        g.process(&mut out);
        assert!(!g.is_open());
        let full = out.iter().take_while(|s| s.abs() == 0.01).count() / 2;
        assert!((690..710).contains(&full), "fully open for {full} frames");
        let closed = out.iter().position(|&s| s == 0.0).expect("closes") / 2;
        assert!(
            (full + 230..full + 250).contains(&closed),
            "closed at {closed}"
        );
        // A gate that never reached the open threshold doesn't open on a level between.
        let mut g = NoiseGate::new(RATE, params()).expect("valid");
        let mut out = square(0.05, 48, 4800);
        g.process(&mut out);
        assert_eq!(peak(&out), 0.0);
    }

    #[test]
    fn attack_ramps_and_range_limits() {
        let p = GateParams {
            attack_ms: 10.0,
            range_db: -20.0,
            ..params()
        };
        let mut g = NoiseGate::new(RATE, p).expect("valid");
        let mut out = square(0.5, 48, 4800);
        g.process(&mut out);
        // Starts at the range (−20 dB) and reaches unity after 480 frames.
        assert!((ratio_db(out[0].abs(), 0.5) + 20.0).abs() < 0.2);
        assert!(out[2 * 240].abs() < 0.5);
        assert_eq!(out[2 * 480].abs(), 0.5);
        // Closing stops at the range.
        let mut quiet = square(0.001, 48, 48_000);
        g.process(&mut quiet);
        assert!((ratio_db(quiet[quiet.len() - 1].abs(), 0.001) + 20.0).abs() < 0.01);
    }

    #[test]
    fn block_size_independent() {
        let mut sig = sine(0.5, 300.0, RATE, 9600);
        sig.extend(sine(0.001, 300.0, RATE, 19_200));
        sig.extend(sine(0.3, 300.0, RATE, 9600));
        let mut whole = sig.clone();
        NoiseGate::new(RATE, params())
            .expect("valid")
            .process(&mut whole);
        let mut split = sig;
        let mut g = NoiseGate::new(RATE, params()).expect("valid");
        for chunk in split.chunks_mut(2 * 137) {
            g.process(chunk);
        }
        assert_eq!(whole, split);
    }

    #[test]
    fn rejects_bad_params() {
        let bad = [
            GateParams {
                close_threshold_db: -10.0,
                ..params()
            },
            GateParams {
                open_threshold_db: 1.0,
                ..params()
            },
            GateParams {
                attack_ms: -1.0,
                ..params()
            },
            GateParams {
                range_db: 3.0,
                ..params()
            },
            GateParams {
                hold_ms: f32::NAN,
                ..params()
            },
        ];
        for p in bad {
            assert!(NoiseGate::new(RATE, p).is_err(), "{p:?}");
        }
        let mut g = NoiseGate::new(RATE, GateParams::default()).expect("valid");
        assert!(g.set_params(bad[0]).is_err());
        assert_eq!(g.params(), GateParams::default());
        g.reset();
        assert_eq!(g.gain(), 0.0);
    }
}
