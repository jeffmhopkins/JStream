//! Time bases and timestamps.
//!
//! Every timestamp is an integer tick count in an explicit time base, so arithmetic is
//! exact and conversions round only where the caller says how. The engine's media clock
//! (docs/adr/0014-media-clock.md) counts in [`Timebase::NANOS`].

use std::cmp::Ordering;
use std::fmt;

use crate::MediaError;

/// The length of one tick, in seconds, as a reduced fraction `num / den`.
///
/// Both parts are non-zero, so a time base is always positive. Common values have
/// constants: [`Timebase::NANOS`] (the media clock), [`Timebase::MPEG_90K`] (H.264 and
/// MP4 video) and [`Timebase::MILLIS`] (FLV). An audio stream usually ticks once per
/// sample frame: [`Timebase::per_second`]`(48_000)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Timebase {
    num: u32,
    den: u32,
}

impl Timebase {
    /// One nanosecond: the media clock's time base.
    pub const NANOS: Self = Self {
        num: 1,
        den: 1_000_000_000,
    };
    /// One microsecond.
    pub const MICROS: Self = Self {
        num: 1,
        den: 1_000_000,
    };
    /// One millisecond, as used by FLV and RTMP timestamps.
    pub const MILLIS: Self = Self { num: 1, den: 1_000 };
    /// 1/90000 s, the MPEG system clock used for H.264 and MP4 video tracks.
    pub const MPEG_90K: Self = Self {
        num: 1,
        den: 90_000,
    };

    /// Creates the time base `num / den` seconds per tick, in lowest terms.
    ///
    /// # Errors
    /// [`MediaError::InvalidTimebase`] when either part is zero.
    pub fn new(num: u32, den: u32) -> Result<Self, MediaError> {
        if num == 0 || den == 0 {
            return Err(MediaError::InvalidTimebase { num, den });
        }
        let g = gcd(num, den);
        Ok(Self {
            num: num / g,
            den: den / g,
        })
    }

    /// One tick per `1 / hz` seconds, e.g. the sample rate of an audio stream.
    ///
    /// # Errors
    /// [`MediaError::InvalidTimebase`] when `hz` is zero.
    pub fn per_second(hz: u32) -> Result<Self, MediaError> {
        Self::new(1, hz)
    }

    /// The numerator, in lowest terms; always greater than zero.
    pub fn num(self) -> u32 {
        self.num
    }

    /// The denominator, in lowest terms; always greater than zero.
    pub fn den(self) -> u32 {
        self.den
    }
}

impl fmt::Display for Timebase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

/// How [`MediaTime::rescale`] rounds a result that falls between two ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rounding {
    /// Toward negative infinity.
    Down,
    /// Toward positive infinity.
    Up,
    /// To the nearest tick; exact halves round away from zero.
    Nearest,
}

/// A point in time (or a duration): a signed tick count in a [`Timebase`].
///
/// Comparison is by value across time bases: one tick at 1/1000 s equals 90 ticks at
/// 1/90000 s. Arithmetic needs both operands in the same time base; convert with
/// [`MediaTime::rescale`] first, choosing how to round.
///
/// ```
/// use jstream_media::{MediaTime, Rounding, Timebase};
///
/// let frame = MediaTime::new(1, Timebase::new(1001, 30_000)?); // one NTSC frame
/// let in_90k = frame.rescale(Timebase::MPEG_90K, Rounding::Nearest)?;
/// assert_eq!(in_90k.ticks(), 3003);
/// assert_eq!(frame, in_90k);
/// # Ok::<(), jstream_media::MediaError>(())
/// ```
#[derive(Clone, Copy, Debug)]
pub struct MediaTime {
    ticks: i64,
    timebase: Timebase,
}

impl MediaTime {
    /// `ticks` ticks of `timebase`.
    pub fn new(ticks: i64, timebase: Timebase) -> Self {
        Self { ticks, timebase }
    }

    /// Zero in `timebase`.
    pub fn zero(timebase: Timebase) -> Self {
        Self::new(0, timebase)
    }

    /// The tick count.
    pub fn ticks(self) -> i64 {
        self.ticks
    }

    /// The time base the ticks are counted in.
    pub fn timebase(self) -> Timebase {
        self.timebase
    }

    /// The same instant in `to`, rounded as `rounding` says when it falls between ticks.
    ///
    /// # Errors
    /// [`MediaError::Overflow`] when the result doesn't fit in `i64` ticks.
    pub fn rescale(self, to: Timebase, rounding: Rounding) -> Result<Self, MediaError> {
        if to == self.timebase {
            return Ok(self);
        }
        // ticks * (num / den) seconds, divided by (to.num / to.den) seconds per tick.
        // |ticks| <= 2^63 and each u32 factor is < 2^32, so the products fit in i128.
        let n = i128::from(self.ticks) * i128::from(self.timebase.num) * i128::from(to.den);
        let d = i128::from(self.timebase.den) * i128::from(to.num);
        let q = div_round(n, d, rounding);
        let ticks = i64::try_from(q).map_err(|_| MediaError::Overflow { what: "rescale" })?;
        Ok(Self::new(ticks, to))
    }

    /// `self + rhs`, both in the same time base.
    ///
    /// # Errors
    /// [`MediaError::TimebaseMismatch`] when the time bases differ, and
    /// [`MediaError::Overflow`] when the sum doesn't fit in `i64` ticks.
    pub fn checked_add(self, rhs: Self) -> Result<Self, MediaError> {
        let tb = self.same_timebase(rhs)?;
        let ticks = self
            .ticks
            .checked_add(rhs.ticks)
            .ok_or(MediaError::Overflow { what: "time sum" })?;
        Ok(Self::new(ticks, tb))
    }

    /// `self - rhs`, both in the same time base.
    ///
    /// # Errors
    /// [`MediaError::TimebaseMismatch`] when the time bases differ, and
    /// [`MediaError::Overflow`] when the difference doesn't fit in `i64` ticks.
    pub fn checked_sub(self, rhs: Self) -> Result<Self, MediaError> {
        let tb = self.same_timebase(rhs)?;
        let ticks = self
            .ticks
            .checked_sub(rhs.ticks)
            .ok_or(MediaError::Overflow {
                what: "time difference",
            })?;
        Ok(Self::new(ticks, tb))
    }

    /// The value in seconds, for display and non-exact math only.
    pub fn as_secs_f64(self) -> f64 {
        self.ticks as f64 * f64::from(self.timebase.num) / f64::from(self.timebase.den)
    }

    fn same_timebase(self, rhs: Self) -> Result<Timebase, MediaError> {
        if self.timebase == rhs.timebase {
            Ok(self.timebase)
        } else {
            Err(MediaError::TimebaseMismatch {
                left: self.timebase,
                right: rhs.timebase,
            })
        }
    }

    /// Both values scaled to the common unit `1 / (den_a * den_b)` seconds, exactly.
    fn cross(self, other: Self) -> (i128, i128) {
        let a =
            i128::from(self.ticks) * i128::from(self.timebase.num) * i128::from(other.timebase.den);
        let b = i128::from(other.ticks)
            * i128::from(other.timebase.num)
            * i128::from(self.timebase.den);
        (a, b)
    }
}

impl PartialEq for MediaTime {
    fn eq(&self, other: &Self) -> bool {
        let (a, b) = self.cross(*other);
        a == b
    }
}

impl Eq for MediaTime {}

impl PartialOrd for MediaTime {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MediaTime {
    fn cmp(&self, other: &Self) -> Ordering {
        let (a, b) = self.cross(*other);
        a.cmp(&b)
    }
}

impl fmt::Display for MediaTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.ticks, self.timebase)
    }
}

/// `n / d` rounded as asked. `d` is positive.
fn div_round(n: i128, d: i128, rounding: Rounding) -> i128 {
    match rounding {
        Rounding::Down => n.div_euclid(d),
        // `n` is far from i128::MIN (see `rescale`), so negating it cannot overflow.
        Rounding::Up => -(-n).div_euclid(d),
        Rounding::Nearest => {
            let mag = (2 * n.unsigned_abs() + d.unsigned_abs()) / (2 * d.unsigned_abs());
            // `mag` <= |n|, which came from an i128, so it fits back into one.
            let mag = i128::try_from(mag).unwrap_or(i128::MAX);
            if n < 0 { -mag } else { mag }
        }
    }
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::{MediaTime, Rounding, Timebase};
    use crate::MediaError;

    fn tb(num: u32, den: u32) -> Timebase {
        Timebase::new(num, den).unwrap()
    }

    #[test]
    fn timebase_rejects_zero_parts() {
        assert_eq!(
            Timebase::new(0, 1),
            Err(MediaError::InvalidTimebase { num: 0, den: 1 })
        );
        assert!(Timebase::new(1, 0).is_err());
        assert!(Timebase::per_second(0).is_err());
    }

    #[test]
    fn timebase_reduces_to_lowest_terms() {
        let t = tb(2002, 60_000);
        assert_eq!((t.num(), t.den()), (1001, 30_000));
        assert_eq!(tb(1, 1_000), Timebase::MILLIS);
        assert_eq!(Timebase::per_second(90_000).unwrap(), Timebase::MPEG_90K);
    }

    #[test]
    fn constants_are_reduced() {
        for t in [
            Timebase::NANOS,
            Timebase::MICROS,
            Timebase::MILLIS,
            Timebase::MPEG_90K,
        ] {
            assert_eq!(tb(t.num(), t.den()), t);
        }
    }

    #[test]
    fn rescale_exact() {
        let ms = MediaTime::new(1_500, Timebase::MILLIS);
        let ns = ms.rescale(Timebase::NANOS, Rounding::Down).unwrap();
        assert_eq!(ns.ticks(), 1_500_000_000);
        assert_eq!(ns.timebase(), Timebase::NANOS);
        let back = ns.rescale(Timebase::MILLIS, Rounding::Nearest).unwrap();
        assert_eq!(back.ticks(), 1_500);
    }

    #[test]
    fn rescale_to_same_timebase_is_identity() {
        let t = MediaTime::new(-7, Timebase::MPEG_90K);
        let r = t.rescale(Timebase::MPEG_90K, Rounding::Up).unwrap();
        assert_eq!(r.ticks(), -7);
    }

    #[test]
    fn rescale_rounding_modes_positive() {
        // 5 ticks at 1/90000 s = 0.0555... ms
        let t = MediaTime::new(5, Timebase::MPEG_90K);
        let ms = |r| t.rescale(Timebase::MILLIS, r).unwrap().ticks();
        assert_eq!(ms(Rounding::Down), 0);
        assert_eq!(ms(Rounding::Up), 1);
        assert_eq!(ms(Rounding::Nearest), 0);
        // 60 ticks at 1/90000 s = 0.666... ms
        let t = MediaTime::new(60, Timebase::MPEG_90K);
        assert_eq!(
            t.rescale(Timebase::MILLIS, Rounding::Nearest)
                .unwrap()
                .ticks(),
            1
        );
    }

    #[test]
    fn rescale_rounding_modes_negative() {
        // -1.5 ms in whole milliseconds from microseconds
        let t = MediaTime::new(-1_500, Timebase::MICROS);
        let ms = |r| t.rescale(Timebase::MILLIS, r).unwrap().ticks();
        assert_eq!(ms(Rounding::Down), -2);
        assert_eq!(ms(Rounding::Up), -1);
        assert_eq!(ms(Rounding::Nearest), -2, "halves round away from zero");
        let t = MediaTime::new(1_500, Timebase::MICROS);
        assert_eq!(
            t.rescale(Timebase::MILLIS, Rounding::Nearest)
                .unwrap()
                .ticks(),
            2
        );
    }

    #[test]
    fn rescale_ntsc_frames_to_90k() {
        let frame = tb(1001, 30_000);
        let t = MediaTime::new(30_000, frame); // 1001 s
        let r = t.rescale(Timebase::MPEG_90K, Rounding::Down).unwrap();
        assert_eq!(r.ticks(), 1001 * 90_000);
    }

    #[test]
    fn rescale_overflow_is_an_error() {
        let t = MediaTime::new(i64::MAX, Timebase::MILLIS);
        assert_eq!(
            t.rescale(Timebase::NANOS, Rounding::Down),
            Err(MediaError::Overflow { what: "rescale" })
        );
        let t = MediaTime::new(i64::MIN, tb(u32::MAX, 1));
        assert!(t.rescale(tb(1, u32::MAX), Rounding::Nearest).is_err());
    }

    #[test]
    fn rescale_extremes_in_range_do_not_panic() {
        let t = MediaTime::new(i64::MIN, Timebase::NANOS);
        let s = t.rescale(tb(1, 1), Rounding::Up).unwrap();
        assert_eq!(s.ticks(), i64::MIN / 1_000_000_000);
        let s = t.rescale(tb(1, 1), Rounding::Down).unwrap();
        assert_eq!(s.ticks(), i64::MIN / 1_000_000_000 - 1);
    }

    #[test]
    fn add_and_sub_in_one_timebase() {
        let a = MediaTime::new(10, Timebase::MILLIS);
        let b = MediaTime::new(3, Timebase::MILLIS);
        assert_eq!(a.checked_add(b).unwrap().ticks(), 13);
        assert_eq!(b.checked_sub(a).unwrap().ticks(), -7);
    }

    #[test]
    fn arithmetic_overflow_is_an_error() {
        let max = MediaTime::new(i64::MAX, Timebase::NANOS);
        let one = MediaTime::new(1, Timebase::NANOS);
        assert_eq!(
            max.checked_add(one),
            Err(MediaError::Overflow { what: "time sum" })
        );
        let min = MediaTime::new(i64::MIN, Timebase::NANOS);
        assert!(min.checked_sub(one).is_err());
    }

    #[test]
    fn arithmetic_across_timebases_is_refused() {
        let a = MediaTime::new(1, Timebase::MILLIS);
        let b = MediaTime::new(1, Timebase::NANOS);
        assert_eq!(
            a.checked_add(b),
            Err(MediaError::TimebaseMismatch {
                left: Timebase::MILLIS,
                right: Timebase::NANOS
            })
        );
        assert!(a.checked_sub(b).is_err());
    }

    #[test]
    fn comparison_is_by_value_across_timebases() {
        let ms = MediaTime::new(1, Timebase::MILLIS);
        let k90 = MediaTime::new(90, Timebase::MPEG_90K);
        assert_eq!(ms, k90);
        assert!(MediaTime::new(89, Timebase::MPEG_90K) < ms);
        assert!(MediaTime::new(-1, Timebase::NANOS) < MediaTime::zero(Timebase::MILLIS));
        assert_eq!(
            MediaTime::new(i64::MAX, tb(u32::MAX, 1))
                .cmp(&MediaTime::new(i64::MAX, tb(1, u32::MAX))),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn display_and_seconds() {
        let t = MediaTime::new(3003, Timebase::MPEG_90K);
        assert_eq!(t.to_string(), "3003@1/90000");
        assert!((t.as_secs_f64() - 0.033_366_666).abs() < 1e-6);
    }
}
