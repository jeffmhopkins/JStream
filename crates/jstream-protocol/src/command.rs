//! Commands: edits and actions (ADR-0013, docs/adr/0013-command-model.md).
//!
//! - An [`Edit`] changes the collection state. Edits are only ever sent inside a
//!   [`Batch`], which the engine applies atomically, increments `rev` by one, and records
//!   as one undo step for the sending client.
//! - An [`Action`] has side effects outside the collection state (starting an output,
//!   going live). Actions are never undoable and never batchable.
//!
//! The split is enforced by the types: a [`Batch`] can only hold edits.

use crate::id::{IdempotencyKey, ItemId, OutputId, Rev, SceneId, SourceId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A change to the collection state: undoable, batchable and atomic.
///
/// Serialized with an internal `type` tag holding the command name (`noun.verb`,
/// R-API-002). Every variant documents its inverse. The engine (`jstream-graph`)
/// computes the inverse from the state *before* the edit, so the protocol only has to be
/// closed under inversion: every inverse is itself a sequence of edits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum Edit {
    /// Creates an empty scene. Inverse: `scene.delete`.
    #[serde(rename = "scene.create")]
    SceneCreate {
        /// The new scene's ID, chosen by the client. Rejected if it exists.
        scene_id: SceneId,
        /// Display name.
        name: String,
    },
    /// Renames a scene. Inverse: `scene.rename` back to the previous name.
    #[serde(rename = "scene.rename")]
    SceneRename {
        /// The scene to rename.
        scene_id: SceneId,
        /// The new display name.
        name: String,
    },
    /// Deletes a scene and its items. Inverse: `scene.create` followed by one `item.add`
    /// (with its transform) per deleted item, in order.
    #[serde(rename = "scene.delete")]
    SceneDelete {
        /// The scene to delete.
        scene_id: SceneId,
    },
    /// Places a source in a scene as a new item, on top. Inverse: `item.remove`.
    #[serde(rename = "item.add")]
    ItemAdd {
        /// The scene to add to.
        scene_id: SceneId,
        /// The new item's ID, chosen by the client. Rejected if it exists.
        item_id: ItemId,
        /// The source the item shows.
        source_id: SourceId,
        /// Initial transform. Omitted means [`ItemTransform::IDENTITY`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transform: Option<ItemTransform>,
    },
    /// Removes an item from its scene. The source itself stays. Inverse: `item.add` with
    /// the removed item's scene, ID, source and transform. (Restoring its stacking
    /// position needs an item-order edit, which is not in this first cut.)
    #[serde(rename = "item.remove")]
    ItemRemove {
        /// The item to remove.
        item_id: ItemId,
    },
    /// Replaces an item's transform. Inverse: `item.set_transform` with the previous one.
    #[serde(rename = "item.set_transform")]
    ItemSetTransform {
        /// The item to move.
        item_id: ItemId,
        /// The new transform.
        transform: ItemTransform,
    },
}

impl Edit {
    /// The command name, as used in the `type` tag (`scene.create`).
    pub fn name(&self) -> &'static str {
        match self {
            Edit::SceneCreate { .. } => "scene.create",
            Edit::SceneRename { .. } => "scene.rename",
            Edit::SceneDelete { .. } => "scene.delete",
            Edit::ItemAdd { .. } => "item.add",
            Edit::ItemRemove { .. } => "item.remove",
            Edit::ItemSetTransform { .. } => "item.set_transform",
        }
    }
}

/// A command with side effects outside the collection state: never undoable and never
/// batchable.
///
/// Serialized with an internal `type` tag holding the command name. The response only
/// says the action was accepted; how it goes is reported by events
/// (`output.state_changed`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum Action {
    /// Starts an output (stream, recording or virtual camera).
    #[serde(rename = "output.start")]
    OutputStart {
        /// The output to start.
        output_id: OutputId,
    },
    /// Stops an output.
    #[serde(rename = "output.stop")]
    OutputStop {
        /// The output to stop.
        output_id: OutputId,
    },
}

impl Action {
    /// The command name, as used in the `type` tag (`output.start`).
    pub fn name(&self) -> &'static str {
        match self {
            Action::OutputStart { .. } => "output.start",
            Action::OutputStop { .. } => "output.stop",
        }
    }
}

/// One or more edits, applied atomically.
///
/// The engine checks `expected_rev`, then applies the edits in order, each against the
/// state the previous one left. If any edit fails, nothing is applied and `rev` doesn't
/// change. On success `rev` increases by exactly one, whatever the number of edits, and
/// the batch becomes one undo step for the sending client.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Batch {
    /// The edits, in order. An empty list is rejected with `empty_batch`.
    #[schemars(length(min = 1))]
    pub edits: Vec<Edit>,
    /// Apply only if the state is still at this revision, else fail with `rev_conflict`
    /// (optimistic concurrency). Omitted means apply against whatever is current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_rev: Option<Rev>,
    /// Makes a retry safe: a batch whose key the engine has already applied for this
    /// client is not applied again, and gets the original result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<IdempotencyKey>,
}

impl Batch {
    /// A batch of one edit, with no `expected_rev` and no idempotency key.
    pub fn single(edit: Edit) -> Self {
        Batch {
            edits: vec![edit],
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

/// The two kinds of command ADR-0013 distinguishes (R-ARCH-006).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CommandClass {
    /// Changes the collection state: undoable, batchable, atomic.
    Edit,
    /// Has side effects: never undoable or batchable.
    Action,
}

/// Any single command, edit or action. A classification view used by hosts (the MCP
/// tool list, the audit log); on the wire edits travel in a [`Batch`] and actions alone.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Command {
    /// A state edit.
    Edit(Edit),
    /// A side-effecting action.
    Action(Action),
}

impl Command {
    /// The command name (`noun.verb`).
    pub fn name(&self) -> &'static str {
        match self {
            Command::Edit(e) => e.name(),
            Command::Action(a) => a.name(),
        }
    }

    /// Whether this is an edit or an action.
    pub fn class(&self) -> CommandClass {
        match self {
            Command::Edit(_) => CommandClass::Edit,
            Command::Action(_) => CommandClass::Action,
        }
    }

    /// Whether the command can be undone. True exactly for edits.
    pub fn is_undoable(&self) -> bool {
        self.class() == CommandClass::Edit
    }

    /// Whether the command may appear in a [`Batch`]. True exactly for edits.
    pub fn is_batchable(&self) -> bool {
        self.class() == CommandClass::Edit
    }
}

impl From<Edit> for Command {
    fn from(edit: Edit) -> Self {
        Command::Edit(edit)
    }
}

impl From<Action> for Command {
    fn from(action: Action) -> Self {
        Command::Action(action)
    }
}

/// A 2D position or scale, in output pixels (position) or as a factor (scale).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Vec2 {
    /// Horizontal component.
    pub x: f32,
    /// Vertical component.
    pub y: f32,
}

/// Where a scene item sits in its scene.
///
/// A deliberately small first cut: position, scale and rotation about the item's
/// top-left corner. The full 3D transform and crop (ARCH data model) replace it in a
/// later minor version, as added fields with defaults.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ItemTransform {
    /// Top-left corner, in pixels of the scene's canvas.
    pub position: Vec2,
    /// Scale factors; 1.0 is the source's native size.
    pub scale: Vec2,
    /// Clockwise rotation, in degrees.
    pub rotation_deg: f32,
}

impl ItemTransform {
    /// No offset, native size, no rotation.
    pub const IDENTITY: ItemTransform = ItemTransform {
        position: Vec2 { x: 0.0, y: 0.0 },
        scale: Vec2 { x: 1.0, y: 1.0 },
        rotation_deg: 0.0,
    };
}

impl Default for ItemTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, Batch, Command, CommandClass, Edit};
    use crate::id::{IdempotencyKey, OutputId, Rev, SceneId};
    use serde_json::json;

    #[test]
    fn edits_and_actions_are_classified_apart() {
        let edit = Command::from(Edit::SceneDelete {
            scene_id: SceneId::new("s"),
        });
        assert_eq!(edit.class(), CommandClass::Edit);
        assert!(edit.is_undoable() && edit.is_batchable());

        let action = Command::from(Action::OutputStop {
            output_id: OutputId::new("o"),
        });
        assert_eq!(action.class(), CommandClass::Action);
        assert!(!action.is_undoable() && !action.is_batchable());
    }

    #[test]
    fn edit_wire_form_is_flat_with_a_type_tag() {
        let edit = Edit::SceneRename {
            scene_id: SceneId::new("scn_1"),
            name: "Intro".into(),
        };
        assert_eq!(
            serde_json::to_value(&edit).ok(),
            Some(json!({"type": "scene.rename", "scene_id": "scn_1", "name": "Intro"}))
        );
    }

    #[test]
    fn batch_omits_unset_options() {
        let batch = Batch::single(Edit::SceneDelete {
            scene_id: SceneId::new("s"),
        });
        assert_eq!(
            serde_json::to_value(&batch).ok(),
            Some(json!({"edits": [{"type": "scene.delete", "scene_id": "s"}]}))
        );
        let batch = batch
            .expecting(Rev(3))
            .with_idempotency_key(IdempotencyKey::new("k"));
        assert_eq!(
            serde_json::to_value(&batch).ok(),
            Some(json!({
                "edits": [{"type": "scene.delete", "scene_id": "s"}],
                "expected_rev": 3,
                "idempotency_key": "k"
            }))
        );
    }

    #[test]
    fn unknown_command_names_are_rejected() {
        let parsed = serde_json::from_value::<Edit>(json!({"type": "scene.explode"}));
        assert!(parsed.is_err());
        // An action is not an edit, so it can't be smuggled into a batch.
        let parsed = serde_json::from_value::<Batch>(
            json!({"edits": [{"type": "output.start", "output_id": "o"}]}),
        );
        assert!(parsed.is_err());
    }
}
