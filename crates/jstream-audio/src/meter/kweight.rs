//! The BS.1770 K-weighting filter: a high-shelf pre-filter (the head's acoustic effect)
//! followed by the RLB high-pass. BS.1770 tabulates the coefficients for 48 kHz only;
//! these are derived for any rate from the analog prototypes' corner frequency, gain and
//! Q, as libebur128 does, which reproduces the 48 kHz table (tested below).

use std::f64::consts::PI;

/// One biquad section, direct form II transposed, in `f64`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn new(b: [f64; 3], a: [f64; 2]) -> Self {
        Self { b, a, z: [0.0; 2] }
    }

    #[inline]
    pub(crate) fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }

    pub(crate) fn reset(&mut self) {
        self.z = [0.0; 2];
    }
}

/// The two K-weighting stages for one channel.
#[derive(Clone, Copy, Debug)]
pub(crate) struct KWeighting {
    shelf: Biquad,
    highpass: Biquad,
}

impl KWeighting {
    /// The filter for `rate` Hz.
    pub(crate) fn new(rate: u32) -> Self {
        let (b, a) = shelf_coefficients(rate);
        let (hb, ha) = highpass_coefficients(rate);
        Self {
            shelf: Biquad::new(b, a),
            highpass: Biquad::new(hb, ha),
        }
    }

    #[inline]
    pub(crate) fn process(&mut self, x: f64) -> f64 {
        self.highpass.process(self.shelf.process(x))
    }

    pub(crate) fn reset(&mut self) {
        self.shelf.reset();
        self.highpass.reset();
    }
}

/// Stage 1, the high shelf: about +4 dB above 2 kHz.
fn shelf_coefficients(rate: u32) -> ([f64; 3], [f64; 2]) {
    let f0 = 1_681.974_450_955_533;
    let gain_db = 3.999_843_853_973_347;
    let q = 0.707_175_236_955_419_6;
    let k = (PI * f0 / f64::from(rate)).tan();
    let vh = 10f64.powf(gain_db / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    (
        [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    )
}

/// Stage 2, the RLB high-pass at about 38 Hz. BS.1770's numerator is `1, -2, 1`
/// unnormalised, kept as is so the 48 kHz case matches the table.
fn highpass_coefficients(rate: u32) -> ([f64; 3], [f64; 2]) {
    let f0 = 38.135_470_876_024_44;
    let q = 0.500_327_037_323_877_3;
    let k = (PI * f0 / f64::from(rate)).tan();
    let a0 = 1.0 + k / q + k * k;
    (
        [1.0, -2.0, 1.0],
        [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BS.1770 Table 1 (stage 1) and Table 2 (stage 2), 48 kHz.
    #[test]
    fn coefficients_at_48k_match_bs1770_tables() {
        let (b, a) = shelf_coefficients(48_000);
        let table_b = [
            1.535_124_859_586_97,
            -2.691_696_189_406_38,
            1.198_392_810_852_85,
        ];
        let table_a = [-1.690_659_293_182_41, 0.732_480_774_215_85];
        for (got, want) in b.iter().zip(table_b).chain(a.iter().zip(table_a)) {
            assert!((got - want).abs() < 1e-9, "stage 1: {got} vs {want}");
        }
        let (b, a) = highpass_coefficients(48_000);
        assert_eq!(b, [1.0, -2.0, 1.0]);
        let table_a = [-1.990_047_454_833_98, 0.990_072_250_366_21];
        for (got, want) in a.iter().zip(table_a) {
            assert!((got - want).abs() < 1e-9, "stage 2: {got} vs {want}");
        }
    }

    /// The filter's gain at `freq`, measured by running a sine through it.
    fn gain_db(rate: u32, freq: f64) -> f64 {
        let mut k = KWeighting::new(rate);
        let w = 2.0 * PI * freq / f64::from(rate);
        let n = rate as usize; // 1 s: the high-pass has settled long before the end
        let (mut sin_in, mut sin_out) = (0.0, 0.0);
        for i in 0..n {
            let x = (w * i as f64).sin();
            let y = k.process(x);
            if i >= n / 2 {
                sin_in += x * x;
                sin_out += y * y;
            }
        }
        10.0 * (sin_out / sin_in).log10()
    }

    /// The response is the same curve at any rate, well below Nyquist: about +0.69 dB at
    /// 1 kHz (which the −0.691 in the loudness formula cancels), +4 dB in the treble, and
    /// rolled off in the low bass.
    #[test]
    fn response_is_rate_independent() {
        for rate in [44_100, 48_000, 96_000] {
            let at_1k = gain_db(rate, 1_000.0);
            assert!(
                (at_1k - 0.691).abs() < 0.02,
                "{rate} Hz: 1 kHz gain {at_1k}"
            );
            let at_10k = gain_db(rate, 10_000.0);
            assert!(
                (at_10k - 4.0).abs() < 0.1,
                "{rate} Hz: 10 kHz gain {at_10k}"
            );
            let at_20 = gain_db(rate, 20.0);
            assert!(at_20 < -10.0, "{rate} Hz: 20 Hz gain {at_20}");
        }
    }
}
