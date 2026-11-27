//! A local minidump for a native crash (ADR-0258). An access violation is a
//! Windows exception, not a Rust panic, so [`jstream_crash`]'s hook never runs.
//! [`install`] registers the process's unhandled-exception filter. The filter
//! writes `crash-<pid>-<n>.dmp` beside the panic reports and then returns
//! `EXCEPTION_CONTINUE_SEARCH`, so Windows Error Reporting still runs.
//!
//! The dump is `MiniDumpNormal` with thread info: each thread's stack and the
//! module list, not the process's heap.
//! The filter builds the file name on the stack. It does not allocate, lock,
//! or call back into JStream.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Storage::FileSystem::{
    CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
};
use windows::Win32::System::Diagnostics::Debug::{
    EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION, MiniDumpNormal, MiniDumpWithThreadInfo,
    MiniDumpWriteDump, SetUnhandledExceptionFilter,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId,
};
use windows::core::PCWSTR;

/// How many dumps the folder keeps. Each is larger than a panic report, and the
/// next crash needs a free name, so install drops the oldest past this.
const MAX_DUMPS: usize = 3;

/// A path the filter can extend without allocating. `MAX_PATH` wide characters.
const MAX_WIDE: usize = 260;

/// The folder, as UTF-16 with a trailing `\`, set once by [`install`].
struct Prefix {
    wide: [u16; MAX_WIDE],
    /// Characters of `wide` that are the directory, including the trailing `\`.
    dir_len: usize,
}

/// The folder the filter writes into. Set once; the OS callback has no other
/// place to keep it (ADR-0258).
static PREFIX: OnceLock<Prefix> = OnceLock::new();

/// Distinguishes two dumps from one process. The filter only adds to it.
static SEQUENCE: AtomicU32 = AtomicU32::new(0);

/// Registers the unhandled-exception filter to write dumps into `dir`, and
/// drops the oldest dumps there past [`MAX_DUMPS`]. `false` when `dir` does not
/// fit in `MAX_PATH` or a filter was already installed. The folder must exist.
pub fn install(dir: &Path) -> bool {
    let Some(prefix) = Prefix::from_dir(dir) else {
        return false;
    };
    prune(dir);
    if PREFIX.set(prefix).is_err() {
        return false;
    }
    // SAFETY: `SetUnhandledExceptionFilter(LPTOP_LEVEL_EXCEPTION_FILTER) ->
    // LPTOP_LEVEL_EXCEPTION_FILTER`. `on_exception` is a static function of that
    // type. The previous filter is not kept: this one returns
    // EXCEPTION_CONTINUE_SEARCH, so the system filter still runs.
    unsafe {
        SetUnhandledExceptionFilter(Some(on_exception));
    }
    true
}

impl Prefix {
    fn from_dir(dir: &Path) -> Option<Self> {
        let mut wide = [0u16; MAX_WIDE];
        let mut len = 0;
        for unit in dir.as_os_str().encode_wide() {
            if len + 2 >= MAX_WIDE {
                return None;
            }
            wide[len] = unit;
            len += 1;
        }
        if len == 0 {
            return None;
        }
        if wide[len - 1] != b'\\' as u16 && wide[len - 1] != b'/' as u16 {
            wide[len] = b'\\' as u16;
            len += 1;
        }
        Some(Self { wide, dir_len: len })
    }
}

/// `EXCEPTION_CONTINUE_SEARCH`: the dump is written, then Windows reports the
/// crash as it would have without this filter.
const CONTINUE_SEARCH: i32 = 0;

unsafe extern "system" fn on_exception(info: *const EXCEPTION_POINTERS) -> i32 {
    if let Some(prefix) = PREFIX.get() {
        let _ = write_at(prefix, info);
    }
    CONTINUE_SEARCH
}

/// Writes one dump of this process into `dir`. `info` is the exception the
/// filter was given, or null for a dump of the running process (the test).
/// Only the test calls it; the filter goes through `write_at` with the prefix
/// `install` kept.
#[cfg(test)]
pub(crate) fn write_dump(dir: &Path, info: *const EXCEPTION_POINTERS) -> io::Result<()> {
    let prefix =
        Prefix::from_dir(dir).ok_or_else(|| io::Error::other("the dump path is too long"))?;
    write_at(&prefix, info)
}

fn write_at(prefix: &Prefix, info: *const EXCEPTION_POINTERS) -> io::Result<()> {
    let mut wide = prefix.wide;
    let mut len = prefix.dir_len;
    for unit in "crash-".encode_utf16() {
        push(&mut wide, &mut len, unit)?;
    }
    push_hex(
        &mut wide,
        &mut len,
        // SAFETY: `GetCurrentProcessId() -> DWORD`. No pointer, no failure.
        unsafe { GetCurrentProcessId() },
    )?;
    push(&mut wide, &mut len, b'-' as u16)?;
    push_hex(
        &mut wide,
        &mut len,
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    )?;
    for unit in ".dmp".encode_utf16() {
        push(&mut wide, &mut len, unit)?;
    }
    push(&mut wide, &mut len, 0)?;
    // SAFETY: `CreateFileW(LPCWSTR, ...) -> HANDLE`. `wide` is null-terminated
    // and lives for the call. `CREATE_ALWAYS` replaces a same-named dump. No
    // security descriptor: the file inherits the log folder's ACL, which the
    // apps already restrict.
    let file = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            0x4000_0000 | 0x8000_0000, // GENERIC_WRITE | GENERIC_READ
            FILE_SHARE_READ,
            None,
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|error| io::Error::other(error.to_string()))?;
    let exception = (!info.is_null()).then_some(MINIDUMP_EXCEPTION_INFORMATION {
        // SAFETY: `GetCurrentThreadId() -> DWORD`. No pointer, no failure.
        ThreadId: unsafe { GetCurrentThreadId() },
        // The filter is given `*const`; the dump record wants `*mut` and only reads it.
        ExceptionPointers: info as *mut EXCEPTION_POINTERS,
        ClientPointers: false.into(),
    });
    let kind = MiniDumpNormal | MiniDumpWithThreadInfo;
    // SAFETY: `MiniDumpWriteDump(HANDLE, DWORD, HANDLE, MINIDUMP_TYPE,
    // PMINIDUMP_EXCEPTION_INFORMATION, ...) -> BOOL`. The process and file
    // handles are live. `exception` (when set) points at a local whose
    // `ExceptionPointers` is the pointer the filter was given, still live for
    // this call. No callback.
    let wrote = unsafe {
        MiniDumpWriteDump(
            GetCurrentProcess(),
            GetCurrentProcessId(),
            file,
            kind,
            exception.as_ref().map(|info| info as *const _),
            None,
            None,
        )
    };
    // SAFETY: `CloseHandle(HANDLE) -> BOOL`. `file` is the handle just opened
    // and not yet owned by anything else.
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(file);
    }
    wrote.map_err(|error| io::Error::other(error.to_string()))?;
    Ok(())
}

fn push(wide: &mut [u16; MAX_WIDE], len: &mut usize, unit: u16) -> io::Result<()> {
    if *len >= MAX_WIDE {
        return Err(io::Error::other("the dump path is too long"));
    }
    wide[*len] = unit;
    *len += 1;
    Ok(())
}

fn push_hex(wide: &mut [u16; MAX_WIDE], len: &mut usize, mut value: u32) -> io::Result<()> {
    let mut digits = [0u16; 8];
    let mut start = 8;
    if value == 0 {
        start = 7;
        digits[7] = b'0' as u16;
    } else {
        while value > 0 {
            start -= 1;
            let nibble = (value & 0xf) as u8;
            digits[start] = (if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + (nibble - 10)
            }) as u16;
            value >>= 4;
        }
    }
    for digit in &digits[start..] {
        push(wide, len, *digit)?;
    }
    Ok(())
}

/// Deletes the oldest `crash-*.dmp` files in `dir` until at most [`MAX_DUMPS`]
/// minus one remain, so the next crash has a slot. Other files stay.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut dumps: Vec<_> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("crash-") && name.ends_with(".dmp")
        })
        .collect();
    dumps.sort_by_key(|entry| entry.metadata().and_then(|meta| meta.modified()).ok());
    let keep = MAX_DUMPS.saturating_sub(1);
    let extra = dumps.len().saturating_sub(keep);
    for old in dumps.into_iter().take(extra) {
        let _ = std::fs::remove_file(old.path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dump of this process is a minidump: the file starts with `MDMP`.
    #[test]
    fn a_dump_of_this_process_is_a_minidump() {
        let dir = std::env::temp_dir().join(format!("jstream-dump-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the temp folder");
        write_dump(&dir, std::ptr::null()).expect("MiniDumpWriteDump");
        let file = std::fs::read_dir(&dir)
            .expect("the folder")
            .flatten()
            .find(|entry| entry.file_name().to_string_lossy().ends_with(".dmp"))
            .expect("a .dmp");
        let bytes = std::fs::read(file.path()).expect("read the dump");
        assert!(
            bytes.starts_with(b"MDMP"),
            "a minidump starts with MDMP, got {:?}",
            &bytes[..bytes.len().min(4)]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
