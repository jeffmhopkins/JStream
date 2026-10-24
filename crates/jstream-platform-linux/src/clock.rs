//! The `CLOCK_MONOTONIC` media clock (docs/adr/0014-media-clock.md, ADR-0060 §5).
//!
//! Every timestamp this crate produces is in 100 ns ticks on the `CLOCK_MONOTONIC` time
//! line, the unit the Windows backend uses and the `jstream-platform` README proposes for
//! every platform. PipeWire and V4L2 stamp their data on the same clock in nanoseconds,
//! so mapping one of their timestamps is a division by 100.

use jstream_platform::media::Timebase;

/// 100 ns ticks per second.
const HNS_PER_SEC: u32 = 10_000_000;

/// The time base of every timestamp this crate's backends produce: 100 ns ticks.
pub fn timebase() -> Timebase {
    Timebase::new(1, HNS_PER_SEC).expect("invariant: 1 / 10^7 has no zero part")
}

/// Converts a `timespec` reading (seconds and nanoseconds) to 100 ns ticks, rounding
/// toward zero.
///
/// `None` for nanoseconds outside `0..1_000_000_000` or a result outside `i64`.
pub fn hns_from_timespec(secs: i64, nanos: i64) -> Option<i64> {
    if !(0..1_000_000_000).contains(&nanos) {
        return None;
    }
    let v = i128::from(secs) * i128::from(HNS_PER_SEC) + i128::from(nanos / 100);
    i64::try_from(v).ok()
}

#[cfg(target_os = "linux")]
pub use monotonic::MonotonicClock;

#[cfg(target_os = "linux")]
mod monotonic {
    use std::sync::atomic::{AtomicI64, Ordering};

    use jstream_platform::media::{MediaTime, Timebase};
    use jstream_platform::{MediaClock, PlatformError};

    use super::{hns_from_timespec, timebase};

    /// The media clock on Linux: `CLOCK_MONOTONIC` in 100 ns ticks ([`timebase`]). It is
    /// on the same time line as the timestamps PipeWire and V4L2 report.
    #[derive(Debug)]
    pub struct MonotonicClock {
        timebase: Timebase,
        /// The largest value returned so far, so `now` never decreases even if a reading
        /// fails.
        last: AtomicI64,
    }

    impl MonotonicClock {
        /// Checks that the clock can be read.
        ///
        /// # Errors
        /// [`PlatformError::Os`] if `clock_gettime(CLOCK_MONOTONIC)` fails, which Linux
        /// never does for that clock.
        pub fn new() -> Result<Self, PlatformError> {
            let first = read().ok_or_else(|| PlatformError::Os {
                call: "clock_gettime",
                code: std::io::Error::last_os_error()
                    .raw_os_error()
                    .and_then(|e| u32::try_from(e).ok()),
                message: "CLOCK_MONOTONIC can't be read".into(),
            })?;
            Ok(Self {
                timebase: timebase(),
                last: AtomicI64::new(first),
            })
        }
    }

    /// One reading in 100 ns ticks, or `None` if the call fails.
    fn read() -> Option<i64> {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a valid, writable `timespec` for the duration of the call, and
        // CLOCK_MONOTONIC is a clock ID every Linux kernel accepts. The call doesn't
        // allocate, lock or block (vDSO), so it is safe on the audio thread.
        let rc = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        if rc != 0 {
            return None;
        }
        hns_from_timespec(ts.tv_sec, ts.tv_nsec)
    }

    impl MediaClock for MonotonicClock {
        fn timebase(&self) -> Timebase {
            self.timebase
        }

        fn now(&self) -> MediaTime {
            let reading = read().unwrap_or(i64::MIN);
            let previous = self.last.fetch_max(reading, Ordering::AcqRel);
            MediaTime::new(previous.max(reading), self.timebase)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{hns_from_timespec, timebase};

    #[test]
    fn timebase_is_100ns() {
        assert_eq!((timebase().num(), timebase().den()), (1, 10_000_000));
    }

    #[test]
    fn timespec_converts_to_hns() {
        assert_eq!(hns_from_timespec(0, 0), Some(0));
        assert_eq!(hns_from_timespec(1, 0), Some(10_000_000));
        assert_eq!(hns_from_timespec(2, 999_999_999), Some(29_999_999));
        assert_eq!(hns_from_timespec(0, 150), Some(1));
        assert_eq!(hns_from_timespec(0, 1_000_000_000), None);
        assert_eq!(hns_from_timespec(0, -1), None);
        assert_eq!(hns_from_timespec(i64::MAX, 0), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn monotonic_clock_never_decreases() {
        use jstream_platform::MediaClock;

        let clock = super::MonotonicClock::new().expect("CLOCK_MONOTONIC is readable");
        assert_eq!(clock.timebase(), timebase());
        let mut last = clock.now();
        for _ in 0..10_000 {
            let now = clock.now();
            assert!(now.ticks() >= last.ticks());
            last = now;
        }
        assert!(last.ticks() > 0);
    }
}
