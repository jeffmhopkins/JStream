//! What starting or running the server can fail with.

use std::io;
use std::net::SocketAddr;

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
