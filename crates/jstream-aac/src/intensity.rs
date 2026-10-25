//! Intensity stereo (ISO/IEC 13818-7, 1997; the syntax and the decoder's
//! reconstruction are ISO/IEC 14496-3 §4.6.8.2): in a CPE with a common window, an upper
//! band whose two channels differ mainly in level is coded once, on the left channel, and
//! the right channel carries only its direction (`INTENSITY_HCB` in phase,
//! `INTENSITY_HCB2` out of phase) and an `is_position`, from which the decoder rebuilds
//! it as `±0.5^(is_position/4)` times the left band. The right channel's spectral bits for
//! those bands go to the rest of the frame.
//!
//! The decision follows J. Herre, K. Brandenburg and D. Lederer, "Intensity stereo
//! coding", AES 96th Convention, 1994, preprint 3799: above a start frequency that falls
//! with the bit rate, a band is coded this way only where the two channels' spectra have
//! nearly the same shape (their normalized correlation is at least [`MIN_CORRELATION`]),
//! so that the ear, which follows only the envelope of high-frequency sound, hears the
//! level difference and not the lost waveform detail. The encoder also checks that what
//! the band loses is masked: the error of the rebuilt left and right bands against the
//! originals must stay within each channel's masking threshold, with a slack that grows
//! as the bit rate falls, where coding noise would exceed the thresholds anyway. The
//! left band carries the channels' sum (in the direction of their correlation), scaled to
//! the left channel's energy, and the position is `round(2 log2(E_L / E_R))`, so the
//! rebuilt right band has the right channel's energy.
//!
//! Above [`MAX_BITS_PER_CHANNEL`] intensity stereo is never used: there the bits suffice
//! to code both channels' waveforms.

/// `INTENSITY_HCB2` and `INTENSITY_HCB` (14496-3 Table 4.56): out of phase and in phase.
pub(crate) const INTENSITY_HCB2: u8 = 14;
pub(crate) const INTENSITY_HCB: u8 = 15;
/// Whether a section's codebook is one of the intensity books.
pub(crate) fn is_intensity(book: u8) -> bool {
    book == INTENSITY_HCB || book == INTENSITY_HCB2
}

/// The per-channel bit rate above which intensity stereo is never used.
pub(crate) const MAX_BITS_PER_CHANNEL: u32 = 64_000;
/// The lowest normalized correlation between the channels' band spectra for intensity
/// coding.
const MIN_CORRELATION: f32 = 0.9;
/// The largest `|is_position|` written: a level difference of 45 dB. Keeping positions
/// within ±30 keeps every difference between consecutive ones within the scalefactor
/// codebook's ±60.
pub(crate) const MAX_POSITION: i32 = 30;

/// Where intensity coding starts at `bits_per_channel` bits per second per channel, in
/// Hz, and how far over the masking threshold its error may go; `None` above
/// [`MAX_BITS_PER_CHANNEL`]. 6 kHz with a slack of 4 (6 dB) at 32 kbps and below, rising
/// linearly to 10 kHz and no slack at 64 kbps.
pub(crate) fn start(bits_per_channel: u32) -> Option<(f64, f32)> {
    if bits_per_channel > MAX_BITS_PER_CHANNEL {
        return None;
    }
    let t = ((f64::from(bits_per_channel) - 32_000.0) / 32_000.0).clamp(0.0, 1.0);
    Some((6000.0 + 4000.0 * t, (4.0 - 3.0 * t) as f32))
}

/// The gain a decoder applies to the left band for `position`: `0.5^(position/4)`.
pub(crate) fn scale(position: i32) -> f32 {
    (-0.25 * position as f32).exp2()
}

/// One band's intensity coding: its position and whether the channels are in phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Band {
    pub(crate) position: i32,
    pub(crate) in_phase: bool,
}

impl Band {
    /// The section codebook the right channel names for this band.
    pub(crate) fn book(self) -> u8 {
        if self.in_phase {
            INTENSITY_HCB
        } else {
            INTENSITY_HCB2
        }
    }
}

/// Whether band `l`/`r` (same lines of both channels) should be intensity coded, given
/// their masking thresholds and the slack. When it should, `l` is replaced by the signal
/// the left channel codes and the band is returned.
pub(crate) fn code(l: &mut [f32], r: &[f32], thr: [f32; 2], slack: f32) -> Option<Band> {
    let (mut el, mut er, mut cross) = (0.0f32, 0.0f32, 0.0f32);
    for (&x, &y) in l.iter().zip(r) {
        el += x * x;
        er += y * y;
        cross += x * y;
    }
    // Nothing to gain where either band would code to nothing, or where the energies
    // aren't finite.
    if !(el.is_finite() && er.is_finite()) || el <= thr[0] || er <= thr[1] {
        return None;
    }
    let correlation = cross / (el * er).sqrt();
    if correlation.abs() < MIN_CORRELATION {
        return None;
    }
    let in_phase = correlation > 0.0;
    let d = if in_phase { 1.0 } else { -1.0 };
    let es: f32 = l.iter().zip(r).map(|(&x, &y)| (x + d * y).powi(2)).sum();
    if es <= 0.0 {
        return None;
    }
    let a = (el / es).sqrt();
    let position = (2.0 * (el / er).log2())
        .round()
        .clamp(-MAX_POSITION as f32, MAX_POSITION as f32) as i32;
    let g = d * scale(position);
    let (mut err_l, mut err_r) = (0.0f32, 0.0f32);
    for (&x, &y) in l.iter().zip(r) {
        let s = a * (x + d * y);
        err_l += (x - s).powi(2);
        err_r += (y - g * s).powi(2);
    }
    if err_l > slack * thr[0] || err_r > slack * thr[1] {
        return None;
    }
    for (x, &y) in l.iter_mut().zip(r) {
        *x = a * (*x + d * y);
    }
    Some(Band { position, in_phase })
}

#[cfg(test)]
mod tests {
    use super::{Band, INTENSITY_HCB, INTENSITY_HCB2, MAX_POSITION, code, scale, start};

    fn band(seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..32)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 8) as f32 / (1 << 24) as f32 - 0.5
            })
            .collect()
    }

    fn energy(x: &[f32]) -> f32 {
        x.iter().map(|v| v * v).sum()
    }

    /// A band that is one signal at two levels codes exactly: the left band is unchanged,
    /// and the decoder's rebuilt right band is the original, sign included.
    #[test]
    fn a_panned_band_codes_exactly() {
        let l = band(1);
        for (gain, position, in_phase) in [(0.5f32, 4, true), (-0.25, 8, false), (2.0, -4, true)] {
            let r: Vec<f32> = l.iter().map(|x| gain * x).collect();
            let mut coded = l.clone();
            let b = code(&mut coded, &r, [1e-6, 1e-6], 1.0).expect("intensity coded");
            assert_eq!(b, Band { position, in_phase });
            assert_eq!(
                b.book(),
                if in_phase {
                    INTENSITY_HCB
                } else {
                    INTENSITY_HCB2
                }
            );
            let dir = if in_phase { 1.0 } else { -1.0 };
            for ((&c, &x), &y) in coded.iter().zip(&l).zip(&r) {
                assert!((c - x).abs() < 1e-5, "left {c} for {x}");
                assert!((dir * scale(b.position) * c - y).abs() < 1e-5);
            }
        }
    }

    /// The left band keeps the left channel's energy, and the position the energy ratio,
    /// to the nearest 1.5 dB step.
    #[test]
    fn energies_carry_over() {
        let l = band(2);
        let noise = band(3);
        let r: Vec<f32> = l
            .iter()
            .zip(&noise)
            .map(|(x, n)| 0.3 * x + 0.03 * n)
            .collect();
        let mut coded = l.clone();
        let b = code(&mut coded, &r, [0.01, 0.01], 1.0).expect("intensity coded");
        assert!((energy(&coded) / energy(&l) - 1.0).abs() < 1e-4);
        let want = 2.0 * (energy(&l) / energy(&r)).log2();
        assert!((b.position as f32 - want).abs() <= 0.5);
        let rebuilt = scale(b.position).powi(2) * energy(&coded);
        let db = 10.0 * (rebuilt / energy(&r)).log10();
        assert!(db.abs() <= 0.76, "{db} dB");
    }

    /// Bands that don't share a shape, bands whose error isn't masked, bands that code to
    /// nothing, and non-finite bands stay as they were.
    #[test]
    fn refuses_what_it_should() {
        let l = band(4);
        let other = band(5);
        let mut x = l.clone();
        assert_eq!(code(&mut x, &other, [1e-6, 1e-6], 4.0), None);
        assert_eq!(x, l);
        // Correlated enough, but the error is over a strict threshold.
        let r: Vec<f32> = l
            .iter()
            .zip(&other)
            .map(|(a, b)| 0.5 * a + 0.1 * b)
            .collect();
        assert_eq!(code(&mut x, &r, [1e-6, 1e-6], 1.0), None);
        // Under the thresholds: nothing to code.
        let e = energy(&l);
        assert_eq!(code(&mut x, &l, [e, e], 1.0), None);
        assert_eq!(code(&mut x, &[0.0; 32], [1e-6, 1e-6], 1.0), None);
        let mut inf = l.clone();
        inf[3] = f32::INFINITY;
        assert_eq!(code(&mut inf, &l, [1e-6, 1e-6], 1.0), None);
        assert_eq!(x, l);
    }

    /// Positions stay within ±30, however far apart the levels.
    #[test]
    fn positions_are_clamped() {
        let l = band(6);
        let r: Vec<f32> = l.iter().map(|x| 1e-4 * x).collect();
        let mut x = l.clone();
        let b = code(&mut x, &r, [1e-12, 1e-12], 1.0e15).expect("coded");
        assert_eq!(b.position, MAX_POSITION);
        let mut y = r.clone();
        let b = code(&mut y, &l, [1e-12, 1e-12], 1.0e15).expect("coded");
        assert_eq!(b.position, -MAX_POSITION);
    }

    #[test]
    fn starts_by_rate() {
        assert_eq!(start(16_000), Some((6000.0, 4.0)));
        assert_eq!(start(32_000), Some((6000.0, 4.0)));
        assert_eq!(start(48_000), Some((8000.0, 2.5)));
        assert_eq!(start(64_000), Some((10_000.0, 1.0)));
        assert_eq!(start(64_001), None);
        assert_eq!(scale(0), 1.0);
        assert_eq!(scale(4), 0.5);
        assert_eq!(scale(-8), 4.0);
    }
}
