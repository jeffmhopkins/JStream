//! The streaming adaptive resampler: band-limited (windowed-sinc) interpolation at an
//! arbitrary, continuously adjustable fractional input position.
//!
//! **Algorithm.** Output frame `j` sits at input position `p_j = i + f` (`i` an integer
//! frame index, `0 <= f < 1`), and `p_{j+1} = p_j + step`, where
//! `step = rate_in / rate_out * (1 + u)` and `u` is the controller's correction
//! ([`control`](super::control)). Each output is the dot product of the `taps` input
//! frames `i - taps/2 + 1 ..= i + taps/2` with the kernel
//! `h(x) = 2·fc·sinc(2·fc·x) · w(x)`, evaluated at offsets `x = n - p_j`. The kernel is
//! precomputed for [`PHASES`] values of `f` (plus the end point `f = 1`), and the output
//! linearly interpolates between the results of the two neighbouring phases. The kernel
//! is symmetric around `x = 0`, so the resampler adds no delay: an output frame holds
//! exactly the input instant `p_j`. It only has to wait for `taps / 2` frames of
//! look-ahead.
//!
//! **Parameters.**
//! - `taps = 128 · ceil(rate_in / rate_out)` (128 at or below the engine rate): the
//!   kernel length S3 ran with (rubato, `sinc_len` 128), widened in input frames when
//!   downsampling so the transition band keeps its width in output frequency.
//! - Window `w`: 4-term Blackman–Harris over the kernel span (side lobes about −92 dB).
//! - Cutoff `fc = 0.5 · min(1, rate_out / rate_in) · 0.94`, in cycles per input frame:
//!   just below the lower Nyquist frequency, so that the transition band (about
//!   ±4 / taps cycles per frame) ends near it. Upsampling 44.1 kHz is flat to about
//!   19 kHz.
//! - [`PHASES`] = 256 with linear interpolation: the interpolation error is about
//!   `(π / 256)² / 8 ≈ −96 dB`.
//! - Every phase is normalised to unity DC gain.
//!
//! **Why not rubato,** which the spike measured with: the resampler runs inside the
//! audio thread's packet handler, frame by frame, so it needs no fixed chunks (rubato's
//! asynchronous resampler consumes fixed input chunks, which the spike had to stage, adding
//! up to a chunk of latency), its memory is visibly fixed at construction (R-RT-001), its
//! output position is exact and known (the timing error is computed from it), and the
//! workspace takes no new dependency (R-DEP-004).
//!
//! The coefficient table and histories are allocated by [`Resampler::new`]; nothing after
//! that allocates.

/// Kernel length, in input frames, at or below the engine rate.
pub(crate) const BASE_TAPS: usize = 128;
/// Precomputed fractional phases.
pub(crate) const PHASES: usize = 256;
/// Cutoff as a fraction of the lower Nyquist frequency.
pub(crate) const CUTOFF: f64 = 0.94;
/// The largest `rate_in / rate_out` supported (192 kHz into 48 kHz).
pub(crate) const MAX_DOWN_RATIO: u32 = 4;
/// The smallest `rate_in / rate_out` supported, as `rate_out / rate_in` (8 kHz into
/// 48 kHz).
pub(crate) const MAX_UP_RATIO: u32 = 6;

/// What the resampler hands its caller.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Output {
    /// One stereo frame.
    Frame([f32; 2]),
    /// This many frames of silence (from a long gap, without computing each one).
    Silence(u64),
}

/// One input's resampler state: kernel table, history and fractional position.
#[derive(Clone, Debug)]
pub(crate) struct Resampler {
    taps: usize,
    half: i64,
    /// `(PHASES + 1) * taps` coefficients; row `k` is phase `f = k / PHASES`.
    table: Box<[f32]>,
    /// Left and right input history: `taps` frames, stored twice so any `taps`-long
    /// window is one contiguous slice.
    hist: [Box<[f32]>; 2],
    /// Input frames pushed since the last [`reset`](Self::reset).
    count: i64,
    /// Next output's input position, as integer and fraction (`0 <= p_frac < 1`), so
    /// precision does not decay as the stream gets long.
    p_int: i64,
    p_frac: f64,
    step_nominal: f64,
    step: f64,
}

impl Resampler {
    /// A resampler from `rate_in` to `rate_out` Hz. The caller checks that both are
    /// non-zero and within [`MAX_DOWN_RATIO`] and [`MAX_UP_RATIO`].
    pub(crate) fn new(rate_in: u32, rate_out: u32) -> Self {
        let ratio = f64::from(rate_in) / f64::from(rate_out);
        let factor = rate_in.div_ceil(rate_out).max(1) as usize;
        let taps = BASE_TAPS * factor;
        let fc = 0.5 * (1.0 / ratio).min(1.0) * CUTOFF;
        Self {
            taps,
            half: (taps / 2) as i64,
            table: kernel_table(taps, fc),
            hist: [
                vec![0.0; 2 * taps].into_boxed_slice(),
                vec![0.0; 2 * taps].into_boxed_slice(),
            ],
            count: 0,
            p_int: 0,
            p_frac: 0.0,
            step_nominal: ratio,
            step: ratio,
        }
    }

    /// Clears the history (silence before the next input) and restarts input counting
    /// at 0, with the next output at input position `position` (clamped to `>= 0`).
    pub(crate) fn reset(&mut self, position: f64) {
        for h in &mut self.hist {
            h.fill(0.0);
        }
        self.count = 0;
        let p = if position.is_finite() {
            position.max(0.0)
        } else {
            0.0
        };
        // `p` is at most a few seconds of frames here, far inside i64.
        let whole = p.floor();
        self.p_int = whole as i64;
        self.p_frac = p - whole;
    }

    /// Sets the correction `u`: input is consumed `1 + u` times as fast as nominal.
    pub(crate) fn set_correction(&mut self, u: f64) {
        if u.is_finite() {
            self.step = self.step_nominal * (1.0 + u);
        }
    }

    /// The input position of the next output, in frames since the last reset.
    pub(crate) fn position(&self) -> f64 {
        self.p_int as f64 + self.p_frac
    }

    /// Input frames pushed since the last reset.
    pub(crate) fn input_count(&self) -> i64 {
        self.count
    }

    /// Kernel length in input frames.
    #[cfg(test)]
    pub(crate) fn taps(&self) -> usize {
        self.taps
    }

    fn advance(&mut self) {
        let p = self.p_frac + self.step;
        let whole = p.floor();
        self.p_int += whole as i64;
        self.p_frac = p - whole;
    }

    /// Appends one input frame and emits every output frame it completes.
    pub(crate) fn push(&mut self, frame: [f32; 2], emit: &mut impl FnMut(Output)) {
        let taps = self.taps;
        let slot = (self.count.rem_euclid(taps as i64)) as usize;
        for (c, h) in self.hist.iter_mut().enumerate() {
            h[slot] = frame[c];
            h[slot + taps] = frame[c];
        }
        self.count += 1;
        let newest = self.count - 1;
        // Outputs are emitted as soon as their window is complete, so the window of
        // each one ends exactly at `newest`: it is the last `taps` frames pushed.
        while self.p_int + self.half <= newest {
            let start = (self.count.rem_euclid(taps as i64)) as usize;
            let out = self.interpolate(start);
            emit(Output::Frame(out));
            self.advance();
        }
    }

    /// Appends `frames` frames of silence. Only the first `taps` are pushed one by one;
    /// after that the history is all zeros and the remaining outputs are reported as one
    /// [`Output::Silence`] run, so a long gap costs no dot products.
    pub(crate) fn push_silence(&mut self, frames: u64, emit: &mut impl FnMut(Output)) {
        let direct = frames.min(self.taps as u64);
        for _ in 0..direct {
            self.push([0.0; 2], emit);
        }
        let rest = frames - direct;
        if rest == 0 {
            return;
        }
        self.count = self
            .count
            .saturating_add(i64::try_from(rest).unwrap_or(i64::MAX));
        let newest = self.count - 1;
        let mut silent = 0u64;
        while self.p_int + self.half <= newest {
            silent += 1;
            self.advance();
        }
        if silent > 0 {
            emit(Output::Silence(silent));
        }
    }

    /// The output at the current position from the window starting at history slot
    /// `start`.
    fn interpolate(&self, start: usize) -> [f32; 2] {
        let taps = self.taps;
        let idx = self.p_frac * PHASES as f64;
        // `p_frac` is in [0, 1), so `k` is in 0..PHASES.
        let k = (idx as usize).min(PHASES - 1);
        let a = (idx - k as f64) as f32;
        let row0 = &self.table[k * taps..(k + 1) * taps];
        let row1 = &self.table[(k + 1) * taps..(k + 2) * taps];
        let mut out = [0.0f32; 2];
        for (c, h) in self.hist.iter().enumerate() {
            let w = &h[start..start + taps];
            let (d0, d1) = dot2(w, row0, row1);
            out[c] = d0 + a * (d1 - d0);
        }
        out
    }
}

/// Two dot products of `x` with `a` and `b`, in 8 independent lanes so the compiler
/// can vectorise them (a single running sum would serialise on float addition order).
fn dot2(x: &[f32], a: &[f32], b: &[f32]) -> (f32, f32) {
    let mut sa = [0.0f32; 8];
    let mut sb = [0.0f32; 8];
    let xs = x.chunks_exact(8);
    let rest = xs.remainder().len();
    for ((xc, ac), bc) in xs.zip(a.chunks_exact(8)).zip(b.chunks_exact(8)) {
        for l in 0..8 {
            sa[l] += xc[l] * ac[l];
            sb[l] += xc[l] * bc[l];
        }
    }
    let n = x.len();
    let mut ta: f32 = sa.iter().sum();
    let mut tb: f32 = sb.iter().sum();
    for i in n - rest..n {
        ta += x[i] * a[i];
        tb += x[i] * b[i];
    }
    (ta, tb)
}

/// The kernel table: row `k` holds `h(t - taps/2 + 1 - k/PHASES)` for `t` in
/// `0..taps`, normalised to sum to 1.
fn kernel_table(taps: usize, fc: f64) -> Box<[f32]> {
    let half = (taps / 2) as f64;
    let mut table = vec![0.0f32; (PHASES + 1) * taps];
    let mut row = vec![0.0f64; taps];
    for k in 0..=PHASES {
        let f = k as f64 / PHASES as f64;
        for (t, r) in row.iter_mut().enumerate() {
            let x = t as f64 - half + 1.0 - f;
            *r = 2.0 * fc * sinc(2.0 * fc * x) * blackman_harris((x + half) / taps as f64);
        }
        let sum: f64 = row.iter().sum();
        for (t, r) in row.iter().enumerate() {
            table[k * taps + t] = (r / sum) as f32;
        }
    }
    table.into_boxed_slice()
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// The 4-term Blackman–Harris window on `0..=1`.
fn blackman_harris(z: f64) -> f64 {
    use std::f64::consts::TAU;
    0.358_75 - 0.488_29 * (TAU * z).cos() + 0.141_28 * (2.0 * TAU * z).cos()
        - 0.011_68 * (3.0 * TAU * z).cos()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt::testutil::{fit_sine, tone};

    /// Resamples `secs` of a `freq` Hz tone (amplitude 0.5) from `rate_in` to
    /// `rate_out` with correction `u`; returns the left channel of the output.
    fn resample_tone(rate_in: u32, rate_out: u32, freq: f64, secs: f64, u: f64) -> Vec<f32> {
        let mut r = Resampler::new(rate_in, rate_out);
        r.set_correction(u);
        let mut out = Vec::new();
        let n = (secs * f64::from(rate_in)) as usize;
        for i in 0..n {
            let s = 0.5 * tone(freq, i as f64 / f64::from(rate_in)) as f32;
            r.push([s, -s], &mut |o| match o {
                Output::Frame(f) => {
                    assert_eq!(f[0], -f[1]);
                    out.push(f[0]);
                }
                Output::Silence(_) => panic!("no silence expected"),
            });
        }
        out
    }

    /// A 1 kHz tone keeps its amplitude, frequency and purity through each supported
    /// conversion: the best-fitting sine (on the output's own grid) has amplitude
    /// 0.5 ± 0.00001 at exactly 1 kHz, and what it doesn't explain is below −120 dB
    /// (measured: −136 to −145 dB).
    #[test]
    fn a_1khz_tone_survives_resampling() {
        for (rate_in, rate_out) in [
            (44_100, 48_000),
            (48_000, 48_000),
            (96_000, 48_000),
            (16_000, 48_000),
            (192_000, 48_000),
        ] {
            let out = resample_tone(rate_in, rate_out, 1000.0, 1.0, 0.0);
            // Skip the start-up (history filling with the tone).
            let body = &out[2000..out.len() - 100];
            let fit = fit_sine(body, 1000.0, f64::from(rate_out));
            assert!(
                (fit.amplitude - 0.5).abs() < 1e-5,
                "{rate_in}->{rate_out}: amplitude {}",
                fit.amplitude
            );
            assert!(
                fit.residual_db < -120.0,
                "{rate_in}->{rate_out}: residual {} dB",
                fit.residual_db
            );
        }
    }

    /// Near the top of the band: a 15 kHz tone from 44.1 kHz keeps its level within
    /// 0.01 dB, and the images the interpolation leaves are below −90 dB.
    #[test]
    fn a_15khz_tone_keeps_level_and_purity() {
        let out = resample_tone(44_100, 48_000, 15_000.0, 0.5, 0.0);
        let fit = fit_sine(&out[2000..out.len() - 100], 15_000.0, 48_000.0);
        let db = 20.0 * (fit.amplitude / 0.5).log10();
        assert!(db.abs() < 0.01, "level {db} dB");
        assert!(fit.residual_db < -90.0, "residual {} dB", fit.residual_db);
    }

    /// Downsampling removes what the engine rate can't carry: a 30 kHz tone at 96 kHz
    /// (above 24 kHz) comes out below −90 dB instead of aliasing to 18 kHz.
    #[test]
    fn downsampling_rejects_content_above_the_engine_nyquist() {
        let out = resample_tone(96_000, 48_000, 30_000.0, 0.5, 0.0);
        let body = &out[2000..out.len() - 100];
        let power = body.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / body.len() as f64;
        let db = 10.0 * (power / 0.125).log10();
        assert!(db < -90.0, "alias at {db} dB");
    }

    /// The output count follows the step: ten seconds at 44.1 kHz become 480000 frames
    /// at nominal speed (less the 64-frame look-ahead still pending), and `1 / (1 + u)`
    /// times that with a correction.
    #[test]
    fn output_rate_follows_the_correction() {
        let step = 44_100.0 / 48_000.0;
        for u in [0.0, 0.004, -0.005] {
            let n = resample_tone(44_100, 48_000, 440.0, 10.0, u).len() as f64;
            let want = (441_000.0 - 64.0) / (step * (1.0 + u));
            assert!((n - want).abs() < 2.0, "u {u}: {n} vs {want}");
        }
    }

    /// The kernel adds no delay: with step 1 and a fractional start position, output `j`
    /// is the input signal at `j + f`.
    #[test]
    fn output_sits_exactly_at_its_position() {
        let rate = 48_000;
        let mut r = Resampler::new(rate, rate);
        r.reset(10.25);
        let mut out = Vec::new();
        let sig = |t: f64| 0.5 * tone(500.0, t / f64::from(rate));
        for i in 0..4000 {
            let s = sig(i as f64) as f32;
            r.push([s, s], &mut |o| {
                if let Output::Frame(f) = o {
                    out.push(f[0]);
                }
            });
        }
        for (j, v) in out.iter().enumerate().skip(200) {
            let want = sig(10.25 + j as f64);
            assert!(
                (f64::from(*v) - want).abs() < 1e-4,
                "frame {j}: {v} vs {want}"
            );
        }
    }

    #[test]
    fn rows_have_unity_gain_and_are_symmetric() {
        let r = Resampler::new(44_100, 48_000);
        let taps = r.taps();
        assert_eq!(taps, 128);
        for k in 0..=PHASES {
            let sum: f32 = r.table[k * taps..(k + 1) * taps].iter().sum();
            assert!((sum - 1.0).abs() < 1e-5, "row {k} sums to {sum}");
        }
        // Phase f and phase 1 - f are mirror images.
        let row = |k: usize| &r.table[k * taps..(k + 1) * taps];
        for t in 0..taps {
            assert!((row(64)[t] - row(PHASES - 64)[taps - 1 - t]).abs() < 1e-6);
        }
        assert_eq!(Resampler::new(96_000, 48_000).taps(), 256);
        assert_eq!(Resampler::new(192_000, 48_000).taps(), 512);
    }

    /// A long silent gap is reported as one run, and the frames on either side keep
    /// their positions: total output equals pushing the zeros one by one.
    #[test]
    fn long_silence_is_one_run_with_the_same_output_count() {
        let mut fast = Resampler::new(44_100, 48_000);
        let mut slow = Resampler::new(44_100, 48_000);
        let mut fast_out = 0u64;
        let mut slow_out = 0u64;
        let mut runs = 0;
        for _ in 0..1000 {
            fast.push([0.1, 0.1], &mut |_| fast_out += 1);
            slow.push([0.1, 0.1], &mut |_| slow_out += 1);
        }
        fast.push_silence(44_100, &mut |o| match o {
            Output::Frame(_) => fast_out += 1,
            Output::Silence(n) => {
                runs += 1;
                fast_out += n;
            }
        });
        for _ in 0..44_100 {
            slow.push([0.0, 0.0], &mut |_| slow_out += 1);
        }
        assert_eq!(runs, 1);
        assert_eq!(fast_out, slow_out);
        assert_eq!(fast.position(), slow.position());
        assert_eq!(fast.input_count(), slow.input_count());
        // After the gap both produce identical frames again.
        let mut a = Vec::new();
        let mut b = Vec::new();
        for i in 0..500 {
            let s = (i as f32 * 0.01).sin();
            fast.push([s, s], &mut |o| a.push(o));
            slow.push([s, s], &mut |o| b.push(o));
        }
        assert_eq!(a, b);
    }

    #[test]
    fn reset_clamps_bad_positions() {
        let mut r = Resampler::new(48_000, 48_000);
        r.reset(-3.0);
        assert_eq!(r.position(), 0.0);
        r.reset(f64::NAN);
        assert_eq!(r.position(), 0.0);
        r.set_correction(f64::NAN);
        r.set_correction(0.001);
        let mut n = 0;
        for _ in 0..1000 {
            r.push([0.0; 2], &mut |_| n += 1);
        }
        assert!(n > 900);
    }
}
