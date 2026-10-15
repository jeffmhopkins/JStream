//! Opaque identifiers and the state revision.
//!
//! IDs are opaque strings on the wire (R-API-002, docs/RULES.md#R-API-002): clients
//! never parse them. Creating edits carry a client-chosen ID, so a batch can refer to an
//! object it creates earlier in the same batch, and an undo can re-create it under the
//! same ID.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Defines an opaque string ID newtype with the same API and wire form as the others.
macro_rules! opaque_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[derive(Serialize, Deserialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wraps a string. The protocol layer does not validate it; the engine
            /// rejects IDs it cannot use with `invalid_argument`.
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            /// The ID as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(id: &str) -> Self {
                Self::new(id)
            }
        }

        impl From<String> for $name {
            fn from(id: String) -> Self {
                Self(id)
            }
        }
    };
}

opaque_id!(
    /// A scene in the collection.
    SceneId
);
opaque_id!(
    /// A scene item: one placement of a source inside a scene. Unique across scenes.
    ItemId
);
opaque_id!(
    /// A source (capture, media, color, ...). One instance however many scenes use it.
    SourceId
);
opaque_id!(
    /// An output (RTMP(S), recording, virtual camera).
    OutputId
);
opaque_id!(
    /// An encoder profile in the collection.
    EncoderProfileId
);
opaque_id!(
    /// A folder of the scene list (ADR-0024 §3). Folders organise only.
    FolderId
);
opaque_id!(
    /// A label that sources carry (ADR-0024 §4).
    LabelId
);
opaque_id!(
    /// A connected client, assigned by the server in `welcome`. Undo history and the
    /// audit log are keyed by it.
    ClientId
);
opaque_id!(
    /// Chosen by the client for each request and echoed in its response or error.
    RequestId
);
opaque_id!(
    /// Makes a retried batch safe: the engine applies a batch with a given key at most
    /// once per client and answers a retry with the original result.
    IdempotencyKey
);

/// The collection state revision.
///
/// Every applied batch increments it by exactly one. Queries report the revision they
/// read, and a batch may name the revision it was computed against (`expected_rev`).
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(transparent)]
pub struct Rev(pub u64);

impl Rev {
    /// The revision after this one, or `None` on overflow.
    pub fn next(self) -> Option<Rev> {
        self.0.checked_add(1).map(Rev)
    }
}

impl fmt::Display for Rev {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::{Rev, SceneId};

    #[test]
    fn ids_are_bare_strings_on_the_wire() {
        let id = SceneId::new("scn_main");
        assert_eq!(
            serde_json::to_string(&id).ok().as_deref(),
            Some("\"scn_main\"")
        );
        let back: SceneId = serde_json::from_str("\"scn_main\"").expect("parses");
        assert_eq!(back, id);
        assert_eq!(back.to_string(), "scn_main");
    }

    #[test]
    fn rev_is_a_bare_number_and_increments() {
        assert_eq!(serde_json::to_string(&Rev(7)).ok().as_deref(), Some("7"));
        assert_eq!(Rev(7).next(), Some(Rev(8)));
        assert_eq!(Rev(u64::MAX).next(), None);
    }
}
