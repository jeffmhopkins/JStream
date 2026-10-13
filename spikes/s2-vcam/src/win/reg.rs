//! COM registration of the media source, per user (`HKCU`) or machine-wide (`HKLM`).
//!
//! Both write the same layout under `Software\Classes\CLSID\{clsid}`:
//! a default value with the friendly name, and `InprocServer32` with the DLL path and
//! `ThreadingModel = Both` (Frame Server calls the source from MTA threads).

use std::path::Path;

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
    RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW,
    RegSetValueExW,
};
use windows_core::{HSTRING, PCWSTR, Result};

use crate::proto::{CLSID_STR, FRIENDLY_NAME};

/// Where the CLSID is registered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegScope {
    /// `HKCU\Software\Classes`: no admin rights needed.
    User,
    /// `HKLM\Software\Classes`: needs an elevated process.
    Machine,
}

impl RegScope {
    pub const ALL: [RegScope; 2] = [RegScope::User, RegScope::Machine];

    pub fn as_str(self) -> &'static str {
        match self {
            RegScope::User => "hkcu",
            RegScope::Machine => "hklm",
        }
    }

    fn root(self) -> HKEY {
        match self {
            RegScope::User => HKEY_CURRENT_USER,
            RegScope::Machine => HKEY_LOCAL_MACHINE,
        }
    }
}

fn clsid_key() -> String {
    format!(r"Software\Classes\CLSID\{CLSID_STR}")
}

/// Writes the CLSID's keys in `scope`, pointing at `dll`.
pub fn register(scope: RegScope, dll: &Path) -> Result<()> {
    let base = clsid_key();
    set_string(
        scope.root(),
        &base,
        None,
        &format!("{FRIENDLY_NAME} media source"),
    )?;
    let inproc = format!(r"{base}\InprocServer32");
    set_string(scope.root(), &inproc, None, &dll.to_string_lossy())?;
    set_string(scope.root(), &inproc, Some("ThreadingModel"), "Both")
}

/// Deletes the CLSID's keys in `scope`. `Ok(false)` if there were none.
pub fn unregister(scope: RegScope) -> Result<bool> {
    // SAFETY: plain call with a predefined root key and a valid wide string.
    let err = unsafe { RegDeleteTreeW(scope.root(), &HSTRING::from(clsid_key())) };
    if err == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    err.ok()?;
    Ok(true)
}

/// The DLL path registered in `scope`, if any.
pub fn registered_path(scope: RegScope) -> Option<String> {
    read_string(
        scope.root(),
        &format!(r"{}\InprocServer32", clsid_key()),
        None,
    )
}

/// Reads a `REG_SZ` value (`None` = the default value).
pub fn read_string(root: HKEY, subkey: &str, value: Option<&str>) -> Option<String> {
    let subkey = HSTRING::from(subkey);
    let value = value.map(HSTRING::from);
    let value_ptr = value
        .as_ref()
        .map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr()));
    let mut buf = [0u16; 1024];
    let mut size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: `buf` is writable for `size` bytes; the strings live for the call.
    let err = unsafe {
        RegGetValueW(
            root,
            &subkey,
            value_ptr,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if err.is_err() {
        return None;
    }
    let chars = (size as usize / 2).min(buf.len());
    let s = &buf[..chars];
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    Some(String::from_utf16_lossy(&s[..end]))
}

/// Reads a `REG_DWORD` value.
pub fn read_dword(root: HKEY, subkey: &str, value: &str) -> Option<u32> {
    let mut v = 0u32;
    let mut size = 4u32;
    // SAFETY: `v` is writable for 4 bytes; the strings live for the call.
    let err = unsafe {
        RegGetValueW(
            root,
            &HSTRING::from(subkey),
            &HSTRING::from(value),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut v as *mut u32).cast()),
            Some(&mut size),
        )
    };
    err.is_ok().then_some(v)
}

fn set_string(root: HKEY, subkey: &str, name: Option<&str>, data: &str) -> Result<()> {
    let key = create_key(root, subkey)?;
    let name = name.map(HSTRING::from);
    let name_ptr = name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
    let bytes: Vec<u8> = data
        .encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    // SAFETY: `key.0` is an open key; `bytes` is a NUL-terminated UTF-16 string.
    let err = unsafe { RegSetValueExW(key.0, name_ptr, None, REG_SZ, Some(&bytes)) };
    check(err)
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: we own the open key and close it exactly once.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn create_key(root: HKEY, subkey: &str) -> Result<Key> {
    let mut key = HKEY::default();
    // SAFETY: `key` is a valid out-pointer; the subkey string lives for the call.
    let err = unsafe {
        RegCreateKeyExW(
            root,
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
    check(err)?;
    Ok(Key(key))
}

fn check(err: WIN32_ERROR) -> Result<()> {
    err.ok()
}
