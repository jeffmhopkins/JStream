//! Files only this user may open: the audit log and the collection file. On Unix they
//! are created with mode `0600`; on Windows with a protected DACL for SYSTEM and the
//! user (`jstream_platform_windows::create_private_file`), so neither inherits a shared
//! or synced folder's ACL (#547).
//!
//! Docs: crates/jstream-core/README.md#design

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Opens a new user-only file at `path` for writing, failing with `AlreadyExists` if a
/// file is there.
pub(crate) fn create_new(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    return jstream_platform_windows::create_private_file(path);
    #[cfg(not(windows))]
    {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(path)
    }
}

/// Opens `path` for appending, creating it user-only if it is missing. On Windows an
/// existing file whose owner or DACL lets other users in is refused with
/// `PermissionDenied`, saying why: appending to it would hand them every later line.
pub(crate) fn open_append(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        match create_new(path) {
            // A new file is empty, so writing from its start appends.
            Ok(file) => Ok(file),
            // Windows reports a directory in the way as `AccessDenied`, not
            // `AlreadyExists`, so anything already at `path` is opened, and a directory
            // fails there.
            Err(error)
                if error.kind() == io::ErrorKind::AlreadyExists
                    || std::fs::symlink_metadata(path).is_ok() =>
            {
                let file = OpenOptions::new().read(true).append(true).open(path)?;
                jstream_platform_windows::check_private_file(&file).map_err(|why| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!(
                            "`{}` may be accessible to other users: {why}; make it \
                             user-only or move it away",
                            path.display()
                        ),
                    )
                })?;
                Ok(file)
            }
            Err(error) => Err(error),
        }
    }
    #[cfg(not(windows))]
    {
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(path)
    }
}

#[cfg(test)]
mod tests {
    use super::{create_new, open_append};
    use std::io::{ErrorKind, Read as _, Write as _};

    fn dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jstream-core-private-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn create_new_never_replaces_a_file() {
        let dir = dir("create");
        let path = dir.join("f");
        create_new(&path)
            .expect("created")
            .write_all(b"one")
            .expect("written");
        let err = create_new(&path).expect_err("a second create fails");
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).expect("read"), b"one");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_append_creates_then_appends() {
        let dir = dir("append");
        let path = dir.join("log");
        open_append(&path)
            .expect("created")
            .write_all(b"a\n")
            .expect("written");
        open_append(&path)
            .expect("reopened")
            .write_all(b"b\n")
            .expect("written");
        let mut text = String::new();
        std::fs::File::open(&path)
            .expect("open")
            .read_to_string(&mut text)
            .expect("read");
        assert_eq!(text, "a\nb\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_append_fails_on_a_directory() {
        let dir = dir("dir");
        assert!(open_append(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #547: the audit log and the collection file were created with only their
    /// folder's ACL on Windows.
    #[cfg(windows)]
    #[test]
    fn new_files_pass_the_private_file_check() {
        let dir = dir("dacl");
        for (name, open) in [
            (
                "new",
                create_new as fn(&std::path::Path) -> std::io::Result<std::fs::File>,
            ),
            ("append", open_append),
        ] {
            let path = dir.join(name);
            drop(open(&path).expect("created"));
            let file = std::fs::File::open(&path).expect("open");
            jstream_platform_windows::check_private_file(&file).expect(name);
        }
        // An existing private file reopens for appending.
        drop(open_append(&dir.join("append")).expect("reopened"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn new_files_are_user_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = dir("mode");
        for (name, open) in [
            (
                "new",
                create_new as fn(&std::path::Path) -> std::io::Result<std::fs::File>,
            ),
            ("append", open_append),
        ] {
            let path = dir.join(name);
            drop(open(&path).expect("created"));
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
