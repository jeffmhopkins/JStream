//! The native file, save and folder dialogs the desktop app's Browse buttons open.
//!
//! Windows shows the common item dialog (`IFileOpenDialog` / `IFileSaveDialog`) and
//! returns the filesystem path the person picked. The extension filter and the path
//! checks around it are pure, so they are tested on every platform. Opening the dialog
//! needs a window and a person, which CI does not have.
//!
//! Docs: crates/jstream-platform-windows/README.md#purpose

#[cfg(windows)]
use jstream_platform::PlatformError;

/// What a Browse button asked the dialog to pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickKind {
    /// An existing file.
    Open,
    /// A file to write, which need not exist yet.
    Save,
    /// An existing folder.
    Folder,
}

/// One dialog. `extensions` are types without a dot (`png`); empty means every file.
/// `folder`, when it is an absolute path, is where the dialog starts. `name` is the
/// suggested file name for a save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pick {
    /// Open, save or folder.
    pub kind: PickKind,
    /// The dialog's title. Empty keeps the system title.
    pub title: String,
    /// Accepted extensions, without a leading dot.
    pub extensions: Vec<String>,
    /// A directory to open in, or empty.
    pub folder: String,
    /// A file name to offer, for a save.
    pub name: String,
}

/// Extensions a dialog filter may name: ASCII letters and digits, at most 16, no dot.
/// Anything else is dropped, and repeats after that are dropped.
pub fn dialog_extensions(accept: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for ext in accept {
        let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        let allowed =
            !ext.is_empty() && ext.len() <= 16 && ext.bytes().all(|b| b.is_ascii_alphanumeric());
        if allowed && !out.contains(&ext) {
            out.push(ext);
        }
    }
    out
}

/// The filter rows, as (label, pattern): one row for the accepted types, then
/// "All files" (`*.*`) so a person can still pick past the filter.
pub fn dialog_filters(extensions: &[String]) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    if !extensions.is_empty() {
        let label = extensions
            .iter()
            .map(|ext| ext.to_ascii_uppercase())
            .collect::<Vec<_>>()
            .join(", ");
        let pattern = extensions
            .iter()
            .map(|ext| format!("*.{ext}"))
            .collect::<Vec<_>>()
            .join(";");
        rows.push((label, pattern));
    }
    rows.push(("All files".to_owned(), "*.*".to_owned()));
    rows
}

/// `folder` when it is a path the dialog may open in: absolute, and not a UNC or
/// device path (two leading separators). Anything else is no starting folder.
pub fn starting_folder(folder: &str) -> Option<&str> {
    let folder = folder.trim();
    if folder.is_empty() || folder.starts_with("\\\\") || folder.starts_with("//") {
        return None;
    }
    let bytes = folder.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    let rooted = folder.starts_with('/') || folder.starts_with('\\');
    (drive || rooted).then_some(folder)
}

/// The file name a save dialog should offer: the last segment of `name`, or nothing
/// when that segment is empty or holds a character Win32 refuses in a file name.
pub fn suggested_name(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default().trim();
    let refused = name.is_empty()
        || name.contains(['<', '>', ':', '"', '|', '?', '*'])
        || name.chars().any(|c| c.is_control());
    if refused {
        String::new()
    } else {
        name.to_owned()
    }
}

/// Opens the dialog and returns the path the person picked. `Ok(None)` is a cancel.
/// `owner` is the window handle the dialog is modal to, or 0 for none.
///
/// # Errors
/// [`PlatformError::Os`] when the dialog cannot be created or shown, apart from the
/// person cancelling it.
#[cfg(windows)]
pub fn pick_path(owner: isize, pick: &Pick) -> Result<Option<String>, PlatformError> {
    use std::ffi::c_void;

    use windows::Win32::Foundation::{ERROR_CANCELLED, HWND, RPC_E_CHANGED_MODE};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IBindCtx,
    };
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_OVERWRITEPROMPT,
        FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, FileSaveDialog, IFileDialog,
        IFileOpenDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName,
        SIGDN_FILESYSPATH,
    };
    use windows::core::{HSTRING, Interface, PCWSTR};

    use crate::com::{OsResult, os_error, take_pwstr};

    // The UI thread is already an STA (WebView2). Joining it again is S_FALSE, which
    // `ok` accepts, and is left initialised: uninitialising would tear the webview's
    // apartment down. A thread already in another apartment keeps it.
    // SAFETY: `CoInitializeEx` with a null reserved pointer and the apartment model.
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if hr != RPC_E_CHANGED_MODE {
        hr.ok().map_err(|e| os_error("CoInitializeEx", &e))?;
    }

    let dialog: IFileDialog = if pick.kind == PickKind::Save {
        // SAFETY: activating the shell's save dialog in this process.
        let save: IFileSaveDialog =
            unsafe { CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER) }
                .os("CoCreateInstance")?;
        save.cast().os("IFileDialog")?
    } else {
        // SAFETY: activating the shell's open dialog in this process.
        let open: IFileOpenDialog =
            unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }
                .os("CoCreateInstance")?;
        open.cast().os("IFileDialog")?
    };

    // SAFETY: reading the dialog's own options so the flags below are added to them.
    let mut options = unsafe { dialog.GetOptions() }.os("IFileDialog::GetOptions")?;
    options |= FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST | FOS_NOCHANGEDIR;
    options |= match pick.kind {
        PickKind::Open => FOS_FILEMUSTEXIST,
        PickKind::Save => FOS_OVERWRITEPROMPT,
        PickKind::Folder => FOS_PICKFOLDERS | FOS_FILEMUSTEXIST,
    };
    // SAFETY: `options` is the dialog's own flags plus the ones above.
    unsafe { dialog.SetOptions(options) }.os("IFileDialog::SetOptions")?;

    let extensions = dialog_extensions(&pick.extensions);
    let filters = dialog_filters(&extensions);
    let labels: Vec<HSTRING> = filters
        .iter()
        .map(|(label, _)| HSTRING::from(label))
        .collect();
    let patterns: Vec<HSTRING> = filters
        .iter()
        .map(|(_, pattern)| HSTRING::from(pattern))
        .collect();
    let specs: Vec<COMDLG_FILTERSPEC> = labels
        .iter()
        .zip(&patterns)
        .map(|(label, pattern)| COMDLG_FILTERSPEC {
            pszName: PCWSTR(label.as_ptr()),
            pszSpec: PCWSTR(pattern.as_ptr()),
        })
        .collect();
    // SAFETY: each spec points at an HSTRING in `labels` or `patterns`, both of which
    // outlive Show. The slice's length is the number of filters.
    unsafe { dialog.SetFileTypes(&specs) }.os("IFileDialog::SetFileTypes")?;

    if !pick.title.is_empty() {
        let title = HSTRING::from(&pick.title);
        // SAFETY: `title` is a valid wide string and outlives the call.
        unsafe { dialog.SetTitle(&title) }.os("IFileDialog::SetTitle")?;
    }
    if pick.kind == PickKind::Save {
        let name = suggested_name(&pick.name);
        if !name.is_empty() {
            let name = HSTRING::from(name);
            // SAFETY: `name` is a valid wide string and outlives the call.
            unsafe { dialog.SetFileName(&name) }.os("IFileDialog::SetFileName")?;
        }
        if let Some(ext) = extensions.first() {
            let ext = HSTRING::from(ext);
            // SAFETY: `ext` is a valid wide string and outlives the call.
            unsafe { dialog.SetDefaultExtension(&ext) }.os("IFileDialog::SetDefaultExtension")?;
        }
    }
    if let Some(folder) = starting_folder(&pick.folder) {
        let folder = HSTRING::from(folder);
        // A folder that is gone is not a failure: the dialog opens in its usual place.
        // SAFETY: `folder` is a valid wide string and outlives the call; no bind context.
        let item = unsafe {
            SHCreateItemFromParsingName::<_, Option<&IBindCtx>, IShellItem>(&folder, None)
        };
        if let Ok(item) = item {
            // SAFETY: `item` is the shell item just created.
            let _ = unsafe { dialog.SetFolder(&item) };
        }
    }

    let owner =
        (owner != 0).then(|| HWND(std::ptr::without_provenance_mut::<c_void>(owner as usize)));
    // SAFETY: `owner` is the caller's window, or none when it passed 0.
    if let Err(error) = unsafe { dialog.Show(owner) } {
        // The person pressed Cancel. HRESULT_FROM_WIN32(ERROR_CANCELLED) is 0x800704C7.
        if error.code().0 == ERROR_CANCELLED.to_hresult().0 {
            return Ok(None);
        }
        return Err(os_error("IFileDialog::Show", &error));
    }
    // SAFETY: Show succeeded, so the dialog has a result.
    let item: IShellItem = unsafe { dialog.GetResult() }.os("IFileDialog::GetResult")?;
    // SAFETY: the display name is a COM-allocated wide string; take_pwstr frees it.
    let name =
        unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.os("IShellItem::GetDisplayName")?;
    let path = take_pwstr(name);
    if path.is_empty() {
        Err(PlatformError::Os {
            call: "IShellItem::GetDisplayName",
            code: None,
            message: "the dialog returned no path".to_owned(),
        })
    } else {
        Ok(Some(path))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Pick, PickKind, dialog_extensions, dialog_filters, starting_folder, suggested_name,
    };

    #[test]
    fn a_pick_keeps_what_browse_asked_for() {
        let pick = Pick {
            kind: PickKind::Open,
            title: "Choose a file".into(),
            extensions: vec!["png".into()],
            folder: r"C:\studio".into(),
            name: String::new(),
        };
        assert_eq!(pick.kind, PickKind::Open);
        assert_eq!(pick.title, "Choose a file");
        assert_eq!(pick.extensions, ["png"]);
        assert_eq!(starting_folder(&pick.folder), Some(r"C:\studio"));
        assert_eq!(suggested_name(&pick.name), "");
        assert_ne!(pick.kind, PickKind::Save);
        assert_ne!(pick.kind, PickKind::Folder);
    }

    #[test]
    fn extensions_keep_plain_types_and_drop_the_rest() {
        let accept = vec![
            "PNG".into(),
            ".jpg".into(),
            "png".into(),
            "../x".into(),
            "a b".into(),
            "".into(),
            "cube".into(),
        ];
        assert_eq!(dialog_extensions(&accept), ["png", "jpg", "cube"]);
    }

    #[test]
    fn filters_offer_the_types_and_then_every_file() {
        assert_eq!(
            dialog_filters(&dialog_extensions(&["mp4".into()])),
            [
                ("MP4".into(), "*.mp4".into()),
                ("All files".into(), "*.*".into())
            ]
        );
        assert_eq!(dialog_filters(&[]), [("All files".into(), "*.*".into())]);
    }

    #[test]
    fn a_starting_folder_is_an_absolute_path_and_not_a_share() {
        assert_eq!(
            starting_folder(r"C:\studio\clips"),
            Some(r"C:\studio\clips")
        );
        assert_eq!(starting_folder("/home/me"), Some("/home/me"));
        assert_eq!(starting_folder(r"\\server\share"), None);
        assert_eq!(starting_folder("//server/share"), None);
        assert_eq!(starting_folder("clips"), None);
        assert_eq!(starting_folder("  "), None);
    }

    #[test]
    fn a_suggested_name_is_the_last_segment() {
        assert_eq!(suggested_name(r"C:\videos\show.mp4"), "show.mp4");
        assert_eq!(suggested_name("show.mp4"), "show.mp4");
        assert_eq!(suggested_name("a:b.mp4"), "");
        assert_eq!(suggested_name(""), "");
    }
}
