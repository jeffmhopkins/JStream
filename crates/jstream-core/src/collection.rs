//! The collection file: loaded when the host names it, and saved after every change.
//!
//! `jstream-graph` turns the collection into versioned JSON and back
//! ([`Graph::to_collection_json`], [`Graph::with_collection_json`]); this module does the
//! file system part. A save replaces the file atomically: the JSON goes to a temporary
//! file beside it, which is flushed to disk and then renamed over the old file, so a
//! crash or a full disk leaves either the old collection or the new one, never a torn
//! file.
//!
//! Docs: crates/jstream-core/README.md#design, docs/ARCHITECTURE.md#persistence

use jstream_graph::{Graph, LoadError};
use jstream_protocol::Rev;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

/// Why [`Engine::with_collection_file`](crate::Engine::with_collection_file) could not
/// load or create the collection file. Each names the file; none quotes its contents.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CollectionFileError {
    /// The file exists but could not be read.
    #[error("cannot read the collection file {}: {source}", path.display())]
    Read {
        /// The file.
        path: PathBuf,
        /// The OS error.
        source: io::Error,
    },
    /// The file was read but holds no collection this engine can load. It is left as
    /// it is.
    #[error("cannot load the collection file {}: {source}", path.display())]
    Load {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        source: LoadError,
    },
    /// The file did not exist, and creating it failed.
    #[error("cannot create the collection file {}: {source}", path.display())]
    Create {
        /// The file.
        path: PathBuf,
        /// The OS error.
        source: io::Error,
    },
}

/// Where the collection is saved, and the `rev` the file holds.
pub(crate) struct CollectionFile {
    path: PathBuf,
    saved: Rev,
}

impl CollectionFile {
    /// The folder the file is in, which `collection` asset references are relative to.
    pub(crate) fn folder(&self) -> Option<&Path> {
        match self.path.parent() {
            Some(parent) if parent.as_os_str().is_empty() => Some(Path::new(".")),
            parent => parent,
        }
    }

    /// Loads the collection at `path` into a copy of `graph` (whose kinds check its
    /// sources), or, when there is no file there, saves `graph` to it.
    pub(crate) fn open(
        path: PathBuf,
        graph: &Graph,
    ) -> Result<(CollectionFile, Graph), CollectionFileError> {
        match fs::read_to_string(&path) {
            Ok(json) => match graph.clone().with_collection_json(&json) {
                Ok(loaded) => Ok((
                    CollectionFile {
                        path,
                        saved: loaded.rev(),
                    },
                    loaded,
                )),
                Err(source) => Err(CollectionFileError::Load { path, source }),
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                match write_atomically(&path, graph.to_collection_json().as_bytes()) {
                    Ok(()) => Ok((
                        CollectionFile {
                            path,
                            saved: graph.rev(),
                        },
                        graph.clone(),
                    )),
                    Err(source) => Err(CollectionFileError::Create { path, source }),
                }
            }
            Err(source) => Err(CollectionFileError::Read { path, source }),
        }
    }

    /// The `rev` the file holds.
    pub(crate) fn saved(&self) -> Rev {
        self.saved
    }

    /// Writes `json`, the collection at `rev`, over the file.
    pub(crate) fn save(&mut self, rev: Rev, json: &str) -> io::Result<()> {
        write_atomically(&self.path, json.as_bytes())?;
        self.saved = rev;
        Ok(())
    }

    /// The file's path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// Replaces `path` with `bytes` atomically: writes a temporary file in the same
/// directory, flushes it to disk, renames it over `path` (`std`'s rename replaces an
/// existing file on Windows too), and on Unix flushes the directory so the rename
/// itself survives a power cut. A failure up to the rename removes the temporary file,
/// leaves `path` as it was and is returned. Once the rename is done the file holds the
/// new collection, so a failed directory flush is only logged at `warn`. The new file is
/// user-only, like the daemon's other files: mode `0600` on Unix, a protected DACL for
/// SYSTEM and the user on Windows, which the rename keeps.
///
/// The temporary name carries the process ID, so two processes saving one path don't
/// share it. Within a process, the engine's saves to one file are serialized by its
/// lock; two engines saving the same path would overwrite each other's collections
/// anyway, so that is not a supported setup.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the path names no file"))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    let written = write_new(&tmp, bytes).and_then(|()| fs::rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
        return written;
    }
    #[cfg(unix)]
    {
        let dir = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        if let Err(err) = fs::File::open(dir).and_then(|dir| dir.sync_all()) {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "file saved, but its directory was not flushed to disk"
            );
        }
    }
    Ok(())
}

/// Writes `bytes` to a new user-only file at `path`, replacing a temporary file a
/// save that crashed left there (one of a process with this ID, which the name carries).
/// The replacement is created anew, so it never keeps the old file's ACL.
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = match crate::private_file::create_new(path) {
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            fs::remove_file(path)?;
            crate::private_file::create_new(path)?
        }
        opened => opened?,
    };
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::write_atomically;

    #[test]
    fn a_stale_temporary_file_is_replaced() {
        let dir = std::env::temp_dir().join(format!(
            "jstream-core-collection-tmp-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("c.json");
        let tmp = dir.join(format!(".c.json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, b"left by a crashed save").expect("stale tmp");
        write_atomically(&path, b"{}").expect("saved");
        assert_eq!(std::fs::read(&path).expect("read"), b"{}");
        assert!(!tmp.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
