//! The one command the page may call: a native file, save or folder dialog.
//!
//! The UI reaches it through `browse` on `__JSTREAM_DESKTOP__` (`bootstrap.rs`), not
//! through Tauri's own JavaScript API. The capability grants this command and nothing
//! else.
//!
//! On Windows it is a common-item dialog owned by the calling window. On Linux it is the
//! desktop's own dialog through the FileChooser portal (`jstream-platform-linux`), on a
//! thread of Tauri's pool, so the window keeps painting while the person picks; it is
//! shown unowned, as the window doesn't hand the portal its identifier yet. On macOS it
//! is AppKit's open or save panel, app-modal (`jstream-platform-macos`).
//!
//! Docs: apps/jstream-desktop/README.md#design

#[cfg(windows)]
use jstream_platform_windows::{Pick, PickKind};

/// Opens a dialog and returns the path the person picked, or nothing if they cancelled
/// or the dialog could not be shown.
#[cfg(windows)]
#[tauri::command]
pub(crate) fn pick_path(
    window: tauri::WebviewWindow,
    kind: String,
    accept: Vec<String>,
    title: String,
    name: String,
    folder: String,
) -> Option<String> {
    let kind = match kind.as_str() {
        "open" => PickKind::Open,
        "save" => PickKind::Save,
        "folder" => PickKind::Folder,
        other => {
            tracing::warn!(kind = other, "a browse asked for an unknown dialog");
            return None;
        }
    };
    // The handle is the window the page called from, so the dialog is modal to it.
    // None (0) still shows the dialog, just not owned.
    let owner = window.hwnd().map(|hwnd| hwnd.0 as isize).unwrap_or(0);
    let pick = Pick {
        kind,
        title,
        extensions: accept,
        folder,
        name,
    };
    match jstream_platform_windows::pick_path(owner, &pick) {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(%error, "the file dialog failed");
            None
        }
    }
}

/// Opens the desktop's dialog through the FileChooser portal and returns the path the
/// person picked, or nothing if they cancelled, the dialog could not be shown, or the
/// path isn't UTF-8 (the page's fields hold strings). `async`, so Tauri runs it off the
/// main thread: the portal's dialog is another process, and the window keeps painting.
#[cfg(target_os = "linux")]
#[tauri::command(async)]
pub(crate) fn pick_path(
    kind: String,
    accept: Vec<String>,
    title: String,
    name: String,
    folder: String,
) -> Option<String> {
    use jstream_platform_linux::{FileChoice, FileChoiceKind};
    let kind = match kind.as_str() {
        "open" => FileChoiceKind::Open,
        "save" => FileChoiceKind::Save,
        "folder" => FileChoiceKind::Folder,
        other => {
            tracing::warn!(kind = other, "a browse asked for an unknown dialog");
            return None;
        }
    };
    let choice = FileChoice {
        kind,
        title,
        extensions: accept,
        folder,
        name,
    };
    match jstream_platform_linux::portal_pick_path(&choice, "") {
        Ok(Some(path)) => path
            .into_os_string()
            .into_string()
            .inspect_err(|_| tracing::warn!("the picked path is not UTF-8, so it is dropped"))
            .ok(),
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(%error, "the file dialog failed");
            None
        }
    }
}

/// Opens AppKit's open or save panel and returns the path the person picked, or nothing
/// if they cancelled or the panel could not be shown. Synchronous, so Tauri runs it on
/// the main thread, where AppKit's panels must run.
#[cfg(target_os = "macos")]
#[tauri::command]
pub(crate) fn pick_path(
    kind: String,
    accept: Vec<String>,
    title: String,
    name: String,
    folder: String,
) -> Option<String> {
    use jstream_platform_macos::{Pick, PickKind};
    let kind = match kind.as_str() {
        "open" => PickKind::Open,
        "save" => PickKind::Save,
        "folder" => PickKind::Folder,
        other => {
            tracing::warn!(kind = other, "a browse asked for an unknown dialog");
            return None;
        }
    };
    let pick = Pick {
        kind,
        title,
        extensions: accept,
        folder,
        name,
    };
    match jstream_platform_macos::pick_path(&pick) {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(%error, "the file dialog failed");
            None
        }
    }
}
