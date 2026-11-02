//! The process's CPU time on Linux, for `stats.tick`'s load (protocol 0.48).

#[cfg(target_os = "linux")]
pub use os::LinuxProcessCpu;

#[cfg(target_os = "linux")]
mod os {
    use std::time::Duration;

    use jstream_platform::ProcessCpu;

    /// The engine process's CPU time, from `clock_gettime(CLOCK_PROCESS_CPUTIME_ID)`:
    /// user plus system time across every thread.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct LinuxProcessCpu;

    impl ProcessCpu for LinuxProcessCpu {
        fn cpu_time(&self) -> Option<Duration> {
            let mut ts = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: `ts` is a valid, writable `timespec` for the duration of the call,
            // and CLOCK_PROCESS_CPUTIME_ID names this process's CPU-time clock, which
            // every Linux kernel since 2.6.12 has.
            let rc = unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
            if rc != 0 {
                return None;
            }
            let secs = u64::try_from(ts.tv_sec).ok()?;
            let nanos = u32::try_from(ts.tv_nsec).ok()?;
            Some(Duration::new(secs, nanos))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::LinuxProcessCpu;
        use jstream_platform::ProcessCpu;
        use std::time::Instant;

        /// The clock reads, and grows while this thread spins.
        #[test]
        fn cpu_time_grows_while_the_process_works() {
            let before = LinuxProcessCpu.cpu_time().expect("a first reading");
            let start = Instant::now();
            let mut x = 0u64;
            while start.elapsed().as_millis() < 30 {
                x = std::hint::black_box(x.wrapping_add(1));
            }
            let after = LinuxProcessCpu.cpu_time().expect("a second reading");
            assert!(after > before, "{before:?} then {after:?}");
        }
    }
}
