//! Temporal noise shaping (TNS), an MPEG-2 AAC LC profile tool (ISO/IEC 13818-7, 1997,
//! §4.6.9 of 14496-3): an open-loop linear prediction across frequency, so that the
//! quantization noise the decoder's all-pole synthesis filter puts back follows the
//! signal's temporal envelope inside a window, instead of spreading evenly over it
//! (J. Herre and J. D. Johnston, "Enhancing the performance of perceptual audio coders
//! by using temporal noise shaping (TNS)", AES 101st Convention, 1996, preprint 4384).
//!
//! Per window, the encoder takes the MDCT lines from about 1.4 kHz to the standard's
//! TNS band limit (`TNS_MAX_BANDS`: 40 long bands at 48 kHz, 42 at 44.1 kHz, 14 short at
//! either), and computes their autocorrelation across frequency. It applies a Gaussian
//! lag window to it (Y. Tohkura, F. Itakura and S. Hashimoto, "Spectral smoothing
//! technique in PARCOR speech analysis-synthesis", IEEE Trans. ASSP 26(6), 1978), adds
//! the bands' masking thresholds to its zero lag as a white-noise correction (J. D.
//! Markel and A. H. Gray, *Linear Prediction of Speech*, 1976), so that structure the
//! thresholds would mask anyway, such as a steady tone's window leakage, doesn't count
//! toward the gain, then solves the normal equations with the Levinson-Durbin recursion (N. Levinson, 1947; J.
//! Durbin, 1960). When the prediction gain clears [`GAIN_THRESHOLD`], the reflection
//! coefficients are quantized at 4-bit resolution by the standard's arcsine quantizer,
//! trailing zeros are trimmed, and the lines are filtered by the FIR prediction-error
//! filter built from the *dequantized* coefficients, so that the decoder's synthesis
//! filter inverts it exactly. The filter runs upward in frequency (`direction` 0).
//!
//! The decoder's synthesis filter raises white quantization noise by about the
//! prediction gain in power, so the masking thresholds of the filtered bands are divided
//! by it, and their energies are measured again on the filtered lines.
//!
//! Each channel is filtered before the M/S decision: the decoder undoes M/S first, then
//! TNS (14496-3 §4.6.9.3 runs on the spectrum after M/S).

use crate::bits::BitWriter;
use crate::quant::{Channel, Shape};

/// The most filter coefficients of a long window in the LC profile (14496-3 Table 4.156).
const MAX_ORDER_LONG: usize = 12;
/// The most of a short window.
const MAX_ORDER_SHORT: usize = 7;
/// The TNS band limit at 48 and 44.1 kHz (14496-3 Table 4.155), long and short.
const MAX_BANDS_LONG_48K: usize = 40;
const MAX_BANDS_LONG_44K: usize = 42;
const MAX_BANDS_SHORT: usize = 14;
/// The lowest frequency filtered, in Hz: below it the spectrum carries little temporal
/// structure worth the coefficients.
const START_HZ: f64 = 1400.0;
/// The smallest prediction gain (about 1.5 dB) for which a filter is sent: below it the
/// coefficients cost more than the shaping gives.
pub(crate) const GAIN_THRESHOLD: f64 = 1.4;
/// The lag window's width, in radians per lag: `w(i) = exp(-(LAG * i)^2 / 2)`. Gentle, so
/// that the top orders, which resolve pulses close together in time, survive.
const LAG: f64 = 0.07;
/// The coefficient resolution sent: 4 bits (`coef_res` 1).
const COEF_BITS: u32 = 4;

/// One window's filter, as the bitstream carries it. `order` 0 is no filter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Filter {
    /// The bands the filter covers, counted down from the top of the band table.
    pub(crate) length: u8,
    /// Coefficients sent.
    pub(crate) order: u8,
    /// Whether the indices fit in 3 bits, so their top bit isn't sent.
    pub(crate) compress: bool,
    /// The quantized reflection coefficients, `-8..=7`.
    pub(crate) coef: [i8; MAX_ORDER_LONG],
}

/// A channel's TNS filters for one frame: one per window at most.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tns {
    pub(crate) filters: [Filter; 8],
}

impl Tns {
    /// No filter in any window.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Whether any window has a filter (`tns_data_present`).
    pub(crate) fn present(&self) -> bool {
        self.filters.iter().any(|f| f.order > 0)
    }

    /// The bits of `tns_data()` for a frame of `windows` windows; 0 when not present (the
    /// `tns_data_present` flag is counted with the other fixed bits).
    pub(crate) fn bits(&self, windows: usize) -> u32 {
        if !self.present() {
            return 0;
        }
        let long = windows == 1;
        let (n_filt, length, order) = if long { (2, 6, 5) } else { (1, 4, 3) };
        self.filters[..windows]
            .iter()
            .map(|f| {
                if f.order == 0 {
                    n_filt
                } else {
                    let coef = COEF_BITS - u32::from(f.compress);
                    n_filt + 1 + length + order + 2 + u32::from(f.order) * coef
                }
            })
            .sum()
    }

    /// Writes `tns_data()` (14496-3 Table 4.48) for a frame of `windows` windows, when
    /// [`present`](Self::present).
    pub(crate) fn write(&self, w: &mut BitWriter<'_>, windows: usize) {
        let long = windows == 1;
        let (n_filt, length, order) = if long { (2, 6, 5) } else { (1, 4, 3) };
        for f in &self.filters[..windows] {
            if f.order == 0 {
                w.put(0, n_filt);
                continue;
            }
            w.put(1, n_filt);
            w.bit(COEF_BITS == 4); // coef_res
            w.put(u32::from(f.length), length);
            w.put(u32::from(f.order), order);
            w.bit(false); // direction: upward
            w.bit(f.compress);
            let bits = COEF_BITS - u32::from(f.compress);
            for &c in &f.coef[..usize::from(f.order)] {
                // Two's complement in `bits` bits.
                w.put((c as u32) & ((1 << bits) - 1), bits);
            }
        }
    }
}

/// The quantizer's scale for positive and negative angles (14496-3 §4.6.9.3).
fn iqfac() -> (f64, f64) {
    let half = f64::from(1u32 << (COEF_BITS - 1));
    let quarter = std::f64::consts::FRAC_PI_2;
    ((half - 0.5) / quarter, (half + 0.5) / quarter)
}

/// The quantized index of reflection coefficient `k`.
pub(crate) fn quantize(k: f64) -> i8 {
    let (pos, neg) = iqfac();
    let a = k.clamp(-1.0, 1.0).asin();
    let max = (1i32 << (COEF_BITS - 1)) - 1;
    ((a * if a >= 0.0 { pos } else { neg }).round() as i32).clamp(-max - 1, max) as i8
}

/// The reflection coefficient index `c` stands for, as the decoder reads it.
pub(crate) fn dequantize(c: i8) -> f64 {
    let (pos, neg) = iqfac();
    let c = f64::from(c);
    (c / if c >= 0.0 { pos } else { neg }).sin()
}

/// The prediction-error filter `1 + a[1] z^-1 + … + a[p] z^-p` of reflection
/// coefficients `k` (the standard's step-up recursion), into `a[1..=k.len()]`.
pub(crate) fn lpc(k: &[f64], a: &mut [f64; MAX_ORDER_LONG + 1]) {
    a.fill(0.0);
    a[0] = 1.0;
    let mut b = [0.0f64; MAX_ORDER_LONG + 1];
    for (m, &km) in k.iter().enumerate().map(|(i, k)| (i + 1, k)) {
        for i in 1..m {
            b[i] = a[i] + km * a[m - i];
        }
        a[1..m].copy_from_slice(&b[1..m]);
        a[m] = km;
    }
}

/// The Levinson-Durbin recursion on autocorrelation `r` (`r.len() - 1` is the order):
/// the reflection coefficients into `k`, and the prediction gain `r[0] / error`.
fn levinson(r: &[f64], k: &mut [f64]) -> f64 {
    let p = r.len() - 1;
    let mut a = [0.0f64; MAX_ORDER_LONG + 1];
    let mut tmp = [0.0f64; MAX_ORDER_LONG + 1];
    a[0] = 1.0;
    let mut err = r[0];
    for m in 1..=p {
        if err <= r[0] * 1e-12 {
            k[m - 1..].fill(0.0);
            break;
        }
        let acc: f64 = (0..m).map(|i| a[i] * r[m - i]).sum();
        let km = -acc / err;
        tmp[..=m].copy_from_slice(&a[..=m]);
        for i in 1..m {
            a[i] = tmp[i] + km * tmp[m - i];
        }
        a[m] = km;
        k[m - 1] = km;
        err *= 1.0 - km * km;
    }
    r[0] / err.max(r[0] * 1e-12)
}

/// Filters `x` in place by the prediction-error filter `a` of `order` coefficients,
/// upward: `e[n] = x[n] + sum a[j] x[n - j]`, with nothing before the first line.
pub(crate) fn analysis(x: &mut [f32], a: &[f64; MAX_ORDER_LONG + 1], order: usize) {
    for n in (0..x.len()).rev() {
        let mut e = f64::from(x[n]);
        for j in 1..=order.min(n) {
            e += a[j] * f64::from(x[n - j]);
        }
        x[n] = e as f32;
    }
}

/// What the encoder needs to choose each window's filter.
pub(crate) struct Analyzer {
    /// The first band filtered, long and short.
    start_long: usize,
    start_short: usize,
    /// The TNS band limit, long and short.
    max_long: usize,
    max_short: usize,
    /// The lag window, lag 0 to the long order.
    lag: [f64; MAX_ORDER_LONG + 1],
}

impl Analyzer {
    /// The analyzer for `sample_rate` with the long and short band tables.
    pub(crate) fn new(sample_rate: u32, long: &[u16], short: &[u16]) -> Self {
        let start = |offsets: &[u16], lines: usize| {
            let line_hz = f64::from(sample_rate) / (2 * lines) as f64;
            offsets
                .iter()
                .position(|&o| f64::from(o) * line_hz >= START_HZ)
                .unwrap_or(offsets.len() - 1)
        };
        let mut lag = [0.0; MAX_ORDER_LONG + 1];
        for (i, l) in lag.iter_mut().enumerate() {
            *l = (-0.5 * (LAG * i as f64).powi(2)).exp();
        }
        Self {
            start_long: start(long, 1024),
            start_short: start(short, 128),
            max_long: if sample_rate == 48_000 {
                MAX_BANDS_LONG_48K
            } else {
                MAX_BANDS_LONG_44K
            },
            max_short: MAX_BANDS_SHORT,
            lag,
        }
    }

    /// Chooses and applies channel `ch`'s filters for `frame`: filters its spectrum in
    /// place, divides the filtered bands' thresholds by the prediction gain and measures
    /// their energies again, and records the filters in `ch.tns`.
    pub(crate) fn apply(&self, ch: &mut Channel, frame: &Shape<'_>) {
        ch.tns.clear();
        let long = frame.windows == 1;
        let (start, max, max_order) = if long {
            (self.start_long, self.max_long, MAX_ORDER_LONG)
        } else {
            (self.start_short, self.max_short, MAX_ORDER_SHORT)
        };
        let top = max.min(frame.bands);
        if top <= start {
            return;
        }
        let all_bands = frame.offsets.len() - 1;
        for w in 0..frame.windows {
            let base = w * frame.lines;
            let lo = base + usize::from(frame.offsets[start]);
            let hi = base + usize::from(frame.offsets[top]);
            let x = &mut ch.spec[lo..hi];
            let order = max_order.min(x.len() - 1);
            let mut r = [0.0f64; MAX_ORDER_LONG + 1];
            for (lag, r) in r.iter_mut().enumerate().take(order + 1) {
                *r = (lag..x.len())
                    .map(|n| f64::from(x[n]) * f64::from(x[n - lag]))
                    .sum::<f64>()
                    * self.lag[lag];
            }
            if !(r[0].is_finite() && r[0] > 0.0) {
                continue;
            }
            // White-noise correction by the masking threshold: only structure above
            // what may be masked anyway counts toward the gain.
            let masked: f64 = (start..top)
                .map(|b| f64::from(ch.thr[w * frame.bands + b]))
                .sum();
            r[0] += masked;
            let mut k = [0.0f64; MAX_ORDER_LONG];
            let gain = levinson(&r[..=order], &mut k[..order]);
            if gain < GAIN_THRESHOLD {
                continue;
            }
            let mut f = Filter::default();
            for (c, &k) in f.coef.iter_mut().zip(&k[..order]) {
                *c = quantize(k);
            }
            let sent = f.coef[..order]
                .iter()
                .rposition(|&c| c != 0)
                .map_or(0, |i| i + 1);
            if sent == 0 {
                continue;
            }
            f.order = sent as u8;
            let half = 1i8 << (COEF_BITS - 2);
            f.compress = f.coef[..sent].iter().all(|&c| (-half..half).contains(&c));
            f.length = (all_bands - start) as u8;
            let mut kq = [0.0f64; MAX_ORDER_LONG];
            for (kq, &c) in kq.iter_mut().zip(&f.coef[..sent]) {
                *kq = dequantize(c);
            }
            let mut a = [0.0f64; MAX_ORDER_LONG + 1];
            lpc(&kq[..sent], &mut a);
            analysis(x, &a, sent);
            ch.tns.filters[w] = f;
            for b in start..top {
                let i = w * frame.bands + b;
                ch.thr[i] = (f64::from(ch.thr[i]) / gain) as f32;
                ch.energy[i] = ch.spec[frame.range(i)].iter().map(|v| v * v).sum();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Analyzer, GAIN_THRESHOLD, MAX_ORDER_LONG, Tns, analysis, dequantize, levinson, lpc,
        quantize,
    };
    use crate::bits::BitWriter;
    use crate::quant::{Channel, Shape};
    use crate::tables::{SWB_LONG, SWB_SHORT};

    /// The decoder's all-pole synthesis filter (14496-3 §4.6.9.3, upward):
    /// `y[n] = x[n] - sum a[j] y[n - j]`.
    fn synthesis(x: &mut [f32], a: &[f64; MAX_ORDER_LONG + 1], order: usize) {
        for n in 0..x.len() {
            let mut y = f64::from(x[n]);
            for j in 1..=order.min(n) {
                y -= a[j] * f64::from(x[n - j]);
            }
            x[n] = y as f32;
        }
    }

    #[test]
    fn quantization_round_trips() {
        // Every index decodes to a coefficient that quantizes back to it.
        for c in -8i8..=7 {
            assert_eq!(quantize(dequantize(c)), c, "index {c}");
        }
        // The ends: +-1 and beyond clamp to the extreme indices.
        assert_eq!(quantize(1.0), 7);
        assert_eq!(quantize(-1.0), -8);
        assert_eq!(quantize(2.0), 7);
        assert_eq!(quantize(0.0), 0);
        // A coefficient lands on its nearest index's angle.
        for i in 0..=40 {
            let k = -0.99 + 0.0495 * f64::from(i);
            let c = quantize(k);
            let err = (dequantize(c).asin() - k.asin()).abs();
            assert!(err < 0.11, "k {k}: index {c}, angle off by {err}");
        }
    }

    #[test]
    fn the_standards_synthesis_inverts_the_analysis() {
        let mut x: Vec<f32> = (0..300)
            .map(|n| ((n as f32) * 0.37).sin() * 100.0 + ((n * 7) % 13) as f32)
            .collect();
        let original = x.clone();
        let k = [0.9, -0.5, 0.3, -0.2, 0.1, 0.05];
        let mut kq = [0.0; 6];
        for (q, &k) in kq.iter_mut().zip(&k) {
            *q = dequantize(quantize(k));
        }
        let mut a = [0.0; MAX_ORDER_LONG + 1];
        lpc(&kq, &mut a);
        analysis(&mut x, &a, 6);
        assert_ne!(x, original);
        synthesis(&mut x, &a, 6);
        for (y, x) in x.iter().zip(&original) {
            assert!((y - x).abs() < 1e-2, "{y} vs {x}");
        }
    }

    #[test]
    fn levinson_finds_a_known_predictor() {
        // An AR(1) process with coefficient 0.8 has r[i] = 0.8^i r[0]: k1 = -0.8, the
        // rest 0, and a gain of 1 / (1 - 0.64).
        let r: Vec<f64> = (0..5).map(|i| 0.8f64.powi(i)).collect();
        let mut k = [0.0; 4];
        let gain = levinson(&r, &mut k);
        assert!((k[0] + 0.8).abs() < 1e-9, "{k:?}");
        assert!(k[1..].iter().all(|k| k.abs() < 1e-9), "{k:?}");
        assert!((gain - 1.0 / 0.36).abs() < 1e-9, "{gain}");
    }

    /// An impulse in time is a cosine across the MDCT lines: TNS predicts it and sends a
    /// filter; white lines it leaves alone.
    #[test]
    fn a_click_gets_a_filter_and_noise_does_not() {
        let analyzer = Analyzer::new(48_000, &SWB_LONG, &SWB_SHORT);
        let frame = Shape {
            offsets: &SWB_LONG,
            bands: 49,
            windows: 1,
            lines: 1024,
        };
        let mut ch = Channel::new();
        for (k, s) in ch.spec.iter_mut().enumerate() {
            *s = 1000.0 * (std::f32::consts::PI / 1024.0 * 300.5 * (k as f32 + 0.5)).cos();
        }
        for i in 0..49 {
            ch.thr[i] = 1.0;
        }
        let before = ch.spec.clone();
        analyzer.apply(&mut ch, &frame);
        let f = ch.tns.filters[0];
        assert!(f.order > 0, "no filter on a click");
        assert_eq!(usize::from(f.length), 49 - 13, "from band 13 (1.5 kHz) up");
        // Below the start band nothing changes; above the TNS limit (band 40) neither.
        assert_eq!(ch.spec[..64], before[..64]);
        assert_eq!(
            ch.spec[usize::from(SWB_LONG[40])..],
            before[usize::from(SWB_LONG[40])..]
        );
        assert!(ch.thr[20] < 1.0 / GAIN_THRESHOLD as f32);
        assert_eq!(ch.thr[5], 1.0);

        let mut seed = 9u32;
        for s in &mut ch.spec {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *s = (seed >> 8) as f32 / (1 << 24) as f32 - 0.5;
        }
        analyzer.apply(&mut ch, &frame);
        assert!(!ch.tns.present(), "a filter on white lines: {:?}", ch.tns);
    }

    #[test]
    fn written_bits_match_counted_bits() {
        let mut tns = Tns::default();
        for windows in [1, 8] {
            for (order, coef, compress) in [(12u8, 7i8, false), (3, -4, true), (7, -8, false)] {
                let order = if windows == 8 { order.min(7) } else { order };
                tns.clear();
                for (w, f) in tns.filters[..windows].iter_mut().enumerate() {
                    if w % 3 == 1 {
                        continue;
                    }
                    f.length = 10;
                    f.order = order;
                    f.compress = compress;
                    f.coef[..usize::from(order)].fill(coef);
                }
                let mut out = Vec::new();
                let mut w = BitWriter::new(&mut out);
                tns.write(&mut w, windows);
                assert_eq!(w.bits() as u32, tns.bits(windows), "{windows} windows");
            }
        }
        tns.clear();
        assert_eq!(tns.bits(1), 0);
    }
}
