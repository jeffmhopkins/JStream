//! The file a folder recording output's start records to (ADR-0260): a new,
//! timestamped name in the output's folder, never one that is taken.

use std::io;
use std::path::{Path, PathBuf};

use super::temp_path_for;

/// What every recording file a folder output makes is named after.
pub const RECORDING_PREFIX: &str = "JStream";

/// The most names one start tries: the stamped name, then `_2` to `_99`.
const MAX_TRIES: u32 = 99;

/// The file name for the `n`th try at `stamp` (from 1): `JStream_<stamp>.<extension>`,
/// then `JStream_<stamp>_<n>.<extension>`.
pub fn recording_name(stamp: &str, n: u32, extension: &str) -> String {
    if n <= 1 {
        format!("{RECORDING_PREFIX}_{stamp}.{extension}")
    } else {
        format!("{RECORDING_PREFIX}_{stamp}_{n}.{extension}")
    }
}

/// A new recording's path in `folder`, made with any missing parent: the first of
/// [`recording_name`]'s names for `stamp` (the start's local date and time,
/// `2026-11-28_10-17-30`) where neither the file nor its `.part` name exists, so a
/// recording never overwrites an earlier one, even one started in the same second.
///
/// `Recorder::create` checks again as it opens the file, so a file made in between
/// fails that start rather than being replaced.
///
/// # Errors
/// The folder can't be made, or is not a folder; something at a name can't be checked;
/// or every name up to `_99` is taken ([`io::ErrorKind::AlreadyExists`]).
pub fn new_recording_in(folder: &Path, stamp: &str, extension: &str) -> io::Result<PathBuf> {
    std::fs::create_dir_all(folder)?;
    for n in 1..=MAX_TRIES {
        let path = folder.join(recording_name(stamp, n, extension));
        let part = temp_path_for(&path).unwrap_or_else(|| path.clone());
        if !path.try_exists()? && !part.try_exists()? {
            return Ok(path);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("every recording name for {stamp} is taken"),
    ))
}

#[cfg(test)]
mod tests {
    use super::{new_recording_in, recording_name};
    use std::fs;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "jstream-record-folder-{}-{name}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn names_carry_the_stamp_and_a_count_after_the_first() {
        let stamp = "2026-11-28_10-17-30";
        assert_eq!(
            recording_name(stamp, 1, "mp4"),
            "JStream_2026-11-28_10-17-30.mp4"
        );
        assert_eq!(
            recording_name(stamp, 3, "mp4"),
            "JStream_2026-11-28_10-17-30_3.mp4"
        );
    }

    #[test]
    fn a_missing_folder_is_made_and_a_taken_name_skipped() {
        let dir = TempDir::new("taken");
        let folder = dir.0.join("Videos").join("JStream");
        let stamp = "2026-11-28_10-17-30";
        let first = new_recording_in(&folder, stamp, "mp4").expect("a first name");
        assert!(folder.is_dir());
        assert_eq!(first, folder.join("JStream_2026-11-28_10-17-30.mp4"));
        fs::write(&first, b"done").expect("the first file");
        let second = new_recording_in(&folder, stamp, "mp4").expect("a second name");
        assert_eq!(second, folder.join("JStream_2026-11-28_10-17-30_2.mp4"));
        // A start still writing holds its `.part` name.
        fs::write(folder.join("JStream_2026-11-28_10-17-30_2.mp4.part"), b"").expect("part");
        let third = new_recording_in(&folder, stamp, "mp4").expect("a third name");
        assert_eq!(third, folder.join("JStream_2026-11-28_10-17-30_3.mp4"));
    }

    #[test]
    fn a_file_where_the_folder_goes_fails() {
        let dir = TempDir::new("file");
        fs::create_dir_all(&dir.0).expect("the temp dir");
        let file = dir.0.join("not-a-folder");
        fs::write(&file, b"").expect("a file");
        assert!(new_recording_in(&file, "2026-11-28_10-17-30", "mp4").is_err());
    }
}
