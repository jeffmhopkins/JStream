//! The process's CPU time on Linux, for `stats.tick`'s load (protocol 0.48), and each
//! of its threads' CPU time, for `profile.get` (protocol 0.73, ADR-0103).

#[cfg(target_os = "linux")]
pub use os::{LinuxProcessCpu, LinuxThreadCpu};

/// The CPU time in a `/proc/<pid>/task/<tid>/schedstat` line: its first field, the
/// nanoseconds the thread has run on a CPU.
#[cfg(any(target_os = "linux", test))]
fn schedstat_nanos(line: &str) -> Option<u64> {
    line.split_ascii_whitespace().next()?.parse().ok()
}

/// The user and system time in a `/proc/<pid>/task/<tid>/stat` line, in clock ticks:
/// fields 14 and 15, counted after the `)` that ends the name, which may itself hold
/// spaces and parentheses.
#[cfg(any(target_os = "linux", test))]
fn stat_ticks(line: &str) -> Option<u64> {
    let rest = &line[line.rfind(')')? + 1..];
    // After the name: state is field 3, so utime (14) is the 12th and stime the 13th.
    let mut fields = rest.split_ascii_whitespace().skip(11);
    let user: u64 = fields.next()?.parse().ok()?;
    let system: u64 = fields.next()?.parse().ok()?;
    user.checked_add(system)
}

#[cfg(target_os = "linux")]
mod os {
    use std::fs;
    use std::time::Duration;

    use jstream_platform::{ProcessCpu, ThreadCpu, ThreadTime};

    use super::{schedstat_nanos, stat_ticks};

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

    /// Each thread of the engine's process and its CPU time, from `/proc/self/task`:
    /// the name from each thread's `comm`, the time from its `schedstat` (nanoseconds),
    /// or from its `stat` (clock ticks) on a kernel without schedstats.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct LinuxThreadCpu;

    impl ThreadCpu for LinuxThreadCpu {
        fn threads(&self) -> Option<Vec<ThreadTime>> {
            let tasks = fs::read_dir("/proc/self/task").ok()?;
            // SAFETY: sysconf only reads a configuration value.
            let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
            let ticks = u64::try_from(ticks).ok().filter(|t| *t > 0).unwrap_or(100);
            let mut threads = Vec::new();
            for task in tasks.flatten() {
                let Some(id) = task.file_name().to_str().and_then(|id| id.parse().ok()) else {
                    continue;
                };
                let dir = task.path();
                // A thread that ended while it was read is left out.
                let nanos = fs::read_to_string(dir.join("schedstat"))
                    .ok()
                    .and_then(|line| schedstat_nanos(&line))
                    .or_else(|| {
                        let line = fs::read_to_string(dir.join("stat")).ok()?;
                        stat_ticks(&line)?
                            .checked_mul(1_000_000_000)
                            .map(|n| n / ticks)
                    });
                let Some(nanos) = nanos else {
                    continue;
                };
                let name = fs::read_to_string(dir.join("comm"))
                    .map(|name| name.trim_end_matches('\n').to_owned())
                    .unwrap_or_default();
                threads.push(ThreadTime {
                    id,
                    name,
                    cpu: Duration::from_nanos(nanos),
                });
            }
            Some(threads)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{LinuxProcessCpu, LinuxThreadCpu};
        use jstream_platform::{ProcessCpu, ThreadCpu};
        use std::time::Instant;

        fn spin(millis: u128) {
            let start = Instant::now();
            let mut x = 0u64;
            while start.elapsed().as_millis() < millis {
                x = std::hint::black_box(x.wrapping_add(1));
            }
        }

        /// The clock reads, and grows while this thread spins.
        #[test]
        fn cpu_time_grows_while_the_process_works() {
            let before = LinuxProcessCpu.cpu_time().expect("a first reading");
            spin(30);
            let after = LinuxProcessCpu.cpu_time().expect("a second reading");
            assert!(after > before, "{before:?} then {after:?}");
        }

        /// The calling thread's own CPU time, from `CLOCK_THREAD_CPUTIME_ID`.
        fn thread_cpu() -> std::time::Duration {
            let mut ts = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: `ts` is a valid, writable `timespec` for the duration of the call,
            // and CLOCK_THREAD_CPUTIME_ID names the calling thread's CPU-time clock.
            let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
            assert_eq!(rc, 0, "the thread's CPU clock reads");
            std::time::Duration::new(
                u64::try_from(ts.tv_sec).expect("a non-negative time"),
                u32::try_from(ts.tv_nsec).expect("nanoseconds fit"),
            )
        }

        /// Spins until this thread has had `millis` of CPU, however long a busy machine
        /// takes to give it that (#1322). A wall-clock spin can get far less CPU than it
        /// lasted on a loaded runner.
        fn spin_cpu(millis: u64) {
            let want = thread_cpu() + std::time::Duration::from_millis(millis);
            let deadline = Instant::now() + std::time::Duration::from_secs(60);
            let mut x = 0u64;
            while thread_cpu() < want {
                assert!(Instant::now() < deadline, "no {millis} ms of CPU in 60 s");
                for _ in 0..10_000 {
                    x = std::hint::black_box(x.wrapping_add(1));
                }
            }
        }

        /// A named thread is listed by its name, and its time grows while it spins.
        #[test]
        fn a_named_thread_is_listed_with_its_time() {
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
            let handle = std::thread::Builder::new()
                .name("jstream-spinner".into())
                .spawn(move || {
                    spin_cpu(40);
                    let _ = ready_tx.send(());
                    let _ = rx.recv();
                })
                .expect("spawns");
            ready_rx.recv().expect("it spun");
            let threads = LinuxThreadCpu.threads().expect("lists");
            let spinner = threads
                .iter()
                .find(|t| t.name == "jstream-spinner")
                .expect("listed by name");
            assert!(
                spinner.cpu.as_millis() >= 20,
                "it had 40 ms of CPU: {spinner:?}"
            );
            assert!(threads.len() >= 2, "this test's thread too");
            drop(tx);
            handle.join().expect("joins");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{schedstat_nanos, stat_ticks};

    #[test]
    fn schedstat_reads_its_first_field() {
        assert_eq!(schedstat_nanos("123456789 2000 15\n"), Some(123_456_789));
        assert_eq!(schedstat_nanos(""), None);
    }

    #[test]
    fn stat_reads_user_and_system_after_the_name() {
        // A name with a space and a parenthesis, then state, ppid … utime=7, stime=5.
        let line = "42 (we (ird) name) S 1 42 42 0 -1 4194560 100 0 0 0 7 5 0 0 20 0 1 0";
        assert_eq!(stat_ticks(line), Some(12));
        assert_eq!(stat_ticks("42 (x) S 1"), None);
    }
}
