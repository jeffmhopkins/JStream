//! The process's CPU time on macOS, for `stats.tick`'s load (protocol 0.48).

/// A `timeval`'s seconds and microseconds as nanoseconds, or `None` for a negative
/// part, which `getrusage` never reports.
#[cfg(any(target_os = "macos", test))]
fn timeval_nanos(secs: i64, micros: i64) -> Option<u64> {
    let secs = u64::try_from(secs).ok()?;
    let micros = u64::try_from(micros).ok()?;
    secs.checked_mul(1_000_000_000)?
        .checked_add(micros.checked_mul(1_000)?)
}

#[cfg(target_os = "macos")]
pub use os::MacosProcessCpu;

#[cfg(target_os = "macos")]
mod os {
    use std::mem::MaybeUninit;
    use std::time::Duration;

    use jstream_platform::ProcessCpu;

    use super::timeval_nanos;

    /// The engine process's CPU time, from `getrusage(RUSAGE_SELF)`: user plus system
    /// time across every thread.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct MacosProcessCpu;

    impl ProcessCpu for MacosProcessCpu {
        fn cpu_time(&self) -> Option<Duration> {
            let mut usage = MaybeUninit::<libc::rusage>::zeroed();
            // SAFETY: `usage` is a valid, writable `rusage` for the duration of the
            // call, and `RUSAGE_SELF` names this process.
            let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
            if status != 0 {
                return None;
            }
            // SAFETY: `getrusage` returned 0, so it filled `usage`; it started zeroed
            // in any case, and every bit pattern is a valid `rusage`.
            let usage = unsafe { usage.assume_init() };
            let user = timeval_nanos(usage.ru_utime.tv_sec, i64::from(usage.ru_utime.tv_usec))?;
            let system = timeval_nanos(usage.ru_stime.tv_sec, i64::from(usage.ru_stime.tv_usec))?;
            Some(Duration::from_nanos(user.saturating_add(system)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::timeval_nanos;

    #[test]
    fn a_timeval_becomes_nanoseconds() {
        assert_eq!(timeval_nanos(2, 500), Some(2_000_500_000));
        assert_eq!(timeval_nanos(0, 0), Some(0));
        assert_eq!(timeval_nanos(-1, 0), None);
        assert_eq!(timeval_nanos(0, -1), None);
    }
}
