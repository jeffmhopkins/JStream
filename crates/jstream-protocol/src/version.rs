//! The protocol version, exchanged in the `hello`/`welcome` handshake.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// The version of the protocol this crate implements.
///
/// It follows semver independently of the app (R-API-001): an addition bumps `minor`, and
/// removing or renaming anything bumps `major`. While `major` is 0, a `minor` bump may
/// also break, as in Cargo's semver.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion { major: 0, minor: 1 };

/// A protocol version: `major.minor`. Patch releases never change the wire format, so
/// they are not part of it.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub struct ProtocolVersion {
    /// Incremented by a breaking change.
    pub major: u32,
    /// Incremented by an addition (or, while `major` is 0, by any change).
    pub minor: u32,
}

impl ProtocolVersion {
    /// Whether a server speaking `self` can serve a client speaking `client`.
    ///
    /// The majors must match, and the client must not be newer than the server, because
    /// it could send commands the server doesn't know. While `major` is 0, every minor is
    /// its own compatibility line, so the minors must match exactly.
    pub fn can_serve(self, client: ProtocolVersion) -> bool {
        if self.major != client.major {
            return false;
        }
        if self.major == 0 {
            self.minor == client.minor
        } else {
            client.minor <= self.minor
        }
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

#[cfg(test)]
mod tests {
    use super::{PROTOCOL_VERSION, ProtocolVersion};

    fn v(major: u32, minor: u32) -> ProtocolVersion {
        ProtocolVersion { major, minor }
    }

    #[test]
    fn same_version_is_served() {
        assert!(PROTOCOL_VERSION.can_serve(PROTOCOL_VERSION));
    }

    #[test]
    fn pre_1_0_minors_must_match() {
        assert!(!v(0, 2).can_serve(v(0, 1)));
        assert!(!v(0, 1).can_serve(v(0, 2)));
    }

    #[test]
    fn after_1_0_older_clients_are_served() {
        assert!(v(1, 3).can_serve(v(1, 0)));
        assert!(!v(1, 0).can_serve(v(1, 3)));
        assert!(!v(2, 0).can_serve(v(1, 0)));
    }

    #[test]
    fn displays_as_major_dot_minor() {
        assert_eq!(v(1, 4).to_string(), "1.4");
    }
}
