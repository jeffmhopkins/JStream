//! The process's CPU time on macOS, for `stats.tick`'s load (protocol 0.48), and each
//! of its threads' CPU time, for `profile.get` (protocol 0.73, ADR-0103).

/// A `timeval`'s seconds and microseconds as nanoseconds, or `None` for a negative
/// part, which `getrusage` never reports.
#[cfg(any(target_os = "macos", test))]
fn timeval_nanos(secs: i64, micros: i64) -> Option<u64> {
    let secs = u64::try_from(secs).ok()?;
    let micros = u64::try_from(micros).ok()?;
    secs.checked_mul(1_000_000_000)?
        .checked_add(micros.checked_mul(1_000)?)
}

/// A thread's name from the NUL-padded bytes `thread_info` reports, or empty when
/// unnamed. Bytes past the first NUL are ignored, and a name that isn't UTF-8 is read
/// lossily.
#[cfg(any(target_os = "macos", test))]
fn thread_name(bytes: &[std::ffi::c_char]) -> String {
    let bytes: Vec<u8> = bytes
        .iter()
        .take_while(|b| **b != 0)
        // c_char is i8 on Apple targets: the cast keeps each byte's bits.
        .map(|b| *b as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(target_os = "macos")]
pub use os::{MacosProcessCpu, MacosThreadCpu};

#[cfg(target_os = "macos")]
mod os {
    use std::mem::MaybeUninit;
    use std::time::Duration;

    use jstream_platform::{ProcessCpu, ThreadCpu, ThreadTime};

    use super::{thread_name, timeval_nanos};

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

    /// Each thread of the engine's process and its CPU time: the threads from
    /// `task_threads`, each one's ID from `thread_info(THREAD_IDENTIFIER_INFO)` (the ID
    /// `pthread_threadid_np` gives), and its user plus system time and name from
    /// `thread_info(THREAD_EXTENDED_INFO)` (the name `thread::Builder::name` sets).
    #[derive(Clone, Copy, Debug, Default)]
    pub struct MacosThreadCpu;

    impl ThreadCpu for MacosThreadCpu {
        fn threads(&self) -> Option<Vec<ThreadTime>> {
            let list = ThreadList::read()?;
            Some(
                list.ports()
                    .iter()
                    .filter_map(|p| read_thread(*p))
                    .collect(),
            )
        }
    }

    /// The thread ports `task_threads` handed this task, and the array that holds them.
    /// Dropping it releases each port's send right and frees the array.
    struct ThreadList {
        list: libc::thread_act_array_t,
        len: usize,
    }

    impl ThreadList {
        /// The task's live threads, or `None` if the kernel refused to list them.
        fn read() -> Option<Self> {
            let mut list: libc::thread_act_array_t = std::ptr::null_mut();
            let mut count: libc::mach_msg_type_number_t = 0;
            // SAFETY: `mach_task_self` names this task, and both out-pointers are locals
            // that outlive the call.
            let kr = unsafe {
                mach2::task::task_threads(mach2::traps::mach_task_self(), &mut list, &mut count)
            };
            if kr != libc::KERN_SUCCESS || list.is_null() {
                return None;
            }
            // From here on, drop frees what the kernel handed over. A `u32` count always
            // fits a `usize` on Apple's 64-bit targets.
            Some(Self {
                list,
                len: usize::try_from(count).unwrap_or(0),
            })
        }

        fn ports(&self) -> &[libc::thread_act_t] {
            // SAFETY: `list` points at `len` thread ports the kernel allocated in this
            // task, valid until drop frees them.
            unsafe { std::slice::from_raw_parts(self.list, self.len) }
        }
    }

    impl Drop for ThreadList {
        fn drop(&mut self) {
            for port in self.ports() {
                // SAFETY: task_threads gave this task a send right to each port, released
                // once here.
                let _ = unsafe {
                    mach2::mach_port::mach_port_deallocate(mach2::traps::mach_task_self(), *port)
                };
            }
            // SAFETY: the array was allocated in this task by task_threads, `len` ports
            // long, and is freed once, after the last read of its ports.
            let _ = unsafe {
                libc::vm_deallocate(
                    mach2::traps::mach_task_self(),
                    self.list as libc::vm_address_t,
                    self.len.saturating_mul(size_of::<libc::thread_act_t>()),
                )
            };
        }
    }

    /// One thread's ID, time and name, or `None` when the kernel refuses (it ended).
    fn read_thread(port: libc::thread_act_t) -> Option<ThreadTime> {
        let mut ident = MaybeUninit::<libc::thread_identifier_info>::zeroed();
        let mut count = libc::THREAD_IDENTIFIER_INFO_COUNT;
        // SAFETY: `port` is a live thread port this task holds a right to, `ident` is
        // a writable `thread_identifier_info`, and `count` gives its size in words.
        let kr = unsafe {
            libc::thread_info(
                port,
                libc::THREAD_IDENTIFIER_INFO as libc::thread_flavor_t,
                ident.as_mut_ptr().cast(),
                &mut count,
            )
        };
        if kr != libc::KERN_SUCCESS {
            return None;
        }
        // SAFETY: thread_info succeeded and filled it; it started zeroed in any case,
        // and every bit pattern is a valid `thread_identifier_info`.
        let ident = unsafe { ident.assume_init() };
        let mut info = MaybeUninit::<libc::thread_extended_info>::zeroed();
        let mut count = libc::THREAD_EXTENDED_INFO_COUNT;
        // SAFETY: as above, for a `thread_extended_info`.
        let kr = unsafe {
            libc::thread_info(
                port,
                libc::THREAD_EXTENDED_INFO as libc::thread_flavor_t,
                info.as_mut_ptr().cast(),
                &mut count,
            )
        };
        if kr != libc::KERN_SUCCESS {
            return None;
        }
        // SAFETY: as above.
        let info = unsafe { info.assume_init() };
        Some(ThreadTime {
            id: ident.thread_id,
            name: thread_name(&info.pth_name),
            // The kernel reports both in nanoseconds.
            cpu: Duration::from_nanos(info.pth_user_time.saturating_add(info.pth_system_time)),
        })
    }

    #[cfg(test)]
    mod tests {
        use super::MacosThreadCpu;
        use jstream_platform::ThreadCpu;
        use std::time::Instant;

        fn spin(millis: u128) {
            let start = Instant::now();
            let mut x = 0u64;
            while start.elapsed().as_millis() < millis {
                x = std::hint::black_box(x.wrapping_add(1));
            }
        }

        /// A named thread is listed by its name, with the time it spun and the ID
        /// `pthread_threadid_np` gives it, and its time never goes back.
        #[test]
        fn a_named_thread_is_listed_with_its_time() {
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            let (ready_tx, ready_rx) = std::sync::mpsc::channel::<u64>();
            let handle = std::thread::Builder::new()
                .name("jstream-spinner".into())
                .spawn(move || {
                    spin(40);
                    let mut id = 0u64;
                    // SAFETY: `pthread_self` names this live thread, and `id` outlives
                    // the call.
                    unsafe { libc::pthread_threadid_np(libc::pthread_self(), &mut id) };
                    let _ = ready_tx.send(id);
                    let _ = rx.recv();
                })
                .expect("spawns");
            let id = ready_rx.recv().expect("it spun");
            let threads = MacosThreadCpu.threads().expect("lists");
            let spinner = threads
                .iter()
                .find(|t| t.name == "jstream-spinner")
                .expect("listed by name");
            assert!(
                spinner.cpu.as_millis() >= 20,
                "it spun for 40 ms: {spinner:?}"
            );
            assert_eq!(spinner.id, id, "pthread_threadid_np's ID");
            assert!(threads.len() >= 2, "this test's thread too");
            let spun = spinner.cpu;
            for _ in 0..200 {
                let again = MacosThreadCpu.threads().expect("lists again");
                let spinner = again
                    .iter()
                    .find(|t| t.name == "jstream-spinner")
                    .expect("still listed");
                assert!(spinner.cpu >= spun, "its time never goes back");
            }
            drop(tx);
            handle.join().expect("joins");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{thread_name, timeval_nanos};

    #[test]
    fn a_timeval_becomes_nanoseconds() {
        assert_eq!(timeval_nanos(2, 500), Some(2_000_500_000));
        assert_eq!(timeval_nanos(0, 0), Some(0));
        assert_eq!(timeval_nanos(-1, 0), None);
        assert_eq!(timeval_nanos(0, -1), None);
    }

    #[test]
    fn a_thread_name_stops_at_its_nul() {
        let mut bytes = [0 as std::ffi::c_char; 64];
        for (slot, byte) in bytes.iter_mut().zip(b"jstream-mix\0junk") {
            *slot = *byte as std::ffi::c_char;
        }
        assert_eq!(thread_name(&bytes), "jstream-mix");
        assert_eq!(thread_name(&[0; 64]), "");
        // A name that fills the buffer has no NUL.
        assert_eq!(thread_name(&[b'a' as std::ffi::c_char; 64]), "a".repeat(64));
    }
}
