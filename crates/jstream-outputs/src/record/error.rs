//! The recorder's error type.

use std::io;
use std::path::PathBuf;

use jstream_mux::MuxError;

/// Why a recording could not be started, written or finished.
///
/// [`RecordError::ends_recording`] tells a packet the muxer rejected, after which the
/// recording carries on, from a failure that ended it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RecordError {
    /// The destination path has no file name (it ends in `..` or is a root).
    #[error("recording path {} has no file name", .path.display())]
    NoFileName {
        /// The path given.
        path: PathBuf,
    },
    /// A file already exists at the destination, or at its temporary `.part` name (a
    /// recording a crash left behind). Nothing is overwritten.
    #[error("{} already exists", .path.display())]
    Exists {
        /// The path that exists.
        path: PathBuf,
    },
    /// The muxer refused the stream configuration. Nothing was created.
    #[error("invalid recording configuration: {0}")]
    Config(#[source] MuxError),
    /// The temporary file could not be created, or the destination could not be
    /// checked.
    #[error("could not create {}: {source}", .path.display())]
    Create {
        /// The path that could not be created or checked.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
    /// The disk (or the user's quota) is full. The recording ended; the `.part` file
    /// holds every fragment completed before it.
    #[error("disk full while writing {}: {source}", .path.display())]
    DiskFull {
        /// The temporary path.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
    /// Writing or syncing failed for another reason. The recording ended; the `.part`
    /// file holds every fragment completed before it.
    #[error("could not write {}: {source}", .path.display())]
    Write {
        /// The temporary path.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
    /// The finished file could not be renamed to its destination. The recording is
    /// complete under its temporary name.
    #[error("could not rename {} to {}: {source}", .from.display(), .to.display())]
    Rename {
        /// The temporary path.
        from: PathBuf,
        /// The destination.
        to: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
    /// The muxer rejected a packet (a timestamp going backwards, a codec with no track,
    /// a first video packet that isn't a keyframe, …). Nothing was written and the
    /// recording carries on.
    #[error("packet not recorded: {0}")]
    Packet(#[source] MuxError),
    /// The muxer could not build the last fragment (for example, audio was buffered but
    /// no video keyframe ever gave the file its AVC configuration). The recording
    /// ended; the `.part` file holds every fragment completed before it.
    #[error("could not finish the recording: {0}")]
    Finish(#[source] MuxError),
    /// The recording already ended with an earlier error.
    #[error("the recording already ended")]
    Ended,
}

impl RecordError {
    /// Whether the recording is over: every error but [`RecordError::Packet`].
    pub fn ends_recording(&self) -> bool {
        !matches!(self, Self::Packet(_))
    }

    /// An error from writing or syncing `path`: [`RecordError::DiskFull`] when the disk
    /// or quota is full, [`RecordError::Write`] otherwise.
    pub(crate) fn io(path: PathBuf, source: io::Error) -> Self {
        match source.kind() {
            io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => {
                Self::DiskFull { path, source }
            }
            _ => Self::Write { path, source },
        }
    }
}
