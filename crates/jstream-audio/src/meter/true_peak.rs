//! The true-peak meter of ITU-R BS.1770 Annex 2: each channel is upsampled 4× by a
//! polyphase interpolation filter, and the meter holds the largest absolute value of the
//! upsampled signal.
//!
//! **Filter.** The annex gives a 48-tap filter (four phases of 12 taps) as an example of
//! a suitable interpolator. Its phases differ in gain by up to about 0.25 dB in the
//! treble, so it reads a 12 kHz tone at 48 kHz up to +0.22 dB high at some phases (just
//! outside EBU Tech 3341's tolerance), and it rejects images by only about 40 dB. This
//! meter uses a longer filter built the way the resampler's is
//! (`rt/sinc.rs`): a Blackman–Harris windowed sinc of [`PHASES`]·[`TAPS`]
//! taps at the upsampled rate, cut off at [`CUTOFF`] of the input rate, each phase
//! normalised to unity DC gain. At 48 kHz its phases agree within 0.02 dB up to 20 kHz
//! and images are down by more than 110 dB. It costs 192 multiply-adds per sample.
//!
//! The annex's 12.04 dB attenuation before upsampling is for fixed-point headroom; this
//! meter works in `f32` and leaves it out. The meter also counts the input samples
//! themselves, so a reading is never below the sample peak.
//!
//! **Accuracy.** 4× oversampling can miss a peak that falls between two upsampled points:
//! a sine at `f` Hz reads at worst `cos(π·f / (4·rate))` of its peak (−0.27 dB at 15 kHz
//! and −0.48 dB at 20 kHz for 48 kHz). Up to about 15 kHz at 48 kHz that stays inside
//! EBU Tech 3341's true-peak tolerance (+0.2 / −0.4 dB). BS.1770 asks for 4× at rates up
//! to 48 kHz; higher rates use 4× too, which only makes the meter more accurate.

use std::f64::consts::PI;

/// Taps per phase, in input samples.
const TAPS: usize = 48;
/// Upsampling factor.
const PHASES: usize = 4;
/// Cutoff, as a fraction of the input rate (the input's Nyquist frequency is 0.5).
const CUTOFF: f64 = 23.0 / 48.0;

/// The interpolation filter: `[phase][tap]`, tap 0 multiplying the newest input sample.
fn design_filter() -> [[f32; TAPS]; PHASES] {
    let n = PHASES * TAPS;
    // Cycles per upsampled sample.
    let fc = CUTOFF / PHASES as f64;
    let centre = (n - 1) as f64 / 2.0;
    let mut table = [[0.0f32; TAPS]; PHASES];
    for (k, phase) in table.iter_mut().enumerate() {
        let mut taps = [0.0f64; TAPS];
        for (i, tap) in taps.iter_mut().enumerate() {
            let j = PHASES * i + k;
            let x = j as f64 - centre;
            let sinc = if x == 0.0 {
                1.0
            } else {
                (2.0 * PI * fc * x).sin() / (2.0 * PI * fc * x)
            };
            let t = j as f64 / (n - 1) as f64;
            let window = 0.358_75 - 0.488_29 * (2.0 * PI * t).cos()
                + 0.141_28 * (4.0 * PI * t).cos()
                - 0.011_68 * (6.0 * PI * t).cos();
            *tap = sinc * window;
        }
        let dc: f64 = taps.iter().sum();
        for (out, tap) in phase.iter_mut().zip(taps) {
            *out = (tap / dc) as f32;
        }
    }
    table
}

/// A stereo true-peak meter. Feed it interleaved stereo with
/// [`process`](Self::process); read the peak held since the last
/// [`take_peak`](Self::take_peak) or [`reset`](Self::reset).
///
/// ```
/// use jstream_audio::MeterReading;
/// use jstream_audio::meter::TruePeakMeter;
///
/// let mut tp = TruePeakMeter::new();
/// // A full-scale tone at a quarter of the sample rate, sampled 45° off its peaks: every
/// // sample is ±0.707, but the waveform between them reaches 1.0.
/// let block: Vec<f32> = (0..4800)
///     .flat_map(|n| {
///         let s = (std::f32::consts::FRAC_PI_4 + std::f32::consts::FRAC_PI_2 * n as f32).sin();
///         [s, s]
///     })
///     .collect();
/// tp.process(&block);
/// let [left, _] = tp.take_peak();
/// assert!(MeterReading::to_dbfs(left) > -0.3); // dBTP
/// ```
#[derive(Clone, Debug)]
pub struct TruePeakMeter {
    coeffs: [[f32; TAPS]; PHASES],
    /// Per channel, the last `TAPS` input samples, stored twice so a window is always one
    /// contiguous slice (`history[pos..pos + TAPS]`, oldest first).
    history: [[f32; 2 * TAPS]; 2],
    pos: usize,
    peak: [f32; 2],
}

impl Default for TruePeakMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl TruePeakMeter {
    /// A meter with silent history and no peak. Builds its filter table (3 KB, inline:
    /// nothing on the heap).
    pub fn new() -> Self {
        Self {
            coeffs: design_filter(),
            history: [[0.0; 2 * TAPS]; 2],
            pos: 0,
            peak: [0.0; 2],
        }
    }

    /// Meters a block of interleaved stereo samples. A trailing half frame is ignored;
    /// non-finite samples count as 0. Never allocates.
    pub fn process(&mut self, interleaved: &[f32]) {
        for frame in interleaved.chunks_exact(2) {
            // Write the sample at the newest slot of the window that starts at `pos + 1`.
            let next = (self.pos + 1) % TAPS;
            for (ch, &raw) in frame.iter().enumerate() {
                let x = if raw.is_finite() { raw } else { 0.0 };
                let h = &mut self.history[ch];
                h[self.pos] = x;
                h[self.pos + TAPS] = x;
                // Window oldest to newest is `h[next..next + TAPS]`; the table's taps run
                // newest to oldest.
                let window = &h[next..next + TAPS];
                let mut peak = self.peak[ch].max(x.abs());
                for phase in &self.coeffs {
                    let y: f32 = phase
                        .iter()
                        .zip(window.iter().rev())
                        .map(|(c, s)| c * s)
                        .sum();
                    peak = peak.max(y.abs());
                }
                self.peak[ch] = peak;
            }
            self.pos = next;
        }
    }

    /// The true peak per channel (left, right) since the last take or reset, linear
    /// (1.0 is 0 dBTP).
    pub fn peak(&self) -> [f32; 2] {
        self.peak
    }

    /// Returns the held peak and starts holding a new one. The filter history is kept, so
    /// a peak between two blocks is still caught.
    pub fn take_peak(&mut self) -> [f32; 2] {
        std::mem::take(&mut self.peak)
    }

    /// Clears the held peak and the filter history, as for a new stream. Never
    /// allocates.
    pub fn reset(&mut self) {
        self.history = [[0.0; 2 * TAPS]; 2];
        self.pos = 0;
        self.peak = [0.0; 2];
    }
}

#[cfg(test)]
mod tests {
    use super::super::testsig::sine;
    use super::*;

    const RATE: u32 = 48_000;

    fn to_db(level: f32) -> f32 {
        crate::MeterReading::to_dbfs(level)
    }

    /// The steady-state reading of a tone: the onset from silence is a step, which the
    /// interpolator rings on (as any band-limited reconstruction does), so the first
    /// 10 ms are metered and discarded.
    fn true_peak_db(freq: f64, dbfs: f64, phase: f64) -> f32 {
        let mut s = Vec::new();
        sine(&mut s, RATE, freq, dbfs, 0.1, phase);
        let mut tp = TruePeakMeter::new();
        let (warmup, rest) = s.split_at(2 * 480);
        tp.process(warmup);
        tp.take_peak();
        tp.process(rest);
        let [l, r] = tp.peak();
        assert_eq!(l, r);
        to_db(l)
    }

    fn sample_peak_db(freq: f64, dbfs: f64, phase: f64) -> f32 {
        let mut s = Vec::new();
        sine(&mut s, RATE, freq, dbfs, 0.1, phase);
        to_db(s.iter().fold(0.0f32, |m, x| m.max(x.abs())))
    }

    /// EBU Tech 3341's true-peak tolerance: +0.2 / −0.4 dB.
    fn within_tolerance(got: f32, want: f64) -> bool {
        let d = f64::from(got) - want;
        (-0.4..=0.2).contains(&d)
    }

    /// The classic inter-sample peak: a tone at `rate / 4`, sampled 45° off its crests.
    /// Every sample is −3.01 dB below the waveform's peak; the meter must see the peak.
    #[test]
    fn quarter_rate_tone_between_samples() {
        let phase = std::f64::consts::FRAC_PI_4;
        assert!((sample_peak_db(12_000.0, -6.0, phase) + 9.01).abs() < 0.01);
        let tp = true_peak_db(12_000.0, -6.0, phase);
        assert!(within_tolerance(tp, -6.0), "{tp} dBTP");
        // Full-scale samples, +3 dBTP: the over a sample-peak meter misses.
        let tp = true_peak_db(12_000.0, 3.01, phase);
        assert!(within_tolerance(tp, 3.01), "{tp} dBTP");
    }

    /// Tones up to 15 kHz at any phase read within EBU Tech 3341's tolerance. (The
    /// annex's example filter fails this at 12 kHz; see the module docs.)
    #[test]
    fn tones_read_within_ebu_tolerance() {
        for freq in [
            997.0, 3_000.0, 6_000.0, 8_000.0, 10_000.0, 12_000.0, 15_000.0,
        ] {
            for step in 0..16 {
                let phase = f64::from(step) * std::f64::consts::PI / 16.0;
                let tp = true_peak_db(freq, -6.0, phase);
                assert!(
                    within_tolerance(tp, -6.0),
                    "{freq} Hz, phase {phase}: {tp} dBTP"
                );
            }
        }
    }

    /// Up to 20 kHz the reading stays above the 4× bound plus the passband ripple.
    #[test]
    fn treble_stays_above_the_oversampling_bound() {
        for step in 0..16 {
            let phase = f64::from(step) * std::f64::consts::PI / 16.0;
            let tp = true_peak_db(20_000.0, -6.0, phase);
            assert!((-6.6..=-5.8).contains(&tp), "phase {phase}: {tp} dBTP");
        }
    }

    /// The reading is never below the sample peak, including at DC and for an impulse,
    /// where the filter's phases alone read low. A step from silence overshoots, as the
    /// waveform between the samples does.
    #[test]
    fn never_below_the_sample_peak() {
        let mut tp = TruePeakMeter::new();
        tp.process(&[0.5; 2 * 100]);
        let [step, _] = tp.take_peak();
        assert!((0.5..0.6).contains(&step), "step: {step}");
        tp.process(&[0.5; 2 * 100]);
        let [dc, _] = tp.take_peak();
        assert!((0.5..0.501).contains(&dc), "steady DC: {dc}");
        let mut impulse = vec![0.0; 2 * 100];
        impulse[100] = 1.0;
        impulse[101] = -0.25;
        tp.process(&impulse);
        let [l, r] = tp.take_peak();
        assert!(l >= 1.0 && r >= 0.25, "{l} {r}");
    }

    /// The channels are metered independently.
    #[test]
    fn channels_are_independent() {
        let mut tp = TruePeakMeter::new();
        let block: Vec<f32> = (0..480).flat_map(|_| [0.25, 0.0]).collect();
        tp.process(&block);
        assert_eq!(tp.peak()[1], 0.0);
        assert!((0.25..0.3).contains(&tp.peak()[0]));
    }

    /// Block boundaries change nothing: the history carries the filter across them.
    #[test]
    fn block_size_does_not_matter() {
        let mut s = Vec::new();
        sine(&mut s, RATE, 11_025.0, -1.0, 0.05, 0.3);
        let mut whole = TruePeakMeter::new();
        whole.process(&s);
        for block in [2, 6, 34, 2048] {
            let mut split = TruePeakMeter::new();
            for chunk in s.chunks(block) {
                split.process(chunk);
            }
            assert_eq!(split.peak(), whole.peak(), "block {block}");
        }
    }

    /// Taking the peak starts a new hold; reset also forgets the history.
    #[test]
    fn take_and_reset() {
        let mut tp = TruePeakMeter::new();
        tp.process(&[0.8, -0.9, 0.0, 0.0]);
        assert!(tp.take_peak()[1] >= 0.9);
        tp.process(&[0.0; 2]);
        // The impulse is still in the history and rings into the next outputs, but at
        // well under its sample value.
        assert!(tp.peak()[1] < 0.9);
        tp.reset();
        tp.process(&[0.0; 2 * TAPS]);
        assert_eq!(tp.peak(), [0.0, 0.0]);
    }

    /// NaN and infinity count as silence, so one bad sample can't pin the meter.
    #[test]
    fn non_finite_samples_are_ignored() {
        let mut tp = TruePeakMeter::new();
        tp.process(&[f32::NAN, f32::INFINITY, 0.1, 0.1]);
        let [l, r] = tp.peak();
        assert!(l.is_finite() && r.is_finite() && l < 0.2 && r < 0.2);
    }
}
