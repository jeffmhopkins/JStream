//! The envelope: every JSON message on a connection is a [`ClientMessage`] or a
//! [`ServerMessage`].
//!
//! A connection starts with the handshake: the client sends `hello` with its protocol
//! version, and the server answers `welcome` (or an `unsupported_version` error and
//! closes). After that the client sends `request`s, each with its own ID, and the server
//! sends exactly one `response` or `error` per request, plus `event`s at any time.
//! Responses may arrive out of order; the request ID matches them up.

use crate::command::{Action, Batch};
use crate::error::{ProtocolError, WireError};
use crate::event::Event;
use crate::history::HistoryCall;
use crate::id::{ClientId, RequestId, Rev};
use crate::query::{Query, QueryResult};
use crate::version::ProtocolVersion;
use crate::youtube::ActionResult;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A message from a client to the server. Tagged by `type`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClientMessage {
    /// Opens the session. Must be the first message.
    Hello(Hello),
    /// A batch of edits, an action or a query.
    Request(Request),
}

impl ClientMessage {
    /// Parses one JSON text message from a client. Any parse failure (bad JSON, an
    /// unknown `type`, a missing or mistyped field, or a number that isn't a finite
    /// `f32`) is [`ProtocolError::Malformed`], with serde's description as the reason.
    pub fn parse(text: &str) -> Result<ClientMessage, ProtocolError> {
        serde_json::from_str(text).map_err(|e| ProtocolError::Malformed {
            reason: e.to_string(),
        })
    }
}

/// A message from the server to a client. Tagged by `type`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ServerMessage {
    /// Accepts the handshake.
    Welcome(Welcome),
    /// A request succeeded.
    Response(Response),
    /// A request (or the handshake, or an unparseable message) failed.
    Error(ErrorResponse),
    /// Something happened in the engine.
    Event(EventMessage),
    /// A message type this version of the protocol doesn't know. Never sent; produced
    /// only when parsing a message from a newer server. Clients ignore it.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// The client's opening message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Hello {
    /// The protocol version the client speaks.
    pub protocol: ProtocolVersion,
    /// A name for the client (`jstream-cli`, `claude-desktop`), shown in the audit log.
    pub client_name: String,
}

/// The server's answer to [`Hello`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Welcome {
    /// The protocol version the server speaks.
    pub protocol: ProtocolVersion,
    /// The identity this connection acts as: its undo history and audit-log entries are
    /// keyed by it.
    pub client_id: ClientId,
    /// The current state revision, so the client knows where it starts.
    pub rev: Rev,
}

/// One request: a call plus the client-chosen ID its answer will carry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Request {
    /// Echoed in the matching `response` or `error`.
    pub id: RequestId,
    /// What to do.
    pub call: Call,
}

/// What a request asks for. Tagged by `kind`, so the class of every call (ADR-0013) is
/// explicit on the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Call {
    /// Apply edits atomically. A single edit is a batch of one.
    Batch(Batch),
    /// Perform one action.
    Action {
        /// The action.
        action: Action,
    },
    /// Read something.
    Query {
        /// The query.
        query: Query,
    },
    /// Undo or redo one of this client's own steps (ADR-0032). Carries no command: the
    /// engine applies a batch of edits it already holds.
    History(HistoryCall),
}

/// A successful answer to a [`Request`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Response {
    /// The request's ID.
    pub id: RequestId,
    /// What happened. Its `kind` matches the request's call.
    pub result: CallResult,
}

/// The result of a [`Call`], tagged by the same `kind`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CallResult {
    /// The batch was applied (or had already been applied under its idempotency key).
    Batch {
        /// The revision the batch produced.
        rev: Rev,
    },
    /// The action was accepted. Its progress is reported by events.
    Action {
        /// What the action returns, for the actions that return something
        /// (`youtube.sign_in`, the YouTube broadcast actions). Omitted for the others,
        /// and missing from a server older than 0.20.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<ActionResult>,
    },
    /// The query's answer.
    Query {
        /// The result.
        result: QueryResult,
    },
    /// The undo or redo was applied (or had already been applied under its idempotency
    /// key).
    History {
        /// The revision the undo or redo produced.
        rev: Rev,
        /// The revision the step's batch originally produced. It names the step.
        step_rev: Rev,
        /// How many steps the client can still undo.
        undo_depth: u32,
        /// How many steps the client can redo.
        redo_depth: u32,
    },
}

/// A failed request, a failed handshake, or a message that couldn't be parsed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorResponse {
    /// The failed request's ID. Absent when the failure isn't tied to a request (the
    /// handshake, or a message too broken to read an ID from).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<RequestId>,
    /// What went wrong.
    pub error: WireError,
}

impl ErrorResponse {
    /// An error answering request `id`.
    pub fn for_request(id: RequestId, error: ProtocolError) -> Self {
        ErrorResponse {
            id: Some(id),
            error: error.into(),
        }
    }
}

/// An event, as pushed to clients.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EventMessage {
    /// The event.
    pub event: Event,
}

#[cfg(test)]
mod tests {
    use super::{Call, ClientMessage, ErrorResponse, Request, ServerMessage};
    use crate::command::{Batch, Edit};
    use crate::error::ProtocolError;
    use crate::id::{RequestId, Rev, SceneId};
    use serde_json::json;

    #[test]
    fn request_wire_form() {
        let msg = ClientMessage::Request(Request {
            id: RequestId::new("r1"),
            call: Call::Batch(
                Batch::single(Edit::SceneCreate {
                    scene_id: SceneId::new("scn_1"),
                    name: "Main".into(),
                    role: None,
                })
                .expecting(Rev(4)),
            ),
        });
        assert_eq!(
            serde_json::to_value(&msg).ok(),
            Some(json!({
                "type": "request",
                "id": "r1",
                "call": {
                    "kind": "batch",
                    "edits": [{"type": "scene.create", "scene_id": "scn_1", "name": "Main"}],
                    "expected_rev": 4
                }
            }))
        );
    }

    #[test]
    fn error_wire_form() {
        let msg = ServerMessage::Error(ErrorResponse::for_request(
            RequestId::new("r1"),
            ProtocolError::EmptyBatch,
        ));
        assert_eq!(
            serde_json::to_value(&msg).ok(),
            Some(json!({
                "type": "error",
                "id": "r1",
                "error": {"code": "empty_batch", "message": "a batch must contain at least one edit"}
            }))
        );
    }

    #[test]
    fn non_finite_numbers_are_malformed() {
        let text = r#"{"type":"request","id":"r1","call":{"kind":"batch","edits":[{"type":"item.set_transform","item_id":"i","transform":{"position":{"x":1e39,"y":0},"scale":{"x":1,"y":1},"rotation_deg":0}}]}}"#;
        assert!(matches!(
            ClientMessage::parse(text),
            Err(ProtocolError::Malformed { .. })
        ));
        let ok = text.replace("1e39", "3.4028235e38");
        assert!(ClientMessage::parse(&ok).is_ok());
    }

    /// A settings value written with more digits than an `f64` holds parses to the
    /// nearest `f64`, so the message serializes and parses back to itself: the collection
    /// file saves and reloads it unchanged. Found by the `protocol_decode` fuzz target.
    #[test]
    fn long_floats_parse_back_to_the_same_value() {
        let digits = "4".repeat(110);
        let text = format!(
            r#"{{"type":"request","id":"r1","call":{{"kind":"batch","edits":[{{"type":"source.set_settings","source_id":"s","patch":{{"width":{digits}80.0}}}}]}}}}"#
        );
        let parsed = ClientMessage::parse(&text).expect("parses");
        let json = serde_json::to_string(&parsed).expect("serializes");
        assert_eq!(ClientMessage::parse(&json).ok(), Some(parsed));
    }

    #[test]
    fn unknown_server_messages_parse_for_forward_compatibility() {
        let parsed = serde_json::from_value::<ServerMessage>(json!({"type": "goodbye"}));
        assert_eq!(parsed.ok(), Some(ServerMessage::Unknown));
    }

    #[test]
    fn unknown_client_messages_are_rejected() {
        let parsed = serde_json::from_value::<ClientMessage>(json!({"type": "subscribe"}));
        assert!(parsed.is_err());
    }
}
