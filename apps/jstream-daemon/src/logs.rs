//! Where the daemon keeps its log files and crash reports, and the size-capped,
//! rotating log files themselves (ARCH › Persistence: rotating logs in the data
//! directory; R-SEC-004: they stay local).
//!
//! Each run writes to its own file, `jstream-daemon-<UTC time>-<pid>-<n>.log`, and moves
//! to a new one once the current one would pass [`MAX_FILE_BYTES`]. Whenever a file is
//! opened, the oldest log files beyond [`MAX_FILES`] are deleted, so the directory never
//! holds much more than `MAX_FILES × MAX_FILE_BYTES`, however long or often it runs.
//!
//! Docs: apps/jstream-daemon/README.md#design

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

/// The size at which the daemon moves on to a new log file.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// The log files kept, the current one included.
pub const MAX_FILES: usize = 10;

/// The file name prefix of the daemon's log files.
pub const LOG_PREFIX: &str = "jstream-daemon-";

/// The file name suffix of the daemon's log files.
pub const LOG_SUFFIX: &str = ".log";

/// The default log directory: `logs` in the data folder (ADR-0020: `%APPDATA%\JStream`,
/// or the `data` folder beside the exe when it is portable), as
/// `jstream_platform::paths::data_dir` finds it.
pub fn default_dir() -> Option<PathBuf> {
    jstream_platform::paths::data_dir().map(|dir| dir.join("logs"))
}

/// Creates `dir` and its missing parents. On Unix the directories it creates are
/// user-only (mode 0700); an existing directory is left as it is.
pub fn create_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Opens a new file at `path` for writing, failing if anything is there; user-only on
/// Unix (mode 0600), and with its directory's ACL on Windows.
pub fn create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Deletes the oldest files in `dir` whose names start with `prefix` and end with
/// `suffix`, so at most `keep` of them remain, never touching `except`. The names
/// start with a UTC time, so name order is age order to the second (files from one
/// second order by process ID). Best effort: a file that can't be deleted is left for
/// a later prune. On Windows `std` opens files with `FILE_SHARE_DELETE`, so deleting
/// another daemon's open log is expected to succeed there too (untested on Windows).
pub fn prune(dir: &Path, prefix: &str, suffix: &str, keep: usize, except: Option<&Path>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(prefix) && name.ends_with(suffix))
        .filter(|name| except.and_then(Path::file_name) != Some(name.as_ref()))
        .collect();
    names.sort();
    let keep = keep.saturating_sub(usize::from(except.is_some()));
    let excess = names.len().saturating_sub(keep);
    for name in &names[..excess] {
        let _ = fs::remove_file(dir.join(name));
    }
}

/// A UTC date and time, to the second, for log file names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Utc {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

impl Utc {
    /// `time` in UTC; times before 1970 read as 1970-01-01.
    pub fn from(time: SystemTime) -> Utc {
        let seconds = time
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX / 2);
        let of_day = u32::try_from(seconds % 86_400).unwrap_or(0);
        // Howard Hinnant's `civil_from_days`, for days since 1970-01-01.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        Utc {
            year,
            month: u32::try_from(month).unwrap_or(1),
            day: u32::try_from(day).unwrap_or(1),
            hour: of_day / 3600,
            minute: of_day / 60 % 60,
            second: of_day % 60,
        }
    }

    /// `20261014T101500Z`: sortable, and valid in a file name everywhere.
    pub fn compact(&self) -> String {
        let Utc {
            year,
            month,
            day,
            hour,
            minute,
            second,
        } = self;
        format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
    }
}

/// The open log file and how much has been written to it.
struct Current {
    path: PathBuf,
    file: File,
    written: u64,
}

/// The daemon's rotating log files in one directory. The log subscriber writes each
/// event with one `write` call (`&LogFiles` is the subscriber's writer), so a file is
/// only ever switched between events.
pub struct LogFiles {
    dir: PathBuf,
    max_file_bytes: u64,
    max_files: usize,
    /// Distinguishes files opened in the same second.
    current: Mutex<(Current, u32)>,
}

impl LogFiles {
    /// Opens a new log file in `dir` (which must exist) with the default limits.
    pub fn open(dir: &Path) -> io::Result<LogFiles> {
        LogFiles::with_limits(dir, MAX_FILE_BYTES, MAX_FILES)
    }

    /// Opens a new log file in `dir`, moving on to another once one would pass
    /// `max_file_bytes`, and keeping `max_files` files at most.
    pub fn with_limits(dir: &Path, max_file_bytes: u64, max_files: usize) -> io::Result<LogFiles> {
        let first = new_file(dir, 0, max_files)?;
        Ok(LogFiles {
            dir: dir.to_owned(),
            max_file_bytes,
            max_files,
            current: Mutex::new((first, 1)),
        })
    }

    /// The file being written now.
    pub fn current_path(&self) -> PathBuf {
        let current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        current.0.path.clone()
    }
}

/// Opens the next log file in `dir` and prunes the oldest beyond `max_files`.
fn new_file(dir: &Path, sequence: u32, max_files: usize) -> io::Result<Current> {
    let stamp = Utc::from(SystemTime::now()).compact();
    let pid = std::process::id();
    let mut sequence = sequence;
    loop {
        let path = dir.join(format!(
            "{LOG_PREFIX}{stamp}-{pid}-{sequence:03}{LOG_SUFFIX}"
        ));
        match create_new(&path) {
            Ok(file) => {
                prune(dir, LOG_PREFIX, LOG_SUFFIX, max_files, Some(&path));
                return Ok(Current {
                    path,
                    file,
                    written: 0,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && sequence < 999 => {
                sequence += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

impl Write for &LogFiles {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut guard = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        let (current, sequence) = &mut *guard;
        let len = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        if current.written > 0 && current.written.saturating_add(len) > self.max_file_bytes {
            // If the next file can't be opened, keep writing to this one rather than
            // losing the line.
            if let Ok(next) = new_file(&self.dir, *sequence, self.max_files) {
                *current = next;
                *sequence = sequence.wrapping_add(1) % 1000;
            }
        }
        current.file.write_all(buf)?;
        current.written = current.written.saturating_add(len);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut guard = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        guard.0.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::{LOG_PREFIX, LOG_SUFFIX, LogFiles, Utc, prune};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, UNIX_EPOCH};

    /// A fresh, empty directory for one test, under the system's temporary directory.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("jstream-daemon-logs-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch directory is created");
        dir
    }

    fn log_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("readable")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .into_string()
                    .expect("UTF-8")
            })
            .filter(|name| name.starts_with(LOG_PREFIX) && name.ends_with(LOG_SUFFIX))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn utc_times_are_civil_dates() {
        let at = |seconds| Utc::from(UNIX_EPOCH + Duration::from_secs(seconds));
        assert_eq!(at(0).compact(), "19700101T000000Z");
        assert_eq!(at(951_782_400).compact(), "20000229T000000Z");
        assert_eq!(at(1_791_972_900).compact(), "20261014T101500Z");
        assert_eq!(at(4_102_444_799).compact(), "20991231T235959Z");
    }

    /// Lines go to one file until it would pass the limit, then to a new one, and only
    /// the newest `max_files` survive, each within the limit.
    #[test]
    fn files_rotate_by_size_and_the_oldest_are_deleted() {
        let dir = scratch("rotate");
        // Two older runs' files (as their names say) and an unrelated file.
        std::fs::write(
            dir.join("jstream-daemon-19990101T000000Z-1-000.log"),
            "old\n",
        )
        .expect("written");
        std::fs::write(
            dir.join("jstream-daemon-19990101T000001Z-1-000.log"),
            "old\n",
        )
        .expect("written");
        std::fs::write(dir.join("notes.txt"), "keep me").expect("written");
        let files = LogFiles::with_limits(&dir, 100, 3).expect("opened");
        let first = files.current_path();
        let line = [b'x'; 39];
        for _ in 0..20 {
            let mut writer = &files;
            writer.write_all(&line).expect("written");
            writer.write_all(b"\n").expect("written");
        }
        (&files).flush().expect("flushed");
        let names = log_files(&dir);
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(names.iter().all(|name| !name.contains("1999")), "{names:?}");
        assert_ne!(files.current_path(), first, "it moved on to new files");
        assert_eq!(
            files.current_path().file_name().and_then(|n| n.to_str()),
            names.last().map(String::as_str),
            "the current file is the newest"
        );
        for name in &names {
            let size = std::fs::metadata(dir.join(name)).expect("exists").len();
            assert!(size <= 100, "{name} has {size} bytes");
            assert!(size > 0, "{name} is empty");
        }
        assert!(dir.join("notes.txt").exists(), "other files are left alone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A line longer than the limit still gets written, to a file of its own.
    #[test]
    fn an_oversized_line_is_written_whole() {
        let dir = scratch("oversized");
        let files = LogFiles::with_limits(&dir, 10, 2).expect("opened");
        (&files).write_all(&[b'y'; 50]).expect("written");
        let text = std::fs::read_to_string(files.current_path()).expect("readable");
        assert_eq!(text.len(), 50);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pruning_keeps_the_newest_and_the_exception() {
        let dir = scratch("prune");
        for name in ["crash-1.txt", "crash-2.txt", "crash-3.txt", "crash-4.txt"] {
            std::fs::write(dir.join(name), name).expect("written");
        }
        prune(&dir, "crash-", ".txt", 2, Some(&dir.join("crash-1.txt")));
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .expect("readable")
            .map(|entry| entry.expect("an entry").file_name())
            .collect();
        left.sort();
        assert_eq!(left, ["crash-1.txt", "crash-4.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn log_files_and_new_directories_are_user_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("modes").join("a/b");
        super::create_dir(&dir).expect("created");
        let mode = |path: &Path| {
            std::fs::metadata(path)
                .expect("exists")
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(&dir), 0o700);
        let files = LogFiles::open(&dir).expect("opened");
        assert_eq!(mode(&files.current_path()), 0o600);
        let _ = std::fs::remove_dir_all(dir.parent().and_then(Path::parent).expect("scratch"));
    }
}
