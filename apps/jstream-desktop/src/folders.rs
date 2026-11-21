//! The person's own folders, which the app saves into so its files show where people look
//! (ADR-0162): the default project in `Documents\JStream`, and screenshots in Downloads.
//!
//! - **The default project** is `Documents\JStream\My project.json` (`~/Documents/JStream`
//!   on macOS and Linux), and `collection.json` in the data folder for a portable app or
//!   when there is no Documents folder ([`default_project`]). The app moved it from the
//!   data folder, where it was before; `host::startup_project` moves it once.
//! - **A screenshot** reaches the app as the PNG's bytes, with its file name, through the
//!   window's `save_screenshot` command, and is written to Downloads under that name, or
//!   `name (2).png` and so on when one is there ([`save_new`]). The command answers the
//!   path, which the page's toast names, with Open and Show in folder. Those two run the
//!   `open_saved` and `show_saved` commands, which take only a path this run saved
//!   ([`Saved`]): no page can open or show any other file through them.
//!
//! Docs: apps/jstream-desktop/README.md#design

use jstream_platform::paths::{self, UserFolder};
use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The collection file in the data folder: the default project before ADR-0162, and still
/// a portable app's.
pub const COLLECTION: &str = "collection.json";

/// Where `folder` is for this user: the shell's known folder on Windows, and the
/// environment's on macOS and Linux (`jstream_platform::paths::user_folder`). `None`,
/// logged on Windows, when it can't be found.
pub fn user_folder(folder: UserFolder) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        match jstream_platform_windows::user_folder(folder) {
            Ok(path) => Some(path),
            Err(error) => {
                tracing::warn!(?folder, %error, "could not find a known folder");
                None
            }
        }
    }
    #[cfg(not(windows))]
    {
        paths::user_folder(folder)
    }
}

/// The default project for an exe in `exe_dir` with the data folder `data`:
/// `<documents>\JStream\My project.json`, or [`COLLECTION`] in `data` when the app is
/// portable (it keeps everything beside it) or there is no `documents` folder.
pub fn default_project(exe_dir: Option<&Path>, data: &Path, documents: Option<&Path>) -> PathBuf {
    match documents {
        Some(documents) if !paths::is_portable(exe_dir) => documents
            .join(paths::PROJECTS_DIR)
            .join(paths::DEFAULT_PROJECT),
        _ => data.join(COLLECTION),
    }
}

/// Whether `name` is a file name a screenshot may be saved under: one plain `.png` name,
/// with no folder, nothing Windows refuses in a name, no leading space and no trailing
/// dot or space before the extension, which Windows would drop.
pub fn is_screenshot_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".png") else {
        return false;
    };
    !stem.is_empty()
        && name.len() <= 200
        && !stem.ends_with(['.', ' '])
        && !name.starts_with(' ')
        && name.chars().all(|c| {
            !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
}

/// `text` with its `%XX` escapes decoded, as `encodeURIComponent` writes them, or
/// `None` when an escape is broken or the bytes aren't UTF-8.
pub fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Writes `bytes` to a new file named `name` in `folder`, or `<stem> (2).png`, `(3)`
/// and so on when the name is taken, as a browser names a repeated download. Nothing is
/// ever replaced: each name is tried with `create_new`. Returns the file written.
///
/// # Errors
/// When `name` isn't a screenshot's name ([`is_screenshot_name`]), the folder can't be
/// made, a hundred names are taken, or the file can't be written (it is then removed).
pub fn save_new(folder: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    if !is_screenshot_name(name) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a screenshot's file name",
        ));
    }
    std::fs::create_dir_all(folder)?;
    let stem = name.strip_suffix(".png").unwrap_or(name);
    for n in 1..=100 {
        let path = if n == 1 {
            folder.join(name)
        } else {
            folder.join(format!("{stem} ({n}).png"))
        };
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        return Ok(path);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "a hundred screenshots already have this name",
    ))
}

/// The files this run saved for the page, the only ones `open_saved` and `show_saved`
/// act on.
#[derive(Default)]
pub struct Saved(Mutex<HashSet<PathBuf>>);

impl Saved {
    /// Notes `path` as saved by this run.
    pub fn add(&self, path: PathBuf) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path);
    }

    /// `path` as this run saved it, if it did.
    pub fn get(&self, path: &str) -> Option<PathBuf> {
        let path = PathBuf::from(path);
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&path)
            .then_some(path)
    }
}

/// Opens `file` in the app the system picks for it, or, with `reveal`, its folder with
/// it selected (Explorer's `/select`, Finder's `open -R`; Linux opens the folder, as
/// `xdg-open` can't select). The path is one argument, never parsed by a shell. Waited
/// for on a thread of its own, so the opener doesn't linger as a zombie.
pub fn open(file: &Path, reveal: bool) {
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt as _;
        let explorer = std::env::var_os("SystemRoot")
            .map(|root| Path::new(&root).join("explorer.exe"))
            .unwrap_or_else(|| "explorer.exe".into());
        let mut command = std::process::Command::new(explorer);
        if reveal {
            // Explorer reads `/select,"<path>"` itself, not as an argv entry, so it is
            // passed raw. A Windows path can't hold `"`, so the quotes hold it whole.
            command.raw_arg(format!("/select,\"{}\"", file.display()));
        } else {
            command.arg(file);
        }
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("/usr/bin/open");
        if reveal {
            command.arg("-R");
        }
        command.arg(file);
        command
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        match file.parent() {
            Some(folder) if reveal => command.arg(folder),
            _ => command.arg(file),
        };
        command
    };
    match command.spawn() {
        Ok(mut child) => {
            let reaped = std::thread::Builder::new()
                .name("jstream-file-opener".into())
                .spawn(move || {
                    let _ = child.wait();
                });
            if let Err(error) = reaped {
                tracing::warn!(%error, "could not wait for the file opener");
            }
        }
        Err(error) => tracing::warn!(%error, "could not open a saved file"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jstream-desktop-folders-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        dir
    }

    /// #1559: the default project is in Documents, where Explorer shows it, unless the
    /// app is portable or there is no Documents folder.
    #[test]
    fn the_default_project_is_in_documents_unless_portable() {
        let data = Path::new("/data");
        let documents = Path::new("/home/me/Documents");
        assert_eq!(
            default_project(None, data, Some(documents)),
            documents.join("JStream").join("My project.json")
        );
        assert_eq!(default_project(None, data, None), data.join(COLLECTION));
        let exe = scratch("portable");
        std::fs::write(exe.join(paths::PORTABLE_MARKER), "").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            default_project(Some(&exe), data, Some(documents)),
            data.join(COLLECTION)
        );
        let _ = std::fs::remove_dir_all(exe);
    }

    #[test]
    fn only_a_plain_png_name_is_saved() {
        for name in [
            "Main 2026-11-20 19-07-50.png",
            "a_b_c 2026-01-02 13-14-15.png",
            "Szene ü.png",
        ] {
            assert!(is_screenshot_name(name), "{name}");
        }
        for name in [
            "",
            ".png",
            "x.txt",
            "../x.png",
            "a/b.png",
            "a\\b.png",
            "C:x.png",
            "a .png",
            "a..png",
            " a.png",
            "a\u{0}.png",
            "a?.png",
        ] {
            assert!(!is_screenshot_name(name), "{name}");
        }
        assert!(!is_screenshot_name(&format!("{}.png", "a".repeat(300))));
    }

    /// #1559: a screenshot never replaces a file; a repeated name gets a number.
    #[test]
    fn a_screenshot_never_replaces_a_file() {
        let dir = scratch("save");
        let downloads = dir.join("Downloads");
        let first = save_new(&downloads, "Main.png", b"one").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(first, downloads.join("Main.png"));
        let second = save_new(&downloads, "Main.png", b"two").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(second, downloads.join("Main (2).png"));
        assert_eq!(std::fs::read(&first).unwrap_or_default(), b"one");
        assert_eq!(std::fs::read(&second).unwrap_or_default(), b"two");
        assert!(save_new(&downloads, "../Main.png", b"x").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_name_is_percent_decoded() {
        assert_eq!(
            percent_decode("Szene%20%C3%BC%202026.png").as_deref(),
            Some("Szene ü 2026.png")
        );
        assert_eq!(percent_decode("plain.png").as_deref(), Some("plain.png"));
        for broken in ["%", "%2", "%zz", "%C3"] {
            assert_eq!(percent_decode(broken), None, "{broken}");
        }
    }

    #[test]
    fn only_a_saved_file_is_opened() {
        let saved = Saved::default();
        assert_eq!(saved.get("/x/Main.png"), None);
        saved.add(PathBuf::from("/x/Main.png"));
        assert_eq!(saved.get("/x/Main.png"), Some(PathBuf::from("/x/Main.png")));
        assert_eq!(saved.get("/x/Other.png"), None);
    }
}
