//! The shared kernel objects the DLL opens: the control-block mapping, NT handles, and QPC
//! time. The producer (the engine) creates them, with the contract's DACL; the DLL only
//! opens them by name.

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetSecurityInfo, SE_KERNEL_OBJECT,
};
use windows::Win32::Security::{OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID};
use windows::Win32::System::Memory::{
    FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW,
    UnmapViewOfFile,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_core::{HSTRING, PWSTR, Result};

use jstream_vcam_proto::{Control, MAPPING_SIZE};

/// Closes a Win32 handle on drop.
pub(crate) struct OwnedHandle(pub(crate) HANDLE);

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

/// Frees memory the system allocated with `LocalAlloc` on drop.
struct LocalMem(*mut core::ffi::c_void);

impl Drop for LocalMem {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the system allocated the block with LocalAlloc for us, and it is
            // freed exactly once, after every pointer into it is last used.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.0)));
            }
        }
    }
}

/// `sid` as a string (`S-1-5-…`).
///
/// # Safety
/// `sid` must point to a valid SID for the duration of the call.
pub(crate) unsafe fn sid_string(sid: PSID) -> Result<String> {
    let mut text = PWSTR::null();
    // SAFETY: the caller guarantees `sid`; `text` is a valid out-pointer.
    unsafe { ConvertSidToStringSidW(sid, &mut text) }?;
    let _free = LocalMem(text.0.cast());
    // SAFETY: on success `text` is a NUL-terminated wide string, freed only by `_free`
    // after this read.
    Ok(unsafe { text.to_string() }?)
}

/// The owner SID, as a string, of the kernel object `handle` opens (a section, a shared
/// texture or a fence). The handle must carry `READ_CONTROL`.
pub(crate) fn owner_sid(handle: &OwnedHandle) -> Result<String> {
    let mut owner = PSID::default();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `GetSecurityInfo(HANDLE, SE_OBJECT_TYPE, SECURITY_INFORMATION, PSID*, PSID*,
    // PACL*, PACL*, PSECURITY_DESCRIPTOR*) -> DWORD`: `handle` is live while borrowed; the
    // out-pointers are valid. On success `owner` points into `sd`, which `_free` releases
    // after the last use.
    let err = unsafe {
        GetSecurityInfo(
            handle.0,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            Some(&mut sd),
        )
    };
    let _free = LocalMem(sd.0);
    err.to_hresult().ok()?;
    if owner.0.is_null() {
        return Err(windows_core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            "the object has no owner",
        ));
    }
    // SAFETY: `owner` points into `sd`, which is still allocated.
    unsafe { sid_string(owner) }
}

/// The control block's file mapping, mapped read-write into this process.
pub(crate) struct Mapping {
    handle: OwnedHandle,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

// SAFETY: the view is process-wide memory, only reached through `Control`, whose fields are
// all atomics.
unsafe impl Send for Mapping {}

impl Mapping {
    /// Opens an existing named mapping of at least [`MAPPING_SIZE`] bytes.
    pub(crate) fn open(name: &str) -> Result<Self> {
        // SAFETY: plain call with a valid wide string.
        let handle =
            unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, &HSTRING::from(name)) }?;
        let handle = OwnedHandle(handle);
        // SAFETY: `handle` is a live section handle. Mapping MAPPING_SIZE bytes fails if the
        // section is smaller, so a successful view is at least that large.
        let view = unsafe { MapViewOfFile(handle.0, FILE_MAP_ALL_ACCESS, 0, 0, MAPPING_SIZE) };
        if view.Value.is_null() {
            return Err(windows_core::Error::from_thread());
        }
        Ok(Self { handle, view })
    }

    /// The mapping's owner SID, as a string. The contract's DACL grants LOCAL SERVICE
    /// `GA`, so the handle opened with `FILE_MAP_ALL_ACCESS` carries `READ_CONTROL`.
    pub(crate) fn owner_sid(&self) -> Result<String> {
        owner_sid(&self.handle)
    }

    pub(crate) fn control(&self) -> &Control {
        // SAFETY: the view is MAPPING_SIZE bytes (at least `size_of::<Control>()`, which the
        // contract asserts at compile time), page-aligned, and mapped until `self` drops.
        // `Control` is all atomics, for which every bit pattern is valid and which the
        // other process may change concurrently.
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
pub(crate) fn qpc() -> u64 {
    let mut v = 0i64;
    // SAFETY: `v` is a valid out-pointer; cannot fail on Windows XP and later.
    let _ = unsafe { QueryPerformanceCounter(&mut v) };
    u64::try_from(v).unwrap_or(0)
}

/// QPC ticks per second.
pub(crate) fn qpc_freq() -> u64 {
    let mut v = 0i64;
    // SAFETY: `v` is a valid out-pointer; cannot fail on Windows XP and later.
    let _ = unsafe { QueryPerformanceFrequency(&mut v) };
    u64::try_from(v).unwrap_or(1).max(1)
}
