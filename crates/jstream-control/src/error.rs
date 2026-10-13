//! What a [`Control`](crate::Control) call can fail with.

use jstream_protocol::{ProtocolError, WireError};

/// Why a call through [`Control`](crate::Control) failed.
///
/// Either the engine answered with a protocol error (it has a stable `code`, so clients
/// match on [`ProtocolError`]), or the transport failed before an answer arrived.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ControlError {
    /// The engine refused or failed the request. The wire form keeps the engine's message,
    /// so an error code this client doesn't know ([`ProtocolError::Unknown`]) is still
    /// explained.
    #[error("{}", .0.message)]
    Protocol(WireError),
    /// The connection to the engine failed or closed before the answer arrived. Whether
    /// the call took effect is unknown: re-read the state, or retry a batch that carries
    /// an idempotency key. The in-process implementation never returns it.
    #[error("connection to the engine lost: {reason}")]
    Disconnected {
        /// What the transport reported.
        reason: String,
    },
}

impl ControlError {
    /// The protocol error, if the engine answered with one.
    pub fn protocol(&self) -> Option<&ProtocolError> {
        match self {
            ControlError::Protocol(wire) => Some(&wire.error),
            ControlError::Disconnected { .. } => None,
        }
    }
}

impl From<WireError> for ControlError {
    fn from(error: WireError) -> Self {
        ControlError::Protocol(error)
    }
}

impl From<ProtocolError> for ControlError {
    fn from(error: ProtocolError) -> Self {
        ControlError::Protocol(error.into())
    }
}

#[cfg(test)]
mod tests {
    use super::ControlError;
    use jstream_protocol::{ProtocolError, Rev};

    #[test]
    fn protocol_errors_keep_their_code_and_message() {
        let e = ControlError::from(ProtocolError::RevConflict {
            expected: Rev(1),
            current: Rev(2),
        });
        assert_eq!(e.to_string(), "expected rev 1, but the state is at rev 2");
        assert_eq!(e.protocol().map(ProtocolError::code), Some("rev_conflict"));
        let lost = ControlError::Disconnected {
            reason: "closed".into(),
        };
        assert_eq!(lost.protocol(), None);
    }
}
