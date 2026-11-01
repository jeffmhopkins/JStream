//! The engine process's own CPU time, for `stats.tick`'s load (protocol 0.48).

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
