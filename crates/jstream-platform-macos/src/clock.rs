//! The `mach_absolute_time` media clock (docs/adr/0014-media-clock.md, ADR-0063 §5).
//!
//! Every timestamp this crate produces is in 100 ns ticks on the `mach_absolute_time` time
//! line, the unit the Windows and Linux backends use. Core Media's host clock,
//! ScreenCaptureKit, AVFoundation and Core Audio's host times are on that time line, in
//! Mach ticks whose length `mach_timebase_info` gives (1 ns on Intel, 125/3 ns on Apple
//! Silicon), so mapping one of their timestamps is [`hns_from_mach`].

use jstream_platform::media::Timebase;

/// 100 ns ticks per second.
const HNS_PER_SEC: u32 = 10_000_000;

/// Nanoseconds per 100 ns tick.
const NS_PER_HNS: u128 = 100;

/// The time base of every timestamp this crate's backends produce: 100 ns ticks.
pub fn timebase() -> Timebase {
    Timebase::new(1, HNS_PER_SEC).expect("invariant: 1 / 10^7 has no zero part")
}

/// Converts a Mach absolute time (`mach_absolute_time`, or a Core Media or Core Audio host
/// time) to 100 ns ticks, rounding toward zero. `numer` and `denom` are the
/// `mach_timebase_info` ratio: one Mach tick is `numer / denom` nanoseconds.
///
/// `None` for a zero `denom` or a result outside `i64`.
pub fn hns_from_mach(ticks: u64, numer: u32, denom: u32) -> Option<i64> {
    if denom == 0 {
        return None;
    }
    let v = u128::from(ticks) * u128::from(numer) / (u128::from(denom) * NS_PER_HNS);
    i64::try_from(v).ok()
}

#[cfg(target_os = "macos")]
pub use mach::MachClock;

#[cfg(target_os = "macos")]
mod mach {
    use std::sync::atomic::{AtomicI64, Ordering};

    use jstream_platform::media::{MediaTime, Timebase};
    use jstream_platform::{MediaClock, PlatformError};
    use mach2::mach_time::{mach_absolute_time, mach_timebase_info};

    use super::{hns_from_mach, timebase};

    /// `KERN_SUCCESS` from <mach/kern_return.h>.
    const KERN_SUCCESS: i32 = 0;

    /// The media clock on macOS: `mach_absolute_time` in 100 ns ticks ([`timebase`]). It
    /// is on the same time line as Core Media's host clock, so ScreenCaptureKit,
    /// AVFoundation and Core Audio timestamps map onto it with [`hns_from_mach`].
    ///
    /// Like the Mach clock, it doesn't advance while the Mac sleeps.
    #[derive(Debug)]
    pub struct MachClock {
        timebase: Timebase,
        /// The `mach_timebase_info` ratio, read once: it is fixed for the boot.
        numer: u32,
        denom: u32,
        /// The largest value returned so far, so `now` never decreases.
        last: AtomicI64,
    }

    impl MachClock {
        /// Reads the Mach time base and checks that the clock converts.
        ///
        /// # Errors
        /// [`PlatformError::Os`] if `mach_timebase_info` fails or reports a zero
        /// denominator, which macOS never does.
        pub fn new() -> Result<Self, PlatformError> {
            let mut info = mach_timebase_info { numer: 0, denom: 0 };
            // SAFETY: `info` is a valid, writable `mach_timebase_info` for the duration of
            // the call, which only fills it in. Declared by `mach2` as
            // `kern_return_t mach_timebase_info(mach_timebase_info_t info)`.
            let rc = unsafe { mach_timebase_info(&mut info) };
            if rc != KERN_SUCCESS || info.denom == 0 {
                return Err(PlatformError::Os {
                    call: "mach_timebase_info",
                    code: u32::try_from(rc).ok(),
                    message: format!(
                        "the Mach time base can't be read ({}/{})",
                        info.numer, info.denom
                    ),
                });
            }
            let first = hns_from_mach(read(), info.numer, info.denom).unwrap_or(0);
            Ok(Self {
                timebase: timebase(),
                numer: info.numer,
                denom: info.denom,
                last: AtomicI64::new(first),
            })
        }

        /// Maps a Mach absolute time, such as a `CMSampleBuffer`'s presentation time on
        /// the host clock or an `AudioTimeStamp`'s `mHostTime`, onto this clock.
        pub fn from_host_time(&self, ticks: u64) -> MediaTime {
            let hns = hns_from_mach(ticks, self.numer, self.denom).unwrap_or(i64::MAX);
            MediaTime::new(hns, self.timebase)
        }
    }

    /// One reading in Mach ticks.
    fn read() -> u64 {
        // SAFETY: `uint64_t mach_absolute_time(void)` takes no arguments, can't fail, and
        // doesn't allocate, lock or block (it reads the commpage), so it is safe on the
        // audio thread.
        unsafe { mach_absolute_time() }
    }

    impl MediaClock for MachClock {
        fn timebase(&self) -> Timebase {
            self.timebase
        }

        fn now(&self) -> MediaTime {
            let reading = hns_from_mach(read(), self.numer, self.denom).unwrap_or(i64::MIN);
            let previous = self.last.fetch_max(reading, Ordering::AcqRel);
            MediaTime::new(previous.max(reading), self.timebase)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{hns_from_mach, timebase};

    #[test]
    fn timebase_is_100ns() {
        assert_eq!((timebase().num(), timebase().den()), (1, 10_000_000));
    }

    #[test]
    fn intel_ticks_are_nanoseconds() {
        assert_eq!(hns_from_mach(0, 1, 1), Some(0));
        assert_eq!(hns_from_mach(1_000_000_000, 1, 1), Some(10_000_000));
        assert_eq!(hns_from_mach(199, 1, 1), Some(1));
    }

    #[test]
    fn apple_silicon_ticks_are_125_thirds_of_a_nanosecond() {
        // 24 MHz: 24,000,000 ticks are one second.
        assert_eq!(hns_from_mach(24_000_000, 125, 3), Some(10_000_000));
        // 12 ticks are 500 ns, 5 hns.
        assert_eq!(hns_from_mach(12, 125, 3), Some(5));
        assert_eq!(hns_from_mach(2, 125, 3), Some(0));
    }

    #[test]
    fn bad_inputs_are_refused() {
        assert_eq!(hns_from_mach(1, 1, 0), None);
        // u64::MAX ticks of 1 µs overflow i64 hns.
        assert_eq!(hns_from_mach(u64::MAX, 1000, 1), None);
        // ... but u64::MAX ticks of 125/3 ns don't: they are 7.7e18 hns.
        assert_eq!(
            hns_from_mach(u64::MAX, 125, 3),
            Some(7_686_143_364_045_646_506)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mach_clock_never_decreases() {
        use jstream_platform::MediaClock;

        let clock = super::MachClock::new().expect("the Mach time base is readable");
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
