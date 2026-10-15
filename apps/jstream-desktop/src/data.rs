//! Where the app keeps its state ([ADR-0020](docs/adr/0020-portable-windows-distribution.md)):
//! `%APPDATA%\JStream` by default (the daemon's data folder), or a `data` folder beside
//! the exe when a `portable` marker file sits next to it.
//!
//! Docs: apps/jstream-desktop/README.md#usage

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The marker file that makes the app portable.
pub const PORTABLE_MARKER: &str = "portable";

/// The folder state goes in, next to the exe, when the app is portable.
const PORTABLE_DIR: &str = "data";

/// The data folder for an exe in `exe_dir`, reading the environment through `var`.
/// `None` when it can't be found (no `APPDATA`, or no `HOME` elsewhere).
///
/// Portable when `exe_dir` holds a [`PORTABLE_MARKER`] file. Otherwise the platform's
/// data folder, as the daemon picks it: `%APPDATA%\JStream` on Windows,
/// `~/Library/Application Support/JStream` on macOS, `$XDG_DATA_HOME/jstream` (else
/// `~/.local/share/jstream`) elsewhere.
pub fn dir(exe_dir: Option<&Path>, var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if let Some(exe_dir) = exe_dir
        && exe_dir.join(PORTABLE_MARKER).is_file()
    {
        return Some(exe_dir.join(PORTABLE_DIR));
    }
    let set = |name: &str| var(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        set("APPDATA").map(|d| d.join("JStream"))
    } else if cfg!(target_os = "macos") {
        set("HOME").map(|h| h.join("Library/Application Support/JStream"))
    } else {
        set("XDG_DATA_HOME")
            .map(|d| d.join("jstream"))
            .or_else(|| set("HOME").map(|h| h.join(".local/share/jstream")))
    }
}

#[cfg(test)]
mod tests {
    use super::{PORTABLE_MARKER, dir};
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;

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
        let exe = std::env::temp_dir().join(format!("jstream-desktop-data-{}", std::process::id()));
        let _ = fs::remove_dir_all(&exe);
        fs::create_dir_all(&exe).unwrap_or_else(|e| panic!("{e}"));
        let everywhere = env(&[("APPDATA", "/appdata"), ("HOME", "/home/me")]);
        assert_ne!(dir(Some(&exe), &everywhere), Some(exe.join("data")));
        fs::write(exe.join(PORTABLE_MARKER), "").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(dir(Some(&exe), &everywhere), Some(exe.join("data")));
        let _ = fs::remove_dir_all(&exe);
    }

    #[cfg(windows)]
    #[test]
    fn windows_uses_appdata() {
        assert_eq!(
            dir(None, env(&[("APPDATA", r"C:\Users\me\AppData\Roaming")])),
            Some(PathBuf::from(r"C:\Users\me\AppData\Roaming\JStream"))
        );
        assert_eq!(dir(None, env(&[("APPDATA", "")])), None);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn elsewhere_uses_the_xdg_data_folder() {
        assert_eq!(
            dir(None, env(&[("XDG_DATA_HOME", "/x"), ("HOME", "/h")])),
            Some(PathBuf::from("/x/jstream"))
        );
        assert_eq!(
            dir(None, env(&[("HOME", "/h")])),
            Some(PathBuf::from("/h/.local/share/jstream"))
        );
        assert_eq!(dir(None, env(&[])), None);
    }
}
