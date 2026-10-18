//! Quantization and the two nested iteration loops of ISO/IEC 13818-7 Annex B
//! (informative, 1997): an inner loop that finds the global quantizer step meeting the
//! bit budget, and an outer loop that amplifies the scalefactor bands whose quantization
//! noise exceeds the masking threshold, until none does, every band has been amplified,
//! or the iterations run out. The quantizer is Annex B's: `q = int(|x|^(3/4) / step +
//! 0.4054)`.
//!
//! The outer loop starts from scalefactors shaped by the thresholds (noise power follows
//! the square of the step, `2^(sf/2)`), and keeps the best result it has seen, by total
//! noise over threshold, as Annex B does.

use crate::huffman::{self, BOOKS, MAX_QUANT, NEVER, Section, ZERO_HCB};

/// The scalefactor offset of the dequantizer: `2^((sf - SF_OFFSET)/4)`.
const SF_OFFSET: i32 = 100;
/// Annex B's rounding offset.
const MAGIC: f32 = 0.4054;
/// The most outer-loop iterations per channel and frame.
const MAX_OUTER: usize = 12;
/// The widest range of scalefactors within a channel: consecutive differences must fit
/// the scalefactor codebook's -60..=60.
const SF_RANGE: i32 = 60;
/// The most bands in a frame: 8 short windows of 14 bands, or 49 long bands.
pub(crate) const MAX_BANDS: usize = 8 * 14;

/// How a frame's lines split into windows and bands.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Shape<'a> {
    /// Band offsets within a window.
    pub(crate) offsets: &'a [u16],
    /// Coded bands per window (`max_sfb`).
    pub(crate) bands: usize,
    /// Windows: 1 or 8. Each short window is its own group.
    pub(crate) windows: usize,
    /// Lines per window: 1024 or 128.
    pub(crate) lines: usize,
}

impl Shape<'_> {
    /// The lines of band `i` (window-major), as a range.
    pub(crate) fn range(&self, i: usize) -> std::ops::Range<usize> {
        let (w, b) = (i / self.bands, i % self.bands);
        let base = w * self.lines;
        base + usize::from(self.offsets[b])..base + usize::from(self.offsets[b + 1])
    }

    /// Bands in the frame, over every window.
    pub(crate) fn count(&self) -> usize {
        self.bands * self.windows
    }

    /// The section length field's width: 5 bits for long windows, 3 for short.
    pub(crate) fn len_bits(&self) -> u32 {
        if self.windows == 1 { 5 } else { 3 }
    }
}

/// One channel of one frame: its spectrum, the model's output for it, and what the
/// loops chose.
pub(crate) struct Channel {
    /// The MDCT lines, window-major.
    pub(crate) spec: Vec<f32>,
    /// Energy and masking threshold per band (window-major).
    pub(crate) energy: [f32; MAX_BANDS],
    pub(crate) thr: [f32; MAX_BANDS],
    /// Quantized lines.
    pub(crate) q: Vec<i32>,
    /// Scalefactor per band; meaningful where the band isn't all zeros.
    pub(crate) sf: [i32; MAX_BANDS],
    /// Sections per window, and how many each window has.
    pub(crate) sections: [[Section; 49]; 8],
    pub(crate) section_count: [usize; 8],
    /// The `global_gain` to write: the first coded band's scalefactor.
    pub(crate) global_gain: i32,
    /// The TNS filters applied to `spec`, per window.
    pub(crate) tns: crate::tns::Tns,
    // Scratch.
    xp: Vec<f32>,
    amp: [i32; MAX_BANDS],
    best_amp: [i32; MAX_BANDS],
    sf_min: [i32; MAX_BANDS],
    costs: [[u32; BOOKS]; MAX_BANDS],
    sums: [[u32; BOOKS]; 49],
}

impl Channel {
    pub(crate) fn new() -> Self {
        Self {
            spec: vec![0.0; 1024],
            energy: [0.0; MAX_BANDS],
            thr: [0.0; MAX_BANDS],
            q: vec![0; 1024],
            sf: [0; MAX_BANDS],
            sections: [[Section::default(); 49]; 8],
            section_count: [0; 8],
            global_gain: SF_OFFSET,
            tns: crate::tns::Tns::default(),
            xp: vec![0.0; 1024],
            amp: [0; MAX_BANDS],
            best_amp: [0; MAX_BANDS],
            sf_min: [0; MAX_BANDS],
            costs: [[NEVER; BOOKS]; MAX_BANDS],
            sums: [[0; BOOKS]; 49],
        }
    }
}

/// Tables shared by every channel.
pub(crate) struct Quantizer {
    /// `q^(4/3)` for every quantized magnitude.
    pow43: Vec<f32>,
}

/// The quantizer step's reciprocal for `|x|^(3/4)`: `2^(-3/16 (sf - 100))`.
fn inv_step(sf: i32) -> f32 {
    (-0.1875 * (sf - SF_OFFSET) as f32).exp2()
}

/// The dequantizer gain: `2^((sf - 100)/4)`.
fn gain(sf: i32) -> f32 {
    (0.25 * (sf - SF_OFFSET) as f32).exp2()
}

impl Quantizer {
    pub(crate) fn new() -> Self {
        Self {
            pow43: (0..=MAX_QUANT)
                .map(|q| (q as f32).powf(4.0 / 3.0))
                .collect(),
        }
    }

    /// Quantizes band `i` at `sf` into `ch.q`, and returns its largest magnitude.
    fn quantize_band(ch: &mut Channel, shape: &Shape<'_>, i: usize, sf: i32) -> i32 {
        let s = inv_step(sf);
        let mut max = 0;
        for k in shape.range(i) {
            // In range: sf is at least sf_min, which keeps it at most MAX_QUANT.
            let a = ((ch.xp[k] * s + MAGIC) as i32).min(MAX_QUANT);
            max = max.max(a);
            ch.q[k] = if ch.spec[k] < 0.0 { -a } else { a };
        }
        max
    }

    /// The scalefactor of band `i` for global step `g`.
    fn sf_of(ch: &Channel, i: usize, g: i32) -> i32 {
        (g - ch.amp[i]).max(ch.sf_min[i]).max(0)
    }

    /// Quantizes every band at global step `g`, and returns the bits of the channel's
    /// sections, scalefactors and spectrum. `exact` sections greedily; otherwise the
    /// count is an upper bound of the exact one.
    fn count(&self, ch: &mut Channel, shape: &Shape<'_>, g: i32, exact: bool) -> u32 {
        let n = shape.count();
        let mut first_sf = None;
        let mut last_sf: Option<i32> = None;
        let mut sf_bits = 0;
        for i in 0..n {
            let sf = Self::sf_of(ch, i, g);
            ch.sf[i] = sf;
            Self::quantize_band(ch, shape, i, sf);
            let range = shape.range(i);
            let c = &mut ch.costs[i];
            huffman::band_costs(&ch.q[range], exact, c);
            if c[usize::from(ZERO_HCB)] != 0 {
                // The first coded band's scalefactor is the global gain, a zero difference.
                first_sf.get_or_insert(sf);
                sf_bits += huffman::sf_bits(sf - last_sf.unwrap_or(sf));
                last_sf = Some(sf);
            }
        }
        ch.global_gain = first_sf.unwrap_or(g);
        let len_bits = shape.len_bits();
        let mut bits = sf_bits;
        for w in 0..shape.windows {
            let costs = &ch.costs[w * shape.bands..(w + 1) * shape.bands];
            if exact {
                let (count, b) =
                    huffman::sections(costs, len_bits, &mut ch.sections[w], &mut ch.sums);
                ch.section_count[w] = count;
                bits += b;
            } else {
                bits += huffman::estimate(costs, len_bits);
            }
        }
        bits
    }

    /// The quantization noise of band `i` as quantized now.
    fn noise(&self, ch: &Channel, shape: &Shape<'_>, i: usize) -> f32 {
        let g = gain(ch.sf[i]);
        shape
            .range(i)
            .map(|k| {
                let d = ch.spec[k].abs() - self.pow43[ch.q[k].unsigned_abs() as usize] * g;
                d * d
            })
            .sum()
    }

    /// The smallest global step in `lo..=255` whose count fits `budget`, by bisection
    /// (bits fall as the step grows); 255 when none fits.
    fn search(&self, ch: &mut Channel, shape: &Shape<'_>, lo: i32, budget: u32) -> i32 {
        let (mut lo, mut hi) = (lo, 255);
        if self.count(ch, shape, lo, false) <= budget {
            return lo;
        }
        // Invariant: lo doesn't fit; hi fits or is 255.
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if self.count(ch, shape, mid, false) <= budget {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        hi
    }

    /// Runs both loops on `ch` for `budget` bits of sections, scalefactors and spectrum,
    /// leaving the result in `ch.q`, `ch.sf`, `ch.sections` and `ch.global_gain`, and
    /// returns its bits.
    pub(crate) fn run(&self, ch: &mut Channel, shape: &Shape<'_>, budget: u32) -> u32 {
        let n = shape.count();
        let lines = shape.windows * shape.lines;
        for k in 0..lines {
            ch.xp[k] = ch.spec[k].abs().powf(0.75);
        }
        // The smallest scalefactor that keeps each band within the escape codebook, and
        // the first amplification, shaped by the thresholds: noise power goes with
        // 2^(sf/2), so halving the allowed noise per line takes 2 steps.
        let mut reference = 0.0f32;
        for i in 0..n {
            let range = shape.range(i);
            let width = range.len() as f32;
            let peak = ch.xp[range].iter().fold(0.0f32, |a, &b| a.max(b));
            ch.sf_min[i] = if peak > 0.0 {
                // xp * 2^(-3/16 (sf-100)) + 0.4054 <= 8191.
                (SF_OFFSET as f32 + (16.0 / 3.0) * (peak / (MAX_QUANT as f32 - 0.5)).log2()).ceil()
                    as i32
            } else {
                0
            };
            if ch.energy[i] > ch.thr[i] {
                reference = reference.max(ch.thr[i] / width);
            }
        }
        for i in 0..n {
            let width = shape.range(i).len() as f32;
            ch.amp[i] = if ch.energy[i] > ch.thr[i] && reference > 0.0 {
                let ratio = reference / (ch.thr[i] / width).max(1e-12);
                ((2.0 * ratio.log2()).round() as i32).clamp(0, SF_RANGE)
            } else {
                0
            };
        }
        let floor = (0..n)
            .map(|i| ch.sf_min[i])
            .max()
            .unwrap_or(0)
            .clamp(0, 255);
        let mut g = self.search(ch, shape, floor, budget);
        let mut best = (f32::INFINITY, g);
        ch.best_amp = ch.amp;
        for _ in 0..MAX_OUTER {
            // `search` leaves the bands quantized at its last probe, not necessarily `g`.
            self.count(ch, shape, g, false);
            let mut score = 0.0f32;
            let mut over = [false; MAX_BANDS];
            let mut any_over = false;
            let mut coded = 0;
            for (i, over) in over.iter_mut().enumerate().take(n) {
                if ch.energy[i] <= ch.thr[i] {
                    continue;
                }
                coded += 1;
                let noise = self.noise(ch, shape, i);
                if noise > ch.thr[i] {
                    *over = true;
                    any_over = true;
                    score += (noise / ch.thr[i]).log2();
                }
            }
            if score < best.0 {
                best = (score, g);
                ch.best_amp = ch.amp;
            }
            if !any_over {
                break;
            }
            let mut amplified = 0;
            for (i, &over) in over.iter().enumerate().take(n) {
                if over && ch.amp[i] < SF_RANGE && g - ch.amp[i] > ch.sf_min[i].max(0) {
                    ch.amp[i] += 1;
                    amplified += 1;
                }
            }
            // Annex B's other stop conditions: nothing left to amplify, or every coded
            // band amplified (which would only move the global step).
            if amplified == 0 || amplified == coded {
                break;
            }
            g = self.search(ch, shape, g, budget);
        }
        ch.amp = ch.best_amp;
        self.count(ch, shape, best.1, true)
    }
}
