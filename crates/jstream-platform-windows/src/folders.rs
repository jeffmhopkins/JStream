//! The person's own folders the desktop app saves into (ADR-0163): Documents, for the
//! default project, Downloads, for screenshots, and Videos, for recordings (ADR-0260). They are known folders, which a
//! person or OneDrive's backup can move (Documents is often under `OneDrive`), so they
//! are asked of the shell rather than built from `USERPROFILE`.
//!
//! Docs: crates/jstream-platform-windows/README.md#purpose

use jstream_platform::PlatformError;
use jstream_platform::paths::UserFolder;
use std::path::PathBuf;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};

/// Where `folder` is for the current user, as Explorer shows it.
///
/// # Errors
/// [`PlatformError::Os`] when the shell can't name it (a profile without one).
pub fn user_folder(folder: UserFolder) -> Result<PathBuf, PlatformError> {
    let id = match folder {
        UserFolder::Documents => &FOLDERID_Documents,
        UserFolder::Downloads => &FOLDERID_Downloads,
        UserFolder::Videos => &FOLDERID_Videos,
    };
    // SAFETY: a known-folder GUID constant and no token: the current user's folder.
    let path = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }
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
