//! Shared kernel objects: the explicit security descriptor, the control-block mapping,
//! QPC time and the log folder.

use std::path::PathBuf;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
    LocalFree,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::CreateDirectoryW;
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_core::{HSTRING, Result};

use crate::proto::{Control, MAPPING_SIZE};

/// DACL for the texture, fence and control block: full access for SYSTEM, Administrators,
/// LOCAL SERVICE (the Frame Server service account) and interactive users (the producer).
/// Protected (`P`), so nothing is inherited.
pub const SDDL_OBJECTS: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;LS)(A;;GA;;;IU)";

/// The same principals for the log folder, inherited by the files in it (`OICI`).
pub const SDDL_LOG_DIR: &str =
    "D:P(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)(A;OICI;GA;;;LS)(A;OICI;GA;;;IU)";

/// A `SECURITY_ATTRIBUTES` built from an SDDL string. Frees the descriptor on drop.
pub struct SecurityAttributes {
    sd: PSECURITY_DESCRIPTOR,
    sa: SECURITY_ATTRIBUTES,
}

impl SecurityAttributes {
    pub fn from_sddl(sddl: &str) -> Result<Self> {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` is a valid NUL-terminated wide string for the call's duration;
        // `sd` is a valid out-pointer. The descriptor is freed with LocalFree on drop.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(sddl),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
        }?;
        Ok(Self {
            sd,
            sa: SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0,
                bInheritHandle: false.into(),
            },
        })
    }

    /// Valid while `self` is alive.
    pub fn as_ptr(&self) -> *const SECURITY_ATTRIBUTES {
        &self.sa
    }
}

impl Drop for SecurityAttributes {
    fn drop(&mut self) {
        // SAFETY: `sd` was allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW
        // with LocalAlloc and is freed exactly once here.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.sd.0)));
        }
    }
}

/// Closes a Win32 handle on drop.
pub struct OwnedHandle(pub HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: we own the handle and close it exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

// SAFETY: a kernel handle is a process-wide value usable from any thread.
unsafe impl Send for OwnedHandle {}
// SAFETY: as above; `&OwnedHandle` only exposes the handle value.
unsafe impl Sync for OwnedHandle {}

/// The control block's file mapping, mapped read-write into this process.
pub struct Mapping {
    _handle: OwnedHandle,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    /// The name it was created or opened by.
    pub name: String,
    /// Whether `create` found the mapping already present (a producer restart).
    pub existed: bool,
}

// SAFETY: the view is process-wide memory, and it is only accessed through `Control`,
// whose fields are all atomics.
unsafe impl Send for Mapping {}
// SAFETY: as above.
unsafe impl Sync for Mapping {}

impl Mapping {
    /// Creates (or opens, if it already exists) a named pagefile-backed mapping.
    pub fn create(name: &str, sa: &SecurityAttributes) -> Result<Self> {
        // SAFETY: `sa` outlives the call; the name is a valid wide string.
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                Some(sa.as_ptr()),
                PAGE_READWRITE,
                0,
                MAPPING_SIZE as u32,
                &HSTRING::from(name),
            )
        }?;
        // SAFETY: reads this thread's last-error value, set by the call above.
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        Self::map(OwnedHandle(handle), name, existed)
    }

    /// Opens an existing named mapping.
    pub fn open(name: &str) -> Result<Self> {
        // SAFETY: plain call with a valid wide string.
        let handle =
            unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, &HSTRING::from(name)) }?;
        Self::map(OwnedHandle(handle), name, true)
    }

    fn map(handle: OwnedHandle, name: &str, existed: bool) -> Result<Self> {
        // SAFETY: `handle` is a live section handle of at least MAPPING_SIZE bytes.
        let view = unsafe { MapViewOfFile(handle.0, FILE_MAP_ALL_ACCESS, 0, 0, MAPPING_SIZE) };
        if view.Value.is_null() {
            return Err(windows_core::Error::from_thread());
        }
        Ok(Self {
            _handle: handle,
            view,
            name: name.to_owned(),
            existed,
        })
    }

    pub fn control(&self) -> &Control {
        // SAFETY: the view is MAPPING_SIZE bytes (>= size_of::<Control>(), checked at compile
        // time in proto), page-aligned, zero-initialised by the OS, and mapped until `self`
        // drops. `Control` is all atomics, for which every bit pattern is valid and which
        // may be mutated concurrently by the other process.
        unsafe { &*(self.view.Value as *const Control) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: `view` came from MapViewOfFile and is unmapped exactly once; no reference
        // from `control()` outlives `self`.
        unsafe {
            let _ = UnmapViewOfFile(self.view);
        }
    }
}

/// The current QPC value. QPC is system-wide, so values compare across processes.
pub fn qpc() -> u64 {
    let mut v = 0i64;
    // SAFETY: `v` is a valid out-pointer; cannot fail on Windows XP and later.
    let _ = unsafe { QueryPerformanceCounter(&mut v) };
    v as u64
}

/// QPC ticks per second.
pub fn qpc_freq() -> u64 {
    let mut v = 0i64;
    // SAFETY: `v` is a valid out-pointer; cannot fail on Windows XP and later.
    let _ = unsafe { QueryPerformanceFrequency(&mut v) };
    (v as u64).max(1)
}

/// `%ProgramData%\JStreamS2`: the DLL writes its logs here. Frame Server runs the DLL as
/// LOCAL SERVICE, whose `%TEMP%` the user can't read, so a folder both can reach is used.
pub fn log_dir() -> PathBuf {
    let base = std::env::var_os("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into());
    PathBuf::from(base).join("JStreamS2")
}

/// Creates the log folder with [`SDDL_LOG_DIR`], if it doesn't exist yet.
pub fn ensure_log_dir() -> Result<PathBuf> {
    let dir = log_dir();
    let sa = SecurityAttributes::from_sddl(SDDL_LOG_DIR)?;
    let wide = HSTRING::from(dir.as_os_str());
    // SAFETY: `sa` outlives the call; the path is a valid wide string.
    match unsafe { CreateDirectoryW(&wide, Some(sa.as_ptr())) } {
        Ok(()) => Ok(dir),
        Err(e) if e.code() == ERROR_ALREADY_EXISTS.to_hresult() => Ok(dir),
        Err(e) => Err(e),
    }
}
