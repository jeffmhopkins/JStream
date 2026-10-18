//! Registers the virtual camera's media source DLL (`jstream_vcam_windows.dll`) for the
//! Windows camera Frame Server, from an administrators-only copy, and tells whether that
//! copy is still the DLL the host ships
//! ([ADR-0066](../../../docs/adr/0066-portable-windows-distribution-v2.md)).
//!
//! Registering is optional, and only an explicit opt-in step calls [`register_vcam`].
//! Frame Server ignores per-user registration, so the step needs an elevated process
//! (ARCHITECTURE › Packaging, spike S2). Inside it, [`register_vcam`] creates
//! `%ProgramFiles%\JStream\vcam\` with a protected DACL ([`COPY_DACL`]: SYSTEM and
//! Administrators full control, LOCAL SERVICE and Users read and execute, nothing
//! inherited), copies the shipped DLL there with the same DACL, and points
//! `HKLM\Software\Classes\CLSID\{clsid}\InprocServer32` at the copy. The DLL in the
//! portable folder, which the user can usually write to, is never registered (#242). A
//! non-elevated host asks for elevation once by relaunching itself with the `runas` verb
//! ([`run_elevated`]). [`unregister_vcam`] removes the keys and the copy.
//!
//! [`vcam_registration`] needs no elevation. It compares the registered copy with the
//! shipped DLL by content, not by path, so an update whose DLL changed is reported as
//! [`VcamRegistration::Outdated`] and re-registering is a new opt-in. The path, content
//! and DACL decisions are plain data, so they are tested on every platform.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// The media source DLL's file name, as the `jstream-vcam-windows` crate builds it. The
/// host ships it beside its own executable, and the registered copy has the same name.
pub const VCAM_DLL: &str = "jstream_vcam_windows.dll";

/// The folder, under Program Files, that holds the registered copy: `JStream\vcam`.
pub fn vcam_copy_dir(program_files: &Path) -> PathBuf {
    program_files.join("JStream").join("vcam")
}

/// The registered copy's path under `program_files`.
pub fn vcam_copy_path(program_files: &Path) -> PathBuf {
    vcam_copy_dir(program_files).join(VCAM_DLL)
}

/// Where a new copy is written before it replaces the registered one, so a failed copy
/// never leaves a half-written DLL at the registered path.
pub(crate) fn staging_path(dir: &Path) -> PathBuf {
    dir.join(format!("{VCAM_DLL}.new"))
}

/// Where the replaced copy is moved, `n` counting attempts: Frame Server may still have
/// the old copy loaded, which Windows lets us rename but not delete.
pub(crate) fn retired_path(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("{VCAM_DLL}.old{n}"))
}

/// Whether `name`, a file in the copy's folder, is a leftover from an earlier step: a
/// staged or retired copy. Those are deleted when they no longer are loaded.
pub(crate) fn is_leftover(name: &str) -> bool {
    name.strip_prefix(VCAM_DLL).is_some_and(|rest| {
        rest == ".new"
            || rest
                .strip_prefix(".old")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// The machine-wide registration, compared with the DLL the host ships.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VcamRegistration {
    /// Not registered: the camera can't be used until the optional step registers it.
    Missing,
    /// The administrators-only copy is registered and has the shipped DLL's content.
    Current {
        /// The registered copy.
        copy: PathBuf,
    },
    /// Registered, but not for a copy of this DLL. Frame Server keeps loading what the
    /// registration names, if it exists, until the user opts in to register again.
    Outdated {
        /// The path the registration names.
        registered: PathBuf,
        /// Why it isn't current.
        why: Outdated,
    },
}

/// Why a registration is [`VcamRegistration::Outdated`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outdated {
    /// It names a DLL outside JStream's Program Files folder, such as a portable folder
    /// an older JStream registered before ADR-0066, or a `regsvr32` by hand.
    Elsewhere,
    /// The copy's content differs from the shipped DLL: JStream was updated, or another
    /// JStream registered its own DLL.
    Differs,
    /// The registration names the copy, but the copy is gone.
    CopyMissing,
}

/// Decides the registration from the registered `InprocServer32` path, if any, the
/// copy's expected path, and `same`, which compares the copy's content with the shipped
/// DLL's. `same` runs only when the registration names the copy. Windows paths are
/// case-insensitive and may carry the `\\?\` prefix, quotes or `/` separators, so both
/// paths are normalised first. A copy that can't be found is
/// [`Outdated::CopyMissing`]; any other read error is returned.
pub(crate) fn decide(
    registered: Option<&str>,
    copy: &Path,
    same: impl FnOnce() -> io::Result<bool>,
) -> io::Result<VcamRegistration> {
    let Some(registered) = registered.map(|r| r.trim().trim_matches('"')) else {
        return Ok(VcamRegistration::Missing);
    };
    if registered.is_empty() {
        return Ok(VcamRegistration::Missing);
    }
    let outdated = |why| VcamRegistration::Outdated {
        registered: PathBuf::from(registered),
        why,
    };
    if normalise(registered) != normalise(&copy.to_string_lossy()) {
        return Ok(outdated(Outdated::Elsewhere));
    }
    match same() {
        Ok(true) => Ok(VcamRegistration::Current {
            copy: copy.to_path_buf(),
        }),
        Ok(false) => Ok(outdated(Outdated::Differs)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(outdated(Outdated::CopyMissing)),
        Err(e) => Err(e),
    }
}

fn normalise(path: &str) -> String {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    path.replace('/', r"\").to_lowercase()
}

/// Whether the files at `a` and `b` have the same content: the lengths first, then the
/// bytes, a block at a time.
pub(crate) fn same_contents(a: &Path, b: &Path) -> io::Result<bool> {
    let (mut a, mut b) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    if a.metadata()?.len() != b.metadata()?.len() {
        return Ok(false);
    }
    let (mut x, mut y) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let n = fill(&mut a, &mut x)?;
        let m = fill(&mut b, &mut y)?;
        if n != m || x[..n] != y[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

/// Reads until `buf` is full or the file ends; returns how much was read.
fn fill(file: &mut std::fs::File, buf: &mut [u8]) -> io::Result<usize> {
    let mut read = 0;
    while read < buf.len() {
        match file.read(&mut buf[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(read)
}

/// A well-known account in the copy's DACL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trustee {
    /// `NT AUTHORITY\SYSTEM`.
    LocalSystem,
    /// `BUILTIN\Administrators`.
    Administrators,
    /// `NT AUTHORITY\LOCAL SERVICE`, the account Frame Server loads the DLL as.
    LocalService,
    /// `BUILTIN\Users`, so the user's apps can read the DLL's version.
    Users,
}

/// What a [`Trustee`] may do with the copy and its folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// `FILE_ALL_ACCESS`.
    Full,
    /// `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE`.
    ReadExecute,
}

impl Access {
    /// The access mask. An ACE on a file must hold specific rights, because generic ones
    /// are not mapped when access is checked.
    pub const fn mask(self) -> u32 {
        match self {
            // FILE_ALL_ACCESS: STANDARD_RIGHTS_REQUIRED | SYNCHRONIZE | 0x1FF.
            Access::Full => 0x001F_01FF,
            // FILE_GENERIC_READ (0x120089) | FILE_GENERIC_EXECUTE (0x1200A0).
            Access::ReadExecute => 0x0012_00A9,
        }
    }
}

/// The DACL on the copy's folder and on the copy itself. It is applied protected, so
/// nothing is inherited from Program Files, and only SYSTEM and Administrators can
/// write, rename, delete or re-permission either.
pub const COPY_DACL: [(Trustee, Access); 4] = [
    (Trustee::LocalSystem, Access::Full),
    (Trustee::Administrators, Access::Full),
    (Trustee::LocalService, Access::ReadExecute),
    (Trustee::Users, Access::ReadExecute),
];

/// What [`unregister_vcam`] removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Unregistered {
    /// The `HKLM` class keys existed and were deleted.
    pub keys: bool,
    /// The copy's folder existed and was deleted.
    pub copy: bool,
}

#[cfg(windows)]
pub use os::{
    is_elevated, program_files, register_vcam, run_elevated, unregister_vcam, vcam_registration,
};

#[cfg(windows)]
mod os {
    use std::io;
    use std::path::{Path, PathBuf};

    use jstream_platform::PlatformError;
    use jstream_vcam_proto::{CLSID_STR, FRIENDLY_NAME};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_CANCELLED, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, HANDLE, HLOCAL,
        LocalFree, WAIT_OBJECT_0, WIN32_ERROR,
    };
    use windows::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW,
        TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
    };
    use windows::Win32::Security::{
        ACE_FLAGS, ACL, CreateWellKnownSid, DACL_SECURITY_INFORMATION, GetTokenInformation,
        NO_INHERITANCE, PROTECTED_DACL_SECURITY_INFORMATION, PSID, SECURITY_MAX_SID_SIZE,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
        WELL_KNOWN_SID_TYPE, WinBuiltinAdministratorsSid, WinBuiltinUsersSid, WinLocalServiceSid,
        WinLocalSystemSid,
    };
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
        RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegSetValueExW,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken, WaitForSingleObject,
    };
    use windows::Win32::UI::Shell::{
        FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
        SHELLEXECUTEINFOW, SHGetKnownFolderPath, ShellExecuteExW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use windows::core::{HSTRING, PCWSTR, PWSTR, w};

    use super::{
        Access, COPY_DACL, Trustee, Unregistered, VcamRegistration, decide, is_leftover,
        retired_path, same_contents, staging_path, vcam_copy_dir, vcam_copy_path,
    };
    use crate::com::OsResult;

    /// How many retired-copy names to try before giving up: each one is a copy Frame
    /// Server still had loaded when it was replaced.
    const RETIRED_NAMES: u32 = 64;

    fn clsid_key() -> String {
        format!(r"Software\Classes\CLSID\{CLSID_STR}")
    }

    fn win32(call: &'static str, err: WIN32_ERROR) -> Result<(), PlatformError> {
        err.ok().os(call)
    }

    fn io_error(call: &'static str, path: &Path, e: &io::Error) -> PlatformError {
        PlatformError::Os {
            call,
            code: e.raw_os_error().map(i32::cast_unsigned),
            message: format!("{}: {e}", path.display()),
        }
    }

    /// The 64-bit Program Files folder (`FOLDERID_ProgramFiles`), where the registered
    /// copy lives under `JStream\vcam`.
    pub fn program_files() -> Result<PathBuf, PlatformError> {
        // SAFETY: a known-folder GUID constant and no token (the current user's view,
        // which for Program Files is the machine's).
        let path = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, None) }
            .map_err(|e| crate::com::os_error("SHGetKnownFolderPath", &e))?;
        // SAFETY: on success the call returns a NUL-terminated string it allocated with
        // CoTaskMemAlloc; it is read once, then freed exactly once.
        let text = unsafe { path.to_string() };
        // SAFETY: as above.
        unsafe { CoTaskMemFree(Some(path.0.cast_const().cast())) };
        let text = text.map_err(|e| PlatformError::Os {
            call: "SHGetKnownFolderPath",
            code: None,
            message: e.to_string(),
        })?;
        Ok(PathBuf::from(text))
    }

    /// Reads the default value of `HKLM\{clsid}\InprocServer32`, or `None` when the key
    /// doesn't exist.
    fn registered_path() -> Result<Option<String>, PlatformError> {
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
                return Ok(None);
            }
            if status == ERROR_MORE_DATA {
                // `bytes` now holds the size the value needs.
                buf.resize((bytes as usize).div_ceil(2) + 1, 0);
                continue;
            }
            win32("RegGetValueW", status)?;
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            return Ok(Some(String::from_utf16_lossy(&buf[..len])));
        }
    }

    /// Reads the machine-wide registration and compares the copy it names with `dll`,
    /// the media source the host ships, by content. Needs no elevation: the copy is
    /// readable by Users.
    pub fn vcam_registration(dll: &Path) -> Result<VcamRegistration, PlatformError> {
        let registered = registered_path()?;
        let copy = vcam_copy_path(&program_files()?);
        decide(registered.as_deref(), &copy, || same_contents(&copy, dll))
            .map_err(|e| io_error("comparing the registered copy", &copy, &e))
    }

    /// The optional step's elevated work, in order: create
    /// `%ProgramFiles%\JStream\vcam\` and give it the protected [`COPY_DACL`], copy
    /// `dll` there under a staging name with the same DACL, move it over the previous
    /// copy (renaming that one aside if Frame Server still has it loaded), and point the
    /// `HKLM` class keys at the copy. Returns the copy's path. The process must be
    /// elevated ([`is_elevated`]); otherwise it fails with access denied before the
    /// registry is touched.
    pub fn register_vcam(dll: &Path) -> Result<PathBuf, PlatformError> {
        if !dll.is_file() {
            return Err(PlatformError::NotFound {
                kind: "virtual camera DLL",
                id: dll.display().to_string(),
            });
        }
        let dir = vcam_copy_dir(&program_files()?);
        std::fs::create_dir_all(&dir).map_err(|e| io_error("create_dir_all", &dir, &e))?;
        set_copy_dacl(&dir, SUB_CONTAINERS_AND_OBJECTS_INHERIT)?;
        remove_leftovers(&dir);

        let staged = staging_path(&dir);
        std::fs::copy(dll, &staged).map_err(|e| io_error("copy", &staged, &e))?;
        set_copy_dacl(&staged, NO_INHERITANCE)?;
        let copy = dir.join(super::VCAM_DLL);
        if copy.exists() {
            retire(&dir, &copy)?;
        }
        std::fs::rename(&staged, &copy).map_err(|e| io_error("rename", &copy, &e))?;
        remove_leftovers(&dir);
        if !same_contents(&copy, dll).map_err(|e| io_error("comparing the copy", &copy, &e))? {
            return Err(PlatformError::InvalidState {
                what: "registering the virtual camera",
                state: "the copy under Program Files differs from the DLL it was copied from",
            });
        }

        let base = clsid_key();
        set_string(&base, None, &format!("{FRIENDLY_NAME} media source"))?;
        let inproc = format!(r"{base}\InprocServer32");
        set_string(&inproc, Some("ThreadingModel"), "Both")?;
        // The path last, so a reader never sees it before the threading model.
        set_string(&inproc, None, &copy.to_string_lossy())?;
        Ok(copy)
    }

    /// Moves the previous copy aside under the first free retired name. Windows lets a
    /// loaded DLL be renamed, but not replaced or deleted.
    fn retire(dir: &Path, copy: &Path) -> Result<(), PlatformError> {
        for n in 0..RETIRED_NAMES {
            let to = retired_path(dir, n);
            if to.exists() {
                continue;
            }
            return std::fs::rename(copy, &to).map_err(|e| io_error("rename", copy, &e));
        }
        Err(PlatformError::InvalidState {
            what: "registering the virtual camera",
            state: "too many replaced copies are still loaded; restart Windows and try again",
        })
    }

    /// Deletes staged and retired copies left by an earlier step, best effort: one Frame
    /// Server still has loaded stays until a later step.
    fn remove_leftovers(dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_str().is_some_and(is_leftover) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// Removes the `HKLM` class keys, then the copy's folder, and `%ProgramFiles%\JStream`
    /// if that is then empty. Keys first, so nothing is pointed at a missing file. The
    /// process must be elevated. A copy Frame Server still has loaded can't be deleted;
    /// that fails after the keys are gone, and running it again once the camera is
    /// closed finishes it.
    pub fn unregister_vcam() -> Result<Unregistered, PlatformError> {
        // SAFETY: plain call with a predefined root key and a valid wide string. With a
        // subkey, it deletes that key and everything under it.
        let err = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, &HSTRING::from(clsid_key())) };
        let keys = err != ERROR_FILE_NOT_FOUND;
        if keys {
            win32("RegDeleteTreeW", err)?;
        }
        let root = program_files()?;
        let dir = vcam_copy_dir(&root);
        let copy = dir.exists();
        if copy {
            std::fs::remove_dir_all(&dir).map_err(|e| io_error("remove_dir_all", &dir, &e))?;
        }
        // Only if empty: something else may share the folder.
        let _ = std::fs::remove_dir(root.join("JStream"));
        Ok(Unregistered { keys, copy })
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

    fn sid_type(trustee: Trustee) -> WELL_KNOWN_SID_TYPE {
        match trustee {
            Trustee::LocalSystem => WinLocalSystemSid,
            Trustee::Administrators => WinBuiltinAdministratorsSid,
            Trustee::LocalService => WinLocalServiceSid,
            Trustee::Users => WinBuiltinUsersSid,
        }
    }

    /// A well-known SID in a buffer of the maximum SID size.
    struct Sid([u8; SECURITY_MAX_SID_SIZE as usize]);

    impl Sid {
        fn new(trustee: Trustee) -> Result<Self, PlatformError> {
            let mut sid = Sid([0u8; SECURITY_MAX_SID_SIZE as usize]);
            let mut len = SECURITY_MAX_SID_SIZE;
            // SAFETY: the buffer holds `len` bytes, the maximum a SID needs.
            unsafe {
                CreateWellKnownSid(
                    sid_type(trustee),
                    None,
                    Some(PSID(sid.0.as_mut_ptr().cast())),
                    &mut len,
                )
            }
            .os("CreateWellKnownSid")?;
            Ok(sid)
        }

        fn as_pwstr(&mut self) -> PWSTR {
            PWSTR(self.0.as_mut_ptr().cast())
        }
    }

    /// Replaces `path`'s DACL with [`COPY_DACL`], protected so nothing is inherited.
    /// `inheritance` is what the entries pass on: the folder's reach the files created
    /// in it, the copy's reach nothing.
    fn set_copy_dacl(path: &Path, inheritance: ACE_FLAGS) -> Result<(), PlatformError> {
        let mut sids = COPY_DACL
            .iter()
            .map(|&(trustee, _)| Sid::new(trustee))
            .collect::<Result<Vec<_>, _>>()?;
        let entries: Vec<EXPLICIT_ACCESS_W> = COPY_DACL
            .iter()
            .zip(sids.iter_mut())
            .map(
                |(&(_, access), sid): (&(Trustee, Access), &mut Sid)| EXPLICIT_ACCESS_W {
                    grfAccessPermissions: access.mask(),
                    grfAccessMode: SET_ACCESS,
                    grfInheritance: inheritance,
                    Trustee: TRUSTEE_W {
                        TrusteeForm: TRUSTEE_IS_SID,
                        TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                        ptstrName: sid.as_pwstr(),
                        ..Default::default()
                    },
                },
            )
            .collect();
        let mut dacl: *mut ACL = core::ptr::null_mut();
        // SAFETY: `entries` and the SIDs they point at (`sids`) live for the call; no old
        // ACL, so the new one holds exactly these entries; `dacl` is a valid out-pointer.
        let err = unsafe { SetEntriesInAclW(Some(&entries), None, &mut dacl) };
        win32("SetEntriesInAclW", err)?;
        let _dacl = Local(dacl.cast());
        let name = HSTRING::from(path.as_os_str());
        // SAFETY: `dacl` is a valid ACL that `_dacl` frees after the call; the name lives
        // for the call. PROTECTED_DACL_SECURITY_INFORMATION drops inherited entries.
        let err = unsafe {
            SetNamedSecurityInfoW(
                &name,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(dacl),
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
    /// under `HKLM` and Program Files needs. `false` if it can't be told.
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
                    what: "the virtual camera's optional registration step",
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

        /// Reading the registration works without elevation on any machine. CI runners
        /// have no camera registered, so it is never current for a DLL in a temp folder.
        #[test]
        fn the_registration_reads() {
            let dll = std::env::temp_dir().join(VCAM_DLL);
            match super::vcam_registration(&dll) {
                Ok(VcamRegistration::Missing | VcamRegistration::Outdated { .. }) => {}
                other => panic!("unexpected: {other:?}"),
            }
        }

        /// Program Files is found, and is a folder.
        #[test]
        fn program_files_is_found() {
            let dir = super::program_files().expect("Program Files");
            assert!(dir.is_dir(), "{}", dir.display());
        }

        /// The elevation probe answers without failing.
        #[test]
        fn elevation_is_told() {
            let _ = super::is_elevated();
        }

        /// Every DACL trustee resolves to a SID.
        #[test]
        fn the_dacls_sids_are_created() {
            for (trustee, _) in super::COPY_DACL {
                super::Sid::new(trustee).expect("well-known SID");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Access, COPY_DACL, Outdated, Trustee, Unregistered, VCAM_DLL, VcamRegistration, decide,
        is_leftover, retired_path, same_contents, staging_path, vcam_copy_dir, vcam_copy_path,
    };
    use std::io;
    use std::path::{Path, PathBuf};

    const COPY: &str = r"C:\Program Files\JStream\vcam\jstream_vcam_windows.dll";

    fn same() -> io::Result<bool> {
        Ok(true)
    }

    fn decided(registered: Option<&str>, same: io::Result<bool>) -> VcamRegistration {
        decide(registered, Path::new(COPY), || same).expect("decided")
    }

    /// The tests spell the DLL's name out; it must be the crate's.
    #[test]
    fn the_dll_name_is_the_media_source_crates() {
        assert!(COPY.ends_with(VCAM_DLL));
    }

    #[test]
    fn the_copy_lives_under_program_files_jstream_vcam() {
        let root = Path::new("Program Files");
        assert_eq!(
            vcam_copy_dir(root),
            root.join("JStream").join("vcam"),
            "the folder ADR-0066 names"
        );
        assert_eq!(vcam_copy_path(root), vcam_copy_dir(root).join(VCAM_DLL));
        let dir = vcam_copy_dir(root);
        for staged in [
            staging_path(&dir),
            retired_path(&dir, 0),
            retired_path(&dir, 12),
        ] {
            assert_eq!(staged.parent(), Some(dir.as_path()), "beside the copy");
            assert_ne!(staged, vcam_copy_path(root));
        }
    }

    #[test]
    fn only_staged_and_retired_copies_are_leftovers() {
        let dir = Path::new("d");
        for leftover in [
            staging_path(dir),
            retired_path(dir, 0),
            retired_path(dir, 63),
        ] {
            let name = leftover.file_name().and_then(|n| n.to_str()).expect("name");
            assert!(is_leftover(name), "{name}");
        }
        for kept in [
            VCAM_DLL,
            "jstream_vcam_windows.dll.old",
            "jstream_vcam_windows.dll.oldx",
            "jstream_vcam_windows.dll.new2",
            "other.dll.new",
            "vcam.log",
        ] {
            assert!(!is_leftover(kept), "{kept}");
        }
    }

    #[test]
    fn unregistering_nothing_removes_nothing() {
        let none = Unregistered::default();
        assert!(!none.keys && !none.copy);
    }

    #[test]
    fn nothing_registered_is_missing() {
        assert_eq!(decided(None, same()), VcamRegistration::Missing);
        assert_eq!(decided(Some("  "), same()), VcamRegistration::Missing);
    }

    #[test]
    fn the_copy_with_the_same_content_is_current_whatever_its_spelling() {
        for registered in [
            COPY,
            r"c:\program files\jstream\VCAM\JSTREAM_VCAM_WINDOWS.DLL",
            r"\\?\C:\Program Files\JStream\vcam\jstream_vcam_windows.dll",
            "C:/Program Files/JStream/vcam/jstream_vcam_windows.dll",
            r#""C:\Program Files\JStream\vcam\jstream_vcam_windows.dll""#,
        ] {
            assert_eq!(
                decided(Some(registered), same()),
                VcamRegistration::Current {
                    copy: PathBuf::from(COPY)
                },
                "{registered}"
            );
        }
    }

    /// A registration of the portable folder's DLL (ADR-0020's layout) is never current,
    /// even when its content matches: it is the user-writable DLL #242 is about. Its
    /// content isn't even read.
    #[test]
    fn a_dll_outside_the_copy_folder_is_outdated_unread() {
        let portable = r"C:\Users\me\Downloads\JStream\jstream_vcam_windows.dll";
        let registration = decide(Some(portable), Path::new(COPY), || {
            panic!("the content of a DLL elsewhere is not compared")
        })
        .expect("decided");
        assert_eq!(
            registration,
            VcamRegistration::Outdated {
                registered: PathBuf::from(portable),
                why: Outdated::Elsewhere
            }
        );
    }

    #[test]
    fn a_copy_with_other_content_is_outdated() {
        assert_eq!(
            decided(Some(COPY), Ok(false)),
            VcamRegistration::Outdated {
                registered: PathBuf::from(COPY),
                why: Outdated::Differs
            }
        );
    }

    #[test]
    fn a_missing_copy_is_outdated_and_other_errors_are_returned() {
        assert_eq!(
            decided(Some(COPY), Err(io::ErrorKind::NotFound.into())),
            VcamRegistration::Outdated {
                registered: PathBuf::from(COPY),
                why: Outdated::CopyMissing
            }
        );
        let denied = decide(Some(COPY), Path::new(COPY), || {
            Err(io::ErrorKind::PermissionDenied.into())
        });
        assert_eq!(
            denied.map_err(|e| e.kind()),
            Err(io::ErrorKind::PermissionDenied)
        );
    }

    /// Files under a fresh folder in the temp directory, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!(
                "jstream-vcam-register-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch folder");
            Scratch(dir)
        }

        fn file(&self, name: &str, content: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, content).expect("scratch file");
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn contents_are_compared_byte_for_byte() {
        let scratch = Scratch::new("contents");
        // Larger than one 64 KiB block, so the loop runs more than once.
        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let a = scratch.file("a", &big);
        let b = scratch.file("b", &big);
        assert!(same_contents(&a, &b).expect("compared"));

        let mut late = big.clone();
        *late.last_mut().expect("bytes") ^= 1;
        let c = scratch.file("c", &late);
        assert!(
            !same_contents(&a, &c).expect("compared"),
            "last byte differs"
        );

        let d = scratch.file("d", &big[..big.len() - 1]);
        assert!(!same_contents(&a, &d).expect("compared"), "length differs");

        let empty1 = scratch.file("e1", b"");
        let empty2 = scratch.file("e2", b"");
        assert!(same_contents(&empty1, &empty2).expect("compared"));

        let missing = scratch.0.join("missing");
        assert_eq!(
            same_contents(&missing, &a).map_err(|e| e.kind()),
            Err(io::ErrorKind::NotFound)
        );
    }

    /// The DACL ADR-0066 names: SYSTEM and Administrators full control, LOCAL SERVICE
    /// and Users read and execute, and no one else.
    #[test]
    fn the_copy_dacl_is_the_adrs() {
        assert_eq!(
            COPY_DACL,
            [
                (Trustee::LocalSystem, Access::Full),
                (Trustee::Administrators, Access::Full),
                (Trustee::LocalService, Access::ReadExecute),
                (Trustee::Users, Access::ReadExecute),
            ]
        );
    }

    /// Only full control can change the file: read and execute holds no right to write,
    /// append, delete, re-permission or take ownership, and does hold read and execute.
    #[test]
    fn read_execute_grants_no_way_to_change_the_copy() {
        const FILE_READ_DATA: u32 = 0x1;
        const FILE_WRITE_DATA: u32 = 0x2;
        const FILE_APPEND_DATA: u32 = 0x4;
        const FILE_WRITE_EA: u32 = 0x10;
        const FILE_EXECUTE: u32 = 0x20;
        const FILE_DELETE_CHILD: u32 = 0x40;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        const DELETE: u32 = 0x1_0000;
        const WRITE_DAC: u32 = 0x4_0000;
        const WRITE_OWNER: u32 = 0x8_0000;
        let changes = FILE_WRITE_DATA
            | FILE_APPEND_DATA
            | FILE_WRITE_EA
            | FILE_DELETE_CHILD
            | FILE_WRITE_ATTRIBUTES
            | DELETE
            | WRITE_DAC
            | WRITE_OWNER;
        let read_execute = Access::ReadExecute.mask();
        assert_eq!(read_execute & changes, 0, "{read_execute:#x}");
        assert_eq!(
            read_execute & (FILE_READ_DATA | FILE_EXECUTE),
            FILE_READ_DATA | FILE_EXECUTE
        );
        assert_eq!(Access::Full.mask() & changes, changes);
        for (trustee, access) in COPY_DACL {
            let writes = access.mask() & changes != 0;
            let admin = matches!(trustee, Trustee::LocalSystem | Trustee::Administrators);
            assert_eq!(writes, admin, "{trustee:?}");
        }
    }
}
