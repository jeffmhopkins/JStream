//! The native file, save and folder dialogs the desktop app's Browse buttons open.
//!
//! macOS shows AppKit's open and save panels (`NSOpenPanel`, `NSSavePanel`), app-modal,
//! and returns the filesystem path the person picked. They run on the main thread, where
//! the window's command calls them. The extension list and the path checks around them
//! are pure, so they are tested on every platform; the same rules as
//! `jstream-platform-windows`' dialog, which this mirrors. Showing a panel needs a window
//! and a person, which CI does not have.
//!
//! Docs: crates/jstream-platform-macos/README.md#purpose

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
    /// The panel's title. Empty keeps the system's.
    pub title: String,
    /// Accepted extensions, without a leading dot.
    pub extensions: Vec<String>,
    /// A directory to open in, or empty.
    pub folder: String,
    /// A file name to offer, for a save.
    pub name: String,
}

/// Extensions a panel may accept: ASCII letters and digits, at most 16, no dot, in lower
/// case. Anything else is dropped, and repeats after that are dropped.
pub fn panel_extensions(accept: &[String]) -> Vec<String> {
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

/// `folder` when it is a path the panel may open in: absolute, and not `//`-prefixed.
/// Anything else is no starting folder.
pub fn panel_folder(folder: &str) -> Option<&str> {
    let folder = folder.trim();
    (folder.starts_with('/') && !folder.starts_with("//")).then_some(folder)
}

/// The file name a save panel should offer: the last segment of `name`, or nothing when
/// that segment is empty, `.` or `..`, or holds a colon (which Finder shows as a slash)
/// or a control character.
pub fn panel_name(name: &str) -> String {
    let name = name.rsplit('/').next().unwrap_or_default().trim();
    let refused = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(':')
        || name.chars().any(|c| c.is_control());
    if refused {
        String::new()
    } else {
        name.to_owned()
    }
}

#[cfg(target_os = "macos")]
pub use panel::pick_path;

#[cfg(target_os = "macos")]
mod panel {
    use super::{Pick, PickKind, panel_extensions, panel_folder, panel_name};
    use jstream_platform::PlatformError;
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSModalResponseOK, NSOpenPanel, NSSavePanel};
    use objc2_foundation::{NSArray, NSString, NSURL};

    /// Shows the panel, app-modal, and returns the path the person picked. `Ok(None)` is
    /// a cancel.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when it isn't called on the main thread, where AppKit's
    /// panels run.
    pub fn pick_path(pick: &Pick) -> Result<Option<String>, PlatformError> {
        let mtm = MainThreadMarker::new().ok_or_else(|| PlatformError::Os {
            call: "NSOpenPanel",
            code: None,
            message: "a file dialog opens only on the main thread".into(),
        })?;
        let panel: Retained<NSSavePanel> = match pick.kind {
            PickKind::Open | PickKind::Folder => {
                let open = NSOpenPanel::openPanel(mtm);
                let folder = pick.kind == PickKind::Folder;
                open.setCanChooseFiles(!folder);
                open.setCanChooseDirectories(folder);
                open.setAllowsMultipleSelection(false);
                Retained::into_super(open)
            }
            PickKind::Save => {
                let save = NSSavePanel::savePanel(mtm);
                let name = panel_name(&pick.name);
                if !name.is_empty() {
                    save.setNameFieldStringValue(&NSString::from_str(&name));
                }
                save
            }
        };
        panel.setCanCreateDirectories(pick.kind != PickKind::Open);
        if !pick.title.is_empty() {
            let title = NSString::from_str(&pick.title);
            // The title bar of a panel is often hidden; the message shows inside it.
            panel.setTitle(Some(&title));
            panel.setMessage(Some(&title));
        }
        let extensions = panel_extensions(&pick.extensions);
        if pick.kind != PickKind::Folder && !extensions.is_empty() {
            let types: Vec<Retained<NSString>> = extensions
                .iter()
                .map(|ext| NSString::from_str(ext))
                .collect();
            // `allowedContentTypes` needs UniformTypeIdentifiers, which no crate in the
            // tree binds; the extension list still works on every supported release.
            #[expect(deprecated, reason = "the replacement needs UTType bindings")]
            panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&types)));
        }
        if let Some(folder) = panel_folder(&pick.folder) {
            let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(folder), true);
            panel.setDirectoryURL(Some(&url));
        }
        if panel.runModal() != NSModalResponseOK {
            return Ok(None);
        }
        Ok(panel
            .URL()
            .and_then(|url| url.path())
            .map(|path| path.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::{Pick, PickKind, panel_extensions, panel_folder, panel_name};

    #[test]
    fn a_pick_keeps_what_browse_asked_for() {
        let pick = Pick {
            kind: PickKind::Save,
            title: "Save the project".into(),
            extensions: vec!["json".into()],
            folder: "/Users/studio".into(),
            name: "show.json".into(),
        };
        assert_eq!(panel_folder(&pick.folder), Some("/Users/studio"));
        assert_eq!(panel_name(&pick.name), "show.json");
        assert_eq!(panel_extensions(&pick.extensions), ["json"]);
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
        assert_eq!(panel_extensions(&accept), ["png", "jpg", "cube"]);
    }

    #[test]
    fn only_an_absolute_folder_starts_the_panel() {
        assert_eq!(panel_folder(" /Volumes/Media "), Some("/Volumes/Media"));
        for bad in ["", "studio", "~/Movies", "//server/share", r"C:\studio"] {
            assert_eq!(panel_folder(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_suggested_name_is_its_last_segment() {
        assert_eq!(panel_name("/Users/me/show.json"), "show.json");
        for bad in ["", "dir/", "..", ".", "a:b", "line\nbreak"] {
            assert_eq!(panel_name(bad), "", "{bad:?}");
        }
    }
}
