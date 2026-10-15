//! Registers the virtual camera's media source DLL (`jstream_vcam_windows.dll`) for the
//! Windows camera Frame Server, and tells whether the registration still points at it.
//!
//! Frame Server ignores per-user registration, so the DLL is registered machine-wide,
//! which needs an elevated process (ARCHITECTURE › Packaging, spike S2):
//! `HKLM\Software\Classes\CLSID\{clsid}` with `InprocServer32` naming the DLL and
//! `ThreadingModel = Both`, plus read and execute for LOCAL SERVICE (Frame Server's
//! account) on the DLL file. A non-elevated host asks for elevation once by relaunching
//! itself with the `runas` verb ([`run_elevated`]).
//!
//! A portable install can be moved, which leaves the registration naming the old path:
//! [`vcam_registration`] compares it with the DLL the host ships. That comparison is
//! plain data, so it is tested on every platform.

use std::path::{Path, PathBuf};

/// The media source DLL's file name, as the `jstream-vcam-windows` crate builds it. The
/// host ships it beside its own executable.
pub const VCAM_DLL: &str = "jstream_vcam_windows.dll";

/// Where the media source's machine-wide registration points, compared with the DLL the
/// host ships.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VcamRegistration {
    /// Not registered: the camera can't be used until it is.
    Missing,
    /// Registered for this DLL.
    Current,
    /// Registered for another path, such as the folder an install was moved from. The
    /// camera loads that DLL, if it still exists, until this one is registered.
    Moved {
        /// The path the registration names.
        registered: PathBuf,
    },
}

/// Compares the registered `InprocServer32` path, if any, with the shipped `dll`. Windows
/// paths are case-insensitive and may carry the `\\?\` prefix or `/` separators, so both
/// are normalised first.
pub(crate) fn decide(registered: Option<&str>, dll: &Path) -> VcamRegistration {
    let Some(registered) = registered.map(|r| r.trim().trim_matches('"')) else {
        return VcamRegistration::Missing;
    };
    if registered.is_empty() {
        return VcamRegistration::Missing;
    }
    if normalise(registered) == normalise(&dll.to_string_lossy()) {
        VcamRegistration::Current
    } else {
        VcamRegistration::Moved {
            registered: PathBuf::from(registered),
        }
    }
}

fn normalise(path: &str) -> String {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    path.replace('/', r"\").to_lowercase()
}

#[cfg(windows)]
pub use os::{is_elevated, register_vcam, run_elevated, vcam_registration};

#[cfg(windows)]
mod os {
    use std::path::Path;

    use jstream_platform::PlatformError;
    use jstream_vcam_proto::{CLSID_STR, FRIENDLY_NAME};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_CANCELLED, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, HANDLE, HLOCAL,
        LocalFree, WAIT_OBJECT_0, WIN32_ERROR,
    };
    use windows::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, SE_FILE_OBJECT, SetEntriesInAclW,
        SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
    };
    use windows::Win32::Security::{
        ACL, CreateWellKnownSid, DACL_SECURITY_INFORMATION, GetTokenInformation, NO_INHERITANCE,
        PSECURITY_DESCRIPTOR, PSID, SECURITY_MAX_SID_SIZE, TOKEN_ELEVATION, TOKEN_QUERY,
        TokenElevation, WinLocalServiceSid,
    };
    use windows::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
        RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken, WaitForSingleObject,
    };
    use windows::Win32::UI::Shell::{
        SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use windows::core::{HSTRING, PCWSTR, PWSTR, w};

    use super::{VcamRegistration, decide};
    use crate::com::OsResult;

    /// `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE`: an ACE on a file must hold specific
    /// rights, because generic ones are not mapped when access is checked.
    const READ_EXECUTE: u32 = 0x0012_0089 | 0x0012_00A0;

    fn clsid_key() -> String {
        format!(r"Software\Classes\CLSID\{CLSID_STR}")
    }

    fn win32(call: &'static str, err: WIN32_ERROR) -> Result<(), PlatformError> {
        err.ok().os(call)
    }

    /// Reads the machine-wide registration and compares it with `dll`, the media source
    /// the host ships. Needs no elevation.
    pub fn vcam_registration(dll: &Path) -> Result<VcamRegistration, PlatformError> {
        let subkey = HSTRING::from(format!(r"{}\InprocServer32", clsid_key()));
        let mut buf = vec![0u16; 1024];
        loop {
            let mut bytes = u32::try_from(buf.len() * 2).unwrap_or(u32::MAX);
            // SAFETY: `buf` is writable for `bytes` bytes and `bytes` is a valid in/out
            // pointer; RRF_RT_REG_SZ makes the call null-terminate what it writes. A null
            // value name reads the key's default value.
            let status = unsafe {
                RegGetValueW(
                    HKEY_LOCAL_MACHINE,
                    &subkey,
                    PCWSTR::null(),
                    RRF_RT_REG_SZ,
                    None,
                    Some(buf.as_mut_ptr().cast()),
                    Some(&mut bytes),
                )
            };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(decide(None, dll));
            }
            if status == ERROR_MORE_DATA {
                // `bytes` now holds the size the value needs.
                buf.resize((bytes as usize).div_ceil(2) + 1, 0);
                continue;
            }
            win32("RegGetValueW", status)?;
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let registered = String::from_utf16_lossy(&buf[..len]);
            return Ok(decide(Some(&registered), dll));
        }
    }

    /// Registers `dll` machine-wide: the CLSID's keys under `HKLM`, then read and execute
    /// for LOCAL SERVICE on the file. Replaces a registration for another path. The
    /// process must be elevated ([`is_elevated`]); otherwise this fails with access
    /// denied, leaving nothing half-written.
    pub fn register_vcam(dll: &Path) -> Result<(), PlatformError> {
        if !dll.is_file() {
            return Err(PlatformError::NotFound {
                kind: "virtual camera DLL",
                id: dll.display().to_string(),
            });
        }
        let base = clsid_key();
        set_string(&base, None, &format!("{FRIENDLY_NAME} media source"))?;
        let inproc = format!(r"{base}\InprocServer32");
        set_string(&inproc, Some("ThreadingModel"), "Both")?;
        // The path last, so a reader never sees it before the threading model.
        set_string(&inproc, None, &dll.to_string_lossy())?;
        grant_local_service(dll)
    }

    fn set_string(subkey: &str, name: Option<&str>, data: &str) -> Result<(), PlatformError> {
        let key = Key::create(subkey)?;
        let name = name.map(HSTRING::from);
        let name_ptr = name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
        let bytes: Vec<u8> = data
            .encode_utf16()
            .chain(Some(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        // SAFETY: `key.0` is an open key; `bytes` is a NUL-terminated UTF-16 string; the
        // name (or null, for the default value) lives for the call.
        let err = unsafe { RegSetValueExW(key.0, name_ptr, None, REG_SZ, Some(&bytes)) };
        win32("RegSetValueExW", err)
    }

    /// An open registry key, closed on drop.
    struct Key(HKEY);

    impl Key {
        fn create(subkey: &str) -> Result<Self, PlatformError> {
            let mut key = HKEY::default();
            // SAFETY: `key` is a valid out-pointer; the subkey string lives for the call.
            let err = unsafe {
                RegCreateKeyExW(
                    HKEY_LOCAL_MACHINE,
                    &HSTRING::from(subkey),
                    None,
                    PCWSTR::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_WRITE,
                    None,
                    &mut key,
                    None,
                )
            };
            win32("RegCreateKeyExW", err)?;
            Ok(Key(key))
        }
    }

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: we own the open key and close it exactly once.
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    /// Memory the security APIs allocate with `LocalAlloc`, freed on drop.
    struct Local(*mut core::ffi::c_void);

    impl Drop for Local {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: the pointer came from an API that allocates with LocalAlloc and
                // is freed exactly once.
                unsafe {
                    let _ = LocalFree(Some(HLOCAL(self.0)));
                }
            }
        }
    }

    /// Adds an allow ACE for LOCAL SERVICE (read and execute) to the file's DACL, keeping
    /// the entries it has. Frame Server runs as LOCAL SERVICE, which can't read a user's
    /// profile without it.
    fn grant_local_service(dll: &Path) -> Result<(), PlatformError> {
        let path = HSTRING::from(dll.as_os_str());
        let mut sid_buf = [0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut sid_len = SECURITY_MAX_SID_SIZE;
        let sid = PSID(sid_buf.as_mut_ptr().cast());
        // SAFETY: `sid` points at a buffer of `sid_len` bytes, the maximum a SID needs.
        unsafe { CreateWellKnownSid(WinLocalServiceSid, None, Some(sid), &mut sid_len) }
            .os("CreateWellKnownSid")?;

        let mut dacl: *mut ACL = core::ptr::null_mut();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: valid out-pointers; `dacl` points into `sd`, which `_sd` frees after
        // the last use of `dacl`.
        let err = unsafe {
            GetNamedSecurityInfoW(
                &path,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(&mut dacl),
                None,
                &mut sd,
            )
        };
        win32("GetNamedSecurityInfoW", err)?;
        let _sd = Local(sd.0);
        if dacl.is_null() {
            // A null DACL already allows everyone; a new one would allow only LOCAL SERVICE.
            return Ok(());
        }

        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: READ_EXECUTE,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: NO_INHERITANCE,
            Trustee: TRUSTEE_W {
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                ptstrName: PWSTR(sid.0.cast()),
                ..Default::default()
            },
        };
        let mut new_dacl: *mut ACL = core::ptr::null_mut();
        // SAFETY: `entry` and the SID it points at live for the call; `dacl` is the
        // file's DACL (or null), still owned by `_sd`; `new_dacl` is a valid out-pointer.
        let err = unsafe { SetEntriesInAclW(Some(&[entry]), Some(dacl), &mut new_dacl) };
        win32("SetEntriesInAclW", err)?;
        let _new = Local(new_dacl.cast());
        // SAFETY: `new_dacl` is a valid ACL that `_new` frees after the call.
        let err = unsafe {
            SetNamedSecurityInfoW(
                &path,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(new_dacl),
                None,
            )
        };
        win32("SetNamedSecurityInfoW", err)
    }

    /// A handle, closed on drop.
    struct Owned(HANDLE);

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: we own the handle and close it exactly once.
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    /// Whether this process runs elevated (an administrator's full token), as writing
    /// under `HKLM` needs. `false` if it can't be told.
    pub fn is_elevated() -> bool {
        let mut token = HANDLE::default();
        // SAFETY: the pseudo-handle needs no closing; `token` is a valid out-pointer.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
            return false;
        }
        let token = Owned(token);
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        // SAFETY: `elevation` is writable for the size passed; `len` is a valid
        // out-pointer.
        let ok = unsafe {
            GetTokenInformation(
                token.0,
                TokenElevation,
                Some((&raw mut elevation).cast()),
                u32::try_from(size_of::<TOKEN_ELEVATION>()).unwrap_or(0),
                &mut len,
            )
        };
        ok.is_ok() && elevation.TokenIsElevated != 0
    }

    /// Runs this executable again, elevated, with `parameters` as its command line, and
    /// waits for it: one UAC prompt. Returns its exit code. The child gets no window, so
    /// it reports through its exit code and the state it leaves.
    ///
    /// `parameters` is passed to the shell as one string, so callers pass fixed
    /// arguments only.
    pub fn run_elevated(parameters: &str) -> Result<u32, PlatformError> {
        let exe = std::env::current_exe().map_err(|e| PlatformError::Os {
            call: "current_exe",
            code: e.raw_os_error().map(i32::cast_unsigned),
            message: e.to_string(),
        })?;
        let file = HSTRING::from(exe.as_os_str());
        let params = HSTRING::from(parameters);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: u32::try_from(size_of::<SHELLEXECUTEINFOW>()).unwrap_or(0),
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        // SAFETY: `info` is initialised with its size; the strings it points at live for
        // the call. With SEE_MASK_NOCLOSEPROCESS the call hands us the process handle.
        if let Err(e) = unsafe { ShellExecuteExW(&mut info) } {
            if e.code() == ERROR_CANCELLED.to_hresult() {
                return Err(PlatformError::InvalidState {
                    what: "registering the virtual camera",
                    state: "the administrator prompt was declined",
                });
            }
            return Err(crate::com::os_error("ShellExecuteExW", &e));
        }
        let process = Owned(info.hProcess);
        // SAFETY: `process.0` is the child's handle, open until `process` drops.
        if unsafe { WaitForSingleObject(process.0, INFINITE) } != WAIT_OBJECT_0 {
            return Err(PlatformError::Os {
                call: "WaitForSingleObject",
                code: None,
                message: "the elevated process could not be waited for".to_owned(),
            });
        }
        let mut code = 0u32;
        // SAFETY: as above; `code` is a valid out-pointer.
        unsafe { GetExitCodeProcess(process.0, &mut code) }.os("GetExitCodeProcess")?;
        Ok(code)
    }

    #[cfg(test)]
    mod tests {
        use super::super::{VCAM_DLL, VcamRegistration};

        /// Reading the registration works without elevation on any machine; CI runners
        /// have no camera registered, but a developer's machine may.
        #[test]
        fn the_registration_reads() {
            let dll = std::env::temp_dir().join(VCAM_DLL);
            match super::vcam_registration(&dll) {
                Ok(VcamRegistration::Missing | VcamRegistration::Moved { .. }) => {}
                other => panic!("unexpected: {other:?}"),
            }
        }

        /// The elevation probe answers without failing.
        #[test]
        fn elevation_is_told() {
            let _ = super::is_elevated();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{VCAM_DLL, VcamRegistration, decide};
    use std::path::{Path, PathBuf};

    const DLL: &str = r"C:\JStream\jstream_vcam_windows.dll";

    /// The tests spell the DLL's name out; it must be the crate's.
    #[test]
    fn the_dll_name_is_the_media_source_crates() {
        assert!(DLL.ends_with(VCAM_DLL));
    }

    #[test]
    fn nothing_registered_is_missing() {
        assert_eq!(decide(None, Path::new(DLL)), VcamRegistration::Missing);
        assert_eq!(
            decide(Some("  "), Path::new(DLL)),
            VcamRegistration::Missing
        );
    }

    #[test]
    fn the_same_path_is_current_whatever_its_spelling() {
        for registered in [
            DLL,
            r"c:\jstream\JSTREAM_VCAM_WINDOWS.DLL",
            r"\\?\C:\JStream\jstream_vcam_windows.dll",
            "C:/JStream/jstream_vcam_windows.dll",
            r#""C:\JStream\jstream_vcam_windows.dll""#,
        ] {
            assert_eq!(
                decide(Some(registered), Path::new(DLL)),
                VcamRegistration::Current,
                "{registered}"
            );
        }
        assert_eq!(
            decide(
                Some(DLL),
                Path::new(r"\\?\C:\JStream\jstream_vcam_windows.dll")
            ),
            VcamRegistration::Current
        );
    }

    #[test]
    fn another_path_is_a_moved_install() {
        let old = r"D:\Old\JStream\jstream_vcam_windows.dll";
        assert_eq!(
            decide(Some(old), Path::new(DLL)),
            VcamRegistration::Moved {
                registered: PathBuf::from(old)
            }
        );
    }
}
