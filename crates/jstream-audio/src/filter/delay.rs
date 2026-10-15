//! An audio delay: a deliberate shift (ADR-0028, D15), with its ring buffer allocated
//! at its maximum when it is built.

use super::{AudioFilter, check, check_rate, finite, ms_to_frames};
use crate::AudioError;

/// The longest delay a [`Delay`] can be built for, ms (ADR-0028's audio delay filter).
pub const MAX_DELAY_MS: f32 = 5000.0;

/// Delays audio by a set time, up to a maximum fixed when it is built.
///
/// Per ADR-0028 a delay is a deliberate shift, not latency: [`latency_frames`] is 0, so
/// the mixer doesn't compensate it away, and the mixer stamps the output later by
/// [`delay_frames`](Self::delay_frames). A change of delay jumps the read position at
/// once (the frames in between are skipped or repeated); settings changes rebuild the
/// audio snapshot anyway.
///
/// [`latency_frames`]: AudioFilter::latency_frames
///
/// ```
/// use jstream_audio::filter::{AudioFilter, Delay};
///
/// let mut delay = Delay::new(48_000, 500.0)?; // room for 500 ms
/// delay.set_delay_ms(100.0)?;
/// assert_eq!(delay.delay_frames(), 4800);
/// assert_eq!(delay.latency_frames(), 0);
/// # Ok::<(), jstream_audio::AudioError>(())
/// ```
#[derive(Clone, Debug)]
pub struct Delay {
    rate: u32,
    /// `max_frames + 1` frames, so a delay of `max_frames` never reads the slot being
    /// written.
    ring: Vec<[f32; 2]>,
    write: usize,
    delay: usize,
}

impl Delay {
    /// A delay with room for `max_ms` (0 to [`MAX_DELAY_MS`], rounded to whole frames),
    /// set to 0. Allocates the whole ring, up to 7.7 MB at 192 kHz and 5 s.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] for a rate outside 8000..=192000 Hz or a maximum
    /// out of range.
    pub fn new(rate: u32, max_ms: f32) -> Result<Self, AudioError> {
        check_rate(rate)?;
        check(max_ms, 0.0..=MAX_DELAY_MS, "max delay must be 0..=5000 ms")?;
        Ok(Self {
            rate,
            ring: vec![[0.0; 2]; ms_to_frames(max_ms, rate) + 1],
            write: 0,
            delay: 0,
        })
    }

    /// The longest delay this filter has room for, frames.
    pub fn max_frames(&self) -> usize {
        self.ring.len() - 1
    }

    /// The current delay, frames.
    pub fn delay_frames(&self) -> usize {
        self.delay
    }

    /// Sets the delay, rounded to whole frames.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] when it is negative, not finite or longer than the
    /// maximum; the delay is unchanged.
    pub fn set_delay_ms(&mut self, ms: f32) -> Result<(), AudioError> {
        check(ms, 0.0..=MAX_DELAY_MS, "delay must be 0..=5000 ms")?;
        self.set_delay_frames(ms_to_frames(ms, self.rate))
    }

    /// Sets the delay in frames.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] when it is longer than the maximum; the delay is
    /// unchanged.
    pub fn set_delay_frames(&mut self, frames: usize) -> Result<(), AudioError> {
        if frames > self.max_frames() {
            return Err(AudioError::InvalidConfig {
                what: "delay must not exceed the maximum the filter was built for",
            });
        }
        self.delay = frames;
        Ok(())
    }
}

impl AudioFilter for Delay {
    fn process(&mut self, block: &mut [f32]) {
        let cap = self.ring.len();
        for frame in block.chunks_exact_mut(2) {
            self.ring[self.write] = [finite(frame[0]), finite(frame[1])];
            let out = self.ring[(self.write + cap - self.delay) % cap];
            frame.copy_from_slice(&out);
            self.write = (self.write + 1) % cap;
        }
    }

    fn latency_frames(&self) -> usize {
        0
    }

    fn reset(&mut self) {
        self.ring.fill([0.0; 2]);
        self.write = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::super::testsig::sine;
    use super::*;

    const RATE: u32 = 48_000;

    #[test]
    fn shifts_by_the_delay() {
        let sig = sine(0.5, 1000.0, RATE, 9600);
        for (ms, frames) in [(0.0, 0), (10.0, 480), (100.0, 4800)] {
            let mut d = Delay::new(RATE, 100.0).expect("valid");
            d.set_delay_ms(ms).expect("in range");
            assert_eq!(d.delay_frames(), frames);
            let mut out = sig.clone();
            for chunk in out.chunks_mut(2 * 333) {
                d.process(chunk);
            }
            assert!(out[..2 * frames].iter().all(|&s| s == 0.0));
            assert_eq!(&out[2 * frames..], &sig[..sig.len() - 2 * frames]);
        }
    }

    #[test]
    fn maximum_and_changes() {
        let mut d = Delay::new(RATE, 10.0).expect("valid");
        assert_eq!(d.max_frames(), 480);
        assert!(d.set_delay_frames(481).is_err());
        assert!(d.set_delay_ms(11.0).is_err());
        assert!(d.set_delay_ms(f32::NAN).is_err());
        assert_eq!(d.delay_frames(), 0);
        // At the maximum, the frame written 480 frames ago comes out.
        d.set_delay_frames(480).expect("in range");
        let sig: Vec<f32> = (0..2000).flat_map(|n| [n as f32, -(n as f32)]).collect();
        let mut out = sig.clone();
        d.process(&mut out);
        assert_eq!(&out[2 * 480..], &sig[..sig.len() - 2 * 480]);
        // Shortening the delay reads newer frames at once.
        d.set_delay_frames(10).expect("in range");
        let mut next = vec![0.0f32; 2];
        d.process(&mut next);
        assert_eq!(next, [1990.0, -1990.0]);
        d.reset();
        let mut z = vec![1.0f32; 2 * 20];
        d.process(&mut z);
        assert!(z[..2 * 10].iter().all(|&s| s == 0.0));
        assert!(Delay::new(RATE, 5001.0).is_err());
        assert!(Delay::new(RATE, 5000.0).is_ok());
    }
}
