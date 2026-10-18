//! Files only their user may open, for the host's secrets on Windows (#344): the control
//! token, the paired devices and the logs. Unix hosts get the same with mode `0600`.
//!
//! A new file is created with a protected DACL for SYSTEM and the user
//! ([`sddl_user_only`]), in the same `CreateFileW` call, so it never exists with its
//! folder's inherited entries, which in a shared folder (`C:\JStream`, `C:\Users\Public`,
//! a synced folder) let other local users read it. An existing file is judged by its
//! owner and DACL under the rule in [`security`](crate::security): owned by the user,
//! SYSTEM or Administrators, and no one else granted more than reading its descriptor or
//! attributes.

use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use jstream_vcam_proto::sddl_user_only;
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Security::Authorization::SE_FILE_OBJECT;
use windows::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE,
};
use windows::core::HSTRING;

use crate::vcam::shared::{SecurityAttributes, user_sid};

/// Creates a new file at `path` for reading and writing, with a DACL that lets in only
/// SYSTEM and this process's user. Fails with [`io::ErrorKind::AlreadyExists`] if a file
/// is already there, like `OpenOptions::create_new`; it never replaces one.
pub fn create_private_file(path: &Path) -> io::Result<File> {
    let sid = user_sid().map_err(io::Error::other)?;
    let sddl = sddl_user_only(&sid)
        .ok_or_else(|| io::Error::other("this user's SID can't be written as a DACL"))?;
    let sa = SecurityAttributes::from_sddl(&sddl).map_err(io::Error::other)?;
    // SAFETY: `CreateFileW(LPCWSTR, DWORD, DWORD, LPSECURITY_ATTRIBUTES, DWORD, DWORD,
    // HANDLE) -> HANDLE`: the name is a valid wide string for the call, and `sa` (whose
    // descriptor it copies onto the new file) outlives it; no template file.
    let handle = unsafe {
        CreateFileW(
            &HSTRING::from(path.as_os_str()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            Some(sa.as_ptr()),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(win32_io_error)?;
    // SAFETY: `handle` is a live file handle CreateFileW just returned, owned by nothing
    // else; the `File` takes it over and closes it once.
    Ok(File::from(unsafe {
        OwnedHandle::from_raw_handle(handle.0)
    }))
}

/// Whether `file` may hold a secret: `Err` says why not, as a phrase ("it is owned by
/// another account (…)"). `file` must be open for reading, which carries `READ_CONTROL`.
pub fn check_private_file(file: &File) -> Result<(), String> {
    let sid = user_sid().map_err(|e| format!("this user's SID is unreadable ({e})"))?;
    let sddl = crate::security::sddl_of(HANDLE(file.as_raw_handle()), SE_FILE_OBJECT)
        .map_err(|e| format!("its security descriptor is unreadable ({e})"))?;
    crate::security::check(&sddl, &sid, &[]).map_err(|refusal| refusal.to_string())
}

/// An `io::Error` whose kind matches the Win32 error, so `ERROR_FILE_EXISTS` reads as
/// `AlreadyExists`: windows-rs carries it as an `HRESULT` (`0x8007xxxx`), which
/// `io::Error::from_raw_os_error` doesn't decode.
fn win32_io_error(e: windows::core::Error) -> io::Error {
    let code = e.code().0 as u32;
    if code & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((code & 0xFFFF) as i32)
    } else {
        io::Error::other(e)
    }
}

#[cfg(test)]
mod tests {
    use super::{check_private_file, create_private_file};
    use std::io::{ErrorKind, Write};

    #[test]
    fn a_new_private_file_passes_its_own_check_and_is_never_replaced() {
        let dir = std::env::temp_dir().join(format!(
            "jstream-platform-windows-private-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch directory is created");
        let path = dir.join("secret");
        let mut file = create_private_file(&path).expect("created");
        file.write_all(b"s3cret\n").expect("written");
        assert_eq!(check_private_file(&file), Ok(()));
        drop(file);
        let reopened = std::fs::File::open(&path).expect("readable");
        assert_eq!(check_private_file(&reopened), Ok(()));
        let again = create_private_file(&path).expect_err("already there");
        assert_eq!(again.kind(), ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).expect("kept"), b"s3cret\n");
        drop(reopened);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
