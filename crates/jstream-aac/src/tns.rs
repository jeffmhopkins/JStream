//! Temporal noise shaping (ISO/IEC 13818-7 §4.6.9, 1997; the syntax is ISO/IEC 14496-3
//! Table 4.61): an open-loop linear prediction across frequency, whose residual is coded
//! in place of each window's spectrum, so the quantization noise follows the signal's
//! temporal envelope within the window instead of spreading evenly over it (J. Herre and
//! J. D. Johnston, "Enhancing the performance of perceptual audio coders by using
//! temporal noise shaping (TNS)", AES 101st Convention, 1996, preprint 4384).
//!
//! Per window, the lines from about 1.4 kHz up to the standard's `TNS_MAX_BANDS` limit
//! are analysed: their autocorrelation, smoothed by a Gaussian lag window (Y. Tohkura,
//! F. Itakura and S. Hashimoto, "Spectral smoothing technique in PARCOR speech
//! analysis-synthesis", IEEE Trans. ASSP 26(6), 1978), gives the reflection coefficients
//! through the Levinson-Durbin recursion (N. Levinson, 1947; J. Durbin, 1960). A filter
//! is used only when its prediction gain clears [`MIN_GAIN`]. Its coefficients are
//! quantized as the standard's decoder dequantizes them (`sin` of the index over its
//! arcsine step, at 4-bit resolution), and the spectrum is filtered with the dequantized
//! ones, so the decoder's all-pole filter inverts it exactly. One filter per window,
//! upward in frequency.

use std::f32::consts::FRAC_PI_2;

use crate::bits::BitWriter;
use crate::quant::Shape;

/// The highest filter order AAC-LC allows (14496-3 Table 4.156): 12 for long windows, 7
/// for short ones.
pub(crate) const MAX_ORDER_LONG: usize = 12;
pub(crate) const MAX_ORDER_SHORT: usize = 7;
/// Below this prediction gain (about 1.5 dB) the filter's side information costs more
/// than it shapes.
pub(crate) const MIN_GAIN: f32 = 2.5;
/// Where the analysed range starts, in Hz: below it the ear's temporal resolution gives
/// TNS little to do.
const START_HZ: f64 = 1400.0;
/// The coefficients' resolution in bits (`coef_res` 1).
const RES_BITS: u32 = 4;
/// The fewest lines a window's range may hold for a filter to be tried.
const MIN_LINES: usize = 16;

/// One window's filter: `order` 0 for none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Filter {
    /// The bands it covers, counted down from the top band of the window length
    /// (`length`).
    pub(crate) length: u8,
    pub(crate) order: u8,
    /// Whether each coefficient is written in 3 bits instead of 4 (`coef_compress`).
    pub(crate) compress: bool,
    /// The quantized reflection coefficients, -8..=7.
    pub(crate) coef: [i8; MAX_ORDER_LONG],
}

/// A channel's TNS data for one frame: a filter per window.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Tns {
    pub(crate) filters: [Filter; 8],
}

impl Tns {
    /// Whether any window of `frame` has a filter (`tns_data_present`).
    pub(crate) fn present(&self, frame: &Shape<'_>) -> bool {
        self.filters[..frame.windows].iter().any(|f| f.order > 0)
    }

    /// The bits `tns_data()` takes for `frame`, 0 when it isn't present.
    pub(crate) fn bits(&self, frame: &Shape<'_>) -> u32 {
        if !self.present(frame) {
            return 0;
        }
        let long = frame.windows == 1;
        let (n_filt, length, order) = if long { (2, 6, 5) } else { (1, 4, 3) };
        self.filters[..frame.windows]
            .iter()
            .map(|f| {
                if f.order == 0 {
                    n_filt
                } else {
                    n_filt + 1 + length + order + 2 + u32::from(f.order) * coef_bits(f.compress)
                }
            })
            .sum()
    }

    /// Writes `tns_data()` (14496-3 Table 4.61); only when [`present`](Self::present).
    pub(crate) fn write(&self, w: &mut BitWriter<'_>, frame: &Shape<'_>) {
        let long = frame.windows == 1;
        let (n_filt, length, order) = if long { (2, 6, 5) } else { (1, 4, 3) };
        for f in &self.filters[..frame.windows] {
            if f.order == 0 {
                w.put(0, n_filt);
                continue;
            }
            w.put(1, n_filt);
            w.bit(RES_BITS == 4); // coef_res
            w.put(u32::from(f.length), length);
            w.put(u32::from(f.order), order);
            w.bit(false); // direction: upward
            w.bit(f.compress);
            let bits = coef_bits(f.compress);
            let mask = (1u32 << bits) - 1;
            for &c in &f.coef[..usize::from(f.order)] {
                w.put(c as u32 & mask, bits);
            }
        }
    }
}

/// A coefficient's width: `coef_res` bits, less one when compressed.
fn coef_bits(compress: bool) -> u32 {
    RES_BITS - u32::from(compress)
}

/// The arcsine quantizer's steps for positive and negative indices (14496-3 §4.6.9.3).
fn steps() -> (f32, f32) {
    let half = (1u32 << (RES_BITS - 1)) as f32;
    ((half - 0.5) / FRAC_PI_2, (half + 0.5) / FRAC_PI_2)
}

/// The index a reflection coefficient quantizes to.
pub(crate) fn quantize(k: f32) -> i8 {
    let (pos, neg) = steps();
    let a = k.clamp(-1.0, 1.0).asin();
    let half = 1i32 << (RES_BITS - 1);
    let i = (a * if a >= 0.0 { pos } else { neg }).round() as i32;
    i.clamp(-half, half - 1) as i8
}

/// The reflection coefficient a decoder reads for index `i`.
pub(crate) fn dequantize(i: i8) -> f32 {
    let (pos, neg) = steps();
    let i = f32::from(i);
    (i / if i >= 0.0 { pos } else { neg }).sin()
}

/// The direct-form coefficients `a[1..=order]` (in `lpc[..order]`) of the reflection
/// coefficients `k`, by the decoder's step-up recursion (14496-3 §4.6.9.3).
pub(crate) fn step_up(k: &[f32], lpc: &mut [f32; MAX_ORDER_LONG]) {
    let mut b = [0.0f32; MAX_ORDER_LONG];
    for m in 1..=k.len() {
        for i in 1..m {
            b[i] = lpc[i - 1] + k[m - 1] * lpc[m - i - 1];
        }
        lpc[..m - 1].copy_from_slice(&b[1..m]);
        lpc[m - 1] = k[m - 1];
    }
}

/// The analysis (all-zero) filter, in place: `e[i] = x[i] + Σ a[j] x[i-j]`, with the
/// filter's memory starting empty at `x[0]`. The decoder's all-pole filter inverts it.
pub(crate) fn analysis_filter(x: &mut [f32], lpc: &[f32]) {
    for i in (0..x.len()).rev() {
        let mut e = x[i];
        for (j, &a) in lpc.iter().enumerate().take(i) {
            e += a * x[i - j - 1];
        }
        x[i] = e;
    }
}

/// Levinson-Durbin on the autocorrelation `r[..=order]`: the reflection coefficients in
/// `k`, and the prediction error's power. `None` when the recursion breaks down.
fn levinson(r: &[f32], k: &mut [f32]) -> Option<f32> {
    let order = k.len();
    let mut a = [0.0f32; MAX_ORDER_LONG + 1];
    let mut tmp = [0.0f32; MAX_ORDER_LONG + 1];
    a[0] = 1.0;
    let mut err = r[0];
    for m in 1..=order {
        let mut acc = r[m];
        for i in 1..m {
            acc += a[i] * r[m - i];
        }
        if err <= 0.0 {
            return None;
        }
        let km = -acc / err;
        if !km.is_finite() || km.abs() >= 1.0 {
            return None;
        }
        k[m - 1] = km;
        tmp[..m].copy_from_slice(&a[..m]);
        for i in 1..m {
            a[i] = tmp[i] + km * tmp[m - i];
        }
        a[m] = km;
        err *= 1.0 - km * km;
    }
    Some(err)
}

/// The analysis set up for one stream: where each window length's range starts, and
/// where `TNS_MAX_BANDS` ends it.
pub(crate) struct Analyser {
    /// The first band of the range, and the band limit, for long and short windows.
    start_long: usize,
    start_short: usize,
    max_long: usize,
    max_short: usize,
    /// Bands of each window length (`num_swb`), which `length` counts down from.
    swb_long: usize,
    swb_short: usize,
    lag: [f32; MAX_ORDER_LONG + 1],
}

impl Analyser {
    /// For `sample_rate` (44.1 or 48 kHz), with the band offsets of each window length.
    pub(crate) fn new(sample_rate: u32, long: &[u16], short: &[u16]) -> Self {
        // TNS_MAX_BANDS for AAC-LC (14496-3 Table 4.156).
        let max_long = if sample_rate == 48_000 { 40 } else { 42 };
        let max_short = 14;
        let start = |offsets: &[u16], lines: usize, max: usize| {
            let hz_per_line = f64::from(sample_rate) / 2.0 / lines as f64;
            (0..offsets.len() - 1)
                .find(|&b| f64::from(offsets[b]) * hz_per_line >= START_HZ)
                .unwrap_or(max)
                .min(max)
        };
        let mut lag = [0.0f32; MAX_ORDER_LONG + 1];
        for (k, l) in lag.iter_mut().enumerate() {
            let x = 0.08 * k as f32;
            *l = (-0.5 * x * x).exp();
        }
        Self {
            start_long: start(long, 1024, max_long),
            start_short: start(short, 128, max_short),
            max_long,
            max_short,
            swb_long: long.len() - 1,
            swb_short: short.len() - 1,
            lag,
        }
    }

    /// Analyses each window of `frame` in `spec` and, where a filter pays, filters that
    /// window's range in place and records the filter in `tns`; other windows get none.
    pub(crate) fn apply(&self, spec: &mut [f32], frame: &Shape<'_>, tns: &mut Tns) {
        let long = frame.windows == 1;
        let (start, max, swb, max_order) = if long {
            (
                self.start_long,
                self.max_long,
                self.swb_long,
                MAX_ORDER_LONG,
            )
        } else {
            (
                self.start_short,
                self.max_short,
                self.swb_short,
                MAX_ORDER_SHORT,
            )
        };
        let top = max.min(frame.bands);
        let bottom = start.min(top);
        let lo = usize::from(frame.offsets[bottom]);
        let hi = usize::from(frame.offsets[top]);
        *tns = Tns::default();
        for w in 0..frame.windows {
            let base = w * frame.lines;
            // Each line weighted by its band's RMS, so loud bands don't rule the
            // prediction.
            let mut weight = [0.0f32; 49];
            for b in bottom..top {
                let range = base + usize::from(frame.offsets[b])..base + usize::from(frame.offsets[b + 1]);
                let width = range.len() as f32;
                let e: f32 = spec[range].iter().map(|v| v * v).sum();
                weight[b] = if e > 0.0 { (width / e).sqrt() } else { 0.0 };
            }
            let x = &mut spec[base + lo..base + hi];
            let offsets = &frame.offsets[bottom..=top];
            if let Some(f) = self.filter(x, offsets, &weight[bottom..top], max_order) {
                tns.filters[w] = Filter {
                    length: (swb - bottom) as u8,
                    ..f
                };
            }
        }
    }

    /// The filter for one window's range `x`, applied to it, or `None` (and `x` as it
    /// was) when none pays.
    ///
    /// `offsets` are the range's band edges and `weight` each band's weight for the
    /// analysis.
    fn filter(
        &self,
        x: &mut [f32],
        offsets: &[u16],
        weight: &[f32],
        max_order: usize,
    ) -> Option<Filter> {
        if x.len() < MIN_LINES.max(2 * max_order) {
            return None;
        }
        let first = usize::from(offsets[0]);
        let mut wx = [0.0f32; 1024];
        let mut energy = 0.0f64;
        for (b, &g) in weight.iter().enumerate() {
            for i in usize::from(offsets[b])..usize::from(offsets[b + 1]) {
                let i = i - first;
                wx[i] = x[i] * g;
                energy += f64::from(x[i]) * f64::from(x[i]);
            }
        }
        if !(energy.is_finite() && energy > 1e-3 * x.len() as f64) {
            return None;
        }
        let wx = &wx[..x.len()];
        let mut r = [0.0f32; MAX_ORDER_LONG + 1];
        for (lag, r) in r.iter_mut().enumerate().take(max_order + 1) {
            let mut acc = 0.0f64;
            for i in lag..wx.len() {
                acc += f64::from(wx[i]) * f64::from(wx[i - lag]);
            }
            *r = acc as f32;
        }
        if !(r[0].is_finite() && r[0] > 0.0) {
            return None;
        }
        for (r, l) in r.iter_mut().zip(self.lag) {
            *r *= l;
        }
        let mut k = [0.0f32; MAX_ORDER_LONG];
        let err = levinson(&r[..=max_order], &mut k[..max_order])?;
        if err <= 0.0 || r[0] / err < MIN_GAIN {
            return None;
        }
        let mut f = Filter::default();
        let mut order = 0;
        for (i, &k) in k[..max_order].iter().enumerate() {
            f.coef[i] = quantize(k);
            if f.coef[i] != 0 {
                order = i + 1;
            }
        }
        if order == 0 {
            return None;
        }
        f.order = order as u8;
        f.compress = f.coef[..order].iter().all(|&c| (-4..=3).contains(&c));
        let mut kq = [0.0f32; MAX_ORDER_LONG];
        for (q, &c) in kq.iter_mut().zip(&f.coef[..order]) {
            *q = dequantize(c);
        }
        let mut lpc = [0.0f32; MAX_ORDER_LONG];
        step_up(&kq[..order], &mut lpc);
        analysis_filter(x, &lpc[..order]);
        Some(f)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Analyser, MAX_ORDER_LONG, Tns, analysis_filter, dequantize, levinson, quantize, step_up,
    };
    use crate::bits::BitWriter;
    use crate::quant::Shape;
    use crate::tables::{SWB_LONG, SWB_SHORT};

    /// The decoder's all-pole filter, as 14496-3 §4.6.9.3 runs it upward.
    fn synthesis_filter(y: &mut [f32], lpc: &[f32]) {
        for i in 0..y.len() {
            for (j, &a) in lpc.iter().enumerate().take(i) {
                y[i] -= y[i - j - 1] * a;
            }
        }
    }

    #[test]
    fn coefficients_quantize_as_the_decoder_reads_them() {
        for i in -8i8..=7 {
            assert_eq!(quantize(dequantize(i)), i, "index {i}");
        }
        assert_eq!(quantize(0.0), 0);
        assert_eq!(quantize(0.999), 7);
        assert_eq!(quantize(-0.999), -8);
        // The decoder's step: (8 - 0.5) / (pi/2) for positive indices.
        assert!((dequantize(1) - (1.0f32 / (7.5 / std::f32::consts::FRAC_PI_2)).sin()).abs() < 1e-7);
        assert!((dequantize(-1) + (1.0f32 / (8.5 / std::f32::consts::FRAC_PI_2)).sin()).abs() < 1e-7);
    }

    #[test]
    fn step_up_matches_the_recursion() {
        let mut lpc = [0.0; MAX_ORDER_LONG];
        step_up(&[0.5], &mut lpc);
        assert_eq!(lpc[0], 0.5);
        step_up(&[0.5, -0.25], &mut lpc);
        // a1 = k1 + k2 k1, a2 = k2.
        assert!((lpc[0] - (0.5 - 0.25 * 0.5)).abs() < 1e-7);
        assert_eq!(lpc[1], -0.25);
    }

    #[test]
    fn the_decoder_filter_inverts_the_analysis_filter() {
        let mut lpc = [0.0; MAX_ORDER_LONG];
        let k: Vec<f32> = [3i8, -2, 5, 1, -7, 0, 2].iter().map(|&i| dequantize(i)).collect();
        step_up(&k, &mut lpc);
        let x: Vec<f32> = (0..300).map(|i| ((i * 37 % 101) as f32 - 50.0) * 3.0).collect();
        let mut y = x.clone();
        analysis_filter(&mut y, &lpc[..k.len()]);
        assert_ne!(x, y);
        synthesis_filter(&mut y, &lpc[..k.len()]);
        for (a, b) in x.iter().zip(&y) {
            assert!((a - b).abs() < 1e-2 * a.abs().max(1.0), "{a} vs {b}");
        }
    }

    #[test]
    fn levinson_finds_a_known_predictor() {
        // An AR(1) process x[n] = 0.9 x[n-1] + e: r[k] = 0.9^k (scaled).
        let r: Vec<f32> = (0..3).map(|k| 0.9f32.powi(k)).collect();
        let mut k = [0.0; 2];
        let err = levinson(&r, &mut k).expect("stable");
        assert!((k[0] + 0.9).abs() < 1e-6, "{k:?}");
        assert!(k[1].abs() < 1e-6, "{k:?}");
        assert!((err - (1.0 - 0.81)).abs() < 1e-6);
    }

    fn long() -> Shape<'static> {
        Shape {
            offsets: &SWB_LONG,
            bands: 49,
            windows: 1,
            lines: 1024,
        }
    }

    /// The MDCT of a click: a spectrum the prediction finds very regular.
    fn click_spectrum() -> Vec<f32> {
        let n = 2048.0f64;
        let n0 = (n / 2.0 + 1.0) / 2.0;
        let at = 1500.0;
        (0..1024)
            .map(|k| {
                (1000.0 * (2.0 * std::f64::consts::PI / n * (at + n0) * (k as f64 + 0.5)).cos())
                    as f32
            })
            .collect()
    }

    #[test]
    fn a_click_gets_a_filter_and_noise_does_not() {
        let a = Analyser::new(48_000, &SWB_LONG, &SWB_SHORT);
        let frame = long();
        let mut spec = click_spectrum();
        let before = spec.clone();
        let mut tns = Tns::default();
        a.apply(&mut spec, &frame, &mut tns);
        let f = tns.filters[0];
        assert!(f.order > 0, "{f:?}");
        assert!(tns.present(&frame));
        // The range is untouched below about 1.4 kHz and above TNS_MAX_BANDS (40 at
        // 48 kHz), and filtered between.
        let lo = usize::from(SWB_LONG[49 - usize::from(f.length)]);
        let hi = usize::from(SWB_LONG[40]);
        assert!((1400.0..2000.0).contains(&(lo as f64 * 24_000.0 / 1024.0)), "{lo}");
        assert_eq!(&spec[..lo], &before[..lo]);
        assert_eq!(&spec[hi..], &before[hi..]);
        let e = |s: &[f32]| s.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
        assert!(e(&spec[lo..hi]) < 0.5 * e(&before[lo..hi]), "the residual is smaller");

        // White noise doesn't predict.
        let mut seed = 1u64;
        let mut noise: Vec<f32> = (0..1024)
            .map(|_| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((seed >> 33) as f32 / (1u64 << 31) as f32 - 0.5) * 1000.0
            })
            .collect();
        let kept = noise.clone();
        a.apply(&mut noise, &frame, &mut tns);
        assert!(!tns.present(&frame));
        assert_eq!(noise, kept);

        // Silence neither.
        let mut silence = vec![0.0; 1024];
        a.apply(&mut silence, &frame, &mut tns);
        assert!(!tns.present(&frame));
    }

    #[test]
    fn the_range_stops_at_max_sfb() {
        let a = Analyser::new(44_100, &SWB_LONG, &SWB_SHORT);
        let frame = Shape {
            bands: 30,
            ..long()
        };
        let mut spec = click_spectrum();
        let before = spec.clone();
        let mut tns = Tns::default();
        a.apply(&mut spec, &frame, &mut tns);
        assert!(tns.present(&frame));
        let hi = usize::from(SWB_LONG[30]);
        assert_eq!(&spec[hi..], &before[hi..]);
    }

    #[test]
    fn written_bits_match_counted_bits() {
        let a = Analyser::new(48_000, &SWB_LONG, &SWB_SHORT);
        for frame in [
            long(),
            Shape {
                offsets: &SWB_SHORT,
                bands: 14,
                windows: 8,
                lines: 128,
            },
        ] {
            let mut spec = vec![0.0; 1024];
            for w in 0..frame.windows {
                let n = 2.0 * frame.lines as f64;
                let n0 = (n / 2.0 + 1.0) / 2.0;
                // Clicks in every other window; the rest silent.
                if w % 2 == 0 {
                    for k in 0..frame.lines {
                        spec[w * frame.lines + k] = (1000.0
                            * (2.0 * std::f64::consts::PI / n * (100.0 + n0) * (k as f64 + 0.5))
                                .cos()) as f32;
                    }
                }
            }
            let mut tns = Tns::default();
            a.apply(&mut spec, &frame, &mut tns);
            assert!(tns.present(&frame), "{frame:?}");
            let mut out = Vec::new();
            let mut w = BitWriter::new(&mut out);
            tns.write(&mut w, &frame);
            assert_eq!(w.finish() as u32, tns.bits(&frame), "{frame:?}");
        }
    }
}
