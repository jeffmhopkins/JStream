//! Browse buttons beside a path field: a native file, save or folder dialog when the
//! page runs in the desktop app and is connected to that app's own engine.
//!
//! A browser has no dialog to open, so the button is not rendered. Connected to some
//! other engine, the button is there and disabled: a path it picked would be on this
//! machine, and the engine is not. A file picked under the collection folder is stored
//! as a `collection` path; any other file is an `absolute` one. App assets are the
//! app's own files, so that root offers no button.
//!
//! Docs: crates/jstream-ui/README.md#design

use crate::host::{ConnectedEngine, desktop, has_browse, same_engine};
use jstream_control::protocol::AssetRoot;
use leptos::prelude::*;
use leptos::task::spawn_local;

/// Which dialog a button opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowseKind {
    /// An existing file.
    Open,
    /// A file to write.
    Save,
    /// An existing folder.
    Folder,
}

impl BrowseKind {
    /// The dialog kind the desktop command takes.
    fn as_str(self) -> &'static str {
        match self {
            BrowseKind::Open => "open",
            BrowseKind::Save => "save",
            BrowseKind::Folder => "folder",
        }
    }
}

/// Where a picked file is stored. Under `collection` (the data folder) it is a
/// `collection` path with `/` separators; otherwise it is the absolute path as picked.
/// A path that is not inside the folder, or that would have to climb out of it, stays
/// absolute.
pub fn place_asset(collection: Option<&str>, picked: &str) -> (AssetRoot, String) {
    if let Some(folder) = collection
        && let Some(relative) = under_folder(folder, picked)
    {
        return (AssetRoot::Collection, relative);
    }
    (AssetRoot::Absolute, picked.to_owned())
}

/// The directory a dialog should open in for a path already in the field: the parent
/// of an absolute path, or of a collection-relative path joined onto `collection`.
pub fn start_folder(root: AssetRoot, path: &str, collection: Option<&str>) -> String {
    let full = match root {
        AssetRoot::Collection => collection
            .map(|folder| join_folder(folder, path))
            .unwrap_or_default(),
        AssetRoot::Absolute => path.to_owned(),
        _ => String::new(),
    };
    parent_of(&full)
}

/// The file name at the end of `path`, for a save dialog's suggestion.
pub fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// `picked` relative to `folder`, with `/` separators, when `picked` is a file inside
/// `folder` and the relative path does not climb out. Windows-looking paths compare
/// without regard to case, because the UI is compiled once and the desktop app runs
/// on Windows.
fn under_folder(folder: &str, picked: &str) -> Option<String> {
    let folded = windows_style(folder) || windows_style(picked);
    let folder = segments(folder);
    let picked = segments(picked);
    if folder.is_empty() || picked.len() <= folder.len() {
        return None;
    }
    if picked.contains(&"..") {
        return None;
    }
    let same = |left: &str, right: &str| {
        if folded {
            left.eq_ignore_ascii_case(right)
        } else {
            left == right
        }
    };
    if !folder
        .iter()
        .zip(&picked)
        .all(|(left, right)| same(left, right))
    {
        return None;
    }
    Some(picked[folder.len()..].join("/"))
}

/// Whether `path` is written the Windows way: a drive letter, or a backslash.
fn windows_style(path: &str) -> bool {
    path.contains('\\')
        || path
            .as_bytes()
            .get(..2)
            .is_some_and(|bytes| bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

fn segments(path: &str) -> Vec<&str> {
    path.split(['/', '\\'])
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect()
}

fn join_folder(folder: &str, path: &str) -> String {
    if path.is_empty() {
        return folder.to_owned();
    }
    let folder = folder.trim_end_matches(['/', '\\']);
    let path = path.trim_start_matches(['/', '\\']);
    let sep = if folder.contains('\\') || path.contains('\\') {
        '\\'
    } else {
        '/'
    };
    format!("{folder}{sep}{path}")
}

/// Whether the open session is the desktop app's own engine.
fn local_engine(connected: Option<ConnectedEngine>) -> bool {
    let Some(engine) = desktop() else {
        return false;
    };
    connected.is_some_and(|connected| connected.0.with(|url| same_engine(&engine.url, url)))
}

fn parent_of(path: &str) -> String {
    let path = path.trim().trim_end_matches(['/', '\\']);
    match path.rfind(['/', '\\']) {
        Some(at) if at > 0 => path[..at].to_owned(),
        _ => String::new(),
    }
}

/// A Browse… button, or nothing when the page has no desktop dialog. `shown` hides it
/// without removing the dialog (an app-asset root offers no file to pick).
///
/// `name` and `folder` are read when the button is pressed. `on_pick` receives the
/// path the person picked.
pub fn browse_button(
    kind: BrowseKind,
    accept: Vec<String>,
    title: String,
    shown: impl Fn() -> bool + Clone + Send + Sync + 'static,
    name: impl Fn() -> String + Clone + Send + Sync + 'static,
    folder: impl Fn() -> String + Clone + Send + Sync + 'static,
    on_pick: impl Fn(String) + Clone + Send + Sync + 'static,
) -> AnyView {
    if !has_browse() {
        return ().into_any();
    }
    let connected = use_context::<ConnectedEngine>();
    let busy = RwSignal::new(false);
    let remote = "This engine is on another machine, so Browse opens no dialog here.";
    let tip = title.clone();
    let open = move |_| {
        if busy.get_untracked() || !local_engine(connected) {
            return;
        }
        busy.set(true);
        let accept = accept.clone();
        let title = title.clone();
        let name = name.clone();
        let folder = folder.clone();
        let on_pick = on_pick.clone();
        let kind = kind.as_str();
        spawn_local(async move {
            let picked = crate::host::browse(kind, &accept, &title, &name(), &folder()).await;
            busy.try_set(false);
            if let Some(path) = picked {
                on_pick(path);
            }
        });
    };
    view! {
        <button
            type="button"
            class="browse"
            hidden=move || !shown()
            disabled=move || busy.get() || !local_engine(connected)
            title=move || {
                if local_engine(connected) {
                    tip.clone()
                } else {
                    remote.to_owned()
                }
            }
            on:click=open
        >
            "Browse…"
        </button>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::{file_name, place_asset, start_folder, under_folder};
    use jstream_control::protocol::AssetRoot;

    #[test]
    fn a_file_under_the_collection_is_stored_relative_to_it() {
        assert_eq!(
            place_asset(
                Some(r"C:\Users\me\AppData\Roaming\JStream"),
                r"C:\Users\me\AppData\Roaming\JStream\clips\a.png"
            ),
            (AssetRoot::Collection, "clips/a.png".into())
        );
        // The desktop app's folder and a picked path need not agree on case or separators.
        assert_eq!(
            place_asset(Some(r"C:\Studio\\"), "c:/studio/clips/a.PNG"),
            (AssetRoot::Collection, "clips/a.PNG".into())
        );
        assert_eq!(
            under_folder("/studio", "/studio/clips/a.png"),
            Some("clips/a.png".into())
        );
    }

    #[test]
    fn a_file_outside_the_collection_stays_absolute() {
        assert_eq!(
            place_asset(Some(r"C:\studio"), r"C:\studio-extra\a.png"),
            (AssetRoot::Absolute, r"C:\studio-extra\a.png".into())
        );
        assert_eq!(
            place_asset(Some(r"C:\studio"), r"D:\videos\show.mp4"),
            (AssetRoot::Absolute, r"D:\videos\show.mp4".into())
        );
        // Climbing out is not a collection path.
        assert_eq!(
            place_asset(Some(r"C:\studio"), r"C:\studio\..\secret\a.png"),
            (AssetRoot::Absolute, r"C:\studio\..\secret\a.png".into())
        );
        assert_eq!(
            place_asset(None, "/tmp/a.png"),
            (AssetRoot::Absolute, "/tmp/a.png".into())
        );
        // The folder itself is not a file inside it.
        assert_eq!(
            place_asset(Some(r"C:\studio"), r"C:\studio"),
            (AssetRoot::Absolute, r"C:\studio".into())
        );
    }

    #[test]
    fn a_posix_folder_keeps_its_case() {
        assert_eq!(under_folder("/Studio", "/studio/a.png"), None);
        assert_eq!(
            under_folder("/studio", "/studio/a.png"),
            Some("a.png".into())
        );
    }

    #[test]
    fn the_dialog_starts_in_the_file_s_folder() {
        assert_eq!(
            start_folder(AssetRoot::Absolute, r"D:\videos\show.mp4", None),
            r"D:\videos"
        );
        assert_eq!(
            start_folder(AssetRoot::Collection, "clips/a.png", Some(r"C:\studio")),
            r"C:\studio\clips"
        );
        assert_eq!(
            start_folder(AssetRoot::App, "fonts/a.ttf", Some(r"C:\studio")),
            ""
        );
        assert_eq!(file_name(r"D:\videos\show.mp4"), "show.mp4");
        assert_eq!(file_name(""), "");
    }
}
