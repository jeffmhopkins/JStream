//! Where JStream keeps its state (ADR-0020):
//! the platform's per-user data folder (`%APPDATA%\JStream` on Windows), or a `data`
//! folder beside the exe when a `portable` marker file sits next to it, so the whole app
//! can live on a USB stick. Every host (the desktop app, the daemon) finds it here, so
//! they agree on one folder.
//!
//! The folder is found from the environment, not a crate: three cases don't pay for
//! `dirs`, which adds `dirs-sys` and, on Windows, `windows-sys`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The marker file beside the exe that makes JStream portable. Its contents are ignored.
pub const PORTABLE_MARKER: &str = "portable";

/// The folder, beside the exe, that holds the state of a portable JStream.
pub const PORTABLE_DATA_DIR: &str = "data";

/// The file in the data folder that keeps the UI settings (ADR-0084), such as the web
/// UI's dock. The desktop app and the daemon both keep them there, so both show the
/// same workspace.
pub const UI_SETTINGS_FILE: &str = "ui-settings.json";

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
    use super::{PORTABLE_DATA_DIR, PORTABLE_MARKER, data_dir_for};
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
}
