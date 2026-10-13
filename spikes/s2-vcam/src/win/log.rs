//! The DLL's log: one file per host process in [`super::shared::log_dir`], plus
//! `OutputDebugString` (visible in DebugView). Each line is appended with its own
//! open/write/close, so the producer can read and delete the files while Frame Server
//! still has the DLL loaded. Logging is sparse (no per-frame lines), so this is cheap.

use std::fmt::Display;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::WindowsProgramming::GetUserNameW;
use windows_core::{HSTRING, PWSTR};

use super::shared;

/// Log file prefix; the producer collects `vcam-*.log`.
pub const FILE_PREFIX: &str = "vcam-";

static DIR_READY: AtomicBool = AtomicBool::new(false);

/// Appends one line to this process's log file and to the debugger output.
pub fn log(msg: impl Display) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let pid = std::process::id();
    let line = format!(
        "{}.{:03} pid={pid} tid={:?} {msg}\r\n",
        now.as_secs(),
        now.subsec_millis(),
        std::thread::current().id()
    );
    // SAFETY: the HSTRING is a valid NUL-terminated wide string for the call's duration.
    unsafe { OutputDebugStringW(&HSTRING::from(format!("[JStream S2] {line}"))) };

    if !DIR_READY.load(Ordering::Relaxed) {
        // The producer normally creates it; create it here if the DLL got loaded first.
        let _ = shared::ensure_log_dir();
        DIR_READY.store(true, Ordering::Relaxed);
    }
    let path = shared::log_dir().join(format!("{FILE_PREFIX}{pid}.log"));
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// The full path of this process's exe.
pub fn process_path() -> String {
    let mut buf = [0u16; 1024];
    // SAFETY: `buf` is a valid, writable buffer; None means the exe module.
    let n = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n.min(buf.len())])
}

/// The account this thread runs as (for Frame Server: `LOCAL SERVICE`).
pub fn user_name() -> String {
    let mut buf = [0u16; 257];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` u16s; `len` is updated to the length written, including
    // the terminating NUL.
    match unsafe { GetUserNameW(Some(PWSTR(buf.as_mut_ptr())), &mut len) } {
        Ok(()) => String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1)]),
        Err(e) => format!("unknown ({e})"),
    }
}
