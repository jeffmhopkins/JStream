//! The analysis filterbank: the MDCT of ISO/IEC 13818-7 §15 (14496-3 §4.6.11), its sine
//! and Kaiser-Bessel-derived windows, and the block switching window shapes.
//!
//! The MDCT is computed as a DCT-IV of the folded input, and the DCT-IV through a complex
//! FFT of a quarter of the window length, the standard fast algorithm (P. Duhamel, Y.
//! Mahieux and J. P. Petit, "A fast algorithm for the implementation of filter banks based
//! on time domain aliasing cancellation", ICASSP 1991). The unit tests compare it with the
//! defining sum.

use std::f64::consts::PI;

/// A complex number of the FFT.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Cpx {
    re: f32,
    im: f32,
}

impl Cpx {
    fn mul(self, o: Self) -> Self {
        Self {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
}

/// An in-place radix-2 complex FFT of one power-of-two size, with its twiddles and
/// bit-reversal permutation computed once.
struct Fft {
    n: usize,
    /// `exp(-2πik/n)` for `k < n/2`.
    twiddles: Vec<Cpx>,
    /// The bit-reversed index of each index.
    reversed: Vec<u32>,
}

impl Fft {
    fn new(n: usize) -> Self {
        debug_assert!(n.is_power_of_two() && n >= 2);
        let bits = n.trailing_zeros();
        let twiddles = (0..n / 2)
            .map(|k| {
                let a = -2.0 * PI * k as f64 / n as f64;
                Cpx {
                    re: a.cos() as f32,
                    im: a.sin() as f32,
                }
            })
            .collect();
        let reversed = (0..n as u32)
            .map(|i| i.reverse_bits() >> (32 - bits))
            .collect();
        Self {
            n,
            twiddles,
            reversed,
        }
    }

    /// Transforms `data` (length `n`) in place: `X[k] = Σ x[j] exp(-2πijk/n)`.
    fn run(&self, data: &mut [Cpx]) {
        debug_assert_eq!(data.len(), self.n);
        for (i, &r) in self.reversed.iter().enumerate() {
            let r = r as usize;
            if r > i {
                data.swap(i, r);
            }
        }
        let mut len = 2;
        while len <= self.n {
            let half = len / 2;
            let step = self.n / len;
            for start in (0..self.n).step_by(len) {
                for j in 0..half {
                    let t = data[start + j + half].mul(self.twiddles[j * step]);
                    let u = data[start + j];
                    data[start + j] = Cpx {
                        re: u.re + t.re,
                        im: u.im + t.im,
                    };
                    data[start + j + half] = Cpx {
                        re: u.re - t.re,
                        im: u.im - t.im,
                    };
                }
            }
            len *= 2;
        }
    }
}

/// The MDCT of one window length `2m`, producing `m` coefficients, scaled as ISO/IEC
/// 13818-7 Annex B's encoder filterbank so that the standard's IMDCT (with its `2/N`)
/// reconstructs the input: `X[k] = 2 Σ z[n] cos(2π/N (n + n0)(k + 1/2))`, `N = 2m`,
/// `n0 = (N/2 + 1)/2`.
pub(crate) struct Mdct {
    m: usize,
    fft: Fft,
    /// `exp(-iπ(n + 1/8)/m)` for `n < m/2`: the DCT-IV's pre- and post-twiddle.
    twiddle: Vec<Cpx>,
    folded: Vec<f32>,
    work: Vec<Cpx>,
}

impl Mdct {
    /// An MDCT with `m` outputs (`m` a power of two, at least 8).
    pub(crate) fn new(m: usize) -> Self {
        debug_assert!(m.is_power_of_two() && m >= 8);
        let twiddle = (0..m / 2)
            .map(|n| {
                let a = -PI * (n as f64 + 0.125) / m as f64;
                Cpx {
                    re: a.cos() as f32,
                    im: a.sin() as f32,
                }
            })
            .collect();
        Self {
            m,
            fft: Fft::new(m / 2),
            twiddle,
            folded: vec![0.0; m],
            work: vec![Cpx::default(); m / 2],
        }
    }

    /// Transforms `input` (`2m` windowed samples) into `out` (`m` coefficients).
    pub(crate) fn forward(&mut self, input: &[f32], out: &mut [f32]) {
        let m = self.m;
        debug_assert_eq!(input.len(), 2 * m);
        debug_assert_eq!(out.len(), m);
        // Fold the 2m inputs into the m inputs of a DCT-IV.
        let h = m / 2;
        for i in 0..h {
            self.folded[i] = -input[3 * h - 1 - i] - input[3 * h + i];
        }
        for i in h..m {
            self.folded[i] = input[i - h] - input[3 * h - 1 - i];
        }
        // DCT-IV through an m/2-point complex FFT.
        for (n, w) in self.work.iter_mut().enumerate() {
            let z = Cpx {
                re: self.folded[2 * n],
                im: self.folded[m - 1 - 2 * n],
            };
            *w = z.mul(self.twiddle[n]);
        }
        self.fft.run(&mut self.work);
        for (k, w) in self.work.iter().enumerate() {
            let y = w.mul(self.twiddle[k]);
            out[2 * k] = 2.0 * y.re;
            out[m - 1 - 2 * k] = -2.0 * y.im;
        }
    }
}

/// The window shape of a frame (`window_shape`): it shapes the right half of the frame's
/// window and the left half of the next frame's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WindowShape {
    /// The sine window (`window_shape` 0).
    #[default]
    Sine,
    /// The Kaiser-Bessel-derived window (`window_shape` 1), alpha 4 for long windows and
    /// 6 for short ones.
    Kbd,
}

impl WindowShape {
    /// The bit `ics_info` writes.
    pub(crate) fn bit(self) -> bool {
        self == Self::Kbd
    }
}

/// The modified Bessel function of the first kind, order zero, by its power series.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..60 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// The rising half (`n/2` values) of the sine window of length `n`.
fn sine_rise(n: usize) -> Vec<f32> {
    (0..n / 2)
        .map(|i| ((PI / n as f64) * (i as f64 + 0.5)).sin() as f32)
        .collect()
}

/// The rising half (`n/2` values) of the KBD window of length `n` with `alpha`
/// (ISO/IEC 13818-7 §15.2.1).
fn kbd_rise(n: usize, alpha: f64) -> Vec<f32> {
    let half = n / 2;
    let kernel: Vec<f64> = (0..=half)
        .map(|j| {
            let r = (j as f64 - n as f64 / 4.0) / (n as f64 / 4.0);
            bessel_i0(PI * alpha * (1.0 - r * r).max(0.0).sqrt())
        })
        .collect();
    let total: f64 = kernel.iter().sum();
    let mut acc = 0.0;
    kernel[..half]
        .iter()
        .map(|k| {
            acc += k;
            (acc / total).sqrt() as f32
        })
        .collect()
}

/// The rising halves of every window the encoder uses.
pub(crate) struct Windows {
    long: [Vec<f32>; 2],
    short: [Vec<f32>; 2],
}

/// Long and short window lengths.
pub(crate) const LONG: usize = 2048;
/// The short window length.
pub(crate) const SHORT: usize = 256;

impl Windows {
    pub(crate) fn new() -> Self {
        Self {
            long: [sine_rise(LONG), kbd_rise(LONG, 4.0)],
            short: [sine_rise(SHORT), kbd_rise(SHORT, 6.0)],
        }
    }

    fn idx(shape: WindowShape) -> usize {
        usize::from(shape.bit())
    }

    /// The rising half of the long window of `shape` (1024 values).
    pub(crate) fn long(&self, shape: WindowShape) -> &[f32] {
        &self.long[Self::idx(shape)]
    }

    /// The rising half of the short window of `shape` (128 values).
    pub(crate) fn short(&self, shape: WindowShape) -> &[f32] {
        &self.short[Self::idx(shape)]
    }
}

#[cfg(test)]
mod tests {
    use super::{Cpx, Fft, Mdct, WindowShape, Windows, bessel_i0, kbd_rise, sine_rise};
    use std::f64::consts::PI;

    fn lcg(seed: &mut u64) -> f32 {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((*seed >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }

    /// The MDCT's defining sum, in f64.
    fn direct(input: &[f32]) -> Vec<f64> {
        let n = input.len();
        let n0 = (n as f64 / 2.0 + 1.0) / 2.0;
        (0..n / 2)
            .map(|k| {
                2.0 * input
                    .iter()
                    .enumerate()
                    .map(|(i, &x)| {
                        f64::from(x)
                            * (2.0 * PI / n as f64 * (i as f64 + n0) * (k as f64 + 0.5)).cos()
                    })
                    .sum::<f64>()
            })
            .collect()
    }

    #[test]
    fn fft_matches_the_dft() {
        let mut seed = 7;
        let n = 64;
        let x: Vec<Cpx> = (0..n)
            .map(|_| Cpx {
                re: lcg(&mut seed),
                im: lcg(&mut seed),
            })
            .collect();
        let mut y = x.clone();
        Fft::new(n).run(&mut y);
        for (k, got) in y.iter().enumerate() {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (j, v) in x.iter().enumerate() {
                let a = -2.0 * PI * (j * k) as f64 / n as f64;
                re += f64::from(v.re) * a.cos() - f64::from(v.im) * a.sin();
                im += f64::from(v.re) * a.sin() + f64::from(v.im) * a.cos();
            }
            assert!((f64::from(got.re) - re).abs() < 1e-4, "re {k}");
            assert!((f64::from(got.im) - im).abs() < 1e-4, "im {k}");
        }
    }

    #[test]
    fn mdct_matches_the_defining_sum() {
        for m in [128, 1024] {
            let mut seed = m as u64;
            let input: Vec<f32> = (0..2 * m).map(|_| lcg(&mut seed)).collect();
            let mut out = vec![0.0; m];
            Mdct::new(m).forward(&input, &mut out);
            let want = direct(&input);
            let scale = want.iter().fold(0.0f64, |a, b| a.max(b.abs()));
            for (k, (&g, &w)) in out.iter().zip(&want).enumerate() {
                assert!(
                    (f64::from(g) - w).abs() < 1e-4 * scale,
                    "m {m} k {k}: {g} vs {w}"
                );
            }
        }
    }

    /// Princen-Bradley: each window's rising half squared plus its mirror squared is one,
    /// which is what makes the overlap-add reconstruct.
    #[test]
    fn windows_are_power_complementary() {
        let w = Windows::new();
        for shape in [WindowShape::Sine, WindowShape::Kbd] {
            for rise in [w.long(shape), w.short(shape)] {
                let n = rise.len();
                for i in 0..n {
                    let s = rise[i] * rise[i] + rise[n - 1 - i] * rise[n - 1 - i];
                    assert!((s - 1.0).abs() < 1e-5, "{shape:?} {n} {i}: {s}");
                }
            }
        }
        assert_eq!(sine_rise(256).len(), 128);
        assert_eq!(kbd_rise(2048, 4.0).len(), 1024);
    }

    #[test]
    fn bessel_i0_known_values() {
        assert!((bessel_i0(0.0) - 1.0).abs() < 1e-15);
        assert!((bessel_i0(1.0) - 1.266_065_877_752_008_4).abs() < 1e-12);
        assert!((bessel_i0(10.0) - 2_815.716_628_466_254).abs() < 1e-6);
    }
}
