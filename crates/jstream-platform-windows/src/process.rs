//! The process's CPU time on Windows, for `stats.tick`'s load (protocol 0.48), each
//! of its threads' CPU time, for `profile.get` (protocol 0.73, ADR-0103), and the
//! Multimedia Class Scheduler Service tasks of its real-time threads (ADR-0117 §8).
//!
//! # Behaviour
//!
//! **`WindowsProcessCpu`** (`ProcessCpu`): the process's kernel and user time from
//! `GetProcessTimes`, for `stats.tick`'s load. **`WindowsThreadCpu`** (`ThreadCpu`): the
//! process's threads from a ToolHelp snapshot, each one's kernel and user time from
//! `GetThreadTimes` and its name from `GetThreadDescription`, for `profile.get`.
//! **`WindowsThreadPriority`** (`ThreadPriority`): MMCSS through
//! `AvSetMmThreadCharacteristicsW`, the `Playback` task for video and `Pro Audio` for
//! audio, reverted when the guard drops.

/// Adds a FILETIME's two halves into its 100 ns count, as `GetProcessTimes` reports
/// kernel and user time.
#[cfg(any(windows, test))]
fn filetime_hns(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

#[cfg(windows)]
pub use os::{WindowsProcessCpu, WindowsThreadCpu, WindowsThreadPriority};

#[cfg(windows)]
mod os {
    use std::time::Duration;

    use jstream_platform::{
        ProcessCpu, RaisedThread, ThreadCpu, ThreadPriority, ThreadRole, ThreadTime,
    };
    use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, HLOCAL, LocalFree};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows::Win32::System::Threading::{
        AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, GetCurrentProcess,
        GetCurrentProcessId, GetProcessTimes, GetThreadDescription, GetThreadTimes, OpenThread,
        THREAD_QUERY_LIMITED_INFORMATION,
    };
    use windows::core::w;

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

    /// Real-time threads in the Multimedia Class Scheduler Service: the render thread in
    /// its `Playback` task, the mixer in `Pro Audio`, so a busy desktop doesn't starve
    /// them of their period. The service reverts a thread when its handle is reverted.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct WindowsThreadPriority;

    impl ThreadPriority for WindowsThreadPriority {
        fn raise(&self, role: ThreadRole) -> Option<RaisedThread> {
            let task = match role {
                ThreadRole::Video => w!("Playback"),
                ThreadRole::Audio => w!("Pro Audio"),
            };
            let mut index = 0u32;
            // SAFETY: `task` is a static NUL-terminated string and `index` a local that
            // outlives the call.
            let handle = unsafe { AvSetMmThreadCharacteristicsW(task, &mut index) }.ok()?;
            Some(RaisedThread::new(move || {
                // SAFETY: the handle came from `AvSetMmThreadCharacteristicsW` on this
                // thread, which `RaisedThread` stays on, and is reverted once.
                let _ = unsafe { AvRevertMmThreadCharacteristics(handle) };
            }))
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

    #[cfg(test)]
    mod tests {
        use super::WindowsThreadCpu;
        use jstream_platform::ThreadCpu;
        use std::time::Instant;
        use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

        fn handles() -> u32 {
            let mut count = 0;
            // SAFETY: the pseudo-handle needs no closing, and `count` outlives the call.
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) }
                .expect("counts handles");
            count
        }

        fn spin(millis: u128) {
            let start = Instant::now();
            let mut x = 0u64;
            while start.elapsed().as_millis() < millis {
                x = std::hint::black_box(x.wrapping_add(1));
            }
        }

        /// A named thread is listed by the name `thread::Builder::name` gave it, with
        /// the time it spun, and listing many times leaves no handles open.
        #[test]
        fn a_named_thread_is_listed_with_its_time() {
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
            let handle = std::thread::Builder::new()
                .name("jstream-spinner".into())
                .spawn(move || {
                    spin(200);
                    let _ = ready_tx.send(());
                    let _ = rx.recv();
                })
                .expect("spawns");
            ready_rx.recv().expect("it spun");
            let threads = WindowsThreadCpu.threads().expect("lists");
            let spinner = threads
                .iter()
                .find(|t| t.name == "jstream-spinner")
                .expect("listed by name");
            assert!(
                spinner.cpu.as_millis() >= 20,
                "it spun for 200 ms: {spinner:?}"
            );
            assert!(threads.len() >= 2, "this test's thread too");
            let spun = spinner.cpu;
            let before = handles();
            for _ in 0..200 {
                let again = WindowsThreadCpu.threads().expect("lists again");
                let spinner = again
                    .iter()
                    .find(|t| t.name == "jstream-spinner")
                    .expect("still listed");
                assert!(spinner.cpu >= spun, "its time never goes back");
            }
            // Other threads of the test binary may open a few handles meanwhile, but not
            // one per listing.
            let after = handles();
            assert!(
                after < before.saturating_add(50),
                "{before} handles, then {after} after 200 listings"
            );
            drop(tx);
            handle.join().expect("joins");
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
