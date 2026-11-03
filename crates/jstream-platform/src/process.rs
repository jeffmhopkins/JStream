//! The engine process's own CPU time, for `stats.tick`'s load (protocol 0.48), and each
//! of its threads' CPU time, for `profile.get` (protocol 0.73, ADR-0103).

use std::time::Duration;

/// Reads the CPU time the current process has used.
///
/// A platform crate implements it on the OS's process accounting (on Windows
/// `GetProcessTimes`, on macOS `getrusage`; Linux has none yet), and the host hands it to the
/// engine. The engine's stats thread calls it about once a second, never a real-time
/// thread, so it may make a system call.
pub trait ProcessCpu: Send + Sync {
    /// CPU time the process has used since it started, on every thread and in both
    /// user and kernel mode, or `None` if the OS refused to say. Never decreases
    /// between calls that return `Some`.
    fn cpu_time(&self) -> Option<Duration>;
}

/// One thread of the process and the CPU time it has used, as [`ThreadCpu`] reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadTime {
    /// The OS's ID for the thread, unique among the process's live threads. The OS may
    /// reuse it once the thread has ended.
    pub id: u64,
    /// The thread's name, as the OS reports it, or empty for an unnamed thread. Linux
    /// keeps the first 15 bytes of a name.
    pub name: String,
    /// The CPU time it has used since it started, in user and kernel mode.
    pub cpu: Duration,
}

/// Reads the CPU time each thread of the current process has used.
///
/// A platform crate implements it on the OS's thread accounting (on Linux
/// `/proc/self/task`, on Windows the Tool Help thread list with `GetThreadTimes`), and the
/// host hands it to the engine. Only the engine's control side calls it, when it reads
/// a profile (about once a second at most), never a real-time thread, so it may make
/// system calls and allocate.
pub trait ThreadCpu: Send + Sync {
    /// Every live thread of the process, or `None` if the OS refused to list them. A
    /// thread that ends while it is read may be left out.
    fn threads(&self) -> Option<Vec<ThreadTime>>;
}
