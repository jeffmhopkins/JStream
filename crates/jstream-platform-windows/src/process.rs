//! The process's CPU time on Windows, for `stats.tick`'s load (protocol 0.48), and each
//! of its threads' CPU time, for `profile.get` (protocol 0.71, ADR-0103).

/// Adds a FILETIME's two halves into its 100 ns count, as `GetProcessTimes` reports
/// kernel and user time.
#[cfg(any(windows, test))]
fn filetime_hns(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

#[cfg(windows)]
pub use os::{WindowsProcessCpu, WindowsThreadCpu};

#[cfg(windows)]
mod os {
    use std::time::Duration;

    use jstream_platform::{ProcessCpu, ThreadCpu, ThreadTime};
    use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, HLOCAL, LocalFree};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, GetThreadDescription,
        GetThreadTimes, OpenThread, THREAD_QUERY_LIMITED_INFORMATION,
    };

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

    /// A handle this module opened, closed on drop.
    struct Owned(HANDLE);

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: the handle was opened by this module and is closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    /// Each thread of the engine's process and its CPU time: the threads from a Tool
    /// Help snapshot, each one's kernel plus user time from `GetThreadTimes`, and its
    /// name from `GetThreadDescription` (which Rust's `thread::Builder::name` sets).
    #[derive(Clone, Copy, Debug, Default)]
    pub struct WindowsThreadCpu;

    impl ThreadCpu for WindowsThreadCpu {
        fn threads(&self) -> Option<Vec<ThreadTime>> {
            // SAFETY: plain call; the handle is owned by `Owned`.
            let snapshot = Owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }.ok()?);
            // SAFETY: plain call with no arguments.
            let pid = unsafe { GetCurrentProcessId() };
            let mut entry = THREADENTRY32 {
                dwSize: u32::try_from(size_of::<THREADENTRY32>()).ok()?,
                ..Default::default()
            };
            let mut threads = Vec::new();
            // SAFETY: `entry` is a valid THREADENTRY32 with `dwSize` set, for both calls.
            let mut next = unsafe { Thread32First(snapshot.0, &mut entry) };
            while next.is_ok() {
                if entry.th32OwnerProcessID == pid
                    && let Some(thread) = read_thread(entry.th32ThreadID)
                {
                    threads.push(thread);
                }
                // SAFETY: as above.
                next = unsafe { Thread32Next(snapshot.0, &mut entry) };
            }
            Some(threads)
        }
    }

    /// One thread's time and name, or `None` when it can't be opened (it ended).
    fn read_thread(id: u32) -> Option<ThreadTime> {
        // SAFETY: plain call; the handle is owned by `Owned`.
        let thread =
            Owned(unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, id) }.ok()?);
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: `thread.0` is an open thread handle with query access, and the four
        // out-pointers are locals that outlive the call.
        unsafe { GetThreadTimes(thread.0, &mut created, &mut exited, &mut kernel, &mut user) }
            .ok()?;
        let hns = filetime_hns(kernel.dwHighDateTime, kernel.dwLowDateTime)
            .saturating_add(filetime_hns(user.dwHighDateTime, user.dwLowDateTime));
        Some(ThreadTime {
            id: u64::from(id),
            name: description(&thread).unwrap_or_default(),
            cpu: Duration::from_nanos(hns.saturating_mul(100)),
        })
    }

    /// The thread's description, its name, or `None` when it has none.
    fn description(thread: &Owned) -> Option<String> {
        // SAFETY: `thread.0` is an open thread handle with query access.
        let text = unsafe { GetThreadDescription(thread.0) }.ok()?;
        if text.is_null() {
            return None;
        }
        // SAFETY: on success the OS returns a NUL-terminated string it allocated.
        let name = unsafe { text.to_string() }.ok();
        // SAFETY: the string was allocated by the OS with LocalAlloc and is freed once.
        let _ = unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
        name.filter(|name| !name.is_empty())
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
