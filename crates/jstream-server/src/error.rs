//! What starting or running the server can fail with.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Why the server could not start or stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServerError {
    /// The control token is unusable.
    #[error("invalid control token: {reason}")]
    InvalidToken {
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A configured extra `Host` can never match a request.
    #[error("invalid extra host `{host}`: {reason}")]
    InvalidHost {
        /// The entry as configured.
        host: String,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A configured connection limit or timeout is zero, so the server would refuse or
    /// drop everything.
    #[error("invalid server config: `{field}` must be greater than zero")]
    InvalidLimit {
        /// The [`ServerConfig`](crate::ServerConfig) field.
        field: &'static str,
    },
    /// The configured UI directory is missing or isn't a directory.
    #[error("unusable UI directory `{}`: {source}", dir.display())]
    UiDir {
        /// The directory as configured.
        dir: PathBuf,
        /// What is wrong with it.
        source: io::Error,
    },
    /// The device file (`ServerConfig::device_file`) can't be read or is broken.
    #[error("unusable device file `{}`: {reason}", path.display())]
    DeviceFile {
        /// The file as configured.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// The listening socket could not be opened.
    #[error("could not listen on {addr}: {source}")]
    Bind {
        /// The address asked for.
        addr: SocketAddr,
        /// What the OS reported.
        source: io::Error,
    },
    /// The listener failed while serving.
    #[error("the server stopped: {0}")]
    Serve(io::Error),
}
