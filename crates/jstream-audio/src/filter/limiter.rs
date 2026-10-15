//! A look-ahead peak limiter whose output never exceeds its ceiling.
//!
//! **Gain.** For each frame the limiter computes the gain `r = min(1, ceiling / peak)`
//! that would bring the frame's larger absolute sample down to the ceiling. With a
//! window of `W = lookahead + 1` frames it takes the minimum of `r` over the last `W`
//! frames (a sliding minimum, a fixed-size monotonic queue), lets that recover no
//! faster than the release time constant (a recovery is never allowed above the
//! minimum), and averages the result over the last `W` frames (a box filter). The audio
//! is delayed by `lookahead` frames. Every value in the box covering a frame's delayed
//! output had that frame in its minimum window, so the averaged gain is at most that
//! frame's `r`: the output stays under the ceiling, and the gain falls as a smooth
//! ramp over the look-ahead instead of a step. A final clamp at the ceiling catches
//! only rounding, or samples already in the delay line when the ceiling is lowered.
//!
//! It limits sample peaks, not true peaks; a true-peak limiter would oversample the
//! detector as [`TruePeakMeter`](crate::meter::TruePeakMeter) does.

use super::{
    AudioFilter, check, check_rate, db_to_lin, finite, frame_level, ms_to_frames, one_pole,
};
use crate::AudioError;

/// The longest look-ahead, ms.
pub const MAX_LOOKAHEAD_MS: f32 = 20.0;

/// A [`Limiter`]'s settings that can change while it runs. The look-ahead is fixed when
/// it is built, since it sets the latency.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LimiterParams {
    /// The highest output sample, dBFS, −30 to 0.
    pub ceiling_db: f32,
    /// Time constant for the gain to recover, ms, 1 to 5000.
    pub release_ms: f32,
}

impl Default for LimiterParams {
    fn default() -> Self {
        Self {
            ceiling_db: -1.0,
            release_ms: 60.0,
        }
    }
}

impl LimiterParams {
    /// Checks every field's range.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] naming the first field out of range.
    pub fn validate(&self) -> Result<(), AudioError> {
        check(self.ceiling_db, -30.0..=0.0, "ceiling_db must be -30..=0")?;
        check(self.release_ms, 1.0..=5000.0, "release_ms must be 1..=5000")?;
        Ok(())
    }
}

/// A stereo-linked look-ahead limiter. Its latency is the look-ahead.
///
/// ```
/// use jstream_audio::filter::{AudioFilter, Limiter, LimiterParams};
///
/// let mut lim = Limiter::new(48_000, 5.0, LimiterParams { ceiling_db: -1.0, release_ms: 50.0 })?;
/// assert_eq!(lim.latency_frames(), 240);
/// let mut block: Vec<f32> = (0..2048).map(|n| if n % 64 < 32 { 1.5 } else { -1.5 }).collect();
/// lim.process(&mut block);
/// assert!(block.iter().all(|s| s.abs() <= 0.892_f32 + 1e-6)); // −1 dBFS
/// # Ok::<(), jstream_audio::AudioError>(())
/// ```
#[derive(Clone, Debug)]
pub struct Limiter {
    rate: u32,
    params: LimiterParams,
    ceiling: f32,
    release: f32,
    /// `lookahead + 1`.
    window: usize,
    /// Frames since the start (or reset), for the sliding minimum's expiry.
    frame: u64,
    /// The audio delay line, `window` frames; the frame written now is read back
    /// `window - 1` frames later.
    audio: Vec<[f32; 2]>,
    /// The sliding minimum: a ring of `window` (frame, gain) entries holding an
    /// increasing run of gains, oldest first.
    min_frames: Vec<u64>,
    min_gains: Vec<f32>,
    min_head: usize,
    min_len: usize,
    /// The minimum after release smoothing.
    held: f32,
    /// The box filter: the last `window` held values and their sum.
    boxed: Vec<f32>,
    box_sum: f64,
    /// The gain applied to the last output frame.
    gain: f32,
    /// How far the largest output sample went over the ceiling before the final clamp,
    /// so the tests can tell that the gain, not the clamp, holds the ceiling.
    #[cfg(test)]
    overshoot: f32,
}

impl Limiter {
    /// A limiter with `lookahead_ms` of look-ahead (0 to [`MAX_LOOKAHEAD_MS`]; rounded to
    /// whole frames). Allocates its delay line and queues for that look-ahead.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] for a rate outside 8000..=192000 Hz, a look-ahead
    /// out of range, or invalid `params`.
    pub fn new(rate: u32, lookahead_ms: f32, params: LimiterParams) -> Result<Self, AudioError> {
        check_rate(rate)?;
        check(
            lookahead_ms,
            0.0..=MAX_LOOKAHEAD_MS,
            "lookahead_ms must be 0..=20",
        )?;
        let window = ms_to_frames(lookahead_ms, rate) + 1;
        let mut lim = Self {
            rate,
            params,
            ceiling: 1.0,
            release: 0.0,
            window,
            frame: 0,
            audio: vec![[0.0; 2]; window],
            min_frames: vec![0; window],
            min_gains: vec![1.0; window],
            min_head: 0,
            min_len: 0,
            held: 1.0,
            boxed: vec![1.0; window],
            box_sum: window as f64,
            gain: 1.0,
            #[cfg(test)]
            overshoot: 0.0,
        };
        lim.set_params(params)?;
        Ok(lim)
    }

    /// The current settings.
    pub fn params(&self) -> LimiterParams {
        self.params
    }

    /// Changes the ceiling and release.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] when `params` don't
    /// [validate](LimiterParams::validate); the settings are unchanged.
    pub fn set_params(&mut self, params: LimiterParams) -> Result<(), AudioError> {
        params.validate()?;
        self.params = params;
        self.ceiling = db_to_lin(params.ceiling_db);
        self.release = one_pole(params.release_ms, self.rate);
        Ok(())
    }

    /// The gain reduction applied to the last output frame, dB (≥ 0), for a meter.
    pub fn gain_reduction_db(&self) -> f32 {
        -super::lin_to_db(self.gain)
    }

    /// Pushes `r` for the current frame into the sliding minimum and returns the
    /// minimum over the last `window` frames.
    fn sliding_min(&mut self, r: f32) -> f32 {
        let cap = self.window;
        // Drop newer entries that are no smaller: they can never be the minimum again.
        while self.min_len > 0 {
            let back = (self.min_head + self.min_len - 1) % cap;
            if self.min_gains[back] >= r {
                self.min_len -= 1;
            } else {
                break;
            }
        }
        // Expire the oldest entry once it leaves the window.
        if self.min_len > 0 && self.frame - self.min_frames[self.min_head] >= cap as u64 {
            self.min_head = (self.min_head + 1) % cap;
            self.min_len -= 1;
        }
        let slot = (self.min_head + self.min_len) % cap;
        self.min_frames[slot] = self.frame;
        self.min_gains[slot] = r;
        self.min_len += 1;
        self.min_gains[self.min_head]
    }
}

impl AudioFilter for Limiter {
    fn process(&mut self, block: &mut [f32]) {
        let cap = self.window;
        for frame in block.chunks_exact_mut(2) {
            let input = [finite(frame[0]), finite(frame[1])];
            let level = frame_level(&input);
            let r = if level > self.ceiling {
                self.ceiling / level
            } else {
                1.0
            };
            let min = self.sliding_min(r);
            // Instant attack; a recovery is smoothed and never passes the minimum.
            self.held = if min <= self.held {
                min
            } else {
                min + self.release * (self.held - min)
            };
            let slot = (self.frame % cap as u64) as usize;
            self.box_sum += f64::from(self.held) - f64::from(self.boxed[slot]);
            self.boxed[slot] = self.held;
            self.gain = (self.box_sum / cap as f64) as f32;
            // Delay line: write now, read the frame written `window - 1` frames ago.
            self.audio[slot] = input;
            let out = self.audio[(slot + 1) % cap];
            for (s, x) in frame.iter_mut().zip(out) {
                let y = x * self.gain;
                #[cfg(test)]
                {
                    self.overshoot = self.overshoot.max(y.abs() - self.ceiling);
                }
                *s = y.clamp(-self.ceiling, self.ceiling);
            }
            self.frame += 1;
        }
    }

    fn latency_frames(&self) -> usize {
        self.window - 1
    }

    fn reset(&mut self) {
        self.frame = 0;
        self.audio.fill([0.0; 2]);
        self.min_head = 0;
        self.min_len = 0;
        self.held = 1.0;
        self.boxed.fill(1.0);
        self.box_sum = self.window as f64;
        self.gain = 1.0;
    }
}

#[cfg(test)]
mod tests {
    use super::super::testsig::{peak, sine, square};
    use super::*;

    const RATE: u32 = 48_000;

    fn params(ceiling_db: f32) -> LimiterParams {
        LimiterParams {
            ceiling_db,
            release_ms: 50.0,
        }
    }

    #[test]
    fn latency_is_the_lookahead() {
        let mut lim = Limiter::new(RATE, 2.0, params(0.0)).expect("valid");
        assert_eq!(lim.latency_frames(), 96);
        // Below the ceiling the limiter is a pure delay of its latency.
        let sig = sine(0.5, 1000.0, RATE, 4800);
        let mut out = sig.clone();
        lim.process(&mut out);
        assert!(out[..2 * 96].iter().all(|&s| s == 0.0));
        assert_eq!(&out[2 * 96..], &sig[..sig.len() - 2 * 96]);
        assert_eq!(
            Limiter::new(RATE, 0.0, params(0.0))
                .expect("valid")
                .latency_frames(),
            0
        );
    }

    #[test]
    fn never_exceeds_the_ceiling() {
        let ceiling = db_to_lin(-3.0);
        for lookahead in [0.0, 0.5, 5.0, 20.0] {
            let mut lim = Limiter::new(RATE, lookahead, params(-3.0)).expect("valid");
            // Bursts of a loud tone, isolated transients and a +12 dB square.
            let mut sig = sine(0.2, 220.0, RATE, 4800);
            sig.extend(sine(3.0, 3000.0, RATE, 4800));
            let mut click = vec![0.0f32; 2 * 4800];
            for i in (0..4800).step_by(301) {
                click[2 * i] = 2.0;
                click[2 * i + 1] = -1.5;
            }
            sig.extend(click);
            sig.extend(square(4.0, 50, 4800));
            let mut out = sig.clone();
            lim.process(&mut out);
            assert!(
                peak(&out) <= ceiling,
                "lookahead {lookahead}: {}",
                peak(&out)
            );
            // The gain alone holds the ceiling, to rounding; the clamp is only a guard.
            assert!(
                lim.overshoot <= 1e-6,
                "lookahead {lookahead}: {}",
                lim.overshoot
            );
        }
    }

    #[test]
    fn ramps_down_ahead_of_a_peak() {
        let lookahead = 240;
        let mut lim = Limiter::new(RATE, 5.0, params(-6.0)).expect("valid");
        let mut sig = vec![0.25f32; 2 * 10_000];
        // A single frame at +6 dBFS at frame 500 needs 12 dB of reduction.
        sig[1000] = 2.0;
        sig[1001] = 2.0;
        let mut out = sig.clone();
        lim.process(&mut out);
        let out_at = |n: usize| out[2 * (n + lookahead)];
        // The gain falls over the look-ahead before the peak, which lands exactly on
        // the ceiling, and nothing earlier is touched.
        assert_eq!(out_at(500 - lookahead - 1), 0.25);
        assert!((out_at(500) - db_to_lin(-6.0)).abs() < 1e-5);
        assert!(out_at(400) < 0.25 && out_at(400) > out_at(450));
        // Then it recovers, monotonically, with the 50 ms release.
        assert!(out_at(520) < 0.25);
        for n in 501..700 {
            assert!(out_at(n + 1) >= out_at(n) - 1e-6);
        }
        assert!(lim.gain_reduction_db() < 1.0);
    }

    #[test]
    fn steady_overload_settles_at_the_ceiling() {
        let mut lim = Limiter::new(RATE, 1.0, params(-1.0)).expect("valid");
        let mut s = square(1.0, 64, 9600);
        lim.process(&mut s);
        let ceiling = db_to_lin(-1.0);
        assert!(
            s[2 * 1000..]
                .iter()
                .all(|x| (x.abs() - ceiling).abs() < 1e-6)
        );
        assert!((lim.gain_reduction_db() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn block_size_independent_and_reset() {
        let mut sig = sine(0.4, 330.0, RATE, 4800);
        sig.extend(sine(2.5, 330.0, RATE, 4800));
        sig.extend(sine(0.4, 330.0, RATE, 9600));
        let mut whole = sig.clone();
        let mut lim = Limiter::new(RATE, 3.0, params(-1.0)).expect("valid");
        lim.process(&mut whole);
        let mut split = sig.clone();
        let mut lim2 = Limiter::new(RATE, 3.0, params(-1.0)).expect("valid");
        for chunk in split.chunks_mut(2 * 61) {
            lim2.process(chunk);
        }
        assert_eq!(whole, split);
        lim.reset();
        let mut again = sig;
        lim.process(&mut again);
        assert_eq!(again, whole);
    }

    #[test]
    fn rejects_bad_settings() {
        assert!(Limiter::new(RATE, 21.0, params(-1.0)).is_err());
        assert!(Limiter::new(RATE, -1.0, params(-1.0)).is_err());
        assert!(Limiter::new(RATE, 1.0, params(1.0)).is_err());
        assert!(
            Limiter::new(
                RATE,
                1.0,
                LimiterParams {
                    release_ms: 0.0,
                    ..params(-1.0)
                }
            )
            .is_err()
        );
        let mut lim = Limiter::new(RATE, 1.0, params(-1.0)).expect("valid");
        assert!(lim.set_params(params(f32::NAN)).is_err());
        assert_eq!(lim.params(), params(-1.0));
    }
}
