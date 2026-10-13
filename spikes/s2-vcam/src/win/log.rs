//! The DLL's log: one file per host process in [`super::shared::log_dir`], plus
//! `OutputDebugString` (visible in DebugView). Each line is appended with its own
//! open/write/close, so the producer can read and delete the files while Frame Server
//! still has the DLL loaded. Logging is sparse (no per-frame lines), so this is cheap.

use std::fmt::Display;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Security::{
    GetTokenInformation, IsWellKnownSid, TOKEN_QUERY, TOKEN_USER, TokenUser, WinLocalServiceSid,
};
use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::System::WindowsProgramming::GetUserNameW;
use windows_core::{HSTRING, PWSTR};

use super::shared;
use crate::proto::{CONSUMER_KNOWN, CONSUMER_LOCAL_SERVICE, CONSUMER_SESSION0, CONSUMER_SVCHOST};

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
    // A new file (first line of this process, or the producer cleared the logs while the
    // DLL stayed loaded) always starts with the identity line, so the producer can tell
    // who hosts the DLL from any log file it finds.
    let file = match std::fs::OpenOptions::new()
        .create_new(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            let header = format!(
                "{}.{:03} pid={pid} {}\r\n",
                now.as_secs(),
                now.subsec_millis(),
                identity()
            );
            let _ = f.write_all(header.as_bytes());
            Ok(f)
        }
        Err(_) => std::fs::OpenOptions::new().append(true).open(&path),
    };
    if let Ok(mut f) = file {
        let _ = f.write_all(line.as_bytes());
    }
}

/// `LOADED dll=… process=… user=… session=… flags=… version=…`: who hosts this DLL.
/// The producer parses `process=`, `user=` and `session=` from it.
pub fn identity() -> String {
    format!(
        "LOADED dll={} process={} user={} session={} flags={:#x} version={}",
        super::dll::module_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("unknown ({e})")),
        process_path(),
        user_name(),
        session_id().map_or_else(|| "unknown".into(), |s| s.to_string()),
        host_flags(),
        env!("CARGO_PKG_VERSION"),
    )
}

/// This process's session id.
pub fn session_id() -> Option<u32> {
    let mut id = 0u32;
    // SAFETY: `id` is a valid out-pointer.
    unsafe { ProcessIdToSessionId(std::process::id(), &mut id) }
        .ok()
        .map(|()| id)
}

/// `proto::CONSUMER_*` flags for this process.
pub fn host_flags() -> u32 {
    let mut flags = CONSUMER_KNOWN;
    if session_id() == Some(0) {
        flags |= CONSUMER_SESSION0;
    }
    if process_path()
        .to_ascii_lowercase()
        .ends_with("\\svchost.exe")
    {
        flags |= CONSUMER_SVCHOST;
    }
    if runs_as_local_service() {
        flags |= CONSUMER_LOCAL_SERVICE;
    }
    flags
}

/// Whether the process token's user is LOCAL SERVICE (S-1-5-19), checked by SID so it
/// doesn't depend on the display language.
fn runs_as_local_service() -> bool {
    let mut token = HANDLE::default();
    // SAFETY: `token` is a valid out-pointer; the pseudo-handle needs no closing.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let token = shared::OwnedHandle(token);
    // TOKEN_USER plus its SID fits easily; u64 elements keep the buffer aligned.
    let mut buf = [0u64; 64];
    let mut len = 0u32;
    // SAFETY: `buf` is writable for the size passed; `len` is a valid out-pointer.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            std::mem::size_of_val(&buf) as u32,
            &mut len,
        )
    };
    if ok.is_err() {
        return false;
    }
    // SAFETY: on success the buffer starts with a TOKEN_USER (suitably aligned) whose SID
    // pointer points into the same buffer, which outlives this read.
    unsafe {
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        IsWellKnownSid(user.User.Sid, WinLocalServiceSid).as_bool()
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
