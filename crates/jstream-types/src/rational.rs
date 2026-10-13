//! Exact rational numbers for frame rates and time bases (e.g. 30000/1001).

use std::fmt;

/// A reduced rational number with a positive denominator.
///
/// Used for frame rates and time bases, where floating point would accumulate drift
/// (NTSC 29.97 fps is exactly `30000/1001`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rational {
    num: i64,
    den: u32,
}

impl Rational {
    /// Creates `num / den` in lowest terms. Returns `None` when `den` is zero.
    pub fn new(num: i64, den: u32) -> Option<Self> {
        if den == 0 {
            return None;
        }
        let g = gcd(num.unsigned_abs(), u64::from(den));
        // `g` divides `den`, which fits in u32, so the quotient does too; `g >= 1` here.
        let den = u32::try_from(u64::from(den) / g).ok()?;
        let num = num / i64::try_from(g).ok()?;
        Some(Self { num, den })
    }

    /// The numerator, in lowest terms.
    pub fn num(self) -> i64 {
        self.num
    }

    /// The denominator, in lowest terms; always greater than zero.
    pub fn den(self) -> u32 {
        self.den
    }

    /// The value as `f64`, for display and non-exact math only.
    pub fn as_f64(self) -> f64 {
        self.num as f64 / f64::from(self.den)
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

#[cfg(test)]
mod tests {
    use super::Rational;

    #[test]
    fn zero_denominator_is_rejected() {
        assert_eq!(Rational::new(1, 0), None);
    }

    #[test]
    fn reduces_to_lowest_terms() {
        let r = Rational::new(60_000, 2_002).unwrap();
        assert_eq!((r.num(), r.den()), (30_000, 1_001));
    }

    #[test]
    fn negative_and_zero_numerators() {
        assert_eq!(Rational::new(-4, 6).unwrap().to_string(), "-2/3");
        assert_eq!(Rational::new(0, 7).unwrap().to_string(), "0/1");
    }

    #[test]
    fn ntsc_frame_rate_as_f64() {
        let fps = Rational::new(30_000, 1_001).unwrap().as_f64();
        assert!((fps - 29.970_029_97).abs() < 1e-6);
    }

    #[test]
    fn extreme_numerator_does_not_overflow() {
        let r = Rational::new(i64::MIN, 2).unwrap();
        assert_eq!((r.num(), r.den()), (i64::MIN / 2, 1));
    }
}
