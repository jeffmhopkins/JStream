//! The DLL's log. Frame Server has no `tracing` subscriber for us, and the DLL depends only
//! on layer-0 crates and `windows` (ADR-0020), so it writes its own short lines:
//!
//! - to the debugger (`OutputDebugString`, visible in DebugView), and
//! - to `JStream\vcam\vcam.log` in the **host account's own** local app data folder. For
//!   Frame Server that is LOCAL SERVICE's profile
//!   (`C:\Windows\ServiceProfiles\LocalService\AppData\Local`), which only that account,
//!   SYSTEM and Administrators can write, so no interactive user can plant a link where
//!   the service writes (spike S2's shared `%ProgramData%` folder allowed exactly that).
//!   The file is capped at [`MAX_LOG_BYTES`]: past that it becomes `vcam.old.log`,
//!   replacing the previous one.
//!
//! Each line opens, appends and closes the file, so nothing stays open inside Frame
//! Server. Logging is sparse (no per-frame lines), so this is cheap.

use std::fmt::Display;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Security::{
    GetTokenInformation, IsWellKnownSid, TOKEN_QUERY, TOKEN_USER, TokenUser, WinLocalServiceSid,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows_core::HSTRING;

use jstream_vcam_proto::{
    CONSUMER_KNOWN, CONSUMER_LOCAL_SERVICE, CONSUMER_SESSION0, CONSUMER_SVCHOST,
};

use super::shared::OwnedHandle;

/// The log file's size limit before it is rotated.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Appends one line to the log file and to the debugger output.
pub(crate) fn log(msg: impl Display) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let line = format!(
        "{}.{:03} pid={} tid={:?} {msg}\r\n",
        now.as_secs(),
        now.subsec_millis(),
        std::process::id(),
        std::thread::current().id()
    );
    // SAFETY: the HSTRING is a valid NUL-terminated wide string for the call's duration.
    unsafe { OutputDebugStringW(&HSTRING::from(format!("[JStream vcam] {line}"))) };

    let Some(dir) = log_dir() else {
        return;
    };
    let path = dir.join("vcam.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = std::fs::rename(&path, dir.join("vcam.old.log"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// `JStream\vcam` in this account's local app data folder, created on first use. `None`
/// if the folder can't be found or created; then only the debugger output is written.
fn log_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = local_app_data()?.join("JStream").join("vcam");
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    })
    .as_ref()
}

/// This process's account's local app data folder.
fn local_app_data() -> Option<PathBuf> {
    // SAFETY: a pointer to a static GUID; no token, so the calling process's account.
    let path =
        unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None) }.ok()?;
    // SAFETY: on success `path` is a NUL-terminated wide string, read once here.
    let text = unsafe { path.to_string() };
    // SAFETY: the shell allocated `path` with CoTaskMemAlloc and nothing else frees it.
    unsafe { CoTaskMemFree(Some(path.0 as *const _)) };
    text.ok().filter(|t| !t.is_empty()).map(PathBuf::from)
}

/// `host: dll=… process=… session=… flags=… version=…`: who hosts this DLL.
pub(crate) fn identity() -> String {
    format!(
        "host: dll={} process={} session={} flags={:#x} version={}",
        super::dll::module_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("unknown ({e})")),
        process_path(),
        session_id().map_or_else(|| "unknown".into(), |s| s.to_string()),
        host_flags(),
        env!("CARGO_PKG_VERSION"),
    )
}

/// This process's session id.
pub(crate) fn session_id() -> Option<u32> {
    let mut id = 0u32;
    // SAFETY: `id` is a valid out-pointer.
    unsafe { ProcessIdToSessionId(std::process::id(), &mut id) }
        .ok()
        .map(|()| id)
}

/// The contract's `CONSUMER_*` flags for this process.
pub(crate) fn host_flags() -> u32 {
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
    let token = OwnedHandle(token);
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

/// The full path of this process's exe (lossy, and empty if it can't be read).
pub(crate) fn process_path() -> String {
    let mut buf = vec![0u16; 1024];
    // SAFETY: `buf` is a valid, writable buffer; None means the exe module.
    let n = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n.min(buf.len())])
}
