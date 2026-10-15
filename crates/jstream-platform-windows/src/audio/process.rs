//! Lists running processes for process loopback: a ToolHelp snapshot.

use jstream_platform::{AudioProcess, PlatformError};
use windows::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_FILES, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};

use crate::com::{OsResult, os_error};

/// A snapshot handle, closed on drop.
struct Snapshot(HANDLE);

impl Drop for Snapshot {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateToolhelp32Snapshot and is closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Every process running now, with its parent and executable name.
pub(crate) fn list() -> Result<Vec<AudioProcess>, PlatformError> {
    // SAFETY: plain call; the handle is owned by `Snapshot`.
    let snapshot = Snapshot(
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
            .os("CreateToolhelp32Snapshot")?,
    );
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut out = Vec::new();
    // SAFETY: `entry` is a valid PROCESSENTRY32W with `dwSize` set, for both calls.
    let mut next = unsafe { Process32FirstW(snapshot.0, &mut entry) };
    let mut call = "Process32FirstW";
    loop {
        match next {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_NO_MORE_FILES.to_hresult() => return Ok(out),
            Err(e) => return Err(os_error(call, &e)),
        }
        let len = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        out.push(AudioProcess {
            pid: entry.th32ProcessID,
            parent_pid: Some(entry.th32ParentProcessID).filter(|&p| p != 0),
            exe: String::from_utf16_lossy(&entry.szExeFile[..len]),
        });
        // SAFETY: as above.
        next = unsafe { Process32NextW(snapshot.0, &mut entry) };
        call = "Process32NextW";
    }
}
