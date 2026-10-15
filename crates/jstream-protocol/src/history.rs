//! History calls: per-client undo and redo (ADR-0032, docs/adr/0032-live-edits-and-undo.md).
//!
//! `history.undo` and `history.redo` are a fourth kind of call, `history`, beside
//! `batch`, `action` and `query`. They carry no command: each asks the engine to apply a
//! batch of edits it already holds, the inverse of one of the client's own steps (undo)
//! or that step again (redo). Every state change they make is an edit, so R-ARCH-006
//! still covers every command.

use crate::id::{IdempotencyKey, Rev};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// An undo or redo request, sent as `{"kind":"history","op":{"type":"history.undo"}}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HistoryCall {
    /// Which way to move through the client's history.
    pub op: HistoryOp,
    /// Apply only if the state is still at this revision, else fail with `rev_conflict`.
    /// Checked before anything else about the history. Omitted means apply against
    /// whatever is current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_rev: Option<Rev>,
    /// Makes a retry safe, sharing the client's batch keys: a retried undo whose answer
    /// was lost gets the original result and does not undo a second step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<IdempotencyKey>,
}

impl HistoryCall {
    /// A history call with no `expected_rev` and no idempotency key.
    pub fn new(op: HistoryOp) -> Self {
        HistoryCall {
            op,
            expected_rev: None,
            idempotency_key: None,
        }
    }

    /// Sets `expected_rev`.
    #[must_use]
    pub fn expecting(mut self, rev: Rev) -> Self {
        self.expected_rev = Some(rev);
        self
    }

    /// Sets the idempotency key.
    #[must_use]
    pub fn with_idempotency_key(mut self, key: IdempotencyKey) -> Self {
        self.idempotency_key = Some(key);
        self
    }
}

/// What a [`HistoryCall`] asks for. Serialized with an internal `type` tag holding the
/// call's name (`history.undo`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum HistoryOp {
    /// Undoes the client's most recent step that is not undone yet: applies its inverse
    /// and moves it to the redo stack. Fails with `history_empty` when there is none, and
    /// with `undo_conflict` when another client has since changed what the step touched.
    #[serde(rename = "history.undo")]
    Undo,
    /// Applies the client's most recently undone step again and moves it back to the
    /// undo stack. The client's next batch clears what can be redone.
    #[serde(rename = "history.redo")]
    Redo,
}

impl HistoryOp {
    /// The call's name, as used in the `type` tag (`history.undo`).
    pub fn name(self) -> &'static str {
        match self {
            HistoryOp::Undo => "history.undo",
            HistoryOp::Redo => "history.redo",
        }
    }

    /// The direction this op moves through the history.
    pub fn direction(self) -> HistoryDirection {
        match self {
            HistoryOp::Undo => HistoryDirection::Undo,
            HistoryOp::Redo => HistoryDirection::Redo,
        }
    }
}

/// A direction through a client's history: the `cause` of a `state.changed` that an
/// undo or redo produced, and the `direction` of a `history_empty` error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HistoryDirection {
    /// Backward: an undo.
    Undo,
    /// Forward again: a redo.
    Redo,
}

impl fmt::Display for HistoryDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            HistoryDirection::Undo => "undo",
            HistoryDirection::Redo => "redo",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{HistoryCall, HistoryOp};
    use crate::envelope::Call;
    use crate::id::{IdempotencyKey, Rev};
    use serde_json::json;

    #[test]
    fn history_call_wire_form() {
        let call = Call::History(
            HistoryCall::new(HistoryOp::Undo)
                .expecting(Rev(7))
                .with_idempotency_key(IdempotencyKey::new("u-1")),
        );
        assert_eq!(
            serde_json::to_value(&call).ok(),
            Some(json!({
                "kind": "history",
                "op": {"type": "history.undo"},
                "expected_rev": 7,
                "idempotency_key": "u-1"
            }))
        );
    }

    /// A history call carries no command (R-ARCH-006): an `edits` or `action` key is
    /// not part of it, and an op is never an edit or action name.
    #[test]
    fn history_call_carries_no_command() {
        let parsed = serde_json::from_value::<Call>(json!({
            "kind": "history",
            "op": {"type": "scene.create", "scene_id": "s", "name": "x"}
        }));
        assert!(parsed.is_err());
    }
}
