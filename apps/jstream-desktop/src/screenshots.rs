//! The window's screenshot commands (ADR-0162 §3): `save_screenshot` writes the PNG the
//! page sends to Downloads and answers its path, and `open_saved` and `show_saved` open
//! that file, or show it in its folder, for the toast's Open and Show in folder. The rules
//! are in `folders.rs`; this is the Tauri side.
//!
//! The page reaches them through `saveScreenshot`, `openSaved` and `showSaved` on
//! `__JSTREAM_DESKTOP__` (`bootstrap.rs`), and the `screenshots` capability grants these
//! three to the main window only. No engine client (an agent, a plugin or a paired device)
//! reaches them.
//!
//! Docs: apps/jstream-desktop/README.md#design

use crate::folders::{self, Saved};
use jstream_platform::paths::UserFolder;

/// The header that carries the file's name, percent-encoded (`encodeURIComponent`), since
/// the body is the PNG itself.
const NAME_HEADER: &str = "x-jstream-name";

/// Saves the PNG in the request's body to Downloads under the name in its
/// [`NAME_HEADER`], never replacing a file, and answers the path it wrote. Off the main
/// thread, so the window keeps painting while the file is written.
///
/// # Errors
/// Why it wasn't saved: no PNG or name, no Downloads folder, or the write failed.
#[tauri::command(async)]
pub(crate) fn save_screenshot(
    request: tauri::ipc::Request<'_>,
    saved: tauri::State<'_, Saved>,
) -> Result<String, String> {
    let tauri::ipc::InvokeBody::Raw(png) = request.body() else {
        return Err("the app got no picture".to_owned());
    };
    let name = request
        .headers()
        .get(NAME_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(folders::percent_decode)
        .ok_or_else(|| "the app got no file name".to_owned())?;
    let downloads = folders::user_folder(UserFolder::Downloads)
        .ok_or_else(|| "the app could not find the Downloads folder".to_owned())?;
    let path = folders::save_new(&downloads, &name, png).map_err(|error| {
        tracing::warn!(%error, "a screenshot was not saved");
        format!(
            "the app could not write it in {}: {error}",
            downloads.display()
        )
    })?;
    tracing::info!(path = %path.display(), "saved a screenshot");
    let text = path.to_string_lossy().into_owned();
    saved.add(path);
    Ok(text)
}

/// Opens `path`, a file this run saved, in the app the system picks for it. Any other
/// path is refused and logged.
#[tauri::command]
pub(crate) fn open_saved(path: String, saved: tauri::State<'_, Saved>) -> bool {
    open(&path, &saved, false)
}

/// Shows `path`, a file this run saved, in its folder. Any other path is refused and
/// logged.
#[tauri::command]
pub(crate) fn show_saved(path: String, saved: tauri::State<'_, Saved>) -> bool {
    open(&path, &saved, true)
}

fn open(path: &str, saved: &Saved, reveal: bool) -> bool {
    match saved.get(path) {
        Some(file) => {
            folders::open(&file, reveal);
            true
        }
        None => {
            tracing::warn!("the page asked to open a file this run didn't save");
            false
        }
    }
}
