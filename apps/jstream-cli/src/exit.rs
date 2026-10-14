//! Exit codes, and the errors that produce them.
//!
//! Every failure has its own exit code, so scripts can branch on it without parsing
//! output. The table is documented in apps/jstream-cli/README.md#usage. The numbering is
//! provisional until the maintainer confirms it; after that, a released code keeps its
//! meaning.

use jstream_control::ControlError;
use jstream_control::protocol::ProtocolError;
use std::fmt;
use std::io;

/// The command succeeded.
pub const OK: u8 = 0;
/// Writing the output failed (other than a closed pipe), or `mcp` could not read its input.
pub const IO: u8 = 1;
/// The command line could not be parsed.
pub const USAGE: u8 = 2;
/// No connection to the engine, or it was lost before an answer (`Disconnected`).
pub const DISCONNECTED: u8 = 3;

/// The exit code for an error the engine answered with: 10 plus a fixed number per
/// protocol error code. A code this CLI doesn't know yet (from a newer engine) is 20.
pub fn for_protocol(error: &ProtocolError) -> u8 {
    match error {
        ProtocolError::UnsupportedVersion { .. } => 10,
        ProtocolError::HandshakeRequired => 11,
        ProtocolError::Malformed { .. } => 12,
        ProtocolError::EmptyBatch => 13,
        ProtocolError::RevConflict { .. } => 14,
        ProtocolError::NotFound { .. } => 15,
        ProtocolError::AlreadyExists { .. } => 16,
        ProtocolError::InvalidArgument { .. } => 17,
        ProtocolError::ActionFailed { .. } => 18,
        ProtocolError::Internal { .. } => 19,
        _ => 20,
    }
}

/// Why a CLI run failed.
#[derive(Debug)]
pub enum CliError {
    /// The arguments were wrong. The message says how.
    Usage(String),
    /// The engine answered with an error, or the connection failed.
    Control(ControlError),
    /// Writing the output failed.
    Io(io::Error),
    /// Reading the input failed (only `mcp` reads it).
    Read(io::Error),
}

impl CliError {
    /// The process exit code for this error.
    pub fn exit_code(&self) -> u8 {
        match self {
            CliError::Usage(_) => USAGE,
            CliError::Io(_) | CliError::Read(_) => IO,
            CliError::Control(ControlError::Protocol(wire)) => for_protocol(&wire.error),
            CliError::Control(_) => DISCONNECTED,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::Usage(message) => f.write_str(message),
            CliError::Control(ControlError::Protocol(wire)) => {
                write!(f, "{} ({})", wire.message, wire.error.code())
            }
            CliError::Control(ControlError::Disconnected { reason }) => {
                write!(f, "no connection to the engine: {reason}")
            }
            CliError::Control(other) => other.fmt(f),
            CliError::Io(error) => write!(f, "could not write the output: {error}"),
            CliError::Read(error) => write!(f, "could not read the input: {error}"),
        }
    }
}

impl From<ControlError> for CliError {
    fn from(error: ControlError) -> Self {
        CliError::Control(error)
    }
}

impl From<io::Error> for CliError {
    fn from(error: io::Error) -> Self {
        CliError::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::{CliError, DISCONNECTED, IO, USAGE, for_protocol};
    use jstream_control::ControlError;
    use jstream_control::protocol::{EntityKind, ProtocolError, ProtocolVersion, Rev};
    use std::collections::BTreeSet;

    /// One value of every protocol error this CLI knows, plus `Unknown`.
    fn every_protocol_error() -> Vec<ProtocolError> {
        let v = ProtocolVersion { major: 0, minor: 1 };
        vec![
            ProtocolError::UnsupportedVersion {
                client: v,
                server: v,
            },
            ProtocolError::HandshakeRequired,
            ProtocolError::Malformed { reason: "x".into() },
            ProtocolError::EmptyBatch,
            ProtocolError::RevConflict {
                expected: Rev(1),
                current: Rev(2),
            },
            ProtocolError::NotFound {
                entity: EntityKind::Scene,
                id: "s".into(),
            },
            ProtocolError::AlreadyExists {
                entity: EntityKind::Scene,
                id: "s".into(),
            },
            ProtocolError::InvalidArgument {
                field: "f".into(),
                reason: "r".into(),
            },
            ProtocolError::ActionFailed { reason: "r".into() },
            ProtocolError::Internal { reason: "r".into() },
            ProtocolError::Unknown,
        ]
    }

    #[test]
    fn every_error_code_has_its_own_exit_code() {
        let codes: Vec<(&str, u8)> = every_protocol_error()
            .iter()
            .map(|e| (e.code(), for_protocol(e)))
            .collect();
        assert_eq!(
            codes,
            [
                ("unsupported_version", 10),
                ("handshake_required", 11),
                ("malformed", 12),
                ("empty_batch", 13),
                ("rev_conflict", 14),
                ("not_found", 15),
                ("already_exists", 16),
                ("invalid_argument", 17),
                ("action_failed", 18),
                ("internal", 19),
                ("unknown", 20),
            ]
        );
        let mut all: BTreeSet<u8> = codes.iter().map(|(_, c)| *c).collect();
        for fixed in [0, IO, USAGE, DISCONNECTED] {
            assert!(all.insert(fixed), "exit code {fixed} is reused");
        }
    }

    #[test]
    fn disconnected_and_usage_have_their_own_codes() {
        let lost = CliError::Control(ControlError::Disconnected {
            reason: "closed".into(),
        });
        assert_eq!(lost.exit_code(), DISCONNECTED);
        assert_eq!(CliError::Usage("bad".into()).exit_code(), USAGE);
        let rev = CliError::Control(
            ProtocolError::RevConflict {
                expected: Rev(1),
                current: Rev(2),
            }
            .into(),
        );
        assert_eq!(rev.exit_code(), 14);
        assert_eq!(
            rev.to_string(),
            "expected rev 1, but the state is at rev 2 (rev_conflict)"
        );
    }
}
