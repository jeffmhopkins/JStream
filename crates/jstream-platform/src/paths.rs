//! Where JStream keeps its state (ADR-0020):
//! the platform's per-user data folder (`%APPDATA%\JStream` on Windows), or a `data`
//! folder beside the exe when a `portable` marker file sits next to it, so the whole app
//! can live on a USB stick. Every host (the desktop app, the daemon) finds it here, so
//! they agree on one folder.
//!
//! It also finds the person's own folders the desktop app saves into, which Explorer and
//! Finder show (ADR-0163): Documents, for the default project, and Downloads, for
//! screenshots. On macOS and Linux they come from here; on Windows a known-folder call
//! in `jstream-platform-windows` finds them, since OneDrive can move Documents.
//!
//! The folders are found from the environment, not a crate: these cases don't pay for
//! `dirs`, which adds `dirs-sys` and, on Windows, `windows-sys`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The marker file beside the exe that makes JStream portable. Its contents are ignored.
pub const PORTABLE_MARKER: &str = "portable";

/// The folder, beside the exe, that holds the state of a portable JStream.
pub const PORTABLE_DATA_DIR: &str = "data";

/// The folder in the data folder that holds the WGSL effect packs the engine loads
/// and hot-reloads (ADR-0035), one pack folder each. The desktop app and the daemon both
/// watch it.
pub const EFFECTS_DIR: &str = "effects";

/// The file in the data folder that keeps the UI settings (ADR-0084), such as the web
/// UI's dock. The desktop app and the daemon both keep them there, so both show the
/// same workspace.
pub const UI_SETTINGS_FILE: &str = "ui-settings.json";

/// The file in the data folder that keeps how many scenes the studio keeps warm
/// (`studio.set_warm`, ADR-0117 §7). The desktop app and the daemon both keep it there,
/// so both keep the same number.
pub const WARM_SCENES_FILE: &str = "warm-scenes.json";

/// The file in the data folder that keeps the projects opened most recently (ADR-0121
/// §6). Only the desktop app keeps one; the daemon opens the file it is given.
pub const RECENT_COLLECTIONS_FILE: &str = "recent-collections.json";

/// The folder the desktop app keeps its default project in, inside the person's
/// Documents folder (ADR-0163 §1): `Documents\JStream` on Windows, `~/Documents/JStream`
/// on macOS and Linux.
pub const PROJECTS_DIR: &str = "JStream";

/// The desktop app's default project file in [`PROJECTS_DIR`] (ADR-0163 §1).
pub const DEFAULT_PROJECT: &str = "My project.json";

/// One of the person's own folders, which the desktop app saves into (ADR-0163).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserFolder {
    /// Documents: the default project's folder is in it.
    Documents,
    /// Downloads: screenshots are saved there, where a browser's download goes.
    Downloads,
}

/// Whether the exe in `exe_dir` is portable: a [`PORTABLE_MARKER`] file sits beside it.
/// A portable JStream keeps its default project in its data folder too, so the whole
/// app stays on its USB stick (ADR-0163 §1).
pub fn is_portable(exe_dir: Option<&Path>) -> bool {
    exe_dir.is_some_and(|dir| dir.join(PORTABLE_MARKER).is_file())
}

/// `folder` on macOS and Linux, from the environment through `var` and, on Linux, the
/// `user-dirs.dirs` file `read` returns the text of (`None` when there is none).
/// `None` on Windows, whose known-folder call finds it instead, and when `HOME` isn't
/// absolute.
///
/// macOS: `~/Documents` or `~/Downloads`. Linux: `XDG_DOCUMENTS_DIR` or
/// `XDG_DOWNLOAD_DIR` in `$XDG_CONFIG_HOME/user-dirs.dirs` (else
/// `~/.config/user-dirs.dirs`), as `xdg-user-dirs` writes them, else `~/Documents` or
/// `~/Downloads`. A line that names the home folder itself, which the spec uses for a
/// folder turned off, or a path that isn't absolute, is ignored.
pub fn user_folder_for(
    folder: UserFolder,
    var: impl Fn(&str) -> Option<OsString>,
    read: impl Fn(&Path) -> Option<String>,
) -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    let home = var("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())?;
    let (name, key) = match folder {
        UserFolder::Documents => ("Documents", "XDG_DOCUMENTS_DIR"),
        UserFolder::Downloads => ("Downloads", "XDG_DOWNLOAD_DIR"),
    };
    if cfg!(target_os = "macos") {
        return Some(home.join(name));
    }
    let config = var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let listed = read(&config.join("user-dirs.dirs"))
        .and_then(|text| xdg_user_dir(&text, key, &home))
        .filter(|dir| *dir != home);
    Some(listed.unwrap_or_else(|| home.join(name)))
}

/// [`user_folder_for`] with the process environment and the file system.
pub fn user_folder(folder: UserFolder) -> Option<PathBuf> {
    user_folder_for(
        folder,
        |name| std::env::var_os(name),
        |path| std::fs::read_to_string(path).ok(),
    )
}

/// The folder `key` names in a `user-dirs.dirs` file's `text`: a line
/// `KEY="$HOME/path"` or `KEY="/absolute/path"`, the only two forms the file holds.
/// The last line for `key` wins, as the shell that sources the file would have it.
fn xdg_user_dir(text: &str, key: &str, home: &Path) -> Option<PathBuf> {
    text.lines().rev().find_map(|line| {
        let value = line
            .trim()
            .strip_prefix(key)?
            .trim_start()
            .strip_prefix('=')?;
        let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
        let path = match value.strip_prefix("$HOME") {
            Some("") => home.to_path_buf(),
            Some(rest) => home.join(rest.strip_prefix('/')?),
            None => PathBuf::from(value),
        };
        path.is_absolute().then_some(path)
    })
}

/// The data folder of this process: [`data_dir_for`] with the running exe's folder and
/// the process environment. `None` when it can't be found (no usable `APPDATA` on
/// Windows, no usable `HOME` elsewhere, and no portable marker).
pub fn data_dir() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_owned));
    data_dir_for(exe_dir.as_deref(), |name| std::env::var_os(name))
}

/// The data folder for an exe in `exe_dir`, reading the environment through `var`.
/// `None` when it can't be found.
///
/// Portable when `exe_dir` holds a [`PORTABLE_MARKER`] file: then
/// `<exe_dir>\`[`PORTABLE_DATA_DIR`]. Otherwise the platform's data folder:
/// `%APPDATA%\JStream` on Windows, `~/Library/Application Support/JStream` on macOS,
/// `$XDG_DATA_HOME/jstream` (else `~/.local/share/jstream`) elsewhere. Only absolute
/// variables count: a relative one would resolve against the working folder, often the
/// exe's, which ADR-0020 keeps unwritten without the portable marker.
pub fn data_dir_for(
    exe_dir: Option<&Path>,
    var: impl Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    if let Some(exe_dir) = exe_dir
        && exe_dir.join(PORTABLE_MARKER).is_file()
    {
        return Some(exe_dir.join(PORTABLE_DATA_DIR));
    }
    let absolute = |name: &str| {
        var(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if cfg!(windows) {
        absolute("APPDATA").map(|dir| dir.join("JStream"))
    } else if cfg!(target_os = "macos") {
        absolute("HOME").map(|home| home.join("Library/Application Support/JStream"))
    } else {
        absolute("XDG_DATA_HOME")
            .map(|dir| dir.join("jstream"))
            .or_else(|| absolute("HOME").map(|home| home.join(".local/share/jstream")))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PORTABLE_DATA_DIR, PORTABLE_MARKER, UserFolder, data_dir_for, is_portable, user_folder_for,
    };
    use std::ffi::OsString;
    use std::fs;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs: Vec<(String, OsString)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn a_portable_marker_keeps_state_beside_the_exe() {
        let exe =
            std::env::temp_dir().join(format!("jstream-platform-paths-{}", std::process::id()));
        let _ = fs::remove_dir_all(&exe);
        fs::create_dir_all(&exe).unwrap_or_else(|e| panic!("{e}"));
        let everywhere = env(&[
            ("APPDATA", "/appdata"),
            ("HOME", "/home/me"),
            ("XDG_DATA_HOME", "/xdg"),
        ]);
        let portable = Some(exe.join(PORTABLE_DATA_DIR));
        assert_ne!(data_dir_for(Some(&exe), &everywhere), portable);
        // A folder named like the marker doesn't count.
        fs::create_dir(exe.join(PORTABLE_MARKER)).unwrap_or_else(|e| panic!("{e}"));
        assert_ne!(data_dir_for(Some(&exe), &everywhere), portable);
        fs::remove_dir(exe.join(PORTABLE_MARKER)).unwrap_or_else(|e| panic!("{e}"));
        fs::write(exe.join(PORTABLE_MARKER), "").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(data_dir_for(Some(&exe), &everywhere), portable);
        assert!(is_portable(Some(&exe)));
        assert!(!is_portable(None));
        // The marker wins even when the environment names no folder.
        assert_eq!(data_dir_for(Some(&exe), env(&[])), portable);
        let _ = fs::remove_dir_all(&exe);
    }

    #[cfg(windows)]
    #[test]
    fn windows_uses_appdata() {
        assert_eq!(
            data_dir_for(None, env(&[("APPDATA", r"C:\Users\me\AppData\Roaming")])),
            Some(std::path::PathBuf::from(
                r"C:\Users\me\AppData\Roaming\JStream"
            ))
        );
        assert_eq!(data_dir_for(None, env(&[("APPDATA", "")])), None);
        assert_eq!(
            data_dir_for(None, env(&[("APPDATA", r"relative\dir")])),
            None
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_uses_application_support() {
        assert_eq!(
            data_dir_for(None, env(&[("HOME", "/Users/me")])),
            Some(std::path::PathBuf::from(
                "/Users/me/Library/Application Support/JStream"
            ))
        );
        assert_eq!(data_dir_for(None, env(&[("HOME", "relative")])), None);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn elsewhere_uses_the_xdg_data_folder() {
        use std::path::PathBuf;
        assert_eq!(
            data_dir_for(None, env(&[("XDG_DATA_HOME", "/x"), ("HOME", "/h")])),
            Some(PathBuf::from("/x/jstream"))
        );
        assert_eq!(
            data_dir_for(None, env(&[("HOME", "/h")])),
            Some(PathBuf::from("/h/.local/share/jstream"))
        );
        assert_eq!(data_dir_for(None, env(&[])), None);
        assert_eq!(data_dir_for(None, env(&[("HOME", "relative")])), None);
        assert_eq!(
            data_dir_for(None, env(&[("XDG_DATA_HOME", "x"), ("HOME", "/h")])),
            Some(PathBuf::from("/h/.local/share/jstream")),
            "a relative XDG_DATA_HOME is ignored"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_saves_in_the_home_folders() {
        let none = |_: &std::path::Path| None;
        assert_eq!(
            user_folder_for(UserFolder::Documents, env(&[("HOME", "/Users/me")]), none),
            Some(std::path::PathBuf::from("/Users/me/Documents"))
        );
        assert_eq!(
            user_folder_for(UserFolder::Downloads, env(&[("HOME", "/Users/me")]), none),
            Some(std::path::PathBuf::from("/Users/me/Downloads"))
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_reads_the_xdg_user_folders() {
        use std::path::{Path, PathBuf};
        let home = env(&[("HOME", "/h")]);
        let file = |text: &'static str| {
            move |path: &Path| {
                (path == Path::new("/h/.config/user-dirs.dirs")).then(|| text.to_owned())
            }
        };
        let dirs = "# written by xdg-user-dirs-update\n\
                    XDG_DOCUMENTS_DIR=\"$HOME/Dokumente\"\n\
                    XDG_DOWNLOAD_DIR=\"/mnt/dl\"\n";
        assert_eq!(
            user_folder_for(UserFolder::Documents, &home, file(dirs)),
            Some(PathBuf::from("/h/Dokumente"))
        );
        assert_eq!(
            user_folder_for(UserFolder::Downloads, &home, file(dirs)),
            Some(PathBuf::from("/mnt/dl"))
        );
        // No file, a folder turned off (the home folder itself) or a relative path: the
        // usual name under home.
        for text in [
            "",
            "XDG_DOCUMENTS_DIR=\"$HOME/\"",
            "XDG_DOCUMENTS_DIR=\"$HOME\"",
            "XDG_DOCUMENTS_DIR=\"docs\"",
        ] {
            assert_eq!(
                user_folder_for(UserFolder::Documents, &home, file(text)),
                Some(PathBuf::from("/h/Documents")),
                "{text}"
            );
        }
        // XDG_CONFIG_HOME moves the file.
        let moved = env(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "/c")]);
        let at_c = |path: &Path| {
            (path == Path::new("/c/user-dirs.dirs")).then(|| "XDG_DOCUMENTS_DIR=\"/d\"".to_owned())
        };
        assert_eq!(
            user_folder_for(UserFolder::Documents, &moved, at_c),
            Some(PathBuf::from("/d"))
        );
        assert_eq!(
            user_folder_for(UserFolder::Documents, env(&[]), file(dirs)),
            None
        );
        assert_eq!(
            user_folder_for(UserFolder::Documents, env(&[("HOME", "rel")]), file(dirs)),
            None
        );
    }
}
