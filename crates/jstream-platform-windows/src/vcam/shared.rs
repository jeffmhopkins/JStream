//! RAII wrappers for the producer's named kernel objects (R-UNSAFE-003): the security
//! descriptor they are created with, their handles, and the control block's mapping.
//! Ported from `jstream-vcam-windows`'s `vcam_smoke` example, the contract's stand-in
//! producer.

use jstream_platform::PlatformError;
use jstream_vcam_proto::{Control, MAPPING_SIZE};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
    LocalFree,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    PAGE_READWRITE, UnmapViewOfFile,
};
use windows::Win32::System::Performance::QueryPerformanceCounter;
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PWSTR};

use crate::com::OsResult;

/// Closes a handle on drop. A named object keeps its name only while a handle to it is
/// open, so the producer holds one for each.
pub(super) struct Handle(pub(super) HANDLE);

// SAFETY: a handle names a kernel object; closing it from any thread is sound, and
// `Handle` owns it exclusively.
unsafe impl Send for Handle {}
// SAFETY: `&Handle` exposes nothing but the value.
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: we own the handle and close it exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// A `SECURITY_ATTRIBUTES` built from an SDDL string. Frees the descriptor on drop.
pub(super) struct SecurityAttributes {
    sd: PSECURITY_DESCRIPTOR,
    sa: SECURITY_ATTRIBUTES,
}

impl SecurityAttributes {
    pub(super) fn from_sddl(sddl: &str) -> Result<Self, PlatformError> {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` is a valid wide string for the call; `sd` is a valid out-pointer,
        // freed with LocalFree on drop.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(sddl),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
        }
        .os("ConvertStringSecurityDescriptorToSecurityDescriptorW")?;
        Ok(Self {
            sd,
            sa: SECURITY_ATTRIBUTES {
                nLength: size_u32(std::mem::size_of::<SECURITY_ATTRIBUTES>()),
                lpSecurityDescriptor: sd.0,
                bInheritHandle: false.into(),
            },
        })
    }

    /// The attributes, valid while `self` lives.
    pub(super) fn as_ptr(&self) -> *const SECURITY_ATTRIBUTES {
        &self.sa
    }
}

impl Drop for SecurityAttributes {
    fn drop(&mut self) {
        // SAFETY: allocated with LocalAlloc by the SDDL conversion; freed exactly once.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.sd.0)));
        }
    }
}

/// The control block's mapping, created (or, if it already exists in this session,
/// opened) by the producer. Unmapped on drop.
pub(super) struct Mapping {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    _handle: Handle,
    /// Whether the mapping already existed: another producer, or the DLL holding an
    /// earlier producer's block open.
    pub(super) existed: bool,
}

// SAFETY: the view is plain shared memory, read and written only through `Control`'s
// atomics, and the handle is owned (see `Handle`).
unsafe impl Send for Mapping {}
// SAFETY: as above; `&Mapping` gives only `&Control`, which is all atomics.
unsafe impl Sync for Mapping {}

impl Mapping {
    pub(super) fn create(name: &str, sa: &SecurityAttributes) -> Result<Self, PlatformError> {
        let size = u32::try_from(MAPPING_SIZE).map_err(|_| PlatformError::Unsupported {
            what: "a control block larger than 4 GiB".into(),
        })?;
        // SAFETY: `sa` outlives the call; the name is a valid wide string.
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                Some(sa.as_ptr()),
                PAGE_READWRITE,
                0,
                size,
                &HSTRING::from(name),
            )
        }
        .os("CreateFileMappingW")?;
        // SAFETY: plain call, right after the call whose status it reads.
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let handle = Handle(handle);
        // SAFETY: `handle` is a live section handle of MAPPING_SIZE bytes.
        let view = unsafe { MapViewOfFile(handle.0, FILE_MAP_ALL_ACCESS, 0, 0, MAPPING_SIZE) };
        if view.Value.is_null() {
            return Err(PlatformError::Os {
                call: "MapViewOfFile",
                code: None,
                message: windows::core::Error::from_thread().message(),
            });
        }
        Ok(Self {
            view,
            _handle: handle,
            existed,
        })
    }

    pub(super) fn control(&self) -> &Control {
        // SAFETY: the view is MAPPING_SIZE bytes, page-aligned and mapped until `self`
        // drops; `Control` is all atomics, so any bit pattern is valid.
        unsafe { &*(self.view.Value as *const Control) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: unmapped exactly once; no `control()` reference outlives `self`.
        unsafe {
            let _ = UnmapViewOfFile(self.view);
        }
    }
}

/// This process's user SID as a string (`S-1-5-21-…`), for the contract's DACL.
pub(super) fn user_sid() -> Result<String, PlatformError> {
    let mut token = HANDLE::default();
    // SAFETY: `token` is a valid out-pointer; the pseudo-handle needs no closing.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .os("OpenProcessToken")?;
    let token = Handle(token);
    // u64s, so the buffer is aligned for TOKEN_USER.
    let mut buf = [0u64; 64];
    let mut len = 0u32;
    // SAFETY: `buf` is writable for the size passed; `len` is a valid out-pointer.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            size_u32(std::mem::size_of_val(&buf)),
            &mut len,
        )
    }
    .os("GetTokenInformation")?;
    let mut text = PWSTR::null();
    // SAFETY: the buffer starts with a TOKEN_USER (suitably aligned) whose SID points into
    // the same live buffer; `text` is a valid out-pointer, freed with LocalFree below.
    unsafe {
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        ConvertSidToStringSidW(user.User.Sid, &mut text)
    }
    .os("ConvertSidToStringSidW")?;
    // SAFETY: on success `text` is a NUL-terminated wide string.
    let sid = unsafe { text.to_string() }.map_err(|_| PlatformError::Unsupported {
        what: "a user SID that isn't UTF-16".into(),
    });
    // SAFETY: allocated by ConvertSidToStringSidW with LocalAlloc, freed once here.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
    }
    sid
}

/// This process's session id, for the `Session\<id>\…` names the DLL opens.
pub(super) fn session_id() -> Result<u32, PlatformError> {
    let mut id = 0u32;
    // SAFETY: `id` is a valid out-pointer.
    unsafe { ProcessIdToSessionId(std::process::id(), &mut id) }.os("ProcessIdToSessionId")?;
    Ok(id)
}

/// The raw QueryPerformanceCounter value, the contract's time line.
pub(super) fn qpc() -> u64 {
    let mut v = 0i64;
    // SAFETY: `v` is a valid out-pointer; QPC can't fail on supported Windows.
    let _ = unsafe { QueryPerformanceCounter(&mut v) };
    u64::try_from(v).unwrap_or(0)
}

/// A struct size as the `u32` Win32 wants. Every size passed here is small.
fn size_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}
