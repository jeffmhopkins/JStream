//! The QPC media clock (docs/adr/0014-media-clock.md).
//!
//! Every timestamp this crate produces is in 100 ns ticks on the QueryPerformanceCounter
//! time line. WASAPI already reports packet times that way (`pu64QPCPosition`), so a raw
//! counter reading is converted with the same formula, [`qpc_to_hns`], and both share one
//! time line.

use jstream_platform::media::Timebase;

/// 100 ns ticks per second.
const HNS_PER_SEC: u32 = 10_000_000;

/// The time base of every timestamp this crate's backends produce: 100 ns ticks, the
/// unit WASAPI and Media Foundation use. It is the unit the `jstream-platform` README
/// proposes the engine fixes for every platform.
pub fn timebase() -> Timebase {
    Timebase::new(1, HNS_PER_SEC).expect("invariant: 1 / 10^7 has no zero part")
}

/// Converts a raw QPC reading to 100 ns ticks, rounding toward zero.
///
/// Computed in 128-bit arithmetic, so it cannot overflow for any reading and frequency.
/// `None` for a non-positive frequency or a result outside `i64`.
#[cfg(any(windows, test))]
pub(crate) fn qpc_to_hns(ticks: i64, freq: i64) -> Option<i64> {
    if freq <= 0 {
        return None;
    }
    let v = i128::from(ticks) * i128::from(HNS_PER_SEC) / i128::from(freq);
    i64::try_from(v).ok()
}

#[cfg(windows)]
pub use qpc::QpcClock;

#[cfg(windows)]
mod qpc {
    use std::sync::atomic::{AtomicI64, Ordering};

    use jstream_platform::media::{MediaTime, Timebase};
    use jstream_platform::{MediaClock, PlatformError};
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

    use super::{qpc_to_hns, timebase};
    use crate::com::OsResult;

    /// The media clock on Windows: QueryPerformanceCounter in 100 ns ticks
    /// ([`timebase`]). It is on the same time line as the packet times WASAPI reports.
    #[derive(Debug)]
    pub struct QpcClock {
        freq: i64,
        timebase: Timebase,
        /// The largest value returned so far, so `now` never decreases even if a reading
        /// fails.
        last: AtomicI64,
    }

    impl QpcClock {
        /// Reads the counter frequency, which is fixed at boot.
        ///
        /// # Errors
        /// [`PlatformError::Os`] if the frequency can't be read or isn't positive (neither
        /// happens on Windows XP or later).
        pub fn new() -> Result<Self, PlatformError> {
            let mut freq = 0i64;
            // SAFETY: `freq` is a valid out-pointer for the duration of the call.
            unsafe { QueryPerformanceFrequency(&mut freq) }.os("QueryPerformanceFrequency")?;
            if qpc_to_hns(0, freq).is_none() {
                return Err(PlatformError::Os {
                    call: "QueryPerformanceFrequency",
                    code: None,
                    message: format!("non-positive frequency {freq}"),
                });
            }
            Ok(Self {
                freq,
                timebase: timebase(),
                last: AtomicI64::new(0),
            })
        }
    }

    impl MediaClock for QpcClock {
        fn timebase(&self) -> Timebase {
            self.timebase
        }

        fn now(&self) -> MediaTime {
            let mut ticks = 0i64;
            // SAFETY: `ticks` is a valid out-pointer for the duration of the call.
            let read = unsafe { QueryPerformanceCounter(&mut ticks) }
                .ok()
                .and_then(|()| qpc_to_hns(ticks, self.freq));
            let now = match read {
                Some(v) => self.last.fetch_max(v, Ordering::Relaxed).max(v),
                None => self.last.load(Ordering::Relaxed),
            };
            MediaTime::new(now, self.timebase)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{qpc_to_hns, timebase};

    #[test]
    fn timebase_is_100ns() {
        assert_eq!((timebase().num(), timebase().den()), (1, 10_000_000));
    }

    #[test]
    fn converts_common_frequencies() {
        // 10 MHz, the usual QPC frequency on Windows 10 and later: ticks are already hns.
        assert_eq!(qpc_to_hns(123_456_789, 10_000_000), Some(123_456_789));
        // 3 MHz: one second, and rounding toward zero.
        assert_eq!(qpc_to_hns(3_000_000, 3_000_000), Some(10_000_000));
        assert_eq!(qpc_to_hns(1, 3_000_000), Some(3));
        assert_eq!(qpc_to_hns(-1, 3_000_000), Some(-3));
    }

    #[test]
    fn large_readings_do_not_overflow() {
        // A year of uptime at 24 MHz overflows `ticks * 10^7` in 64 bits.
        let year = 24_000_000i64 * 3600 * 24 * 365;
        assert_eq!(
            qpc_to_hns(year, 24_000_000),
            Some(10_000_000 * 3600 * 24 * 365)
        );
        assert_eq!(qpc_to_hns(i64::MAX, 1), None);
    }

    #[test]
    fn rejects_bad_frequencies() {
        assert_eq!(qpc_to_hns(1, 0), None);
        assert_eq!(qpc_to_hns(1, -5), None);
    }

    #[cfg(windows)]
    #[test]
    fn qpc_clock_never_decreases() {
        use jstream_platform::MediaClock;

        let clock = super::QpcClock::new().expect("QPC is always available on Windows");
        assert_eq!(clock.timebase(), timebase());
        let mut prev = clock.now();
        for _ in 0..10_000 {
            let t = clock.now();
            assert!(t.ticks() >= prev.ticks());
            prev = t;
        }
        assert!(prev.ticks() > 0);
    }
}
