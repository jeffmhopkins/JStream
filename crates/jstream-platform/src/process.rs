//! The engine process's own CPU time, for `stats.tick`'s load (protocol 0.48), each
//! of its threads' CPU time, for `profile.get` (protocol 0.73, ADR-0103), and the OS
//! scheduling class of its real-time threads (ADR-0117 §8).

use std::time::Duration;

/// Reads the CPU time the current process has used.
///
/// A platform crate implements it on the OS's process accounting (on Windows
/// `GetProcessTimes`, on macOS `getrusage`, on Linux `CLOCK_PROCESS_CPUTIME_ID`), and the
/// host hands it to the engine. The engine's stats thread calls it about once a second,
/// never a real-time thread, so it may make a system call.
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
/// `/proc/self/task`, on Windows the Tool Help thread list with `GetThreadTimes`, on
/// macOS `task_threads` with `thread_info`), and the host hands it to the engine. Only
/// the engine's control side calls it, when it reads a profile (about once a second at
/// most), never a real-time thread, so it may make system calls and allocate.
pub trait ThreadCpu: Send + Sync {
    /// Every live thread of the process, or `None` if the OS refused to list them. A
    /// thread that ends while it is read may be left out.
    fn threads(&self) -> Option<Vec<ThreadTime>>;
}

/// What a real-time thread does, for the OS to schedule it by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadRole {
    /// The render thread: a frame every period, against a deadline.
    Video,
    /// The mixer thread: an audio block every few milliseconds.
    Audio,
}

/// Puts a real-time thread in the OS's scheduling class for its role (ADR-0117 §8): on
/// Windows the Multimedia Class Scheduler Service's `Playback` task for video and its
/// `Pro Audio` task for audio.
///
/// A platform crate implements it and the host hands it to the engine; the thread
/// itself calls it once, as it starts, before its loop. Without one, or where the OS
/// refuses, the thread runs at its normal priority.
pub trait ThreadPriority: Send + Sync {
    /// Raises the calling thread for `role`, until the guard returned drops on that
    /// same thread. `None` when the OS refused: the thread goes on as it was.
    fn raise(&self, role: ThreadRole) -> Option<RaisedThread>;
}

/// A raised thread's way back to its normal class, run when it drops. It stays on the
/// thread it was raised on.
pub struct RaisedThread {
    revert: Option<Box<dyn FnOnce()>>,
}

impl RaisedThread {
    /// Runs `revert` when dropped.
    pub fn new(revert: impl FnOnce() + 'static) -> Self {
        Self {
            revert: Some(Box::new(revert)),
        }
    }
}

impl std::fmt::Debug for RaisedThread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RaisedThread").finish_non_exhaustive()
    }
}

impl Drop for RaisedThread {
    fn drop(&mut self) {
        if let Some(revert) = self.revert.take() {
            revert();
        }
    }
}
