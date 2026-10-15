//! Commands: edits and actions (ADR-0013, docs/adr/0013-command-model.md).
//!
//! - An [`Edit`] changes the collection state. Edits are only ever sent inside a
//!   [`Batch`], which the engine applies atomically, increments `rev` by one, and records
//!   as one undo step for the sending client.
//! - An [`Action`] has side effects outside the collection state (starting an output,
//!   going live, switching the program scene). Actions are never undoable and never
//!   batchable.
//!
//! The split is enforced by the types: a [`Batch`] can only hold edits.

use crate::audio::AudioFollow;
use crate::encoder::EncoderProfile;
use crate::finite::Finite32;
use crate::geometry::ItemGeometry;
use crate::id::{
    BusId, EncoderProfileId, IdempotencyKey, ItemId, OutputId, Rev, SceneId, SourceId,
};
use crate::output::{OutputKind, RtmpTarget, SecretString};
use crate::settings::Settings;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A change to the collection state: undoable, batchable and atomic.
///
/// Serialized with an internal `type` tag holding the command name (`noun.verb`,
/// R-API-002). Every variant documents its inverse. The engine (`jstream-graph`)
/// computes the inverse from the state *before* the edit, so the protocol has to be
/// closed under inversion: every inverse must itself be a sequence of edits, and it is.
///
/// Positions are indexes: scenes in display order (0 is first), items in stacking order
/// within their scene (0 is the bottom).
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
        /// What the scene is for (ADR-0024 §1). Omitted means `scene`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<SceneRole>,
    },
    /// Renames a scene. Inverse: `scene.rename` back to the previous name.
    #[serde(rename = "scene.rename")]
    SceneRename {
        /// The scene to rename.
        scene_id: SceneId,
        /// The new display name.
        name: String,
    },
    /// Changes what a scene is for (ADR-0024 §1): a `scene`, or a `group` placed in
    /// other scenes through a `scene` source. The role changes no engine behaviour: a
    /// group renders, nests and can be placed more than once like any scene, and clients
    /// hide groups from the scene list. Inverse: `scene.set_role` with the previous role.
    #[serde(rename = "scene.set_role")]
    SceneSetRole {
        /// The scene to change.
        scene_id: SceneId,
        /// Its new role.
        role: SceneRole,
    },
    /// Deletes a scene and its items; the sources they placed stay. Rejected with
    /// `invalid_argument` while a `scene` source shows it or any source's settings refer
    /// to it: delete those sources, or clear those references, first (in the same batch
    /// if needed). Inverse: `scene.create` with its role (omitted for `scene`), then
    /// `scene.reorder` to its old display position (unless it was last), then each
    /// deleted item's `item.remove` inverse, from bottom to top.
    #[serde(rename = "scene.delete")]
    SceneDelete {
        /// The scene to delete.
        scene_id: SceneId,
    },
    /// Places an existing source in a scene as a new item, on top. The item refers to the
    /// source, it doesn't copy it: one source can be placed in many scenes, or several
    /// times in one, and every placement shows the same instance (`source.duplicate`
    /// makes a separate copy). Inverse: `item.remove`.
    ///
    /// The item's placement is `geometry` when that is sent, else a legacy placement:
    /// `transform`, or [`ItemTransform::IDENTITY`] when both are omitted (ADR-0022).
    /// Sending both is `invalid_argument`.
    #[serde(rename = "item.add")]
    ItemAdd {
        /// The scene to add to.
        scene_id: SceneId,
        /// The new item's ID, chosen by the client. Rejected if it exists.
        item_id: ItemId,
        /// The source the item shows.
        source_id: SourceId,
        /// **Deprecated** since 0.11 (ADR-0022), removed in 1.0: send `geometry`. The
        /// initial legacy placement, with its 0.3 meaning.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transform: Option<ItemTransform>,
        /// The initial placement as a box on the canvas, checked like
        /// `item.set_geometry`'s.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        geometry: Option<ItemGeometry>,
    },
    /// Removes an item from its scene. The source itself stays. Inverse: `item.add` with
    /// the removed item's scene, ID and source, and its placement in the field matching
    /// its form (`transform` or `geometry`), then `item.reorder` to its old stacking
    /// position (unless it was on top), `item.set_crop` if it was cropped, and
    /// `item.set_visible` if it was hidden.
    #[serde(rename = "item.remove")]
    ItemRemove {
        /// The item to remove.
        item_id: ItemId,
    },
    /// Moves an item to a stacking position in another scene, keeping its ID, source,
    /// placement, crop and visibility (ADR-0024 §1). Grouping is a batch that creates a
    /// group scene, places it, and moves the selected items into it. Rejected with
    /// `invalid_argument` if the move would make a scene contain itself (field
    /// `scene_id`, naming the cycle) or if the index is out of range. Inverse:
    /// `item.move` back to its previous scene and index.
    #[serde(rename = "item.move")]
    ItemMove {
        /// The item to move.
        item_id: ItemId,
        /// The scene to move it to. Its own scene is allowed, and then the move is an
        /// `item.reorder`.
        scene_id: SceneId,
        /// Its position in that scene after the move, counted from the bottom (0). Must
        /// be at most the number of items already in that scene (less than it for the
        /// item's own scene), else `invalid_argument`.
        index: u32,
    },
    /// **Deprecated** since 0.11 (ADR-0022), removed in 1.0: send `item.set_geometry`.
    /// Replaces an item's placement with a legacy one, which keeps its 0.3 meaning.
    /// Inverse: the previous placement, as `item.set_transform` if it was legacy and
    /// `item.set_geometry` if it was a geometry.
    #[serde(rename = "item.set_transform")]
    ItemSetTransform {
        /// The item to move.
        item_id: ItemId,
        /// The scene whose placement of the item to change. See [`Edit::ItemSetVisible`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scene_id: Option<SceneId>,
        /// The new transform.
        transform: ItemTransform,
    },
    /// Replaces an item's placement with a box on the canvas (ADR-0022). A size that is
    /// not greater than zero, a `perspective_px` that is not greater than zero, and any
    /// non-neutral `tilt_deg`, `turn_deg`, `depth_px` or `perspective_px` (the
    /// compositor does not draw 3D yet) are `invalid_argument`. Inverse: the previous
    /// placement, as `item.set_geometry` if it was a geometry and `item.set_transform`
    /// if it was legacy.
    #[serde(rename = "item.set_geometry")]
    ItemSetGeometry {
        /// The item to place.
        item_id: ItemId,
        /// The scene whose placement of the item to change. See [`Edit::ItemSetVisible`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scene_id: Option<SceneId>,
        /// The new geometry.
        geometry: ItemGeometry,
    },
    /// Shows or hides an item. Inverse: `item.set_visible` with the previous value.
    #[serde(rename = "item.set_visible")]
    ItemSetVisible {
        /// The item to show or hide.
        item_id: ItemId,
        /// The scene whose placement of the item to change, as every placement edit
        /// takes it (ADR-0024 §2). Omitted, or the item's own scene, changes the item.
        /// Any other scene is `invalid_argument` until keyframe scenes exist, when naming
        /// a keyframe scene of the item's scene will write that scene's override.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scene_id: Option<SceneId>,
        /// Whether the item is drawn. Items are visible when added.
        visible: bool,
    },
    /// Moves an item to another stacking position in its scene. Inverse: `item.reorder`
    /// back to its previous index.
    #[serde(rename = "item.reorder")]
    ItemReorder {
        /// The item to move.
        item_id: ItemId,
        /// The new position, counted from the bottom (0). Must be less than the number of
        /// items in the scene, else `invalid_argument`. The other items keep their
        /// relative order.
        index: u32,
    },
    /// Changes which source an item places, keeping the item's ID, transform, crop,
    /// visibility and stacking position (ADR-0023). It switches an item between a
    /// reference and a clone: `source.duplicate` then `item.set_source` to the copy makes
    /// a clone, and `item.set_source` back to the copy's `duplicated_from` (then
    /// `source.delete` of the copy if nothing else places it) makes it a reference again.
    /// Rejected with `not_found` if the item or the source doesn't exist, and with
    /// `invalid_argument` naming the cycle if the scene would then contain itself,
    /// directly or through other scenes. Inverse: `item.set_source` with the previous
    /// source.
    #[serde(rename = "item.set_source")]
    ItemSetSource {
        /// The item to change.
        item_id: ItemId,
        /// The source it places from now on.
        source_id: SourceId,
    },
    /// Replaces an item's crop. Inverse: `item.set_crop` with the previous one.
    #[serde(rename = "item.set_crop")]
    ItemSetCrop {
        /// The item to crop.
        item_id: ItemId,
        /// The scene whose placement of the item to change. See [`Edit::ItemSetVisible`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scene_id: Option<SceneId>,
        /// The new crop. Items are added uncropped ([`Crop::NONE`]).
        crop: Crop,
    },
    /// Adds a source to the collection. It appears in no scene until an `item.add`
    /// places it. An audio source (its kind produces audio, or it is a `scene` source)
    /// starts with its kind's default audio settings, and one that is not a `scene`
    /// source sends to the master bus at 0 dB (ADR-0028). Inverse: `source.delete`.
    #[serde(rename = "source.create")]
    SourceCreate {
        /// The new source's ID, chosen by the client. Rejected if it exists.
        source_id: SourceId,
        /// Display name.
        name: String,
        /// What the source shows. A `scene` source must name an existing scene; a
        /// `builtin` one a kind that `kind.list` reports.
        kind: SourceKind,
        /// Initial settings, checked against the kind's descriptor (`kind.describe`).
        /// Parameters left out, or sent as `null`, take their defaults, and the engine
        /// stores the complete map. A `scene` source has no settings. Omitted means all
        /// defaults.
        #[serde(default, skip_serializing_if = "Settings::is_empty")]
        settings: Settings,
        /// The source this one was copied from, as `source.duplicate` records it. A plain
        /// link: it may name a source that no longer exists (`source.delete`'s inverse
        /// restores it that way), and is reported only while that source exists. It must
        /// not name the new source itself, else `invalid_argument`. Omitted means none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duplicated_from: Option<SourceId>,
    },
    /// Renames a source. Inverse: `source.rename` back to the previous name.
    #[serde(rename = "source.rename")]
    SourceRename {
        /// The source to rename.
        source_id: SourceId,
        /// The new display name.
        name: String,
    },
    /// Changes some of a source's settings. Each entry of `patch` is checked against the
    /// kind's descriptor like `source.create`'s settings, and replaces that parameter's
    /// value; `null` resets a parameter to its default (a reference parameter to no
    /// reference). Parameters not in the patch keep their values. A reference that would
    /// make a cycle is rejected with `invalid_argument` naming it. Inverse:
    /// `source.set_settings` with the patched parameters' previous values.
    #[serde(rename = "source.set_settings")]
    SourceSetSettings {
        /// The source to change.
        source_id: SourceId,
        /// Parameter ID to new value, or `null` for the default.
        patch: Settings,
    },
    /// Copies a source under a new ID: a separate instance with the same kind,
    /// settings, audio settings and sends, which changes independently from then on. The copy's `duplicated_from`
    /// names the source it was copied from. Placing the *same* source again is `item.add`
    /// with its existing `source_id`. Inverse: `source.delete` of the copy.
    #[serde(rename = "source.duplicate")]
    SourceDuplicate {
        /// The source to copy.
        source_id: SourceId,
        /// The copy's ID, chosen by the client. Rejected if it exists.
        new_source_id: SourceId,
        /// The copy's display name.
        name: String,
    },
    /// Deletes a source. Rejected with `invalid_argument` while any item still places it,
    /// or any source's settings refer to it: remove those items, or clear those
    /// references, first (in the same batch if needed). Deleting a source that others were
    /// copied from is allowed: their `duplicated_from` then reads as absent. Its sends go
    /// with it. Inverse: `source.create` with its name, kind, settings and
    /// `duplicated_from`, then `send.remove` of its master send if it had none,
    /// `send.set` for each send that `source.create` doesn't restore as it was, and
    /// `source.set_audio` if its audio settings were not the defaults.
    #[serde(rename = "source.delete")]
    SourceDelete {
        /// The source to delete.
        source_id: SourceId,
    },
    /// Moves a scene to another display position. Inverse: `scene.reorder` back to its
    /// previous index.
    #[serde(rename = "scene.reorder")]
    SceneReorder {
        /// The scene to move.
        scene_id: SceneId,
        /// The new position, counted from the first scene (0). Must be less than the
        /// number of scenes, else `invalid_argument`. The other scenes keep their
        /// relative order.
        index: u32,
    },
    /// Adds an output to the collection, `stopped`. Its stream key is not part of it:
    /// set that with the `output.set_key` action. Inverse: `output.delete`.
    #[serde(rename = "output.create")]
    OutputCreate {
        /// The new output's ID, chosen by the client. Rejected if it exists.
        output_id: OutputId,
        /// What it is and where it sends. A URL that is not a valid RTMP(S) server URL,
        /// or that could carry a credential, is rejected with `invalid_argument`.
        kind: OutputKind,
    },
    /// Changes where an RTMP output publishes. A running output keeps its connection;
    /// the new target applies from its next `output.start`. Its stream key is kept.
    /// Inverse: `output.set_target` with the previous target.
    #[serde(rename = "output.set_target")]
    OutputSetTarget {
        /// The output to change.
        output_id: OutputId,
        /// The new target, validated as in `output.create`.
        target: RtmpTarget,
    },
    /// Changes the canvas: every scene's coordinate space, in whole pixels (ADR-0022).
    /// Items keep their canvas-pixel placements; nothing moves or resizes. Zero, or a
    /// side over the engine's maximum (at least 7680, both ways), is `invalid_argument`.
    /// Inverse: `canvas.set_size` with the previous size.
    #[serde(rename = "canvas.set_size")]
    CanvasSetSize {
        /// Canvas width in pixels.
        width: u32,
        /// Canvas height in pixels.
        height: u32,
    },
    /// Deletes an output. Rejected with `invalid_argument` while it runs: stop it first.
    /// Its stream key stays in the credential store, so undoing the delete restores a
    /// working output; `output.clear_key` removes it. Inverse: `output.create` with its
    /// kind.
    #[serde(rename = "output.delete")]
    OutputDelete {
        /// The output to delete.
        output_id: OutputId,
    },
    /// Chooses the encoder profile an output uses, or the engine's default. A running
    /// output keeps its encoder session; the profile applies from its next
    /// `output.start`. Inverse: `output.set_encoder` with the previous profile.
    #[serde(rename = "output.set_encoder")]
    OutputSetEncoder {
        /// The output to change.
        output_id: OutputId,
        /// The profile, which must exist, else `not_found`. Absent or `null` means the
        /// engine's default.
        #[serde(default)]
        profile_id: Option<EncoderProfileId>,
    },
    /// Adds an encoder profile. Inverse: `encoder.delete`.
    #[serde(rename = "encoder.create")]
    EncoderCreate {
        /// The new profile's ID, chosen by the client. Rejected if it exists.
        profile_id: EncoderProfileId,
        /// Display name.
        name: String,
        /// The encoding. Out-of-range values, and codecs or rate controls the engine
        /// doesn't know, are rejected with `invalid_argument`.
        profile: EncoderProfile,
    },
    /// Replaces an encoder profile's name and encoding. Running outputs keep their
    /// encoder sessions; the new encoding applies from each output's next
    /// `output.start`. Inverse: `encoder.update` with the previous name and encoding.
    #[serde(rename = "encoder.update")]
    EncoderUpdate {
        /// The profile to change.
        profile_id: EncoderProfileId,
        /// The new display name.
        name: String,
        /// The new encoding, validated as in `encoder.create`.
        profile: EncoderProfile,
    },
    /// Deletes an encoder profile. Rejected with `invalid_argument` while an output
    /// uses it: point those outputs elsewhere with `output.set_encoder` first (in the
    /// same batch if needed). Inverse: `encoder.create` with its name and encoding.
    #[serde(rename = "encoder.delete")]
    EncoderDelete {
        /// The profile to delete.
        profile_id: EncoderProfileId,
    },
    /// Changes some of an audio source's settings (ADR-0028): the fields sent change,
    /// the others keep their values, and sending none is `invalid_argument`. A source
    /// without audio is `invalid_argument` on `source_id`. A `scene` source has no fader
    /// or mute of its own, so `gain_db` or `muted` on one is `invalid_argument`, and so
    /// is a gain outside −100 to +24 dB. Inverse: `source.set_audio`
    /// with the previous values of the fields sent.
    #[serde(rename = "source.set_audio")]
    SourceSetAudio {
        /// The source to change.
        source_id: SourceId,
        /// Whether the engine opens the source's audio. Off keeps its sends, inactive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        /// When the source is heard.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        follow: Option<AudioFollow>,
        /// The fader, in dB.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gain_db: Option<Finite32>,
        /// Whether it is muted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        muted: Option<bool>,
    },
    /// Adds a bus, at 0 dB, unmuted, with no sends into it. A collection holds at most
    /// 16 buses, the master included; one more is `invalid_argument`. Inverse:
    /// `bus.delete`.
    #[serde(rename = "bus.create")]
    BusCreate {
        /// The new bus's ID, chosen by the client. Rejected if it exists, so `master` is
        /// always taken.
        bus_id: BusId,
        /// Display name.
        name: String,
    },
    /// Renames a bus, the master included. Inverse: `bus.rename` back to the previous
    /// name.
    #[serde(rename = "bus.rename")]
    BusRename {
        /// The bus to rename.
        bus_id: BusId,
        /// The new display name.
        name: String,
    },
    /// Changes a bus's fader or mute, the master's included: the fields sent change,
    /// and sending neither is `invalid_argument`, as is a gain outside −100 to +24 dB.
    /// Inverse: `bus.set_fader` with the previous values of the fields sent.
    #[serde(rename = "bus.set_fader")]
    BusSetFader {
        /// The bus to change.
        bus_id: BusId,
        /// The fader, in dB, applied after the bus's sum.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gain_db: Option<Finite32>,
        /// Whether the bus is muted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        muted: Option<bool>,
    },
    /// Deletes a bus and every send into it. The master bus can't be deleted
    /// (`invalid_argument`). Inverse: `bus.create` with its name, `bus.set_fader` if
    /// its fader or mute were not the defaults, then `send.set` for each send it had.
    #[serde(rename = "bus.delete")]
    BusDelete {
        /// The bus to delete.
        bus_id: BusId,
    },
    /// Sends a source into a bus at a gain, post-fader, or changes that send's gain.
    /// The source must be an audio source that is not a `scene` source, which has no
    /// sends of its own (`invalid_argument` on `source_id`), and the gain must be from
    /// −100 to +24 dB. Buses never feed other buses (ADR-0028), so sends can't form a
    /// cycle. Inverse: `send.remove` for a new send, else `send.set` with the previous
    /// gain.
    #[serde(rename = "send.set")]
    SendSet {
        /// The source sending.
        source_id: SourceId,
        /// The bus it sends into.
        bus_id: BusId,
        /// The send's gain, in dB.
        gain_db: Finite32,
    },
    /// Stops a source sending into a bus. No such send is `invalid_argument` on
    /// `bus_id`. Inverse: `send.set` with its gain.
    #[serde(rename = "send.remove")]
    SendRemove {
        /// The source sending.
        source_id: SourceId,
        /// The bus it sends into.
        bus_id: BusId,
    },
}

impl Edit {
    /// The command name, as used in the `type` tag (`scene.create`).
    pub fn name(&self) -> &'static str {
        match self {
            Edit::SceneCreate { .. } => "scene.create",
            Edit::SceneRename { .. } => "scene.rename",
            Edit::SceneSetRole { .. } => "scene.set_role",
            Edit::SceneDelete { .. } => "scene.delete",
            Edit::ItemAdd { .. } => "item.add",
            Edit::ItemRemove { .. } => "item.remove",
            Edit::ItemMove { .. } => "item.move",
            Edit::ItemSetTransform { .. } => "item.set_transform",
            Edit::ItemSetGeometry { .. } => "item.set_geometry",
            Edit::ItemSetVisible { .. } => "item.set_visible",
            Edit::ItemReorder { .. } => "item.reorder",
            Edit::SceneReorder { .. } => "scene.reorder",
            Edit::ItemSetCrop { .. } => "item.set_crop",
            Edit::ItemSetSource { .. } => "item.set_source",
            Edit::SourceCreate { .. } => "source.create",
            Edit::SourceRename { .. } => "source.rename",
            Edit::SourceSetSettings { .. } => "source.set_settings",
            Edit::SourceDuplicate { .. } => "source.duplicate",
            Edit::SourceDelete { .. } => "source.delete",
            Edit::OutputCreate { .. } => "output.create",
            Edit::OutputSetTarget { .. } => "output.set_target",
            Edit::OutputDelete { .. } => "output.delete",
            Edit::OutputSetEncoder { .. } => "output.set_encoder",
            Edit::EncoderCreate { .. } => "encoder.create",
            Edit::EncoderUpdate { .. } => "encoder.update",
            Edit::EncoderDelete { .. } => "encoder.delete",
            Edit::SourceSetAudio { .. } => "source.set_audio",
            Edit::BusCreate { .. } => "bus.create",
            Edit::BusRename { .. } => "bus.rename",
            Edit::BusSetFader { .. } => "bus.set_fader",
            Edit::BusDelete { .. } => "bus.delete",
            Edit::SendSet { .. } => "send.set",
            Edit::SendRemove { .. } => "send.remove",
            Edit::CanvasSetSize { .. } => "canvas.set_size",
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
    /// Stores an output's stream key in the OS credential store (R-SEC-001), replacing
    /// any key it had. Write-only: the key never comes back in a response, event, error,
    /// `state.get` or log, and `output.status` reports only `key_set`. An action, not an
    /// edit: the key is not collection state, so it is never undone. A running output
    /// uses the new key from its next `output.start`. Fails with `not_found` for an
    /// output not in the collection, `invalid_argument` (field `key`) for an empty key,
    /// one over 1024 bytes or one with whitespace or control characters, and
    /// `action_failed` when the engine has no credential store or the store refuses.
    #[serde(rename = "output.set_key")]
    OutputSetKey {
        /// The output whose key to set.
        output_id: OutputId,
        /// The stream key, as the streaming service shows it.
        key: SecretString,
    },
    /// Removes an output's stream key from the credential store. Succeeds when there is
    /// none, and for an output that is no longer in the collection, so a client can clear
    /// the key of an output it deleted. Fails with `action_failed` when the engine has no
    /// credential store or the store refuses.
    #[serde(rename = "output.clear_key")]
    OutputClearKey {
        /// The output whose key to remove.
        output_id: OutputId,
    },
    /// Restarts the master bus's integrated loudness measurement (ADR-0028). Meters are
    /// runtime state, so this is an action, never an edit. Succeeds, doing nothing, while
    /// the mixer isn't running.
    #[serde(rename = "audio.reset_loudness")]
    AudioResetLoudness {},
    /// Puts a scene on program, with a cut: the program mix (`video.snapshot`, and every
    /// output that shows program) shows it from the next frame. The program scene is
    /// runtime state, not collection state (ADR-0030): the switch changes no `rev`, is
    /// never undone, and emits `studio.changed`, which `studio.get` reads back. Switching
    /// to the scene already on program succeeds and emits nothing. Fails with
    /// `not_found` for a scene not in the collection.
    #[serde(rename = "scene.switch")]
    SceneSwitch {
        /// The scene to put on program.
        scene_id: SceneId,
    },
}

impl Action {
    /// The command name, as used in the `type` tag (`output.start`).
    pub fn name(&self) -> &'static str {
        match self {
            Action::OutputStart { .. } => "output.start",
            Action::OutputStop { .. } => "output.stop",
            Action::OutputSetKey { .. } => "output.set_key",
            Action::OutputClearKey { .. } => "output.clear_key",
            Action::AudioResetLoudness {} => "audio.reset_loudness",
            Action::SceneSwitch { .. } => "scene.switch",
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
///
/// Both components are [`Finite32`]: a non-finite or out-of-`f32`-range number (`1e39`)
/// fails deserialization, so the message is rejected as `malformed`, and in-process code
/// can't build a non-finite value.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Vec2 {
    /// Horizontal component.
    pub x: Finite32,
    /// Vertical component.
    pub y: Finite32,
}

impl Vec2 {
    /// A vector from two finite components, or `None` if either is NaN or infinite.
    pub const fn new(x: f32, y: f32) -> Option<Vec2> {
        match (Finite32::new(x), Finite32::new(y)) {
            (Some(x), Some(y)) => Some(Vec2 { x, y }),
            _ => None,
        }
    }
}

/// A protocol 0.3 placement: where a scene item sits until it is stored as
/// [`ItemGeometry`](crate::ItemGeometry).
///
/// **Deprecated** since 0.11 (ADR-0022) and removed in 1.0, with `item.set_transform`,
/// `item.add`'s `transform` and `ItemInfo.transform`. Until then it keeps the meaning
/// below.
///
/// `position` is the top-left corner in canvas pixels, `scale` is relative to the
/// source's current size, and `rotation_deg` turns clockwise about that corner.
/// ADR-0022 (`docs/adr/0022-item-transform-and-canvas.md`) does not extend this struct
/// with 3D fields. The replacement is [`ItemGeometry`](crate::ItemGeometry): a box,
/// rotation about its centre, and a fit mode. Until 1.0 a legacy placement keeps this
/// meaning, and [`legacy_quad`](crate::legacy_quad) evaluates it. Crop is [`Crop`],
/// applied before the transform.
///
/// Every number is a [`Finite32`], so a non-finite or out-of-range number is rejected at
/// deserialization as `malformed`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ItemTransform {
    /// Top-left corner, in pixels of the scene's canvas.
    pub position: Vec2,
    /// Scale factors; 1.0 is the source's native size.
    pub scale: Vec2,
    /// Clockwise rotation, in degrees.
    pub rotation_deg: Finite32,
}

impl ItemTransform {
    /// No offset, native size, no rotation.
    pub const IDENTITY: ItemTransform = ItemTransform {
        position: Vec2 {
            x: Finite32::ZERO,
            y: Finite32::ZERO,
        },
        scale: Vec2 {
            x: Finite32::ONE,
            y: Finite32::ONE,
        },
        rotation_deg: Finite32::ZERO,
    };
}

impl Default for ItemTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// How many source pixels an item cuts off each edge before it is transformed.
///
/// Whole pixels of the source's own frame, so every value is valid; cropping more than
/// the source's size leaves nothing to draw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Crop {
    /// Pixels cut from the left edge.
    pub left: u32,
    /// Pixels cut from the top edge.
    pub top: u32,
    /// Pixels cut from the right edge.
    pub right: u32,
    /// Pixels cut from the bottom edge.
    pub bottom: u32,
}

impl Crop {
    /// No cropping.
    pub const NONE: Crop = Crop {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
}

/// What a source shows.
///
/// Serialized with an internal `type` tag. A source is one shared instance: every item
/// that places it shows the same capture, and (once filters land) the same filters.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceKind {
    /// Another scene, composited as one layer (a nested scene). An `item.add` that would
    /// make a scene contain itself, directly or through other scenes, is rejected with
    /// `invalid_argument` naming the cycle.
    Scene {
        /// The scene shown.
        scene_id: SceneId,
    },
    /// A built-in source kind named by the engine (`display`, `window`, `webcam`,
    /// `image`, `color`, `text`, `media`). `kind.list` reports the kinds, and
    /// `kind.describe` the settings each takes; the settings travel beside the kind, on
    /// the source.
    Builtin {
        /// The kind's ID, as `kind.list` reports it.
        kind: String,
    },
}

/// What a scene is for (ADR-0024 §1). It changes no engine behaviour; it tells clients,
/// agents and switchers how to show the scene.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SceneRole {
    /// A scene of its own, listed in the scene list. Scenes are created with this role
    /// unless `scene.create` names another.
    #[default]
    Scene,
    /// A group: a scene placed in other scenes to hold items together. Clients hide it
    /// from the scene list and show it under the items that place it.
    Group,
}

impl SceneRole {
    /// Whether this is the default role, `scene`, which query results omit.
    pub fn is_scene(&self) -> bool {
        *self == SceneRole::Scene
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, Batch, Command, CommandClass, Edit, SceneRole};
    use crate::id::{IdempotencyKey, OutputId, Rev, SceneId};
    use serde_json::json;

    #[test]
    fn scene_create_role_is_optional_on_the_wire() {
        let plain: Edit =
            serde_json::from_value(json!({"type": "scene.create", "scene_id": "s", "name": "S"}))
                .expect("parses");
        assert_eq!(
            plain,
            Edit::SceneCreate {
                scene_id: SceneId::new("s"),
                name: "S".into(),
                role: None,
            }
        );
        assert_eq!(
            serde_json::to_value(&plain).expect("serializes"),
            json!({"type": "scene.create", "scene_id": "s", "name": "S"})
        );
        let group = Edit::SceneCreate {
            scene_id: SceneId::new("g"),
            name: "G".into(),
            role: Some(SceneRole::Group),
        };
        assert_eq!(
            serde_json::to_value(&group).expect("serializes"),
            json!({"type": "scene.create", "scene_id": "g", "name": "G", "role": "group"})
        );
    }

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
    fn out_of_range_transform_numbers_are_rejected() {
        // Regression (review round 2, B2): `1e39` used to narrow to an infinite f32.
        for x in ["1e39", "-1e39"] {
            let text = format!(
                r#"{{"type":"item.set_transform","item_id":"i","transform":{{"position":{{"x":{x},"y":0}},"scale":{{"x":1,"y":1}},"rotation_deg":0}}}}"#
            );
            assert!(serde_json::from_str::<Edit>(&text).is_err(), "{x} accepted");
        }
        assert_eq!(super::Vec2::new(f32::INFINITY, 0.0), None);
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
