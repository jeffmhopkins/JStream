//! A feed-forward compressor with a soft knee and an optional sidechain key.
//!
//! The gain computer and the smoothing of the gain reduction in the log domain follow
//! Giannoulis, Massberg and Reiss, "Digital Dynamic Range Compressor Design — A
//! Tutorial and Analysis" (JAES, 2012): the static curve with a quadratic knee, then a
//! one-pole smoother with separate attack and release coefficients (their "smooth
//! branching" detector, placed after the gain computer).

use super::{AudioFilter, check, check_rate, db_to_lin, finite, frame_level, lin_to_db, one_pole};
use crate::AudioError;

/// A [`Compressor`]'s settings. The defaults are OBS's compressor, with a 6 dB knee.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompressorParams {
    /// Where compression starts, dBFS, −60 to 0.
    pub threshold_db: f32,
    /// Input dB over the threshold per output dB, 1 to `inf` (a limiter's curve).
    pub ratio: f32,
    /// The knee's width, dB, centred on the threshold, 0 (hard) to 24.
    pub knee_db: f32,
    /// Time constant for gain reduction to grow, ms, 0 to 500.
    pub attack_ms: f32,
    /// Time constant for gain reduction to recover, ms, 0 to 5000.
    pub release_ms: f32,
    /// Gain applied after compression, dB, 0 to 24.
    pub makeup_db: f32,
}

impl Default for CompressorParams {
    fn default() -> Self {
        Self {
            threshold_db: -18.0,
            ratio: 10.0,
            knee_db: 6.0,
            attack_ms: 6.0,
            release_ms: 60.0,
            makeup_db: 0.0,
        }
    }
}

impl CompressorParams {
    /// Checks every field's range.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] naming the first field out of range.
    pub fn validate(&self) -> Result<(), AudioError> {
        check(
            self.threshold_db,
            -60.0..=0.0,
            "threshold_db must be -60..=0",
        )?;
        check(self.ratio, 1.0..=f32::INFINITY, "ratio must be at least 1")?;
        check(self.knee_db, 0.0..=24.0, "knee_db must be 0..=24")?;
        check(self.attack_ms, 0.0..=500.0, "attack_ms must be 0..=500")?;
        check(self.release_ms, 0.0..=5000.0, "release_ms must be 0..=5000")?;
        check(self.makeup_db, 0.0..=24.0, "makeup_db must be 0..=24")?;
        Ok(())
    }

    /// The static curve: the gain reduction, dB (≥ 0), for a steady level of `level_db`.
    pub fn gain_reduction_db(&self, level_db: f32) -> f32 {
        let slope = 1.0 - 1.0 / self.ratio;
        let over = level_db - self.threshold_db;
        let half = self.knee_db / 2.0;
        if over <= -half {
            0.0
        } else if over < half {
            slope * (over + half) * (over + half) / (2.0 * self.knee_db)
        } else {
            slope * over
        }
    }
}

/// A stereo-linked compressor. Latency 0.
///
/// The detector reads the larger channel's absolute level of each frame, of the input
/// or, with [`process_keyed`](AudioFilter::process_keyed) and
/// [`with_key`](Self::with_key), of the key (ADR-0028's sidechain, for ducking). The
/// static curve's gain reduction is smoothed in dB with the attack and release time
/// constants, then applied with the makeup gain to both channels.
///
/// ```
/// use jstream_audio::filter::{AudioFilter, Compressor, CompressorParams};
///
/// // Duck music by up to 12 dB while a voice on the key is above −30 dBFS.
/// let params = CompressorParams { threshold_db: -30.0, ratio: f32::INFINITY, ..Default::default() };
/// let mut duck = Compressor::new(48_000, params)?.with_key(true);
/// let mut music = vec![0.1f32; 2 * 1024];
/// let voice = vec![0.5f32; 2 * 1024];
/// duck.process_keyed(&mut music, &voice);
/// assert!(duck.gain_reduction_db() > 12.0);
/// # Ok::<(), jstream_audio::AudioError>(())
/// ```
#[derive(Clone, Debug)]
pub struct Compressor {
    rate: u32,
    params: CompressorParams,
    keyed: bool,
    attack: f32,
    release: f32,
    /// Smoothed gain reduction, dB.
    gr: f32,
}

impl Compressor {
    /// A compressor that detects on its own input.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] for a rate outside 8000..=192000 Hz or invalid
    /// `params`.
    pub fn new(rate: u32, params: CompressorParams) -> Result<Self, AudioError> {
        check_rate(rate)?;
        let mut c = Self {
            rate,
            params,
            keyed: false,
            attack: 0.0,
            release: 0.0,
            gr: 0.0,
        };
        c.set_params(params)?;
        Ok(c)
    }

    /// Sets whether the compressor takes a key input. With one, it reports
    /// [`wants_key`](AudioFilter::wants_key) and detects on the key passed to
    /// [`process_keyed`](AudioFilter::process_keyed); [`process`](AudioFilter::process)
    /// then detects on silence, so the gain reduction recovers.
    pub fn with_key(mut self, keyed: bool) -> Self {
        self.keyed = keyed;
        self
    }

    /// The current settings.
    pub fn params(&self) -> CompressorParams {
        self.params
    }

    /// Changes the settings. The smoothed gain reduction carries over.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] when `params` don't
    /// [validate](CompressorParams::validate); the settings are unchanged.
    pub fn set_params(&mut self, params: CompressorParams) -> Result<(), AudioError> {
        params.validate()?;
        self.params = params;
        self.attack = one_pole(params.attack_ms, self.rate);
        self.release = one_pole(params.release_ms, self.rate);
        Ok(())
    }

    /// The gain reduction at the end of the last block, dB (≥ 0, before makeup), for a
    /// gain-reduction meter.
    pub fn gain_reduction_db(&self) -> f32 {
        self.gr
    }

    fn run(&mut self, block: &mut [f32], key: Option<&[f32]>) {
        let makeup = self.params.makeup_db;
        for (i, frame) in block.chunks_exact_mut(2).enumerate() {
            let level = match key {
                None => frame_level(frame),
                Some(key) => key.get(2 * i..2 * i + 2).map_or(0.0, frame_level),
            };
            let target = self.params.gain_reduction_db(lin_to_db(level));
            let coef = if target > self.gr {
                self.attack
            } else {
                self.release
            };
            self.gr = target + coef * (self.gr - target);
            let gain = db_to_lin(makeup - self.gr);
            for s in frame {
                *s = finite(*s) * gain;
            }
        }
    }
}

impl AudioFilter for Compressor {
    fn process(&mut self, block: &mut [f32]) {
        if self.keyed {
            self.run(block, Some(&[]));
        } else {
            self.run(block, None);
        }
    }

    fn process_keyed(&mut self, block: &mut [f32], key: &[f32]) {
        if self.keyed {
            self.run(block, Some(key));
        } else {
            self.run(block, None);
        }
    }

    fn wants_key(&self) -> bool {
        self.keyed
    }

    fn latency_frames(&self) -> usize {
        0
    }

    fn reset(&mut self) {
        self.gr = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::super::testsig::{peak, ratio_db, sine, square};
    use super::*;

    const RATE: u32 = 48_000;

    fn hard(threshold_db: f32, ratio: f32) -> CompressorParams {
        CompressorParams {
            threshold_db,
            ratio,
            knee_db: 0.0,
            attack_ms: 1.0,
            release_ms: 10.0,
            makeup_db: 0.0,
        }
    }

    /// The steady output level of a square wave at `in_db`, dBFS.
    fn steady_out_db(p: CompressorParams, in_db: f32) -> f32 {
        let amp = db_to_lin(in_db);
        let mut s = square(amp, 96, 48_000);
        Compressor::new(RATE, p).expect("valid").process(&mut s);
        lin_to_db(s[s.len() - 1].abs())
    }

    #[test]
    fn static_curve() {
        let p = hard(-18.0, 4.0);
        // Below the threshold: unchanged. Above: 1/4 of the excess.
        assert!((steady_out_db(p, -24.0) + 24.0).abs() < 1e-3);
        assert!((steady_out_db(p, -6.0) - (-18.0 + 12.0 / 4.0)).abs() < 1e-3);
        assert!((steady_out_db(p, 0.0) - (-18.0 + 18.0 / 4.0)).abs() < 1e-3);
        // Infinite ratio: everything above the threshold comes out at it.
        let lim = hard(-12.0, f32::INFINITY);
        assert!((steady_out_db(lim, -3.0) + 12.0).abs() < 1e-3);
        // Makeup applies on top.
        let up = CompressorParams {
            makeup_db: 6.0,
            ..p
        };
        assert!((steady_out_db(up, -30.0) + 24.0).abs() < 1e-3);
    }

    #[test]
    fn soft_knee() {
        let p = CompressorParams {
            knee_db: 12.0,
            ..hard(-20.0, 2.0)
        };
        // Outside the knee the curve is the hard one; at the threshold it is
        // slope · W / 8; the curve and its slope are continuous at the knee's edges.
        assert_eq!(p.gain_reduction_db(-26.0), 0.0);
        assert!((p.gain_reduction_db(-20.0) - 0.5 * 12.0 / 8.0).abs() < 1e-6);
        assert!((p.gain_reduction_db(-14.0) - 0.5 * 6.0).abs() < 1e-5);
        assert!((p.gain_reduction_db(-10.0) - 0.5 * 10.0).abs() < 1e-5);
        let d = 1e-3;
        let slope_in = (p.gain_reduction_db(-14.0) - p.gain_reduction_db(-14.0 - d)) / d;
        assert!((slope_in - 0.5).abs() < 1e-2);
        assert!((steady_out_db(p, -20.0) - (-20.0 - 0.75)).abs() < 1e-3);
    }

    #[test]
    fn attack_and_release_time_constants() {
        let p = CompressorParams {
            attack_ms: 10.0,
            release_ms: 100.0,
            ..hard(-20.0, f32::INFINITY)
        };
        let mut c = Compressor::new(RATE, p).expect("valid");
        // A step from silence to 0 dBFS asks for 20 dB of reduction; after 10 ms the
        // smoothed reduction covers 1 − 1/e of it.
        let mut s = square(1.0, 96, 480);
        c.process(&mut s);
        let want = 20.0 * (1.0 - (-1.0f32).exp());
        assert!((c.gain_reduction_db() - want).abs() < 0.05);
        let mut s = square(1.0, 96, 48_000);
        c.process(&mut s);
        assert!((c.gain_reduction_db() - 20.0).abs() < 1e-3);
        // Back to silence: after 100 ms, 1/e of it remains.
        let mut s = vec![0.0f32; 2 * 4800];
        c.process(&mut s);
        assert!((c.gain_reduction_db() - 20.0 * (-1.0f32).exp()).abs() < 0.05);
    }

    #[test]
    fn sidechain_key_ducks_the_input() {
        let p = CompressorParams {
            threshold_db: -30.0,
            ratio: f32::INFINITY,
            knee_db: 0.0,
            attack_ms: 0.0,
            release_ms: 0.0,
            makeup_db: 0.0,
        };
        // Music at −20 dBFS, voice key at −10 dBFS: the music drops by the key's 20 dB
        // over the threshold, though the music alone is already 10 dB over it.
        let music = sine(0.1, 440.0, RATE, 4800);
        let voice = square(db_to_lin(-10.0), 96, 4800);
        let mut c = Compressor::new(RATE, p).expect("valid").with_key(true);
        assert!(c.wants_key());
        let mut out = music.clone();
        c.process_keyed(&mut out, &voice);
        assert!((ratio_db(peak(&out), peak(&music)) + 20.0).abs() < 1e-3);
        // A silent key: no reduction, even though the input is over the threshold.
        let mut out = music.clone();
        c.process_keyed(&mut out, &[0.0; 2 * 4800]);
        assert_eq!(out, music);
        // A short key reads as silence past its end; `process` on a keyed compressor
        // detects on silence.
        let mut out = music.clone();
        c.process_keyed(&mut out, &voice[..2 * 100]);
        assert!(
            out[..2 * 100]
                .iter()
                .zip(&music)
                .all(|(o, m)| (o - m * 0.1).abs() < 1e-6)
        );
        assert_eq!(&out[2 * 100..], &music[2 * 100..]);
        let mut out = music.clone();
        c.process(&mut out);
        assert_eq!(out, music);
        // Without a key input, a key passed in is ignored.
        let mut c = Compressor::new(RATE, p).expect("valid");
        assert!(!c.wants_key());
        let mut out = music.clone();
        c.process_keyed(&mut out, &voice);
        assert!((ratio_db(peak(&out), peak(&music)) + 10.0).abs() < 1e-2);
    }

    #[test]
    fn block_size_independent_and_reset() {
        let mut sig = sine(0.9, 200.0, RATE, 9600);
        sig.extend(sine(0.05, 200.0, RATE, 9600));
        let mut whole = sig.clone();
        let mut c = Compressor::new(RATE, CompressorParams::default()).expect("valid");
        c.process(&mut whole);
        let mut split = sig;
        let mut c2 = Compressor::new(RATE, CompressorParams::default()).expect("valid");
        for chunk in split.chunks_mut(2 * 100 + 2) {
            c2.process(chunk);
        }
        assert_eq!(whole, split);
        c.reset();
        assert_eq!(c.gain_reduction_db(), 0.0);
    }

    #[test]
    fn rejects_bad_params() {
        let bad = [
            CompressorParams {
                ratio: 0.5,
                ..Default::default()
            },
            CompressorParams {
                threshold_db: 1.0,
                ..Default::default()
            },
            CompressorParams {
                knee_db: f32::NAN,
                ..Default::default()
            },
            CompressorParams {
                release_ms: 6000.0,
                ..Default::default()
            },
        ];
        for p in bad {
            assert!(Compressor::new(RATE, p).is_err(), "{p:?}");
        }
        let mut c = Compressor::new(RATE, CompressorParams::default()).expect("valid");
        assert!(c.set_params(bad[0]).is_err());
        assert_eq!(c.params(), CompressorParams::default());
    }
}
