//! The psychoacoustic model: a masking threshold per scalefactor band, the perceptual
//! entropy that rate control spends bits by, and the attack detector that chooses short
//! windows.
//!
//! Sources, all published before 2003, as ADR-0062 asks:
//! - The threshold follows J. D. Johnston, "Transform coding of audio signals using
//!   perceptual noise criteria", IEEE JSAC 6(2), 1988: band energies spread across
//!   critical bands, an offset between a tone-masking-noise value (14.5 + z dB) and a
//!   noise-masking-tone value (5.5 dB) chosen by a spectral flatness tonality measure,
//!   and the threshold in quiet as a floor. The Bark scale is E. Zwicker and E. Terhardt,
//!   JASA 68(5), 1980; the spreading function is M. R. Schroeder, B. S. Atal and J. L.
//!   Hall, JASA 66(6), 1979; the threshold in quiet is E. Terhardt, "Calculating virtual
//!   pitch", Hearing Research 1, 1979.
//! - Pre-echo control (the threshold may rise at most twofold from one long block to the
//!   next) and perceptual entropy as the measure of a block's bit demand are ISO/IEC
//!   13818-7 Annex B (informative), 1997.
//! - The attack detector compares the energy of high-passed 128-sample blocks with the
//!   blocks before them, after B. Edler, "Codierung von Audiosignalen mit überlappender
//!   Transformation und adaptiven Fensterfunktionen", Frequenz 43(9), 1989, which
//!   introduced AAC's window switching.

/// Long-window offset of the smallest spreading weight kept, in dB.
const SPREAD_FLOOR_DB: f64 = -60.0;
/// The threshold in quiet is capped here, in dB above one 16-bit step: past about
/// 16 kHz Terhardt's formula climbs past any real playback level.
const ATH_CAP_DB: f64 = 40.0;
/// Margin below the threshold in quiet, in dB: the playback level isn't known.
const ATH_MARGIN_DB: f64 = 10.0;
/// Pre-echo control: the most a long block's threshold may rise over the previous one's
/// (13818-7 Annex B's `rpelev`).
const PRE_ECHO_RISE: f32 = 2.0;
/// An attack: a block with this many times the mean energy of the eight before it.
const ATTACK_RATIO: f32 = 10.0;
/// Below this high-passed block energy (in 16-bit units squared, about -60 dBFS over a
/// block) nothing counts as an attack.
const ATTACK_FLOOR: f32 = 128.0 * 32.0 * 32.0;

/// The critical-band rate in Bark of `f` Hz (Zwicker and Terhardt 1980).
fn bark(f: f64) -> f64 {
    13.0 * (0.00076 * f).atan() + 3.5 * (f / 7500.0).powi(2).atan()
}

/// The spreading function in dB at `dz` Bark above the masker (Schroeder et al. 1979):
/// 0 dB at the masker, falling about 25 dB/Bark below it and 10 dB/Bark above it.
fn spreading_db(dz: f64) -> f64 {
    let t = dz + 0.474;
    15.811_389 + 7.5 * t - 17.5 * (1.0 + t * t).sqrt()
}

/// The threshold in quiet in dB SPL at `f` Hz (Terhardt 1979).
fn ath_db(f: f64) -> f64 {
    let k = (f / 1000.0).max(0.01);
    3.64 * k.powf(-0.8) - 6.5 * (-0.6 * (k - 3.3).powi(2)).exp() + 1e-3 * k.powi(4)
}

/// One window length's scalefactor bands, with what the model precomputes for them.
pub(crate) struct Layout {
    /// Band offsets in lines (`bands + 1` of them).
    pub(crate) offsets: &'static [u16],
    /// Bands coded at this bandwidth (`max_sfb`).
    pub(crate) bands: usize,
    /// Bark of each band's centre.
    bark: Vec<f32>,
    /// Spreading weights, `bands x bands`, masker in the column.
    spread: Vec<f32>,
    /// Sum of each row of `spread`.
    spread_sum: Vec<f32>,
    /// The threshold in quiet of each band, as MDCT energy.
    ath: Vec<f32>,
}

impl Layout {
    /// The layout of `offsets` for windows of `lines` lines at `sample_rate`, coding the
    /// bands that start below `bandwidth` Hz.
    pub(crate) fn new(
        offsets: &'static [u16],
        lines: usize,
        sample_rate: u32,
        bandwidth: f64,
    ) -> Self {
        let line_hz = f64::from(sample_rate) / (2 * lines) as f64;
        let all = offsets.len() - 1;
        let bands = (0..all)
            .take_while(|&b| f64::from(offsets[b]) * line_hz < bandwidth)
            .count()
            .max(1);
        let bark: Vec<f32> = (0..bands)
            .map(|b| {
                let mid = f64::from(offsets[b] + offsets[b + 1]) / 2.0;
                bark(mid * line_hz) as f32
            })
            .collect();
        let mut spread = vec![0.0f32; bands * bands];
        for (b, row) in spread.chunks_exact_mut(bands).enumerate() {
            for (j, s) in row.iter_mut().enumerate() {
                let db = spreading_db(f64::from(bark[b] - bark[j]));
                if db > SPREAD_FLOOR_DB {
                    *s = 10f64.powf(db / 10.0) as f32;
                }
            }
        }
        let spread_sum = spread.chunks_exact(bands).map(|r| r.iter().sum()).collect();
        // One 16-bit step of white noise gives an MDCT energy per line of about the
        // window length (the forward transform's factor 2, squared, times N/4).
        let step_energy = (2 * lines) as f64;
        let ath = (0..bands)
            .map(|b| {
                let (lo, hi) = (usize::from(offsets[b]), usize::from(offsets[b + 1]));
                let min_db = (lo..hi)
                    .map(|k| ath_db((k as f64 + 0.5) * line_hz))
                    .fold(f64::INFINITY, f64::min)
                    .min(ATH_CAP_DB);
                let per_line = step_energy * 10f64.powf((min_db - ATH_MARGIN_DB) / 10.0);
                (per_line * (hi - lo) as f64) as f32
            })
            .collect();
        Self {
            offsets,
            bands,
            bark,
            spread,
            spread_sum,
            ath,
        }
    }

    /// The width of band `b` in lines.
    pub(crate) fn width(&self, b: usize) -> usize {
        usize::from(self.offsets[b + 1] - self.offsets[b])
    }
}

/// The energy of each band of one window, into `energy` (`layout.bands` values).
pub(crate) fn band_energies(layout: &Layout, spec: &[f32], energy: &mut [f32]) {
    for (b, e) in energy.iter_mut().enumerate().take(layout.bands) {
        let lo = usize::from(layout.offsets[b]);
        let hi = usize::from(layout.offsets[b + 1]);
        *e = spec[lo..hi].iter().map(|x| x * x).sum();
    }
}

/// The masking threshold of each band of one window, into `thr`, from its spectrum and
/// band energies. `prev` holds the previous long block's thresholds for pre-echo control,
/// and is updated; it is `None` for short windows.
pub(crate) fn thresholds(
    layout: &Layout,
    spec: &[f32],
    energy: &[f32],
    thr: &mut [f32],
    prev: Option<&mut [f32]>,
) {
    let n = layout.bands;
    for b in 0..n {
        // Spread the energy density (per line), so wide bands don't dominate.
        let row = &layout.spread[b * n..(b + 1) * n];
        let mut spread = 0.0f32;
        for j in 0..n {
            spread += row[j] * energy[j] / layout.width(j) as f32;
        }
        let density = spread / layout.spread_sum[b];
        // Tonality from the band's spectral flatness: -5 dB or flatter is noise,
        // -25 dB or less flat is a tone.
        let lo = usize::from(layout.offsets[b]);
        let hi = usize::from(layout.offsets[b + 1]);
        let width = (hi - lo) as f32;
        let mean = energy[b] / width + 1e-9;
        let log_mean: f32 = spec[lo..hi]
            .iter()
            .map(|x| (x * x + 1e-9).ln())
            .sum::<f32>()
            / width;
        let flatness_db = 10.0 * (log_mean.exp() / mean).log10();
        let tonal = ((-5.0 - flatness_db) / 20.0).clamp(0.0, 1.0);
        let offset_db = tonal * (14.5 + layout.bark[b]) + (1.0 - tonal) * 5.5;
        let masked = density * width * 10f32.powf(-offset_db / 10.0);
        thr[b] = masked.max(layout.ath[b]);
    }
    if let Some(prev) = prev {
        for b in 0..n {
            let limit = PRE_ECHO_RISE * prev[b];
            if limit > 0.0 && thr[b] > limit {
                thr[b] = limit.max(layout.ath[b]);
            }
            prev[b] = thr[b];
        }
    }
}

/// The perceptual entropy of bands with `energy` and `thr`: half a bit per line per
/// doubling of signal over threshold, the rate-distortion bound for Gaussian lines.
pub(crate) fn entropy(layout: &Layout, energy: &[f32], thr: &[f32]) -> f32 {
    (0..layout.bands)
        .map(|b| 0.5 * layout.width(b) as f32 * (1.0 + energy[b] / thr[b].max(1e-9)).log2())
        .sum()
}

/// Finds attacks in a block of samples, 128 samples at a time.
#[derive(Debug, Default)]
pub(crate) struct AttackDetector {
    /// The high-passed energies of the eight blocks before the next one.
    history: [f32; 8],
}

impl AttackDetector {
    /// Whether the 1024 samples starting at `start` in each of `channels` hold an attack.
    /// `start` must be at least 1 (the high-pass filter reads the sample before).
    pub(crate) fn attack(&mut self, channels: &[&[f32]], start: usize) -> bool {
        let mut found = false;
        for block in 0..8 {
            let lo = start + block * 128;
            let mut e = 0.0f32;
            for ch in channels {
                for i in lo..lo + 128 {
                    let d = ch[i] - ch[i - 1];
                    e += d * d;
                }
            }
            let mean = self.history.iter().sum::<f32>() / 8.0;
            if e > ATTACK_FLOOR && e > ATTACK_RATIO * mean {
                found = true;
            }
            self.history.rotate_left(1);
            self.history[7] = e;
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AttackDetector, Layout, ath_db, band_energies, bark, entropy, spreading_db, thresholds,
    };
    use crate::tables::{SWB_LONG, SWB_SHORT};

    #[test]
    fn model_curves_have_their_published_shape() {
        // 0 dB at the masker, steeper below it than above.
        assert!(spreading_db(0.0).abs() < 1e-3);
        assert!(spreading_db(-2.0) < spreading_db(2.0));
        assert!(spreading_db(3.0) < -15.0);
        // About 24 Bark at the top of hearing, and 8.5 at 1 kHz.
        assert!((bark(1000.0) - 8.5).abs() < 0.2);
        assert!(bark(20_000.0) > 24.0);
        // Most sensitive around 3-4 kHz, below 0 dB SPL.
        assert!(ath_db(3300.0) < 0.0);
        assert!(ath_db(100.0) > 20.0);
    }

    #[test]
    fn bandwidth_sets_the_coded_bands() {
        let full = Layout::new(&SWB_LONG, 1024, 48_000, 24_000.0);
        assert_eq!(full.bands, 49);
        let cut = Layout::new(&SWB_LONG, 1024, 48_000, 16_000.0);
        // Band 43 starts at 768 lines = 18 kHz; 16 kHz falls inside band 40 (640..672).
        assert!(cut.bands < 49 && cut.bands > 35);
        let short = Layout::new(&SWB_SHORT, 128, 48_000, 16_000.0);
        assert!(short.bands <= 14);
    }

    #[test]
    fn a_tone_masks_less_than_noise_of_the_same_energy() {
        let layout = Layout::new(&SWB_LONG, 1024, 48_000, 20_000.0);
        let mut tone = vec![0.0f32; 1024];
        tone[100] = 1.0e6;
        let mut noise = vec![0.0f32; 1024];
        let mut seed = 1u32;
        for x in &mut noise[96..108] {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *x = ((seed >> 8) as f32 / (1 << 24) as f32 - 0.5) * 1.0e6;
        }
        // Same energy in the band holding line 100.
        let et: f32 = tone.iter().map(|x| x * x).sum();
        let en: f32 = noise.iter().map(|x| x * x).sum();
        for x in &mut noise {
            *x *= (et / en).sqrt();
        }
        let mut e = vec![0.0; 49];
        let mut thr_t = vec![0.0; 49];
        let mut thr_n = vec![0.0; 49];
        band_energies(&layout, &tone, &mut e);
        thresholds(&layout, &tone, &e, &mut thr_t, None);
        let pe_t = entropy(&layout, &e, &thr_t);
        band_energies(&layout, &noise, &mut e);
        thresholds(&layout, &noise, &e, &mut thr_n, None);
        let b = (0..49).find(|&b| SWB_LONG[b + 1] > 100).expect("band");
        assert!(
            thr_t[b] < thr_n[b] / 4.0,
            "tone {} noise {}",
            thr_t[b],
            thr_n[b]
        );
        assert!(pe_t > 0.0);
    }

    #[test]
    fn pre_echo_control_limits_the_rise() {
        let layout = Layout::new(&SWB_LONG, 1024, 48_000, 20_000.0);
        let quiet = vec![10.0f32; 1024];
        let loud = vec![10_000.0f32; 1024];
        let mut e = vec![0.0; 49];
        let mut thr = vec![0.0; 49];
        let mut prev = vec![0.0; 49];
        band_energies(&layout, &quiet, &mut e);
        thresholds(&layout, &quiet, &e, &mut thr, Some(&mut prev));
        let before = prev.clone();
        band_energies(&layout, &loud, &mut e);
        thresholds(&layout, &loud, &e, &mut thr, Some(&mut prev));
        for b in 0..layout.bands {
            assert!(thr[b] <= 2.0 * before[b].max(layout.ath[b]) * 1.0001);
        }
    }

    #[test]
    fn attacks_are_found_after_silence_and_not_in_steady_noise() {
        let mut det = AttackDetector::default();
        let mut seed = 3u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((seed >> 8) as f32 / (1 << 24) as f32 - 0.5) * 8000.0
        };
        let steady: Vec<f32> = (0..1025 * 4).map(|_| noise()).collect();
        assert!(det.attack(&[&steady], 1));
        // Once its history has filled, steady noise holds no attack.
        assert!(!det.attack(&[&steady], 1025));
        assert!(!det.attack(&[&steady], 2049));
        let mut click = vec![0.0f32; 2049];
        click[1500] = 30_000.0;
        let mut det = AttackDetector::default();
        assert!(!det.attack(&[&click], 1));
        assert!(det.attack(&[&click], 1025));
    }
}
