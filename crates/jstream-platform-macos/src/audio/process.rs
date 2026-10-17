//! The processes application audio can record (ADR-0063 §4): Core Audio's process
//! objects (`kAudioHardwarePropertyProcessObjectList`), each a process the HAL knows as
//! an audio client, which is what a process tap can target. Each is listed with its PID,
//! its parent's PID and its executable's file name, read from the kernel by PID
//! (`proc_pidinfo`, `proc_pidpath`).
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

#[cfg(target_os = "macos")]
pub(crate) use system::list;

#[cfg(target_os = "macos")]
mod system {
    use std::ffi::{c_int, c_void};
    use std::mem::{MaybeUninit, size_of};

    use jstream_platform::{AudioProcess, PlatformError};

    use super::super::hal;
    use super::{display_name, parent};

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

    /// The kernel's short name for the process.
    fn short_name(pid: c_int) -> Option<String> {
        let mut buf = [0u8; 256];
        let len = u32::try_from(buf.len()).ok()?;
        // SAFETY: as above, for `buf`.
        let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast::<c_void>(), len) };
        let n = usize::try_from(n).ok().filter(|&n| n > 0)?;
        String::from_utf8(buf[..n].to_vec()).ok()
    }

    /// The parent's PID, from the kernel's BSD info for the process.
    fn parent_pid(pid: c_int) -> Option<u32> {
        let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        let size = c_int::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: `info` is a zeroed `proc_bsdinfo` of `size` bytes, which the call fills
        // (returning `size`) or leaves alone.
        let n = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if n != size {
            return None;
        }
        // SAFETY: zeroed, and filled by the kernel; every field is an integer or a byte
        // array, valid for any bit pattern.
        let info = unsafe { info.assume_init() };
        parent(info.pbi_ppid)
    }

    /// The processes Core Audio knows now. One that exits while being read is left out.
    pub(crate) fn list() -> Result<Vec<AudioProcess>, PlatformError> {
        Ok(hal::process_objects()?
            .into_iter()
            .filter_map(|object| {
                let pid = hal::process_pid(object)?;
                let pid_u = u32::try_from(pid).ok()?;
                let bundle = hal::process_bundle_id(object);
                Some(AudioProcess {
                    pid: pid_u,
                    parent_pid: parent_pid(pid),
                    exe: display_name(
                        path(pid).as_deref(),
                        short_name(pid).as_deref(),
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
    fn no_parent_is_none() {
        assert_eq!(parent(0), None);
        assert_eq!(parent(1), Some(1));
    }
}
