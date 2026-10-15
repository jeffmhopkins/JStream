//! The protocol version, exchanged in the `hello`/`welcome` handshake.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// The version of the protocol this crate implements.
///
/// It follows semver independently of the app (R-API-001), at 0.x as after 1.0: an
/// addition bumps `minor` and old clients keep working; removing or renaming anything
/// bumps `major`, needs an ADR, and needs a minor release that deprecates it first.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion {
    major: 0,
    minor: 20,
};

/// A protocol version: `major.minor`. Patch releases never change the wire format, so
/// they are not part of it.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub struct ProtocolVersion {
    /// Incremented by a breaking change (a removal or rename).
    pub major: u32,
    /// Incremented by an addition.
    pub minor: u32,
}

impl ProtocolVersion {
    /// Whether a server speaking `self` can serve a client speaking `client`.
    ///
    /// The majors must match, and the client's minor must be no newer than the server's:
    /// a newer client could send commands the server doesn't know, while an older one only
    /// uses what every later minor still has. The rule is the same at 0.x.
    pub fn can_serve(self, client: ProtocolVersion) -> bool {
        self.major == client.major && client.minor <= self.minor
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
    fn older_minor_is_served() {
        assert!(v(0, 2).can_serve(v(0, 1)));
        assert!(v(1, 3).can_serve(v(1, 0)));
    }

    #[test]
    fn newer_minor_is_refused() {
        assert!(!v(0, 1).can_serve(v(0, 2)));
        assert!(!v(1, 0).can_serve(v(1, 3)));
    }

    #[test]
    fn different_major_is_refused() {
        assert!(!v(1, 0).can_serve(v(0, 1)));
        assert!(!v(0, 5).can_serve(v(1, 0)));
        assert!(!v(2, 0).can_serve(v(1, 0)));
    }

    #[test]
    fn displays_as_major_dot_minor() {
        assert_eq!(v(1, 4).to_string(), "1.4");
    }
}
