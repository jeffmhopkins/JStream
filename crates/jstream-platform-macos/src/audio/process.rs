//! The processes application audio can record (ADR-0063 §4): Core Audio's process
//! objects (`kAudioHardwarePropertyProcessObjectList`), each a process the HAL knows as
//! an audio client, which is what a process tap can target. Each is listed with its PID,
//! its parent's PID and its executable's file name, read from the kernel by PID
//! (`proc_pidpath`, and `proc_pidinfo`'s short BSD info, which the kernel answers for
//! other users' processes too, such as system daemons).
//!
//! Unlike Windows, which lists every process and records a whole process tree, this lists
//! only processes Core Audio knows, and a tap records the one process chosen, not its
//! children.
//!
//! Naming and the parent's PID are platform independent, so they are tested on every
//! platform.

/// The executable's file name from its full path, without the folder: `obs` from
/// `/Applications/OBS.app/Contents/MacOS/obs`. `None` for an empty name.
pub(crate) fn exe_from_path(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    (!name.is_empty()).then(|| name.to_owned())
}

/// The name a process is listed under: its executable's file name, else the kernel's
/// short name for it, else its bundle ID, else its PID.
pub(crate) fn display_name(
    path: Option<&str>,
    short: Option<&str>,
    bundle: Option<&str>,
    pid: u32,
) -> String {
    path.and_then(exe_from_path)
        .or_else(|| short.filter(|s| !s.is_empty()).map(str::to_owned))
        .or_else(|| bundle.filter(|s| !s.is_empty()).map(str::to_owned))
        .unwrap_or_else(|| pid.to_string())
}

/// The parent PID as the trait reports it: `None` when the kernel reports none (0).
pub(crate) fn parent(ppid: u32) -> Option<u32> {
    (ppid != 0).then_some(ppid)
}

/// The kernel's short name for a process (`pbsi_comm`), up to its first NUL, if it is
/// non-empty UTF-8.
pub(crate) fn comm_name(comm: &[u8]) -> Option<String> {
    let end = comm.iter().position(|&b| b == 0).unwrap_or(comm.len());
    let name = std::str::from_utf8(&comm[..end]).ok()?;
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(target_os = "macos")]
pub(crate) use system::list;

#[cfg(target_os = "macos")]
mod system {
    use std::ffi::{c_int, c_void};
    use std::mem::{MaybeUninit, size_of};

    use jstream_platform::{AudioProcess, PlatformError};

    use super::super::hal;
    use super::{comm_name, display_name, parent};

    /// The executable's full path, from the kernel.
    fn path(pid: c_int) -> Option<String> {
        let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let len = u32::try_from(buf.len()).ok()?;
        // SAFETY: `buf` is `len` writable bytes; the call writes at most that many and
        // returns how many, or 0 on failure.
        let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast::<c_void>(), len) };
        let n = usize::try_from(n).ok().filter(|&n| n > 0)?;
        buf.truncate(n);
        String::from_utf8(buf).ok()
    }

    /// The parent's PID and the kernel's short name for the process, from its short BSD
    /// info (`PROC_PIDT_SHORTBSDINFO`). Unlike the full BSD info, which `proc_name` also
    /// reads, the kernel gives it for other users' processes too.
    fn short_info(pid: c_int) -> (Option<u32>, Option<String>) {
        let mut info = MaybeUninit::<libc::proc_bsdshortinfo>::zeroed();
        let Ok(size) = c_int::try_from(size_of::<libc::proc_bsdshortinfo>()) else {
            return (None, None);
        };
        // SAFETY: `info` is a zeroed `proc_bsdshortinfo` of `size` bytes, which the call
        // fills (returning `size`) or leaves alone.
        let n = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDT_SHORTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if n != size {
            return (None, None);
        }
        // SAFETY: zeroed, and filled by the kernel; every field is an integer or a byte
        // array, valid for any bit pattern.
        let info = unsafe { info.assume_init() };
        let comm: Vec<u8> = info.pbsi_comm.iter().map(|&c| c.to_ne_bytes()[0]).collect();
        (parent(info.pbsi_ppid), comm_name(&comm))
    }

    /// The processes Core Audio knows now. One that exits while being read is left out.
    pub(crate) fn list() -> Result<Vec<AudioProcess>, PlatformError> {
        Ok(hal::process_objects()?
            .into_iter()
            .filter_map(|object| {
                let pid = hal::process_pid(object)?;
                let pid_u = u32::try_from(pid).ok()?;
                let bundle = hal::process_bundle_id(object);
                let (parent_pid, short) = short_info(pid);
                Some(AudioProcess {
                    pid: pid_u,
                    parent_pid,
                    exe: display_name(
                        path(pid).as_deref(),
                        short.as_deref(),
                        bundle.as_deref(),
                        pid_u,
                    ),
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exe_is_the_file_name() {
        assert_eq!(
            exe_from_path("/Applications/OBS.app/Contents/MacOS/obs").as_deref(),
            Some("obs")
        );
        assert_eq!(exe_from_path("launchd").as_deref(), Some("launchd"));
        assert_eq!(exe_from_path("/usr/bin/"), None);
        assert_eq!(exe_from_path(""), None);
    }

    #[test]
    fn names_fall_back_in_order() {
        assert_eq!(
            display_name(Some("/a/b/music"), Some("m"), Some("x.y"), 7),
            "music"
        );
        assert_eq!(
            display_name(None, Some("coreaudiod"), Some("x.y"), 7),
            "coreaudiod"
        );
        assert_eq!(
            display_name(Some(""), Some(""), Some("com.apple.Music"), 7),
            "com.apple.Music"
        );
        assert_eq!(display_name(None, None, None, 42), "42");
    }

    #[test]
    fn the_short_name_ends_at_its_nul() {
        assert_eq!(
            comm_name(b"coreaudiod\0\0\0\0\0\0").as_deref(),
            Some("coreaudiod")
        );
        assert_eq!(
            comm_name(b"sixteen-chars-xx").as_deref(),
            Some("sixteen-chars-xx")
        );
        assert_eq!(comm_name(b"\0abc"), None);
        assert_eq!(comm_name(&[0xFF, 0]), None);
    }

    #[test]
    fn no_parent_is_none() {
        assert_eq!(parent(0), None);
        assert_eq!(parent(1), Some(1));
    }
}
