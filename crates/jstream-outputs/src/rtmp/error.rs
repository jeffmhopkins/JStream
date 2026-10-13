//! The RTMP client's error type.

use std::io;

use jstream_media::MediaError;

/// Why an RTMP operation failed.
///
/// No variant carries the stream key or the full URL (which may hold credentials in its
/// query), so errors are safe to log (docs/RULES.md#R-SEC-001).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RtmpError {
    /// Reading from or writing to the transport failed.
    #[error("I/O error while {during}")]
    Io {
        /// What the client was doing.
        during: &'static str,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// The peer closed the connection.
    #[error("connection closed by the server while {during}")]
    ConnectionClosed {
        /// What the client was doing.
        during: &'static str,
    },
    /// The URL is not `rtmp://host[:port]/app[/...]` or `rtmps://…`.
    #[error("invalid RTMP URL: {reason}")]
    InvalidUrl {
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The stream key is empty.
    #[error("the stream key is empty")]
    EmptyStreamKey,
    /// `rtmps://` was requested, but this build has no TLS transport yet.
    #[error("rtmps:// needs TLS, which is not implemented yet; use rtmp:// or supply a TLS stream")]
    TlsUnsupported,
    /// The server answered the handshake with an RTMP version other than 3.
    #[error("server handshake used RTMP version {version}, expected 3")]
    HandshakeVersion {
        /// The version byte S0 carried.
        version: u8,
    },
    /// The peer broke the chunk-stream or message format.
    #[error("RTMP protocol violation: {reason}")]
    Protocol {
        /// What was wrong.
        reason: String,
    },
    /// An AMF0 value could not be encoded or decoded.
    #[error("AMF0 error at byte {offset}: {reason}")]
    Amf0 {
        /// Byte offset in the message body.
        offset: usize,
        /// What was wrong.
        reason: &'static str,
    },
    /// The server answered a command with `_error`, or with a status of level `error`.
    #[error("server rejected `{command}`: {code} ({description})")]
    Rejected {
        /// The command that was rejected (`connect`, `createStream`, `publish`, …).
        command: &'static str,
        /// The status code, e.g. `NetConnection.Connect.Rejected` or
        /// `NetStream.Publish.BadName`; empty when the server sent none.
        code: String,
        /// The server's description; empty when it sent none.
        description: String,
    },
    /// A command's `_result` did not have the expected shape.
    #[error("unexpected reply to `{command}`: {reason}")]
    UnexpectedReply {
        /// The command that was answered.
        command: &'static str,
        /// What was wrong with the reply.
        reason: &'static str,
    },
    /// A chunk size outside 1..=0x7FFF_FFFF was requested.
    #[error("chunk size {size} is outside 1..=2147483647")]
    InvalidChunkSize {
        /// The size given.
        size: u32,
    },
    /// A message payload was empty or longer than RTMP's 24-bit length field allows.
    #[error("message payload of {len} bytes is not in 1..=16777215")]
    PayloadSize {
        /// The payload length given.
        len: usize,
    },
    /// A media timestamp could not be turned into RTMP milliseconds.
    #[error("timestamp cannot be sent as RTMP milliseconds")]
    Timestamp(#[from] MediaError),
    /// A media timestamp was negative.
    #[error("timestamp {ms} ms is negative; RTMP timestamps start at 0")]
    NegativeTimestamp {
        /// The timestamp in milliseconds.
        ms: i64,
    },
}

impl RtmpError {
    /// An I/O error, or [`RtmpError::ConnectionClosed`] for an unexpected end of stream.
    pub(crate) fn io(during: &'static str, source: io::Error) -> Self {
        if source.kind() == io::ErrorKind::UnexpectedEof {
            Self::ConnectionClosed { during }
        } else {
            Self::Io { during, source }
        }
    }

    pub(crate) fn protocol(reason: impl Into<String>) -> Self {
        Self::Protocol {
            reason: reason.into(),
        }
    }
}
