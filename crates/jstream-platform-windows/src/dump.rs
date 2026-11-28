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
//!
//! The dump gets the crash report's protected DACL for SYSTEM and the user
//! (ADR-0260). Building it looks up the user's SID, which the filter must not
//! do, so [`install`] builds it once and keeps it for the process; the filter
//! only passes it to `CreateFileW`. If it can't be built, install says so and
//! dumps keep the process's default DACL, as before.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::ERROR_FILE_EXISTS;
use windows::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
};
use windows::Win32::System::Diagnostics::Debug::{
    EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION, MiniDumpNormal, MiniDumpWithThreadInfo,
    MiniDumpWriteDump, SetUnhandledExceptionFilter,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId,
};
use windows::core::PCWSTR;

use crate::private_file::user_only_attributes;
use crate::vcam::shared::SecurityAttributes;

/// How many dumps the folder keeps. Each is larger than a panic report, and the
/// next crash needs a free name, so install drops the oldest past this.
const MAX_DUMPS: usize = 3;

/// A path the filter can extend without allocating. `MAX_PATH` wide characters.
const MAX_WIDE: usize = 260;

/// How many names the filter tries before it gives up: a name already taken (a
/// dump left by an earlier process with the same id) is skipped, not replaced,
/// because replacing a file keeps its old DACL.
const NAME_TRIES: u32 = 8;

/// The folder, as UTF-16 with a trailing `\`, and the dump's security
/// attributes, set once by [`install`].
struct Prefix {
    wide: [u16; MAX_WIDE],
    /// Characters of `wide` that are the directory, including the trailing `\`.
    dir_len: usize,
    /// The user-and-SYSTEM DACL, or `None` when it could not be built.
    attributes: Option<SecurityAttributes>,
}

// SAFETY: `attributes` holds a security descriptor that is written once, when it
// is built, and only read after that (by `CreateFileW`, which copies it). Nothing
// frees it while the process runs: the `Prefix` in `PREFIX` is never dropped.
unsafe impl Send for Prefix {}
// SAFETY: as above; sharing it only shares reads of an immutable descriptor.
unsafe impl Sync for Prefix {}

/// The folder the filter writes into. Set once; the OS callback has no other
/// place to keep it (ADR-0258).
static PREFIX: OnceLock<Prefix> = OnceLock::new();

/// Distinguishes two dumps from one process. The filter only adds to it.
static SEQUENCE: AtomicU32 = AtomicU32::new(0);

/// Registers the unhandled-exception filter to write dumps into `dir`, and
/// drops the oldest dumps there past [`MAX_DUMPS`]. `false` when `dir` does not
/// fit in `MAX_PATH` or a filter was already installed. The folder must exist.
///
/// The dump's DACL is built here, outside the filter. If that fails, the
/// failure is logged and dumps keep the process's default DACL.
pub fn install(dir: &Path) -> bool {
    let Some(mut prefix) = Prefix::from_dir(dir) else {
        return false;
    };
    match user_only_attributes() {
        Ok(attributes) => prefix.attributes = Some(attributes),
        Err(error) => tracing::warn!(
            %error,
            "native crash dumps get the default DACL: the user-only one could not be built"
        ),
    }
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
        Some(Self {
            wide,
            dir_len: len,
            attributes: None,
        })
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

/// Writes one dump of this process into `dir`, with the user-only DACL as
/// [`install`] builds it. `info` is the exception the filter was given, or null
/// for a dump of the running process (the test). Only the test calls it; the
/// filter goes through `write_at` with the prefix `install` kept.
#[cfg(test)]
pub(crate) fn write_dump(dir: &Path, info: *const EXCEPTION_POINTERS) -> io::Result<()> {
    let mut prefix =
        Prefix::from_dir(dir).ok_or_else(|| io::Error::other("the dump path is too long"))?;
    prefix.attributes = Some(user_only_attributes()?);
    write_at(&prefix, info)
}

fn write_at(prefix: &Prefix, info: *const EXCEPTION_POINTERS) -> io::Result<()> {
    let attributes = prefix.attributes.as_ref().map(SecurityAttributes::as_ptr);
    let mut tries = 1;
    let file = loop {
        let wide = dump_name(prefix)?;
        // SAFETY: `CreateFileW(LPCWSTR, DWORD, DWORD, LPSECURITY_ATTRIBUTES, DWORD,
        // DWORD, HANDLE) -> HANDLE`. `wide` is null-terminated and lives for the
        // call. `attributes`, when set, points into `prefix` (the one `install`
        // kept for the process, or the test's), alive for the call; CreateFileW
        // copies its descriptor onto the new file. Without it the file keeps the
        // process's default DACL. `CREATE_NEW` never replaces a file, because a
        // replaced file would keep its old DACL.
        let created = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                0x4000_0000 | 0x8000_0000, // GENERIC_WRITE | GENERIC_READ
                FILE_SHARE_READ,
                attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        };
        match created {
            Ok(file) => break file,
            // A dump an earlier process with this id left: the next number.
            Err(error) if error.code() == ERROR_FILE_EXISTS.to_hresult() && tries < NAME_TRIES => {
                tries += 1;
            }
            Err(error) => return Err(io::Error::other(error.to_string())),
        }
    };
    write_into(file, info)
}

/// The next `crash-<pid>-<n>.dmp` in the folder, null-terminated, built on the
/// stack.
fn dump_name(prefix: &Prefix) -> io::Result<[u16; MAX_WIDE]> {
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
    Ok(wide)
}

/// Writes the dump into `file`, then closes it.
fn write_into(
    file: windows::Win32::Foundation::HANDLE,
    info: *const EXCEPTION_POINTERS,
) -> io::Result<()> {
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

    /// A dump of this process is a minidump: the file starts with `MDMP`. Its DACL
    /// is protected and lets in only SYSTEM and this user, as a crash report's
    /// (ADR-0260).
    #[test]
    fn a_dump_of_this_process_is_a_minidump_only_its_user_may_open() {
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
        let opened = std::fs::File::open(file.path()).expect("open the dump");
        assert_eq!(crate::check_private_file(&opened), Ok(()));
        let sddl = crate::security::sddl_of(
            windows::Win32::Foundation::HANDLE(std::os::windows::io::AsRawHandle::as_raw_handle(
                &opened,
            )),
            windows::Win32::Security::Authorization::SE_FILE_OBJECT,
        )
        .expect("the dump's descriptor");
        let dacl = &sddl[sddl.find("D:").expect("a DACL")..];
        assert!(dacl.starts_with("D:P"), "a protected DACL, got {sddl}");
        let entries: Vec<&str> = dacl
            .split('(')
            .skip(1)
            .map(|entry| entry.trim_end_matches(')'))
            .collect();
        // The other entry is the user's: `check_private_file` lets no one else in.
        assert_eq!(entries.len(), 2, "only SYSTEM and the user, got {sddl}");
        assert!(
            entries.iter().any(|entry| entry.ends_with(";;;SY")),
            "SYSTEM is let in, got {sddl}"
        );
        drop(opened);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
