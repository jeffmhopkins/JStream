//! Protocol errors, with stable machine-readable codes.

use crate::history::HistoryDirection;
use crate::id::{ClientId, Rev};
use crate::version::ProtocolVersion;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Why the server refused or failed a request.
///
/// On the wire each variant is an object whose `code` tag is the variant's stable
/// snake_case code (see [`ProtocolError::code`]) and whose other fields are its details.
/// [`WireError`] adds the human-readable message. A code, once released, keeps its
/// meaning and its fields (R-API-001). Clients must handle codes they don't know: they
/// parse as [`ProtocolError::Unknown`], and the message still explains them.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "code", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProtocolError {
    /// The server can't speak the client's protocol version.
    #[error("protocol version {client} is not supported; this server speaks {server}")]
    UnsupportedVersion {
        /// The version the client asked for in `hello`.
        client: ProtocolVersion,
        /// The version the server speaks.
        server: ProtocolVersion,
    },
    /// A request arrived before the `hello`/`welcome` handshake finished.
    #[error("send `hello` before any request")]
    HandshakeRequired,
    /// The message could not be parsed: bad JSON, an unknown `type`, a missing or
    /// mistyped field, or a number that isn't a finite `f32` (NaN, infinity, or out of
    /// range such as `1e39`). `ClientMessage::parse` produces it.
    #[error("malformed message: {reason}")]
    Malformed {
        /// What the parser reported.
        reason: String,
    },
    /// A batch had no edits.
    #[error("a batch must contain at least one edit")]
    EmptyBatch,
    /// The batch's `expected_rev` was not the current revision. Nothing was applied:
    /// re-read the state and retry.
    #[error("expected rev {expected}, but the state is at rev {current}")]
    RevConflict {
        /// The `expected_rev` the batch carried.
        expected: Rev,
        /// The state's revision when the batch was checked.
        current: Rev,
    },
    /// A command named an object that doesn't exist.
    #[error("{entity} `{id}` does not exist")]
    NotFound {
        /// What kind of object.
        entity: EntityKind,
        /// The ID given.
        id: String,
    },
    /// A creating edit used an ID that is already taken.
    #[error("{entity} `{id}` already exists")]
    AlreadyExists {
        /// What kind of object.
        entity: EntityKind,
        /// The ID given.
        id: String,
    },
    /// A field had a value the engine can't accept.
    #[error("invalid `{field}`: {reason}")]
    InvalidArgument {
        /// The field, as a dotted path into the command (`edits.2.name`).
        field: String,
        /// Why it was rejected.
        reason: String,
        /// When a `scene.delete` or `source.delete` is refused because other objects still
        /// refer to its target: every one of those references, in a stable order. The
        /// `delete.plan` query returns the batch that removes them with the target. Empty
        /// otherwise, and then omitted. Missing (from a server older than 0.15)
        /// parses as empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blockers: Vec<Blocker>,
    },
    /// An undo or redo would overwrite another client's work: a batch that client
    /// applied after the step changed a property the step sets, created or deleted an
    /// object the step creates or deletes, or made the step's edits no longer apply.
    /// Nothing was applied, and the step was dropped from the history, so the next undo
    /// reaches the step before it.
    #[error("step at rev {step_rev} conflicts with rev {rev} by client `{client_id}`")]
    UndoConflict {
        /// The revision the step's batch originally produced.
        step_rev: Rev,
        /// The client whose batch conflicts.
        client_id: ClientId,
        /// The revision that batch produced.
        rev: Rev,
    },
    /// There is nothing to undo, or nothing to redo.
    #[error("nothing to {direction}")]
    HistoryEmpty {
        /// Which way the call asked to move.
        direction: HistoryDirection,
    },
    /// An action was accepted for processing but could not be carried out.
    #[error("action failed: {reason}")]
    ActionFailed {
        /// What went wrong.
        reason: String,
    },
    /// The engine hit a bug or an unexpected condition. The request may be retried.
    #[error("internal error: {reason}")]
    Internal {
        /// What went wrong.
        reason: String,
    },
    /// An error code this version of the protocol doesn't know. Never sent; produced
    /// only when parsing an error from a newer server. [`WireError::message`] explains it.
    #[error("unrecognized error code")]
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

impl ProtocolError {
    /// An `invalid_argument` error with no blockers.
    pub fn invalid_argument(field: impl Into<String>, reason: impl Into<String>) -> Self {
        ProtocolError::InvalidArgument {
            field: field.into(),
            reason: reason.into(),
            blockers: Vec::new(),
        }
    }

    /// The stable machine-readable code, as sent in the `code` field.
    pub fn code(&self) -> &'static str {
        match self {
            ProtocolError::UnsupportedVersion { .. } => "unsupported_version",
            ProtocolError::HandshakeRequired => "handshake_required",
            ProtocolError::Malformed { .. } => "malformed",
            ProtocolError::EmptyBatch => "empty_batch",
            ProtocolError::RevConflict { .. } => "rev_conflict",
            ProtocolError::NotFound { .. } => "not_found",
            ProtocolError::AlreadyExists { .. } => "already_exists",
            ProtocolError::InvalidArgument { .. } => "invalid_argument",
            ProtocolError::UndoConflict { .. } => "undo_conflict",
            ProtocolError::HistoryEmpty { .. } => "history_empty",
            ProtocolError::ActionFailed { .. } => "action_failed",
            ProtocolError::Internal { .. } => "internal",
            ProtocolError::Unknown => "unknown",
        }
    }
}

/// The kinds of object an error can name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EntityKind {
    /// A scene.
    Scene,
    /// A scene item.
    Item,
    /// A source.
    Source,
    /// An output.
    Output,
    /// An encoder profile.
    Encoder,
    /// A folder of the scene list.
    Folder,
    /// A label.
    Label,
    /// An audio bus.
    Bus,
}

impl fmt::Display for EntityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EntityKind::Scene => "scene",
            EntityKind::Item => "item",
            EntityKind::Source => "source",
            EntityKind::Output => "output",
            EntityKind::Encoder => "encoder",
            EntityKind::Folder => "folder",
            EntityKind::Label => "label",
            EntityKind::Bus => "bus",
        })
    }
}

/// One reference that stops a `scene.delete`, `source.delete`, `folder.delete` or
/// `label.delete`: the object that refers to the target, and the field of that object
/// that holds the reference.
///
/// ```json
/// {"entity": "source", "id": "webcam", "field": "settings.mask"}
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Blocker {
    /// What kind of object refers to the target: an `item` that places a source, a
    /// `source` that shows a scene, whose settings refer to the target or that carries
    /// a label, or a `scene` or `folder` that sits in a folder.
    pub entity: EntityKind,
    /// That object's ID.
    pub id: String,
    /// The field of that object that refers to the target, as a dotted path into its
    /// query form: `source_id` for an item, `kind.scene_id` for a `scene` source,
    /// `settings.<param>` for a settings reference, `labels` for a source carrying a
    /// label, `folder` for a scene in a folder, and `parent` for a folder in one.
    pub field: String,
}

/// A [`ProtocolError`] as sent on the wire: its `code` and details, plus a message.
///
/// ```json
/// {"code": "rev_conflict", "expected": 4, "current": 5,
///  "message": "expected rev 4, but the state is at rev 5"}
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireError {
    /// The error: `code` plus its detail fields.
    #[serde(flatten)]
    pub error: ProtocolError,
    /// A human-readable explanation, for logs and for agents. Not stable: match on
    /// `code`, never on this.
    pub message: String,
}

impl From<ProtocolError> for WireError {
    fn from(error: ProtocolError) -> Self {
        WireError {
            message: error.to_string(),
            error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Blocker, EntityKind, ProtocolError, WireError};
    use crate::id::Rev;
    use serde_json::json;

    #[test]
    fn wire_form_is_code_details_and_message() {
        let wire = WireError::from(ProtocolError::RevConflict {
            expected: Rev(4),
            current: Rev(5),
        });
        assert_eq!(
            serde_json::to_value(&wire).ok(),
            Some(json!({
                "code": "rev_conflict",
                "expected": 4,
                "current": 5,
                "message": "expected rev 4, but the state is at rev 5"
            }))
        );
    }

    #[test]
    fn messages_carry_context() {
        let e = ProtocolError::NotFound {
            entity: EntityKind::Scene,
            id: "scn_9".into(),
        };
        assert_eq!(e.to_string(), "scene `scn_9` does not exist");
    }

    #[test]
    fn unknown_codes_from_a_newer_server_keep_their_message() {
        let parsed = serde_json::from_value::<WireError>(json!({
            "code": "quota_exceeded", "limit": 3, "message": "too many outputs"
        }))
        .expect("parses");
        assert_eq!(parsed.error, ProtocolError::Unknown);
        assert_eq!(parsed.message, "too many outputs");
    }

    #[test]
    fn blockers_are_omitted_when_empty_and_default_when_missing() {
        let plain = ProtocolError::invalid_argument("edits.0.name", "must not be empty");
        assert_eq!(
            serde_json::to_value(&plain).ok(),
            Some(json!({
                "code": "invalid_argument",
                "field": "edits.0.name",
                "reason": "must not be empty"
            }))
        );
        let blocked = ProtocolError::InvalidArgument {
            field: "edits.0.source_id".into(),
            reason: "in use".into(),
            blockers: vec![Blocker {
                entity: EntityKind::Item,
                id: "i1".into(),
                field: "source_id".into(),
            }],
        };
        let wire = serde_json::to_value(&blocked).expect("serializes");
        assert_eq!(
            wire["blockers"],
            json!([{"entity": "item", "id": "i1", "field": "source_id"}])
        );
        assert_eq!(
            serde_json::from_value::<ProtocolError>(wire).ok(),
            Some(blocked)
        );
    }

    #[test]
    fn is_a_std_error() {
        fn takes(_: &dyn std::error::Error) {}
        takes(&ProtocolError::EmptyBatch);
    }
}
