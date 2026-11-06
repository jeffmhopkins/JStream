//! The one command the page may call: a native file, save or folder dialog.
//!
//! The UI reaches it through `browse` on `__JSTREAM_DESKTOP__` (`bootstrap.rs`), not
//! through Tauri's own JavaScript API. The capability grants this command and nothing
//! else.
//!
//! Docs: apps/jstream-desktop/README.md#design

use jstream_platform_windows::{Pick, PickKind};

/// Opens a dialog and returns the path the person picked, or nothing if they cancelled
/// or the dialog could not be shown.
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
