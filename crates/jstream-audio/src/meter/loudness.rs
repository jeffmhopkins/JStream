//! The BS.1770 loudness meter: momentary (400 ms), short-term (3 s) and integrated
//! (gated) loudness of a stereo signal, in LUFS.
//!
//! **Measure.** Each channel is K-weighted ([`kweight`](super::kweight)), and the loudness
//! of an interval is `−0.691 + 10·log10(Σ G_c · z_c)`, with `z_c` the mean square of
//! channel `c` over the interval and `G_c = 1` for left and right. The meter sums the
//! weighted squares over 100 ms steps and keeps the last 30 steps' sums, so:
//! - **momentary** is the last 4 steps (400 ms) and **short-term** the last 30 (3 s), both
//!   updated every 100 ms (EBU Tech 3341 asks for at least 10 Hz);
//! - each momentary value is also a BS.1770 **gating block** (400 ms, 75 % overlap), which
//!   feeds the integrated measure.
//!
//! **Integrated.** The blocks above the absolute gate (−70 LUFS) give a first loudness;
//! the relative gate is 10 LU below it, and the integrated loudness is that of the blocks
//! above both gates. Keeping every block would grow without bound, so the blocks are
//! counted in a histogram of [`BIN_LU`]-wide loudness bins, each bin holding its block
//! count and the exact sum of its blocks' mean squares. The loudness of any set of whole
//! bins is therefore exact; the only approximation is that the relative gate is applied
//! at a bin edge (the first bin whose lower edge is at or above the gate), so a block
//! within [`BIN_LU`] of the gate may be counted on the wrong side of it. Blocks louder
//! than the top bin go into the top bin, with their exact energy.
//!
//! The histogram is allocated by [`LoudnessMeter::new`]; nothing after it allocates.

use super::kweight::KWeighting;
use crate::error::AudioError;

/// Steps per second: the meter's update rate and the gating blocks' hop.
const STEPS_PER_SECOND: u32 = 10;
/// Steps in a momentary window and gating block (400 ms).
const MOMENTARY_STEPS: usize = 4;
/// Steps in a short-term window (3 s).
const SHORT_TERM_STEPS: usize = 30;
/// The absolute gate, LUFS.
const ABSOLUTE_GATE: f64 = -70.0;
/// The relative gate, LU below the absolutely gated loudness.
const RELATIVE_GATE: f64 = -10.0;
/// Histogram bin width, LU.
pub(crate) const BIN_LU: f64 = 0.02;
/// The top of the histogram, LUFS. Louder blocks share the top bin.
const HISTOGRAM_TOP: f64 = 30.0;
/// Number of histogram bins, from the absolute gate to the top.
const BINS: usize = ((HISTOGRAM_TOP - ABSOLUTE_GATE) / BIN_LU) as usize;

/// Loudness in LUFS of a K-weighted, channel-weighted mean square; `-inf` for 0.
fn lufs(mean_square: f64) -> f64 {
    -0.691 + 10.0 * mean_square.log10()
}

/// A stereo BS.1770 loudness meter. Feed it interleaved stereo at the rate it was built
/// for with [`process`](Self::process).
///
/// ```
/// use jstream_audio::meter::LoudnessMeter;
///
/// let mut meter = LoudnessMeter::new(48_000)?;
/// // A 1 kHz tone at −23 dBFS on both channels reads −23 LUFS (EBU Tech 3341, case 1).
/// let amp = 10f32.powf(-23.0 / 20.0);
/// let block: Vec<f32> = (0..48_000 * 5)
///     .flat_map(|n| {
///         let s = amp * (std::f32::consts::TAU * 1_000.0 * n as f32 / 48_000.0).sin();
///         [s, s]
///     })
///     .collect();
/// meter.process(&block);
/// let integrated = meter.integrated().expect("louder than the absolute gate");
/// assert!((integrated + 23.0).abs() < 0.1);
/// # Ok::<(), jstream_audio::AudioError>(())
/// ```
#[derive(Clone, Debug)]
pub struct LoudnessMeter {
    filters: [KWeighting; 2],
    /// Frames per 100 ms step.
    step_frames: u32,
    /// Frames into the current step.
    frames_in_step: u32,
    /// The weighted sum of squares of the current step so far.
    step_sum: f64,
    /// The last [`SHORT_TERM_STEPS`] completed steps' sums, oldest overwritten first.
    steps: [f64; SHORT_TERM_STEPS],
    /// Where the next completed step goes in `steps`.
    next_step: usize,
    /// Completed steps since the last reset, saturating at [`SHORT_TERM_STEPS`].
    filled: usize,
    momentary: Option<f64>,
    short_term: Option<f64>,
    /// Gating blocks per loudness bin.
    bin_count: Box<[u64]>,
    /// The sum of the gating blocks' mean squares per loudness bin.
    bin_energy: Box<[f64]>,
}

impl LoudnessMeter {
    /// A meter for interleaved stereo at `rate` Hz. The rate must be a whole number of
    /// frames per 100 ms (every standard rate is) and within 8 to 192 kHz, like the
    /// mixer's. Allocates the integrated-loudness histogram (about 80 KB).
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] for any other rate.
    pub fn new(rate: u32) -> Result<Self, AudioError> {
        if !(8_000..=192_000).contains(&rate) || !rate.is_multiple_of(STEPS_PER_SECOND) {
            return Err(AudioError::InvalidConfig {
                what: "loudness meter rate must be 8000..=192000 and a multiple of 10",
            });
        }
        Ok(Self {
            filters: [KWeighting::new(rate); 2],
            step_frames: rate / STEPS_PER_SECOND,
            frames_in_step: 0,
            step_sum: 0.0,
            steps: [0.0; SHORT_TERM_STEPS],
            next_step: 0,
            filled: 0,
            momentary: None,
            short_term: None,
            bin_count: vec![0; BINS].into_boxed_slice(),
            bin_energy: vec![0.0; BINS].into_boxed_slice(),
        })
    }

    /// Meters a block of interleaved stereo samples. A trailing half frame is ignored;
    /// non-finite samples count as 0. Never allocates.
    pub fn process(&mut self, interleaved: &[f32]) {
        for frame in interleaved.chunks_exact(2) {
            let mut sum = 0.0;
            for (filter, &raw) in self.filters.iter_mut().zip(frame) {
                let x = if raw.is_finite() { f64::from(raw) } else { 0.0 };
                let y = filter.process(x);
                sum += y * y;
            }
            self.step_sum += sum;
            self.frames_in_step += 1;
            if self.frames_in_step == self.step_frames {
                self.end_step();
            }
        }
    }

    /// Closes a 100 ms step: updates momentary and short-term, and gates the new block.
    fn end_step(&mut self) {
        self.steps[self.next_step] = self.step_sum;
        self.next_step = (self.next_step + 1) % SHORT_TERM_STEPS;
        self.filled = (self.filled + 1).min(SHORT_TERM_STEPS);
        self.step_sum = 0.0;
        self.frames_in_step = 0;
        let frames = f64::from(self.step_frames);
        if self.filled >= MOMENTARY_STEPS {
            let mean_square = self.last_steps(MOMENTARY_STEPS) / (MOMENTARY_STEPS as f64 * frames);
            let loudness = lufs(mean_square);
            self.momentary = Some(loudness);
            self.add_block(loudness, mean_square);
        }
        if self.filled >= SHORT_TERM_STEPS {
            let mean_square =
                self.last_steps(SHORT_TERM_STEPS) / (SHORT_TERM_STEPS as f64 * frames);
            self.short_term = Some(lufs(mean_square));
        }
    }

    /// The sum of the last `n` completed steps.
    fn last_steps(&self, n: usize) -> f64 {
        (1..=n)
            .map(|back| self.steps[(self.next_step + SHORT_TERM_STEPS - back) % SHORT_TERM_STEPS])
            .sum()
    }

    /// Counts a gating block that passes the absolute gate into its bin.
    fn add_block(&mut self, loudness: f64, mean_square: f64) {
        if loudness.is_nan() || loudness <= ABSOLUTE_GATE {
            return;
        }
        let bin = (((loudness - ABSOLUTE_GATE) / BIN_LU) as usize).min(BINS - 1);
        self.bin_count[bin] += 1;
        self.bin_energy[bin] += mean_square;
    }

    /// The loudness of the last 400 ms, LUFS, updated every 100 ms; `-inf` for silence.
    /// `None` until 400 ms have been metered.
    pub fn momentary(&self) -> Option<f64> {
        self.momentary
    }

    /// The loudness of the last 3 s, LUFS, updated every 100 ms; `-inf` for silence.
    /// `None` until 3 s have been metered.
    pub fn short_term(&self) -> Option<f64> {
        self.short_term
    }

    /// The gated loudness of everything since the last reset, LUFS. `None` while no
    /// gating block has passed the absolute gate (silence, or less than 400 ms metered).
    pub fn integrated(&self) -> Option<f64> {
        let (count, energy) = self.sum_bins(0);
        if count == 0 {
            return None;
        }
        let gate = lufs(energy / count as f64) + RELATIVE_GATE;
        let first = ((gate - ABSOLUTE_GATE) / BIN_LU).ceil().max(0.0) as usize;
        let (count, energy) = self.sum_bins(first.min(BINS));
        // The relative gate is below the loudest block, so `count` is never 0 here.
        (count > 0).then(|| lufs(energy / count as f64))
    }

    /// The block count and energy of bins `first..`.
    fn sum_bins(&self, first: usize) -> (u64, f64) {
        self.bin_count[first..]
            .iter()
            .zip(&self.bin_energy[first..])
            .fold((0, 0.0), |(c, e), (&bc, &be)| (c + bc, e + be))
    }

    /// Restarts the integrated measure (the `audio.reset_loudness` action of ADR-0028).
    /// Momentary and short-term carry on.
    pub fn reset_integrated(&mut self) {
        self.bin_count.fill(0);
        self.bin_energy.fill(0.0);
    }

    /// Forgets everything, filter state included, as for a new stream. Never allocates.
    pub fn reset(&mut self) {
        for f in &mut self.filters {
            f.reset();
        }
        self.frames_in_step = 0;
        self.step_sum = 0.0;
        self.steps = [0.0; SHORT_TERM_STEPS];
        self.next_step = 0;
        self.filled = 0;
        self.momentary = None;
        self.short_term = None;
        self.reset_integrated();
    }
}

#[cfg(test)]
mod tests {
    use super::super::testsig::sine;
    use super::*;

    const RATE: u32 = 48_000;
    /// EBU Tech 3341's tolerance for these cases: ±0.1 LU.
    const TOL: f64 = 0.1;

    /// Runs `(dBFS, seconds)` segments of a 1 kHz stereo tone through a new meter.
    /// `None` for a segment is digital silence.
    fn run(rate: u32, segments: &[(Option<f64>, f64)]) -> LoudnessMeter {
        let mut m = LoudnessMeter::new(rate).unwrap();
        let mut phase = 0.0;
        for &(level, seconds) in segments {
            let mut s = Vec::new();
            match level {
                Some(db) => phase = sine(&mut s, rate, 1_000.0, db, seconds, phase),
                None => s.resize((seconds * f64::from(rate)).round() as usize * 2, 0.0),
            }
            m.process(&s);
        }
        m
    }

    fn assert_near(got: Option<f64>, want: f64, what: &str) {
        let got = got.unwrap_or_else(|| panic!("{what}: no reading"));
        assert!((got - want).abs() <= TOL, "{what}: {got} LUFS, want {want}");
    }

    /// EBU Tech 3341, cases 1 and 2: a steady stereo 1 kHz tone at −23 and −33 dBFS reads
    /// −23 and −33 LUFS (integrated, momentary and short-term), at 48 and 44.1 kHz.
    #[test]
    fn ebu_3341_cases_1_and_2() {
        for rate in [48_000, 44_100] {
            for level in [-23.0, -33.0] {
                let m = run(rate, &[(Some(level), 20.0)]);
                assert_near(m.integrated(), level, "integrated");
                assert_near(m.momentary(), level, "momentary");
                assert_near(m.short_term(), level, "short-term");
            }
        }
    }

    /// EBU Tech 3341, case 3: −36 / −23 / −36 dBFS for 10 / 60 / 10 s. The quiet parts are
    /// 13 LU down, below the relative gate, so they don't count.
    #[test]
    fn ebu_3341_case_3_relative_gate() {
        let m = run(
            RATE,
            &[
                (Some(-36.0), 10.0),
                (Some(-23.0), 60.0),
                (Some(-36.0), 10.0),
            ],
        );
        assert_near(m.integrated(), -23.0, "integrated");
    }

    /// EBU Tech 3341, case 4: −72 / −36 / −23 / −36 / −72 dBFS for 10 / 10 / 60 / 10 / 10 s.
    /// The −72 parts are below the absolute gate.
    #[test]
    fn ebu_3341_case_4_absolute_gate() {
        let m = run(
            RATE,
            &[
                (Some(-72.0), 10.0),
                (Some(-36.0), 10.0),
                (Some(-23.0), 60.0),
                (Some(-36.0), 10.0),
                (Some(-72.0), 10.0),
            ],
        );
        assert_near(m.integrated(), -23.0, "integrated");
    }

    /// EBU Tech 3341, case 5: −26 / −20 / −26 dBFS for 20 / 20.1 / 20 s. Everything is
    /// within 10 LU, so everything counts, and the energy average is −23 LUFS.
    #[test]
    fn ebu_3341_case_5_energy_average() {
        let m = run(
            RATE,
            &[
                (Some(-26.0), 20.0),
                (Some(-20.0), 20.1),
                (Some(-26.0), 20.0),
            ],
        );
        assert_near(m.integrated(), -23.0, "integrated");
    }

    /// A gated block well below the absolute gate and silence give no integrated reading;
    /// silence reads `-inf` momentary and short-term.
    #[test]
    fn silence_and_the_absolute_gate() {
        let m = run(RATE, &[(None, 5.0)]);
        assert_eq!(m.integrated(), None);
        assert_eq!(m.momentary(), Some(f64::NEG_INFINITY));
        assert_eq!(m.short_term(), Some(f64::NEG_INFINITY));
        let m = run(RATE, &[(Some(-75.0), 5.0)]);
        assert_eq!(m.integrated(), None);
        assert_near(m.momentary(), -75.0, "momentary");
    }

    /// The windows are 400 ms and 3 s: no reading before they fill, and a 400 ms burst
    /// in silence reads at full level momentary and 10·log10(0.4 / 3) below it short-term.
    #[test]
    fn window_lengths() {
        let m = run(RATE, &[(Some(-20.0), 0.39)]);
        assert_eq!(m.momentary(), None);
        let m = run(RATE, &[(Some(-20.0), 0.4)]);
        assert_near(m.momentary(), -20.0, "momentary at 400 ms");
        assert_eq!(m.short_term(), None);
        let m = run(RATE, &[(None, 2.6), (Some(-20.0), 0.4)]);
        assert_near(m.momentary(), -20.0, "momentary");
        assert_near(
            m.short_term(),
            -20.0 + 10.0 * (0.4f64 / 3.0).log10(),
            "short-term",
        );
        // 400 ms later the burst has left the momentary window. What's left is the click
        // of the tone stopping mid-cycle, rung out by the K-weighting filter.
        let m = run(RATE, &[(None, 2.6), (Some(-20.0), 0.4), (None, 0.4)]);
        let after = m.momentary().unwrap();
        assert!(after < -50.0, "momentary after the burst: {after}");
    }

    /// The reading doesn't depend on how the signal is split into blocks.
    #[test]
    fn block_size_does_not_matter() {
        let mut s = Vec::new();
        sine(&mut s, RATE, 997.0, -18.0, 4.0, 0.0);
        let mut whole = LoudnessMeter::new(RATE).unwrap();
        whole.process(&s);
        let mut split = LoudnessMeter::new(RATE).unwrap();
        for chunk in s.chunks(2 * 1021) {
            split.process(chunk);
        }
        assert_eq!(split.momentary(), whole.momentary());
        assert_eq!(split.short_term(), whole.short_term());
        assert_eq!(split.integrated(), whole.integrated());
    }

    /// One channel at a level reads 3 dB below both channels at it (G = 1 per channel).
    #[test]
    fn channels_add_in_power() {
        let mut s = Vec::new();
        sine(&mut s, RATE, 1_000.0, -20.0, 5.0, 0.0);
        for frame in s.chunks_exact_mut(2) {
            frame[1] = 0.0;
        }
        let mut m = LoudnessMeter::new(RATE).unwrap();
        m.process(&s);
        assert_near(m.integrated(), -20.0 - 10.0 * 2f64.log10(), "integrated");
    }

    /// `reset_integrated` restarts only the integrated measure; `reset` restarts all.
    #[test]
    fn resets() {
        let mut m = run(RATE, &[(Some(-30.0), 5.0)]);
        m.reset_integrated();
        assert_eq!(m.integrated(), None);
        assert!(m.momentary().is_some() && m.short_term().is_some());
        // The first gating blocks after the reset still overlap the quieter audio before
        // it, so measure long enough that they don't move the result.
        let mut s = Vec::new();
        sine(&mut s, RATE, 1_000.0, -15.0, 30.0, 0.0);
        m.process(&s);
        assert_near(m.integrated(), -15.0, "integrated after reset");
        m.reset();
        assert_eq!(
            (m.momentary(), m.short_term(), m.integrated()),
            (None, None, None)
        );
    }

    /// Non-finite samples count as silence, and a block louder than the histogram's top
    /// still counts with its exact energy.
    #[test]
    fn extreme_input() {
        let mut m = LoudnessMeter::new(RATE).unwrap();
        m.process(&vec![f32::NAN; 2 * RATE as usize]);
        assert_eq!(m.momentary(), Some(f64::NEG_INFINITY));
        let mut s = Vec::new();
        sine(&mut s, RATE, 1_000.0, 36.0, 2.0, 0.0);
        let mut m = LoudnessMeter::new(RATE).unwrap();
        m.process(&s);
        assert_near(m.integrated(), 36.0, "integrated above the top bin");
    }

    #[test]
    fn rejects_unsupported_rates() {
        for rate in [0, 7_990, 44_101, 192_010] {
            assert!(matches!(
                LoudnessMeter::new(rate),
                Err(AudioError::InvalidConfig { .. })
            ));
        }
    }
}
