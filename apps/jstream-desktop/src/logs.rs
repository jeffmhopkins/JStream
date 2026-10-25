//! The app's log file: `logs/jstream-desktop.log` in the data folder, replaced at each
//! start and capped at [`MAX_BYTES`], so it can't fill the disk. A windowed app has no
//! console to log to. As the daemon's logs, it is user-only (#780): the old file is
//! deleted and a new one created, mode `0600` in a `0700` folder on Unix, and with a
//! protected DACL for SYSTEM and the user on Windows
//! (`jstream_platform_windows::create_private_file`), so it never inherits a shared
//! folder's access.
//!
//! Docs: apps/jstream-desktop/README.md#usage

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The largest the log file grows; lines past it are dropped.
pub const MAX_BYTES: u64 = 10 << 20;

/// The log file's name in the log folder.
pub const FILE_NAME: &str = "jstream-desktop.log";

/// The log file. `&LogFile` is a writer, for `tracing-subscriber`.
pub struct LogFile {
    path: PathBuf,
    state: Mutex<(File, u64)>,
    max: u64,
}

impl LogFile {
    /// Creates `dir` (with its parents; user-only on Unix) and replaces the log file in
    /// it with a new, user-only one.
    pub fn create(dir: &Path) -> io::Result<LogFile> {
        Self::with_limit(dir, MAX_BYTES)
    }

    /// As [`LogFile::create`], capped at `max` bytes.
    pub fn with_limit(dir: &Path, max: u64) -> io::Result<LogFile> {
        create_dir(dir)?;
        let path = dir.join(FILE_NAME);
        // A new file, never the old one reopened: truncating would keep its access.
        match fs::remove_file(&path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        let file = create_new(&path)?;
        Ok(LogFile {
            path,
            state: Mutex::new((file, 0)),
            max,
        })
    }

    /// Where the file is.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Creates `dir` and its parents, a new one user-only (mode 0700) on Unix. An existing
/// folder is left as it is.
fn create_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Opens a new file at `path` for writing, failing if anything is there: with a
/// protected DACL for SYSTEM and the user on Windows, mode 0600 elsewhere (the crash
/// reports' own helper).
fn create_new(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    return jstream_platform_windows::create_private_file(path);
    #[cfg(not(windows))]
    jstream_crash::create_new(path)
}

impl Write for &LogFile {
    /// Writes all of `buf`, or drops it whole once the file would pass its cap. Either
    /// way it reports `buf` written, so logging never fails the app.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let (file, written) = &mut *state;
        let len = buf.len() as u64;
        if *written + len <= self.max {
            file.write_all(buf)?;
            *written += len;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.0.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::{FILE_NAME, LogFile};
    use std::io::Write;

    #[test]
    fn the_file_is_replaced_at_start_and_capped() {
        let root =
            std::env::temp_dir().join(format!("jstream-desktop-logs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("logs");
        let read = || std::fs::read_to_string(dir.join(FILE_NAME)).unwrap_or_default();

        let log = LogFile::with_limit(&dir, 10).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(log.path(), dir.join(FILE_NAME));
        let mut writer = &log;
        assert!(writer.write_all(b"12345\n").is_ok());
        assert!(
            writer.write_all(b"too long\n").is_ok(),
            "dropped, not failed"
        );
        assert!(writer.write_all(b"abc\n").is_ok());
        assert!(writer.flush().is_ok());
        assert_eq!(read(), "12345\nabc\n");
        drop(log);

        let log = LogFile::with_limit(&dir, 10).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read(), "", "replaced at the next start");
        drop(log);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The log file is user-only, a replaced one too, and so is a log folder the app
    /// creates (#780).
    #[cfg(unix)]
    #[test]
    fn the_file_and_a_new_folder_are_user_only() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &std::path::Path| {
            std::fs::metadata(path)
                .map(|meta| meta.permissions().mode() & 0o777)
                .unwrap_or_else(|e| panic!("{e}"))
        };
        let root =
            std::env::temp_dir().join(format!("jstream-desktop-private-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("logs");

        let log = LogFile::with_limit(&dir, 10).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(log.path()), 0o600);
        drop(log);

        // A file left readable by others, as an older build's was, is replaced by a new
        // user-only one rather than reopened.
        let path = dir.join(FILE_NAME);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .unwrap_or_else(|e| panic!("{e}"));
        let log = LogFile::with_limit(&dir, 10).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(mode(&path), 0o600);
        drop(log);
        let _ = std::fs::remove_dir_all(&root);
    }
}
