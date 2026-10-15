//! The shared kernel objects the DLL opens: the control-block mapping, NT handles, and QPC
//! time. The producer (the engine) creates them, with the contract's DACL; the DLL only
//! opens them by name.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Memory::{
    FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW,
    UnmapViewOfFile,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_core::{HSTRING, Result};

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

/// The control block's file mapping, mapped read-write into this process.
pub(crate) struct Mapping {
    _handle: OwnedHandle,
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
        Ok(Self {
            _handle: handle,
            view,
        })
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
