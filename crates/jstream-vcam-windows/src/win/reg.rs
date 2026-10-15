//! COM registration of the media source, machine-wide (`HKLM`), for `regsvr32`.
//!
//! It writes `HKLM\Software\Classes\CLSID\{clsid}`: a default value with the friendly
//! name, and `InprocServer32` with the DLL path and `ThreadingModel = Both` (Frame Server
//! calls the source from MTA threads). This is the layout spike S2 proved; per-user
//! registration doesn't work, because Frame Server ignores `HKCU`.

use std::path::Path;

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW,
};
use windows_core::{HSTRING, PCWSTR, Result};

use jstream_vcam_proto::{CLSID_STR, FRIENDLY_NAME};

fn clsid_key() -> String {
    format!(r"Software\Classes\CLSID\{CLSID_STR}")
}

/// Writes the CLSID's keys under `HKLM`, pointing at `dll`.
pub(crate) fn register(dll: &Path) -> Result<()> {
    let base = clsid_key();
    set_string(&base, None, &format!("{FRIENDLY_NAME} media source"))?;
    let inproc = format!(r"{base}\InprocServer32");
    set_string(&inproc, None, &dll.to_string_lossy())?;
    set_string(&inproc, Some("ThreadingModel"), "Both")
}

/// Deletes the CLSID's keys under `HKLM`. `Ok(false)` if there were none.
pub(crate) fn unregister() -> Result<bool> {
    // SAFETY: plain call with a predefined root key and a valid wide string.
    let err = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, &HSTRING::from(clsid_key())) };
    if err == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    err.ok()?;
    Ok(true)
}

fn set_string(subkey: &str, name: Option<&str>, data: &str) -> Result<()> {
    let key = Key::create(subkey)?;
    let name = name.map(HSTRING::from);
    let name_ptr = name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
    let bytes: Vec<u8> = data
        .encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    // SAFETY: `key.0` is an open key; `bytes` is a NUL-terminated UTF-16 string; the name
    // (or null, for the default value) lives for the call.
    let err = unsafe { RegSetValueExW(key.0, name_ptr, None, REG_SZ, Some(&bytes)) };
    check(err)
}

/// An open registry key, closed on drop.
struct Key(HKEY);

impl Key {
    fn create(subkey: &str) -> Result<Self> {
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
        check(err)?;
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

fn check(err: WIN32_ERROR) -> Result<()> {
    err.ok()
}
