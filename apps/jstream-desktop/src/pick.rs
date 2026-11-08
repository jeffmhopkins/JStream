//! The one command the page may call: a native file, save or folder dialog: the common
//! item dialog on Windows, AppKit's open and save panels on macOS.
//!
//! The UI reaches it through `browse` on `__JSTREAM_DESKTOP__` (`bootstrap.rs`), not
//! through Tauri's own JavaScript API. The capability grants this command and nothing
//! else.
//!
//! Docs: apps/jstream-desktop/README.md#design

#[cfg(target_os = "macos")]
use jstream_platform_macos::{Pick, PickKind};
#[cfg(windows)]
use jstream_platform_windows::{Pick, PickKind};

/// Opens a dialog and returns the path the person picked, or nothing if they cancelled
/// or the dialog could not be shown. A synchronous command, so Tauri runs it on the main
/// thread, where macOS's panels must run.
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
    let pick = Pick {
        kind,
        title,
        extensions: accept,
        folder,
        name,
    };
    // On Windows the handle is the window the page called from, so the dialog is modal
    // to it; None (0) still shows the dialog, just not owned. A macOS panel is app-modal.
    #[cfg(windows)]
    let picked = {
        let owner = window.hwnd().map(|hwnd| hwnd.0 as isize).unwrap_or(0);
        jstream_platform_windows::pick_path(owner, &pick)
    };
    #[cfg(target_os = "macos")]
    let picked = {
        let _ = window;
        jstream_platform_macos::pick_path(&pick)
    };
    match picked {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(%error, "the file dialog failed");
            None
        }
    }
}
