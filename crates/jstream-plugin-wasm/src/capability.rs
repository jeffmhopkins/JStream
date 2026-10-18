//! Which `Control` calls a plugin may make.
//!
//! `Control` is full access to the engine (ADR-0006), so a plugin never gets it whole.
//! Each plugin carries a list of command names the user granted (`scene.list`,
//! `item.set_visible`, `output.start`), and a call reaches `Control` only when every
//! command in it is on that list. The list is empty by default. Which commands are safe to
//! offer, and how a user grants them, is issue #348's decision, not this crate's.

use jstream_control::protocol::Call;
use std::collections::BTreeSet;

/// The commands one plugin may call, by protocol name. Empty by default: a plugin with no
/// grants can run, but every call it makes is denied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    granted: BTreeSet<String>,
}

impl Capabilities {
    /// No commands granted.
    pub fn none() -> Self {
        Self::default()
    }

    /// Grants exactly the named commands (`scene.list`, `history.undo`). Names are matched
    /// exactly: there are no wildcards or prefixes.
    pub fn from_names<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Capabilities {
            granted: names.into_iter().map(Into::into).collect(),
        }
    }

    /// Whether the command `name` is granted.
    pub fn allows(&self, name: &str) -> bool {
        self.granted.contains(name)
    }

    /// The granted command names, sorted.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.granted.iter().map(String::as_str)
    }

    /// Checks `call` against the grants. `Ok` when the call names at least one command
    /// and every one of them is granted; otherwise the names that are not, which is empty
    /// for a call that names no command (an empty batch, or a kind of call this version
    /// doesn't know).
    pub fn check(&self, call: &Call) -> Result<(), Vec<&'static str>> {
        let Some(names) = command_names(call).filter(|names| !names.is_empty()) else {
            return Err(Vec::new());
        };
        let denied: Vec<&'static str> = names.into_iter().filter(|n| !self.allows(n)).collect();
        if denied.is_empty() {
            Ok(())
        } else {
            Err(denied)
        }
    }
}

/// The protocol names of the commands `call` carries: every edit of a batch, in order, or
/// the one action, query or history operation. `None` for a kind of call this version
/// doesn't know, which no grant can allow.
pub fn command_names(call: &Call) -> Option<Vec<&'static str>> {
    match call {
        Call::Batch(batch) => Some(batch.edits.iter().map(|edit| edit.name()).collect()),
        Call::Action { action } => Some(vec![action.name()]),
        Call::Query { query } => Some(vec![query.name()]),
        Call::History(history) => Some(vec![history.op.name()]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::Capabilities;
    use jstream_control::protocol::{
        Action, Batch, Call, Edit, HistoryCall, HistoryOp, OutputId, Query, SceneId,
    };

    fn query(query: Query) -> Call {
        Call::Query { query }
    }

    #[test]
    fn nothing_is_granted_by_default() {
        let caps = Capabilities::default();
        assert_eq!(caps, Capabilities::none());
        assert_eq!(
            caps.check(&query(Query::SceneList)),
            Err(vec!["scene.list"])
        );
        let start = Call::Action {
            action: Action::OutputStart {
                output_id: OutputId::from("main"),
            },
        };
        assert_eq!(caps.check(&start), Err(vec!["output.start"]));
    }

    #[test]
    fn a_grant_allows_exactly_its_command() {
        let caps = Capabilities::from_names(["scene.list"]);
        assert_eq!(caps.check(&query(Query::SceneList)), Ok(()));
        assert_eq!(caps.check(&query(Query::StateGet)), Err(vec!["state.get"]));
        assert!(!caps.allows("scene"));
        assert!(!caps.allows("scene.*"));
    }

    #[test]
    fn a_batch_needs_every_edit_granted() {
        let edit = |name: &str| Edit::SceneRename {
            scene_id: SceneId::from("s"),
            name: name.into(),
        };
        let delete = Edit::SceneDelete {
            scene_id: SceneId::from("s"),
        };
        let batch = Call::Batch(Batch {
            edits: vec![edit("a"), delete],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        });
        let caps = Capabilities::from_names(["scene.rename"]);
        assert_eq!(caps.check(&batch), Err(vec!["scene.delete"]));
        let caps = Capabilities::from_names(["scene.rename", "scene.delete"]);
        assert_eq!(caps.check(&batch), Ok(()));
    }

    #[test]
    fn an_empty_batch_is_denied() {
        let caps = Capabilities::from_names(["scene.rename"]);
        let empty = Call::Batch(Batch {
            edits: Vec::new(),
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        });
        assert_eq!(caps.check(&empty), Err(Vec::new()));
    }

    #[test]
    fn history_calls_are_named_by_their_op() {
        let caps = Capabilities::from_names(["history.undo"]);
        assert_eq!(
            caps.check(&Call::History(HistoryCall::new(HistoryOp::Undo))),
            Ok(())
        );
        assert_eq!(
            caps.check(&Call::History(HistoryCall::new(HistoryOp::Redo))),
            Err(vec!["history.redo"])
        );
    }
}
