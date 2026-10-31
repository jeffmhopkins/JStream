//! The process's CPU time on Windows, for `stats.tick`'s load (protocol 0.48).

/// Adds a FILETIME's two halves into its 100 ns count, as `GetProcessTimes` reports
/// kernel and user time.
#[cfg(any(windows, test))]
fn filetime_hns(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

#[cfg(windows)]
pub use os::WindowsProcessCpu;

#[cfg(windows)]
mod os {
    use std::time::Duration;

    use jstream_platform::ProcessCpu;
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    use super::filetime_hns;

    /// The engine process's CPU time, from `GetProcessTimes`: kernel plus user time
    /// across every thread.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct WindowsProcessCpu;

    impl ProcessCpu for WindowsProcessCpu {
        fn cpu_time(&self) -> Option<Duration> {
            let mut created = FILETIME::default();
            let mut exited = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            // SAFETY: `GetCurrentProcess` returns a pseudo-handle that is always valid
            // and needs no closing, and the four out-pointers are locals that outlive
            // the call.
            unsafe {
                GetProcessTimes(
                    GetCurrentProcess(),
                    &mut created,
                    &mut exited,
                    &mut kernel,
                    &mut user,
                )
            }
            .ok()?;
            let hns = filetime_hns(kernel.dwHighDateTime, kernel.dwLowDateTime)
                .saturating_add(filetime_hns(user.dwHighDateTime, user.dwLowDateTime));
            Some(Duration::from_nanos(hns.saturating_mul(100)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::filetime_hns;

    #[test]
    fn a_filetime_joins_its_halves() {
        assert_eq!(filetime_hns(0, 10_000_000), 10_000_000);
        assert_eq!(filetime_hns(1, 0), 1 << 32);
        assert_eq!(filetime_hns(1, 5), (1 << 32) + 5);
    }
}
