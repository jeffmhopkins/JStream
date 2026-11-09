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

use crate::audio::{AudioFollow, BusMonitor};
use crate::binding::Trigger;
use crate::camera_motion::{CameraFollow, CameraMotionModifier};
use crate::encoder::EncoderProfile;
use crate::error::EntityKind;
use crate::filter::{FilterChain, FilterInfo, FilterOwner};
use crate::finite::Finite32;
use crate::frames::FrameStrip;
use crate::geometry::{ItemGeometry, SceneCamera};
use crate::hotkey::Chord;
use crate::id::{
    BusId, DeviceId, EncoderProfileId, FilterId, FolderId, GestureId, IdempotencyKey, ItemId,
    LabelId, OutputId, PluginId, Rev, SceneId, SourceId, TimelineId, TransitionPresetId,
    VideoMixId,
};
use crate::keyframe::OverrideField;
use crate::mix::VideoMix;
use crate::motion::MotionModifier;
use crate::organise::{SceneMeta, SourceMeta};
use crate::output::{OutputKind, RtmpTarget, SecretString};
use crate::plugin::PluginGrant;
use crate::preview::{PreviewCodec, PreviewDraft};
use crate::query::SceneOptions;
use crate::settings::Settings;
use crate::timeline::TimelineInfo;
use crate::transition::{Easing, ItemTransition, KeyframeTiming};
use crate::youtube::{BroadcastPrivacy, BroadcastTransition};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The most scenes besides the program `studio.set_warm` keeps warm (ADR-0117 §7).
pub const MAX_WARM_SCENES: u32 = 8;

/// How many scenes besides the program the studio keeps warm until `studio.set_warm`
/// changes it.
pub const DEFAULT_WARM_SCENES: u32 = 3;

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
    /// Creates a keyframe scene (a variant) of `variant_of` (ADR-0024 §2, ADR-0049): a
    /// scene with no items of its own that shows its parent's items, each at its parent's
    /// values until the variant overrides a field. Placement edits with `scene_id` set to
    /// the variant write its overrides; `item.clear_override` removes them. `variant_of`
    /// must name an existing scene that is not itself a variant (a keyframe of a keyframe
    /// is made on the parent), else `invalid_argument`; a missing one is `not_found`.
    /// The variant has the role `scene`, no folder (it sits under its parent) and no
    /// transition overrides; its own `scene.set_transitions` and `scene.set_meta` apply.
    /// Inverse: `scene.delete`.
    #[serde(rename = "scene.create_variant")]
    SceneCreateVariant {
        /// The new scene's ID, chosen by the client. Rejected if it exists.
        scene_id: SceneId,
        /// The scene it is a keyframe of.
        variant_of: SceneId,
        /// Display name.
        name: String,
    },
    /// Removes one field of a keyframe scene's override of an item (ADR-0049 §2), so
    /// that field follows the parent again. "Reset keyframe" is a batch of these.
    /// `scene_id` must name a keyframe scene and `item_id` an item in its parent's tree
    /// (one of the parent's own, or, since 0.55, a child of a group it places, at any
    /// depth), else `invalid_argument` (`not_found` for a missing one). Clearing a field the variant
    /// doesn't override is allowed and changes nothing. Inverse: the placement edit that
    /// writes the old value with `scene_id` set, or nothing when the field was absent.
    #[serde(rename = "item.clear_override")]
    ItemClearOverride {
        /// The keyframe scene.
        scene_id: SceneId,
        /// The item in the parent's tree.
        item_id: ItemId,
        /// The field to clear.
        field: OverrideField,
    },
    /// Writes a keyframe scene's override of an item's effects, the whole list, which the
    /// keyframe draws in place of the item's own (ADR-0088, since 0.63). `scene_id` must
    /// name a keyframe scene whose tree holds `item_id`, else `invalid_argument` on
    /// `scene_id` (`not_found` for a missing one): an item's own effects are edited with
    /// `filter.*`. Each entry is checked, and its settings completed, as `filter.add`
    /// checks an item effect's; the field is `effects.<n>.<field>`. An empty list is an
    /// override with no effects. One effect of the override is edited with `filter.*`
    /// on the `keyframe` owner. Inverse: `item.set_effects` with the old list, or
    /// `item.clear_override` of `effects` when the keyframe didn't override it.
    #[serde(rename = "item.set_effects")]
    ItemSetEffects {
        /// The keyframe scene.
        scene_id: SceneId,
        /// The item in the parent's tree.
        item_id: ItemId,
        /// The effects, first first: at most
        /// [`MAX_CHAIN_FILTERS`](crate::MAX_CHAIN_FILTERS), else `invalid_argument` on
        /// `effects`.
        effects: Vec<FilterInfo>,
    },
    /// Writes an item's motion modifiers, the whole list (ADR-0099, since 0.81), which
    /// move it on the engine's animation clock as each frame is drawn. Without
    /// `scene_id`, or with its own scene's, it writes the item's own list; with a
    /// keyframe scene whose tree holds the item, it writes that keyframe's override.
    /// Any other scene is `invalid_argument` on `scene_id`. The list holds at most
    /// [`MAX_MOTION_MODIFIERS`](crate::MAX_MOTION_MODIFIERS), else `invalid_argument` on
    /// `motion`; each ID is unique and well formed, and each parameter in its range,
    /// else `invalid_argument` on `motion.<n>.<field>`. An empty list stops all motion
    /// (in a keyframe, an override with none). Inverse: `item.set_motion` with the old
    /// list, or `item.clear_override` of `motion` when the keyframe didn't override it.
    #[serde(rename = "item.set_motion")]
    ItemSetMotion {
        /// The item.
        item_id: ItemId,
        /// The scene whose placement of the item to change. See [`Edit::ItemSetVisible`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scene_id: Option<SceneId>,
        /// The modifiers, first first.
        motion: Vec<MotionModifier>,
    },
    /// Writes a keyframe scene's timing for an item in transitions into that keyframe
    /// (ADR-0129, since 0.99): its delay, easing and duration there, each in place of the
    /// item's own or the preset's. `scene_id` must name a keyframe scene whose tree holds
    /// the item, with the errors of the placement edits addressed to a keyframe; any other
    /// scene is `invalid_argument` on `scene_id`. A `delay_ms` or `duration_ms` over 60000
    /// is `invalid_argument` on `timing.<field>`, and so is a `cubic_bezier` whose `x1` or
    /// `x2` is outside 0 to 1 or whose `y1` or `y2` is outside -2 to 3
    /// (`timing.easing.cubic_bezier.<field>`). A timing with no field set removes the
    /// override. Inverse: `item.set_timing` with the old timing, or `item.clear_override`
    /// of `timing` when the keyframe didn't override it.
    #[serde(rename = "item.set_timing")]
    ItemSetTiming {
        /// The keyframe scene.
        scene_id: SceneId,
        /// The item in the parent's tree.
        item_id: ItemId,
        /// Its timing in transitions into the keyframe.
        timing: KeyframeTiming,
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
    /// other scenes through a `scene` source. A group renders, nests and can be placed
    /// more than once like any scene, and clients hide groups from the scene list. A
    /// keyframe scene may override a group's children, and draws a group it overrides
    /// with those values (since 0.55): so setting a group to `scene` takes its children
    /// out of every keyframe scene's tree and drops their overrides there, reported in
    /// the batch result's `dropped_overrides` (ADR-0040 §1). Inverse: `scene.set_role`
    /// with the previous role, then the writes that restore any dropped overrides.
    #[serde(rename = "scene.set_role")]
    SceneSetRole {
        /// The scene to change.
        scene_id: SceneId,
        /// Its new role.
        role: SceneRole,
    },
    /// Deletes a scene and its items; the sources they placed stay. Rejected with
    /// `invalid_argument` while a `scene` source or a video mix shows it, or any source's
    /// or transition preset's settings refer to it (a stinger's `scene`): delete those
    /// sources, point those mixes elsewhere, or clear those references, first (in the
    /// same batch if needed). Hotkey bindings whose
    /// action names the scene are not blockers: they are removed with it (ADR-0040 §3).
    /// A scene with keyframe scenes is refused the same way, the variants listed as its
    /// blockers; delete them first. Inverse: `scene.create` with its role (omitted for
    /// `scene`), or `scene.create_variant` for a keyframe scene, then `scene.reorder`
    /// to its old display position (unless it was last), `scene.set_transitions` if it
    /// had an override, `scene.set_folder` and `scene.set_meta` if it had a folder or
    /// metadata, then each deleted item's `item.remove` inverse, from bottom to top (a
    /// keyframe scene's overrides instead, as placement edits with its `scene_id`), then
    /// `hotkey.set` for each removed binding.
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
    /// Sending both is `invalid_argument`. That identity draws the source at its own
    /// pixel size from the canvas's top-left, so a `display`, `window` or `webcam` larger
    /// than the canvas shows only its corner: for those, send a `geometry` covering the
    /// canvas (`center` at half the canvas size, `size` the canvas size, `fit_mode`
    /// `fit`), as `jstream-cli` does.
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
        /// The initial opacity, checked like `item.set_opacity`'s. Omitted means 1.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opacity: Option<Finite32>,
        /// The initial blend mode, checked like `item.set_blend`'s. Omitted means
        /// `normal`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blend: Option<BlendMode>,
    },
    /// Removes an item from its scene. The source itself stays. Every keyframe scene
    /// whose tree held the item loses its override of it, and of the items of a group
    /// it was the last path to, reported in the batch result's `dropped_overrides`
    /// (ADR-0040 §1). Inverse: `item.add` with
    /// the removed item's scene, ID and source, its placement in the field matching its
    /// form (`transform` or `geometry`), and its opacity and blend where they differ from
    /// the defaults, then `item.reorder` to its old stacking position (unless it was on
    /// top), `item.set_crop` if it was cropped, `item.set_visible` if it was hidden,
    /// `item.set_transition` if its transition settings were not the default,
    /// `item.set_locked` if it was locked (since 0.71), `item.set_effects_space` if its
    /// effects run in canvas space (since 0.76), and the placement edits that write each
    /// dropped override back.
    #[serde(rename = "item.remove")]
    ItemRemove {
        /// The item to remove.
        item_id: ItemId,
    },
    /// Moves an item to a stacking position in another scene, keeping its ID, source,
    /// placement, crop and visibility (ADR-0024 §1). Grouping is a batch that creates a
    /// group scene, places it, and moves the selected items into it. Rejected with
    /// `invalid_argument` if the move would make a scene contain itself (field
    /// `scene_id`, naming the cycle), if the index is out of range, or if the scene is
    /// a keyframe scene, which has no items of its own. A move out of a keyframe scene's
    /// tree drops the item's overrides there, as `item.remove` does; a move between a
    /// parent and a group it places keeps them. Inverse: `item.move` back to its previous scene and index,
    /// then the placement edits that write each dropped override back.
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
    /// not greater than zero and a `perspective_px` that is not greater than zero are
    /// `invalid_argument`. Any `tilt_deg`, `turn_deg`, `depth_px` and `perspective_px` is
    /// accepted from 0.34 (ADR-0050) if the compositor can draw it: a non-neutral one
    /// whose centre, size, depth or perspective is past 10^15 canvas pixels, or whose
    /// drawn polygon would not be finite or would reach past that, is `invalid_argument`
    /// on `geometry`. Earlier minors
    /// rejected a non-neutral one with `invalid_argument`. Inverse: the previous
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
        /// takes it (ADR-0024 §2). Omitted, or the item's own scene, changes the item. A
        /// keyframe scene whose tree holds the item (of the item's scene, or of a scene
        /// that reaches the item's group through groups it places, at any depth) writes
        /// that scene's override of this
        /// field (since 0.39), whose inverse is `item.clear_override` if the field was
        /// not overridden and this edit with the old value if it was. Any other scene is
        /// `invalid_argument`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scene_id: Option<SceneId>,
        /// Whether the item is drawn. Items are visible when added.
        visible: bool,
    },
    /// Changes how transparent an item is drawn (ADR-0023 §1): its colour and alpha are
    /// multiplied by `opacity` after crop and fit. Above 1 (since 0.83, #1101) it is a
    /// gain on the item's light: its premultiplied colour is multiplied by the opacity
    /// while its alpha is multiplied by 1, so an `additive` item adds more light than its
    /// source draws, and an item in another mode is drawn brighter. A value outside 0 to
    /// [`MAX_OPACITY`] is `invalid_argument`. Items are added at 1. Inverse:
    /// `item.set_opacity` with the previous value.
    #[serde(rename = "item.set_opacity")]
    ItemSetOpacity {
        /// The item to change.
        item_id: ItemId,
        /// The scene whose placement of the item to change. See [`Edit::ItemSetVisible`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scene_id: Option<SceneId>,
        /// From 0 (invisible) through 1 (as the source draws it) to [`MAX_OPACITY`]
        /// (its light that many times over).
        opacity: Finite32,
    },
    /// Changes how an item composites onto what is below it (ADR-0023 §1). A mode the
    /// compositor doesn't draw yet is `invalid_argument`, so stored state never claims
    /// something that isn't drawn; today that is every mode but `normal`. Inverse:
    /// `item.set_blend` with the previous mode.
    #[serde(rename = "item.set_blend")]
    ItemSetBlend {
        /// The item to change.
        item_id: ItemId,
        /// The new blend mode. Items are added with `normal`.
        blend: BlendMode,
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
    /// directly or through other scenes. Retargeting the last item that places a group
    /// takes the group's children out of a keyframe scene's tree and drops their
    /// overrides there, reported in `dropped_overrides` (ADR-0040 §1, since 0.55).
    /// Inverse: `item.set_source` with the previous source, then the writes that restore
    /// any dropped overrides.
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
    /// Copies a source under a new ID: a separate instance with the same kind, settings,
    /// filters (with the same filter IDs), audio settings and sends, labels and favourite
    /// flag, which changes independently from then on. The copy's `duplicated_from`
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
    /// or any source's or filter's settings refer to it: remove those items, or clear
    /// those references, first (in the same batch if needed). Deleting a source that
    /// others were copied from is allowed: their `duplicated_from` then reads as absent.
    /// Its sends go with it, and so do the hotkey bindings whose action names it
    /// (ADR-0040 §3). Inverse: `source.create` with its name, kind, settings and
    /// `duplicated_from`, then `send.remove` of its master send if it had none,
    /// `send.set` for each send that `source.create` doesn't restore as it was,
    /// `source.set_audio` if its audio settings were not the defaults, `filter.add`
    /// for each of its filters, video chain first, in chain order, `source.set_meta` if
    /// it carried labels or was a favourite, then `hotkey.set` for each removed binding.
    #[serde(rename = "source.delete")]
    SourceDelete {
        /// The source to delete.
        source_id: SourceId,
    },
    /// Adds a filter to one of a source's chains (ADR-0026). Rejected with `not_found` if
    /// the owner doesn't exist, `already_exists` if it has a filter with this ID, and
    /// `invalid_argument` if the source has no such chain, the chain is full
    /// ([`MAX_CHAIN_FILTERS`](crate::MAX_CHAIN_FILTERS)), `kind` is not a filter kind of
    /// that chain (`video_filter` or `audio_filter`), a setting fails its descriptor as in
    /// `source.create`, a reference in the settings would close a cycle, or `index` is
    /// past the end. Inverse: `filter.remove`.
    #[serde(rename = "filter.add")]
    FilterAdd {
        /// What owns the chain.
        owner: FilterOwner,
        /// Which chain.
        chain: FilterChain,
        /// The new filter's ID, chosen by the client, unique within its owner.
        filter_id: FilterId,
        /// The filter's kind, as `kind.list` reports it.
        kind: String,
        /// Initial settings, checked against the kind's descriptor and completed with its
        /// defaults, as `source.create`'s are. Omitted means all defaults.
        #[serde(default, skip_serializing_if = "Settings::is_empty")]
        settings: Settings,
        /// Whether it runs. Omitted means enabled.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        /// Its position in the chain, counted from the first filter (0). Must be at most
        /// the number of filters already in the chain. Omitted means last.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<u32>,
    },
    /// Removes a filter from its chain. Inverse: `filter.add` with its chain, kind,
    /// settings, `enabled: false` if it was disabled, and its index.
    #[serde(rename = "filter.remove")]
    FilterRemove {
        /// What owns the filter.
        owner: FilterOwner,
        /// The filter to remove.
        filter_id: FilterId,
    },
    /// Turns a filter on or off. A disabled filter stays in its chain and keeps its
    /// settings, and is bypassed. Inverse: `filter.set_enabled` with the previous value.
    #[serde(rename = "filter.set_enabled")]
    FilterSetEnabled {
        /// What owns the filter.
        owner: FilterOwner,
        /// The filter to change.
        filter_id: FilterId,
        /// Whether it runs.
        enabled: bool,
    },
    /// Changes some of a filter's settings, with `source.set_settings`'s rules: each entry
    /// is checked against the kind's descriptor, `null` resets a parameter to its default
    /// (a reference to none), and a reference that would make a cycle is
    /// `invalid_argument` naming it. Inverse: `filter.set_settings` with the patched
    /// parameters' previous values.
    #[serde(rename = "filter.set_settings")]
    FilterSetSettings {
        /// What owns the filter.
        owner: FilterOwner,
        /// The filter to change.
        filter_id: FilterId,
        /// Parameter ID to new value, or `null` for the default.
        patch: Settings,
    },
    /// Moves a filter to another position in its own chain. Inverse: `filter.reorder`
    /// back to its previous index.
    #[serde(rename = "filter.reorder")]
    FilterReorder {
        /// What owns the filter.
        owner: FilterOwner,
        /// The filter to move.
        filter_id: FilterId,
        /// The new position, counted from the first filter (0). Must be less than the
        /// number of filters in its chain, else `invalid_argument`.
        index: u32,
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
    /// the new target applies from its next `output.start`. Its stream key is kept only
    /// while the server stays the same (scheme, host and port; the path may change): a
    /// new server removes the key from the credential store, so the old key never goes
    /// to it, and needs a new `output.set_key`. Inverse: `output.set_target` with the
    /// previous target, which can't restore a removed key.
    #[serde(rename = "output.set_target")]
    OutputSetTarget {
        /// The output to change.
        output_id: OutputId,
        /// The new target, validated as in `output.create`.
        target: RtmpTarget,
    },
    /// Changes where a recording output writes its file (since 0.74, #1072), keeping its
    /// container, encoder profile, mix, bus and filters. A running output keeps writing
    /// where it started; the new path applies from its next `output.start`, which still
    /// never overwrites. The path is checked as in `output.create`'s
    /// [`OutputKind::Record`](crate::OutputKind::Record). An output that isn't a
    /// recording is `invalid_argument`. Inverse: `output.set_path` with the previous
    /// path.
    #[serde(rename = "output.set_path")]
    OutputSetPath {
        /// The recording output to change.
        output_id: OutputId,
        /// Where the finished recording goes: an absolute path on the engine's machine,
        /// ending in a file name.
        path: String,
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
    /// Sets the collection's base frame rate to 30 or 60 (since 0.89, ADR-0119). A new
    /// collection is 30. The program stage keeps the rate it opened with; a change
    /// applies the next time the stage opens. A preview stream holds the stage open, so
    /// the program keeps the old rate until every consumer stops. `profile.get`'s
    /// `frame_budget_ms` is the rate the stage is actually running at. Mixes run at this
    /// rate divided by their `fps_divisor`, so 60 doubles every mix. Anything else is
    /// `invalid_argument`. Inverse: `canvas.set_frame_rate` with the previous rate.
    #[serde(rename = "canvas.set_frame_rate")]
    CanvasSetFrameRate {
        /// Frames per second: 30 or 60.
        fps: u32,
    },
    /// Sets the space every blend mode of the project mixes in (since 0.101, ADR-0126):
    /// `srgb`, browser-style and the default, or `linear`. It applies to every scene from
    /// the next frame. Inverse: `canvas.set_blending` with the previous space.
    #[serde(rename = "canvas.set_blending")]
    CanvasSetBlending {
        /// The space.
        blending: Blending,
    },
    /// Deletes an output. Rejected with `invalid_argument` while it runs: stop it first.
    /// Its stream key is removed from the credential store, so an output created later
    /// with the same ID doesn't inherit it. Hotkey bindings whose action names the output
    /// are removed with it (ADR-0040 §3). Inverse: `output.create` with its kind,
    /// `output.set_encoder` if it had a profile, then `hotkey.set` for each removed
    /// binding; it can't restore the key: set it again with `output.set_key`.
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
    /// Adds a video mix: what an output's video can show, at what size and rate. A mix
    /// renders only while an output that shows it runs. Inverse: `video_mix.delete`.
    #[serde(rename = "video_mix.create")]
    VideoMixCreate {
        /// The new mix's ID, chosen by the client. Rejected if it exists.
        mix_id: VideoMixId,
        /// Display name.
        name: String,
        /// What it shows, and at what size and rate. An input the engine doesn't know, a
        /// side that is odd or outside 16..=8192, or an `fps_divisor` outside 1..=60 is
        /// rejected with `invalid_argument`, and a `scene` input naming a scene that
        /// doesn't exist with `not_found`.
        mix: VideoMix,
    },
    /// Replaces a video mix's name, input, resolution and rate. Running outputs that
    /// show it follow a new input from the next frame; a new resolution or rate applies
    /// from each output's next `output.start`. Inverse: `video_mix.update` with the
    /// previous name and mix.
    #[serde(rename = "video_mix.update")]
    VideoMixUpdate {
        /// The mix to change.
        mix_id: VideoMixId,
        /// The new display name.
        name: String,
        /// The new mix, validated as in `video_mix.create`.
        mix: VideoMix,
    },
    /// Deletes a video mix. Rejected with `invalid_argument` while an output shows it:
    /// point those outputs elsewhere with `output.set_video_mix` first (in the same batch
    /// if needed). Inverse: `video_mix.create` with its name and mix.
    #[serde(rename = "video_mix.delete")]
    VideoMixDelete {
        /// The mix to delete.
        mix_id: VideoMixId,
    },
    /// Chooses the video mix an output shows, or none: the program at the engine's
    /// canvas size and base rate. A running output keeps its encoder session; the mix
    /// applies from its next `output.start`. Inverse: `output.set_video_mix` with the
    /// previous mix.
    #[serde(rename = "output.set_video_mix")]
    OutputSetVideoMix {
        /// The output to change.
        output_id: OutputId,
        /// The mix, which must exist, else `not_found`. Absent or `null` means none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mix_id: Option<VideoMixId>,
    },
    /// Chooses the buses whose audio an output carries (since 0.59), one per audio
    /// track, in track order. Every output writes one audio track for now, so the list
    /// holds exactly one bus; any other length is `invalid_argument` on `audio_buses`,
    /// as is an output with no audio (a virtual camera). Each bus must exist, else
    /// `not_found`. The master stage's audio chain runs on the bus, then the output's
    /// own chain (ADR-0080 §1). A running output reads its bus, so moving it to another
    /// is `invalid_argument` on `output_id` until it stops. Inverse:
    /// `output.set_audio_buses` with the previous buses.
    #[serde(rename = "output.set_audio_buses")]
    OutputSetAudioBuses {
        /// The output to change.
        output_id: OutputId,
        /// The buses, in track order.
        audio_buses: Vec<BusId>,
    },
    /// Adds a folder to the scene list, last in folder order (ADR-0024 §3). Folders
    /// organise only: they change no rendering, switching or nesting. Inverse:
    /// `folder.delete`.
    #[serde(rename = "folder.create")]
    FolderCreate {
        /// The new folder's ID, chosen by the client. Rejected if it exists.
        folder_id: FolderId,
        /// Display name.
        name: String,
        /// The folder it sits in, which must exist, else `not_found`. Omitted means top
        /// level.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<FolderId>,
    },
    /// Renames a folder. Inverse: `folder.rename` back to the previous name.
    #[serde(rename = "folder.rename")]
    FolderRename {
        /// The folder to rename.
        folder_id: FolderId,
        /// The new display name.
        name: String,
    },
    /// Moves a folder into another folder, or to the top level (ADR-0064: folders nest).
    /// A parent that would put the folder inside itself is `invalid_argument` (field
    /// `parent`), and a parent that doesn't exist `not_found`. Inverse:
    /// `folder.set_parent` with the previous parent.
    #[serde(rename = "folder.set_parent")]
    FolderSetParent {
        /// The folder to move.
        folder_id: FolderId,
        /// Its new parent. Absent or `null` means top level.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<FolderId>,
    },
    /// Moves a folder to another position in folder order. The folders in one parent
    /// show in that order. Inverse: `folder.reorder` back to its previous index.
    #[serde(rename = "folder.reorder")]
    FolderReorder {
        /// The folder to move.
        folder_id: FolderId,
        /// The new position, counted from the first folder (0). Must be less than the
        /// number of folders, else `invalid_argument`.
        index: u32,
    },
    /// Deletes an empty folder. Rejected with `invalid_argument` while a scene or another
    /// folder sits in it, listing them as `blockers`: move them out first, or send the
    /// batch `delete.plan` returns, which moves them up to the folder's parent. Inverse:
    /// `folder.create` with its name and parent, then `folder.reorder` to its old
    /// position (unless it was last).
    #[serde(rename = "folder.delete")]
    FolderDelete {
        /// The folder to delete.
        folder_id: FolderId,
    },
    /// Puts a scene in a folder, or takes it out of one. The scene keeps its position in
    /// the flat display order. Inverse: `scene.set_folder` with the previous folder.
    #[serde(rename = "scene.set_folder")]
    SceneSetFolder {
        /// The scene to file.
        scene_id: SceneId,
        /// The folder, which must exist, else `not_found`. Absent or `null` means none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        folder_id: Option<FolderId>,
    },
    /// Replaces a scene's organisation metadata: the labels it carries, its icon, colour,
    /// favourite flag and whether a multiview shows it. Every field left out takes its
    /// default, so `labels` omitted removes them all. An icon that is empty or longer
    /// than 64 bytes, a colour that isn't `#rrggbb`, or a label listed twice is
    /// `invalid_argument`, and a label that doesn't exist `not_found`; the engine stores
    /// labels ordered by ID. Inverse: `scene.set_meta` with the previous metadata.
    #[serde(rename = "scene.set_meta")]
    SceneSetMeta {
        /// The scene to change.
        scene_id: SceneId,
        /// The new metadata.
        #[serde(flatten)]
        meta: SceneMeta,
    },
    /// Replaces a scene's options (ADR-0089, since 0.65): whether its audio reaches the
    /// Mixer when it is nested, and its background colour. Every field left out takes
    /// its default. A keyframe scene has options of its own, except a background, which
    /// it shows from its parent. `invalid` for a background that isn't a colour, or one
    /// on a keyframe scene. Inverse: `scene.set_options` with the previous options.
    #[serde(rename = "scene.set_options")]
    SceneSetOptions {
        /// The scene to change.
        scene_id: SceneId,
        /// The new options.
        #[serde(flatten)]
        options: SceneOptions,
    },
    /// Sets or removes a scene's camera (ADR-0094, since 0.70). A scene with a camera is a
    /// 3D scene: its items are placed in one 3D space by their geometry and seen through
    /// the camera. On a keyframe scene it writes or, with `camera` omitted, removes the
    /// keyframe's override of its parent's camera. `invalid_argument` for a field of view
    /// outside 1 to 170, a position within 0.001 px of the target, or a coordinate past
    /// the drawable bound. A keyframe of a 2D scene may have a camera of its own
    /// (ADR-0128). Removing a parent's camera also removes its keyframe scenes'
    /// overrides; a removal on a parent that has none removes nothing. Inverse:
    /// `scene.set_camera` with the previous camera, then each removed override.
    #[serde(rename = "scene.set_camera")]
    SceneSetCamera {
        /// The scene to change.
        scene_id: SceneId,
        /// The new camera. Omitted removes it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        camera: Option<SceneCamera>,
    },
    /// Sets a scene's PTZ shots (ADR-0146 §6): the whole list of physical camera
    /// framings the scene sends when it goes on program or preview. At most 16, one per
    /// camera; each names a camera of the machine's PTZ list and either a preset or a
    /// pose. A keyframe scene's shot for a camera replaces its parent's for that camera,
    /// and it follows its parent's other shots. A camera the machine doesn't list is
    /// kept, and skipped when the shot fires. Setting the shots moves no head.
    /// `invalid_argument` for a list `check_ptz_shots` refuses. Inverse:
    /// `scene.set_ptz_shots` with the previous list.
    #[serde(rename = "scene.set_ptz_shots")]
    SceneSetPtzShots {
        /// The scene to change.
        scene_id: SceneId,
        /// The new list. Empty removes every shot.
        #[serde(default)]
        shots: Vec<crate::ptz::PtzShot>,
    },
    /// Sets a scene's camera motion (ADR-0113, since 0.85): the whole list of modifiers that change
    /// its camera as it is drawn. It belongs to the scene family: `invalid_argument` on a
    /// keyframe scene, which draws its parent's, and for a list `check_camera_motion`
    /// refuses. A scene without a camera keeps the list, which draws nothing until it has
    /// one. Inverse: `scene.set_camera_motion` with the previous list.
    #[serde(rename = "scene.set_camera_motion")]
    SceneSetCameraMotion {
        /// The scene to change.
        scene_id: SceneId,
        /// The new list, at most 8. Empty removes every modifier.
        #[serde(default)]
        motion: Vec<CameraMotionModifier>,
    },
    /// Sets or removes a scene's follow camera (ADR-0113, since 0.85): a camera that glides after the
    /// pointer over a display or window capture in the scene. Since 0.91 a follow camera
    /// is a `follow_pointer` filter on the followed item's effects (ADR-0120), and this
    /// edit is kept as a shortcut for those filter edits: with `follow`, it sets that
    /// item's `follow_pointer` filter to its numbers and turns it on, adding one last in
    /// the item's effects when it has none, and removes every other `follow_pointer`
    /// filter of the scene's items; without, it removes them all. It belongs to the scene
    /// family: `invalid_argument` on a keyframe scene, for an item that isn't in the
    /// scene or doesn't show a display or window capture, for a number
    /// `CameraFollow::check` refuses, and when the item's effects are full. The follow
    /// camera reads the pointer only where the program is drawn live: `video.snapshot`
    /// and a frame sheet show it at rest, on the capture's centre. Inverse: the inverse
    /// of those filter edits (`filter.remove`, `filter.add`, `filter.set_settings` and
    /// `filter.set_enabled`).
    #[serde(rename = "scene.set_camera_follow")]
    SceneSetCameraFollow {
        /// The scene to change.
        scene_id: SceneId,
        /// The new follow camera. Omitted removes it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        follow: Option<CameraFollow>,
    },
    /// Adds a label, last in label order (ADR-0024 §4). Rejected with `invalid_argument`
    /// for an empty name, a colour that isn't `#rrggbb`, or an icon checked as
    /// `scene.set_meta`'s. Inverse: `label.delete`.
    #[serde(rename = "label.create")]
    LabelCreate {
        /// The new label's ID, chosen by the client. Rejected if it exists.
        label_id: LabelId,
        /// Display name.
        name: String,
        /// Its colour, sRGB as `#rrggbb`.
        color: String,
        /// An icon name from the UI's icon set. Omitted means none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
    },
    /// Replaces a label's name, colour and icon, validated as in `label.create`. Every
    /// source carrying it shows the change. Inverse: `label.update` with the previous
    /// values.
    #[serde(rename = "label.update")]
    LabelUpdate {
        /// The label to change.
        label_id: LabelId,
        /// The new display name.
        name: String,
        /// The new colour.
        color: String,
        /// The new icon. Omitted means none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
    },
    /// Moves a label to another position in label order. Inverse: `label.reorder` back
    /// to its previous index.
    #[serde(rename = "label.reorder")]
    LabelReorder {
        /// The label to move.
        label_id: LabelId,
        /// The new position, counted from the first label (0). Must be less than the
        /// number of labels, else `invalid_argument`.
        index: u32,
    },
    /// Deletes a label. Rejected with `invalid_argument` while a source or scene carries
    /// it, listing them as `blockers`: take it off those first, or send the batch
    /// `delete.plan` returns. Inverse: `label.create` with its name, colour and icon,
    /// then `label.reorder` to its old position (unless it was last).
    #[serde(rename = "label.delete")]
    LabelDelete {
        /// The label to delete.
        label_id: LabelId,
    },
    /// Replaces a source's organisation metadata: the labels it carries and its
    /// favourite flag. Every field left out takes its default, so `labels` omitted
    /// removes them all. A label that doesn't exist is `not_found`, and one listed twice
    /// `invalid_argument`; the engine stores them ordered by ID. Inverse:
    /// `source.set_meta` with the previous metadata.
    #[serde(rename = "source.set_meta")]
    SourceSetMeta {
        /// The source to change.
        source_id: SourceId,
        /// The new metadata.
        #[serde(flatten)]
        meta: SourceMeta,
    },
    /// Binds a key chord to an action (ADR-0024 §5). A client that listens for keys
    /// sends the action when the chord is pressed; the engine only stores the binding.
    /// Only actions can be bound, so pressing a hotkey never adds to an undo history.
    /// Rejected with `invalid_argument` for a chord that isn't valid (see [`Chord`]) or
    /// an action that can't be bound (`output.set_key`, whose stream key is never stored
    /// in the collection), with `already_exists` (entity `hotkey`) for a chord that is
    /// already bound (rebinding is a batch of `hotkey.clear` then `hotkey.set`), and with
    /// `not_found` for a scene or output the action names that doesn't exist. Deleting
    /// that object later removes the binding with it (ADR-0040 §3). Inverse:
    /// `hotkey.clear`.
    #[serde(rename = "hotkey.set")]
    HotkeySet {
        /// The chord, such as `Ctrl+Digit1` or `F1`.
        chord: Chord,
        /// The action request it runs, checked as an `action` call is.
        action: Action,
    },
    /// Removes a hotkey binding. Rejected with `not_found` (entity `hotkey`) for a chord
    /// that isn't bound. Inverse: `hotkey.set` with its action.
    #[serde(rename = "hotkey.clear")]
    HotkeyClear {
        /// The chord to unbind.
        chord: Chord,
    },
    /// Sets the labels a hotkey binding carries (ADR-0133, since 0.100), the whole set,
    /// so it shows in each label's action group beside the keyframes and timelines that
    /// carry it. Rejected with `not_found` (entity `hotkey`) for a chord that isn't
    /// bound, `not_found` for a label that doesn't exist, and `invalid_argument` on
    /// `labels` for one listed twice. `hotkey.clear` drops the labels with the binding,
    /// and its inverse sets them again; a rebind (`hotkey.clear` then `hotkey.set`)
    /// starts with none. Inverse: `hotkey.set_labels` with the old set.
    #[serde(rename = "hotkey.set_labels")]
    HotkeySetLabels {
        /// The bound chord.
        chord: Chord,
        /// Its labels; empty for none.
        labels: Vec<LabelId>,
    },
    /// Binds a controller trigger, such as a MIDI note, to an action (since 0.96,
    /// ADR-0122 decision 1). The engine runs the action itself when an input it reads
    /// sends the trigger, on the first frame at or after the message's time; running it
    /// never adds to an undo history. Rejected with `invalid_argument` at
    /// `trigger.<field>` for a trigger that isn't valid (see [`Trigger::problem`]) or at
    /// `action.<field>` for an action that can't be bound (as `hotkey.set` refuses), with
    /// `already_exists` (entity `binding`) for a trigger that is already bound
    /// (rebinding is a batch of `binding.clear` then `binding.set`), and with `not_found`
    /// for an object the action names that doesn't exist. Deleting that object later
    /// removes the binding with it, as for hotkeys. Inverse: `binding.clear`.
    #[serde(rename = "binding.set")]
    BindingSet {
        /// What fires it.
        trigger: Trigger,
        /// The action request it runs, checked as an `action` call is.
        action: Action,
    },
    /// Removes a binding. Rejected with `not_found` (entity `binding`) for a trigger that
    /// isn't bound. Inverse: `binding.set` with its action.
    #[serde(rename = "binding.clear")]
    BindingClear {
        /// The trigger to unbind.
        trigger: Trigger,
    },
    /// Stores a timeline in a scene (ADR-0115, since 0.87): a new one, or the whole of
    /// one the scene already holds, in its place. A timeline is timed cues, each running
    /// an action (ADR-0099 decision 4). Rejected with `not_found` for a scene that doesn't
    /// exist; with `already_exists` (entity `timeline`) for an ID another scene's timeline
    /// has; and with `invalid_argument` at `timeline.<field>` for what
    /// [`TimelineInfo::problem`] refuses (an ID, a length, a loop, cues out of time order
    /// or past where they can fire, an action a cue can't run) and for a 33rd timeline in
    /// the scene. The objects a cue names are not checked, and deleting one leaves the cue
    /// in place, inert, like a follow camera's item: when it fires its action fails as the
    /// call would, and its `timeline.cue_fired` carries the error. Changing a timeline
    /// while it plays keeps its start, so its cues fire at their new times from then on.
    /// Inverse: `timeline.delete` for a new timeline, else `timeline.set` with the old one.
    #[serde(rename = "timeline.set")]
    TimelineSet {
        /// The scene that holds it.
        scene_id: SceneId,
        /// The timeline, whole.
        timeline: TimelineInfo,
    },
    /// Deletes a timeline (ADR-0115, since 0.87). A playing one stops, with a
    /// `timeline.changed` `removed`. Hotkey bindings whose action names it are removed
    /// with it (ADR-0040 §3), and listed in the result's `removed_hotkeys`; cues of other
    /// timelines that name it stay, inert. Rejected with `not_found` (entity
    /// `timeline`) for a timeline the collection doesn't hold. Inverse: `timeline.set`
    /// with it, then `hotkey.set` for each removed binding.
    #[serde(rename = "timeline.delete")]
    TimelineDelete {
        /// The timeline to delete.
        timeline_id: TimelineId,
    },
    /// Adds a transition preset (ADR-0029, ADR-0037). Inverse: `transition.delete`.
    #[serde(rename = "transition.create")]
    TransitionCreate {
        /// The new preset's ID, chosen by the client. Rejected if it exists.
        preset_id: TransitionPresetId,
        /// Display name.
        name: String,
        /// A transition kind `kind.list` reports (`cut`, `fade`, `move`), else
        /// `invalid_argument`.
        kind: String,
        /// How long one item takes, in milliseconds; at most 60000.
        duration_ms: u32,
        /// The curve its items follow unless they set their own.
        easing: Easing,
        /// Initial settings, checked against the kind's descriptor as `source.create`'s
        /// are. Parameters left out take their defaults. Omitted means all defaults.
        #[serde(default, skip_serializing_if = "Settings::is_empty")]
        settings: Settings,
    },
    /// Changes a transition preset. Fields left out keep their values; `patch` changes
    /// settings as `source.set_settings` does. A new `kind` replaces the settings with
    /// that kind's defaults, overlaid by `patch`. Inverse: `transition.update` with the
    /// previous values of what it changed (and, after a kind change, the previous kind
    /// and every previous setting).
    #[serde(rename = "transition.update")]
    TransitionUpdate {
        /// The preset to change.
        preset_id: TransitionPresetId,
        /// A new display name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// A new transition kind, checked as in `transition.create`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        /// A new duration, in milliseconds; at most 60000.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u32>,
        /// A new easing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        easing: Option<Easing>,
        /// Parameter ID to new value, or `null` for the default.
        #[serde(default, skip_serializing_if = "Settings::is_empty")]
        patch: Settings,
    },
    /// Deletes a transition preset, and clears every scene's `transition_in` and
    /// `transition_out` that names it. Hotkey bindings whose `scene.switch` names it
    /// are removed with it (ADR-0040 §3). The default preset can't be deleted
    /// (`invalid_argument`): make another one the default first. Inverse:
    /// `transition.create` with its name, kind, timing and settings, then
    /// `scene.set_transitions` for each scene it cleared, then `hotkey.set` for each
    /// removed binding.
    #[serde(rename = "transition.delete")]
    TransitionDelete {
        /// The preset to delete.
        preset_id: TransitionPresetId,
    },
    /// Makes a preset the default: the one a switch uses when neither the switch nor
    /// the scenes name one. Inverse: `transition.set_default` with the previous default.
    #[serde(rename = "transition.set_default")]
    TransitionSetDefault {
        /// The preset, which must exist, else `not_found`.
        preset_id: TransitionPresetId,
    },
    /// Sets the presets a scene overrides switches with: `transition_in` when switching
    /// into it, `transition_out` when switching out of it. A switch that names a preset
    /// wins over both, and `transition_in` wins over the other scene's `transition_out`.
    /// Each must name an existing preset, else `not_found`; omitted means none. Inverse:
    /// `scene.set_transitions` with the previous two.
    #[serde(rename = "scene.set_transitions")]
    SceneSetTransitions {
        /// The scene to change.
        scene_id: SceneId,
        /// The preset for switching into this scene, or none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transition_in: Option<TransitionPresetId>,
        /// The preset for switching out of this scene, or none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transition_out: Option<TransitionPresetId>,
    },
    /// Replaces an item's transition settings (ADR-0029 §6): its delay, easing, whether
    /// it may glide to other scenes, and how it appears and disappears without a
    /// partner. A `delay_ms` over 60000 is `invalid_argument`. Inverse:
    /// `item.set_transition` with the previous settings.
    #[serde(rename = "item.set_transition")]
    ItemSetTransition {
        /// The item to change.
        item_id: ItemId,
        /// Its new settings.
        transition: ItemTransition,
    },
    /// Locks or unlocks an item (ADR-0096, since 0.71). A lock is an editing aid kept
    /// with the collection, as OBS's is: a client doesn't let a locked item be dragged,
    /// resized or turned on its canvas, but the engine still applies every edit to it,
    /// so the Inspector, the menus and a script can change it. It belongs to the item,
    /// not to a placement, so it takes no `scene_id` and a keyframe doesn't override
    /// it. Items are added unlocked. Inverse: `item.set_locked` with the previous value.
    #[serde(rename = "item.set_locked")]
    ItemSetLocked {
        /// The item to lock or unlock.
        item_id: ItemId,
        /// Whether its canvas box is locked.
        locked: bool,
    },
    /// Sets where an item's effects run (ADR-0106, since 0.76): over its box before it is
    /// placed (`box`, the default), or over the canvas after it is placed (`canvas`), so
    /// a mask or a gradient lines up with the canvas however the item is moved, scaled,
    /// turned or flipped. Like a lock, it belongs to the item, so it takes no `scene_id`
    /// and a keyframe doesn't override it; a keyframe's overridden effects run in the
    /// item's space. Items are added with `box`. Inverse: `item.set_effects_space` with
    /// the previous value.
    #[serde(rename = "item.set_effects_space")]
    ItemSetEffectsSpace {
        /// The item to change.
        item_id: ItemId,
        /// Where its effects run.
        space: EffectsSpace,
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
    /// 16 buses (`MAX_BUSES`), the master included; one more is `invalid_argument`.
    /// Inverse: `bus.delete`.
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
    /// Gives a bus a monitor, the master's included, or (with `monitor` omitted or null)
    /// takes its monitor away (since 0.42). A monitor `BusMonitor::problem` refuses is
    /// `invalid_argument` on `monitor.<field>`; a device that isn't connected is not
    /// refused. Inverse: `bus.set_monitor` with the previous monitor, or none.
    #[serde(rename = "bus.set_monitor")]
    BusSetMonitor {
        /// The bus to change.
        bus_id: BusId,
        /// The monitor, or none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        monitor: Option<BusMonitor>,
    },
    /// Deletes a bus and every send into it. The master bus can't be deleted
    /// (`invalid_argument`), nor (since 0.24) a bus a source's settings refer to, nor
    /// (since 0.59) a bus an output carries: that `invalid_argument` lists the
    /// `blockers`, and `delete.plan` clears them, moving such an output to the master
    /// bus. Inverse: `bus.create` with its name, `bus.set_fader` if
    /// its fader or mute were not the defaults, `bus.set_monitor` if it had a monitor
    /// (since 0.42), then `send.set` for each send it had.
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
            Edit::SceneCreateVariant { .. } => "scene.create_variant",
            Edit::ItemClearOverride { .. } => "item.clear_override",
            Edit::ItemSetEffects { .. } => "item.set_effects",
            Edit::ItemSetMotion { .. } => "item.set_motion",
            Edit::ItemSetTiming { .. } => "item.set_timing",
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
            Edit::ItemSetOpacity { .. } => "item.set_opacity",
            Edit::ItemSetBlend { .. } => "item.set_blend",
            Edit::ItemSetSource { .. } => "item.set_source",
            Edit::SourceCreate { .. } => "source.create",
            Edit::SourceRename { .. } => "source.rename",
            Edit::SourceSetSettings { .. } => "source.set_settings",
            Edit::SourceDuplicate { .. } => "source.duplicate",
            Edit::SourceDelete { .. } => "source.delete",
            Edit::FilterAdd { .. } => "filter.add",
            Edit::FilterRemove { .. } => "filter.remove",
            Edit::FilterSetEnabled { .. } => "filter.set_enabled",
            Edit::FilterSetSettings { .. } => "filter.set_settings",
            Edit::FilterReorder { .. } => "filter.reorder",
            Edit::OutputCreate { .. } => "output.create",
            Edit::OutputSetTarget { .. } => "output.set_target",
            Edit::OutputSetPath { .. } => "output.set_path",
            Edit::OutputDelete { .. } => "output.delete",
            Edit::OutputSetEncoder { .. } => "output.set_encoder",
            Edit::EncoderCreate { .. } => "encoder.create",
            Edit::EncoderUpdate { .. } => "encoder.update",
            Edit::EncoderDelete { .. } => "encoder.delete",
            Edit::SourceSetAudio { .. } => "source.set_audio",
            Edit::BusCreate { .. } => "bus.create",
            Edit::BusRename { .. } => "bus.rename",
            Edit::BusSetFader { .. } => "bus.set_fader",
            Edit::BusSetMonitor { .. } => "bus.set_monitor",
            Edit::BusDelete { .. } => "bus.delete",
            Edit::SendSet { .. } => "send.set",
            Edit::SendRemove { .. } => "send.remove",
            Edit::CanvasSetSize { .. } => "canvas.set_size",
            Edit::CanvasSetFrameRate { .. } => "canvas.set_frame_rate",
            Edit::CanvasSetBlending { .. } => "canvas.set_blending",
            Edit::VideoMixCreate { .. } => "video_mix.create",
            Edit::VideoMixUpdate { .. } => "video_mix.update",
            Edit::VideoMixDelete { .. } => "video_mix.delete",
            Edit::OutputSetVideoMix { .. } => "output.set_video_mix",
            Edit::OutputSetAudioBuses { .. } => "output.set_audio_buses",
            Edit::FolderCreate { .. } => "folder.create",
            Edit::FolderRename { .. } => "folder.rename",
            Edit::FolderSetParent { .. } => "folder.set_parent",
            Edit::FolderReorder { .. } => "folder.reorder",
            Edit::FolderDelete { .. } => "folder.delete",
            Edit::SceneSetFolder { .. } => "scene.set_folder",
            Edit::SceneSetMeta { .. } => "scene.set_meta",
            Edit::SceneSetOptions { .. } => "scene.set_options",
            Edit::SceneSetCamera { .. } => "scene.set_camera",
            Edit::SceneSetCameraMotion { .. } => "scene.set_camera_motion",
            Edit::SceneSetPtzShots { .. } => "scene.set_ptz_shots",
            Edit::SceneSetCameraFollow { .. } => "scene.set_camera_follow",
            Edit::LabelCreate { .. } => "label.create",
            Edit::LabelUpdate { .. } => "label.update",
            Edit::LabelReorder { .. } => "label.reorder",
            Edit::LabelDelete { .. } => "label.delete",
            Edit::SourceSetMeta { .. } => "source.set_meta",
            Edit::HotkeySet { .. } => "hotkey.set",
            Edit::HotkeySetLabels { .. } => "hotkey.set_labels",
            Edit::HotkeyClear { .. } => "hotkey.clear",
            Edit::BindingSet { .. } => "binding.set",
            Edit::BindingClear { .. } => "binding.clear",
            Edit::TimelineSet { .. } => "timeline.set",
            Edit::TimelineDelete { .. } => "timeline.delete",
            Edit::TransitionCreate { .. } => "transition.create",
            Edit::TransitionUpdate { .. } => "transition.update",
            Edit::TransitionDelete { .. } => "transition.delete",
            Edit::TransitionSetDefault { .. } => "transition.set_default",
            Edit::SceneSetTransitions { .. } => "scene.set_transitions",
            Edit::ItemSetTransition { .. } => "item.set_transition",
            Edit::ItemSetLocked { .. } => "item.set_locked",
            Edit::ItemSetEffectsSpace { .. } => "item.set_effects_space",
        }
    }
}

/// A command with side effects outside the collection state: never undoable and never
/// batchable.
///
/// Serialized with an internal `type` tag holding the command name. The response says
/// the action was accepted, with an [`ActionResult`](crate::ActionResult) for the
/// actions that return something (a YouTube sign-in's consent URL, a broadcast); how
/// it goes is reported by events (`output.state_changed`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
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
    /// one over 1024 bytes or one with whitespace, control or non-ASCII characters, and
    /// `action_failed` when the engine has no credential store, the store refuses, or
    /// the output's `output.start` is still opening its target (#343): send it again
    /// once the start has settled.
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
    /// Opens the gesture `gesture_id` if it is new, and sets its live values (ADR-0032):
    /// each overrides one parameter of the collection on output, from the next render
    /// tick, without changing `rev`. A value replaces the gesture's previous value for
    /// the same parameter, and takes the parameter over from any other gesture that held
    /// it. Values are validated as a batch's edits are (`not_found`, `invalid_argument`
    /// with the field `values.<n>.<field>`); a value that is not a live-adjustable
    /// setter (`item.set_transform`, `item.set_geometry`, `item.set_crop`, and since
    /// 0.67 `source.set_settings` naming only parameters its kind marks `live`, each a
    /// parameter of its own, ADR-0091), an empty or over-256 list, and a 17th open
    /// gesture are `invalid_argument`. Nothing in a
    /// rejected message is applied. A value whose `scene_id` names a keyframe scene is,
    /// since 0.53 (refused before), that keyframe's parameter, apart from the item's
    /// own: it shows only where the keyframe draws the item, and the item's own value
    /// shows there only in a field the keyframe doesn't override. The gesture ends with
    /// a batch carrying `ends_gesture`, with `live.cancel`, or when its client's session
    /// ends.
    #[serde(rename = "live.set")]
    LiveSet {
        /// The gesture, chosen by the client and unique among its open gestures.
        gesture_id: GestureId,
        /// The values, as setter edits: the same edits the committing batch carries.
        #[schemars(length(min = 1, max = 256))]
        values: Vec<Edit>,
    },
    /// Ends one of the client's gestures without a commit: its parameters show their
    /// collection values again, and no edit is made. Succeeds when the client has no
    /// such gesture open, so a retry is safe.
    #[serde(rename = "live.cancel")]
    LiveCancel {
        /// The gesture to cancel.
        gesture_id: GestureId,
    },
    /// Starts signing in to YouTube with the full `youtube` scope (ADR-0041 §4, kept by ADR-0058) and
    /// returns Google's consent URL (`ActionResult` `youtube.sign_in`). The client opens
    /// it in a browser; Google then redirects to a listener the engine opened on
    /// `127.0.0.1`, and the engine finishes the sign-in, keeps the refresh token in the
    /// OS credential store and emits `youtube.status_changed` (`signed_in`, or
    /// `signed_out` when the user declines or the sign-in times out after 5 minutes).
    /// Sent again while a sign-in waits, it returns the same URL. Sent while signed in,
    /// it starts a new sign-in, which replaces the account once it finishes. Fails with
    /// `action_failed` when no YouTube OAuth client is configured.
    #[serde(rename = "youtube.sign_in")]
    YoutubeSignIn,
    /// Signs out of YouTube: removes the account's token from the credential store,
    /// abandons a sign-in that is waiting, then revokes the token at Google (best
    /// effort).
    /// Emits `youtube.status_changed` `signed_out`. Succeeds when already signed out.
    /// A running `youtube` output keeps its connection. Fails with `action_failed` when
    /// no YouTube OAuth client is configured or the credential store refuses.
    #[serde(rename = "youtube.sign_out")]
    YoutubeSignOut,
    /// Creates a YouTube broadcast (`liveBroadcasts.insert`) and returns it. JStream
    /// moves it through its states itself (`youtube.transition_broadcast`), so automatic
    /// start and stop are off, and its monitor stream is on so `testing` works. Fails
    /// with `invalid_argument` for an empty title or start time or an `other` privacy,
    /// and with `action_failed` when not signed in or YouTube refuses, including when
    /// the day's API quota is used up.
    #[serde(rename = "youtube.create_broadcast")]
    YoutubeCreateBroadcast {
        /// Its title.
        title: String,
        /// Its description. Omitted means empty.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        description: String,
        /// When it is scheduled to start, RFC 3339 (`2026-10-16T19:00:00Z`).
        scheduled_start_time: String,
        /// Who can watch it. Omitted means `private`.
        #[serde(default)]
        privacy: BroadcastPrivacy,
        /// Whether it is made for kids. Omitted means `false`.
        #[serde(default)]
        made_for_kids: bool,
    },
    /// Binds a stream to a broadcast (`liveBroadcasts.bind`) and returns the broadcast.
    /// A `youtube` output sends to its broadcast's bound stream. With no `stream_id`,
    /// the engine first creates a reusable stream named after the broadcast. Fails
    /// with `action_failed` when not signed in or YouTube refuses.
    #[serde(rename = "youtube.bind_broadcast")]
    YoutubeBindBroadcast {
        /// The broadcast.
        broadcast_id: String,
        /// The stream to bind, as `youtube.list_streams` reports it. Omitted creates one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_id: Option<String>,
    },
    /// Moves a broadcast to `testing`, `live` or `complete`
    /// (`liveBroadcasts.transition`) and returns it. `testing` and `live` need its
    /// bound stream to be receiving: start its `youtube` output first. Fails with
    /// `action_failed` when not signed in or YouTube refuses (a transition the
    /// broadcast can't make, or a stream that isn't sending).
    #[serde(rename = "youtube.transition_broadcast")]
    YoutubeTransitionBroadcast {
        /// The broadcast.
        broadcast_id: String,
        /// Where to move it.
        to: BroadcastTransition,
    },
    /// Restarts the master bus's integrated loudness measurement (ADR-0028). Meters are
    /// runtime state, so this is an action, never an edit. Succeeds, doing nothing, while
    /// the mixer isn't running.
    #[serde(rename = "audio.reset_loudness")]
    AudioResetLoudness {},
    /// Opens a preview stream of the program mix, or of the scene `scene_id`, for this
    /// session: low-resolution pictures sent as binary frames on `channel`, only to this
    /// session (ADR-0045). The picture is the canvas scaled down to fit `max_width` ×
    /// `max_height`, with even sides; it follows the canvas when that changes. A picture
    /// is sent when what the stream shows changes, at most `fps` times a second, and at
    /// least once every 2 s (a keyframe), so a client that lost frames recovers. The
    /// engine picks the first of `codecs` it can encode, and every frame names its codec.
    /// The stream shows the mix before any master stage. A scene's stream shows the
    /// scene alone, as program would show it with no transition running, and never makes
    /// a source shown or heard (ADR-0028). It lasts until `preview.unsubscribe`, the
    /// session's end, or an end frame (the engine could not go on, or the scene was
    /// deleted; the frame says why).
    ///
    /// With `source`, the stream shows that source alone, fitted into the canvas as an
    /// item filling it would show it, with no filters: what a display, window or webcam
    /// source would capture, before it exists or before its new settings are saved (the
    /// Properties dialog's picture). The engine opens the capture for the stream alone;
    /// a webcam or capture card already open is shared, not opened twice. The canvas
    /// size is the one at the subscription: subscribe again for other settings.
    ///
    /// With `source_id`, the stream shows that source of the collection alone, fitted
    /// into the canvas as an item filling it would show it, with the source's own
    /// filters and none of an item's: a projector of the source. It follows the
    /// source's settings and filters as they change, and the canvas too. Like a scene's
    /// stream it never makes the source shown or heard. It ends, with an end frame, when
    /// the source is deleted.
    ///
    /// Fails with `invalid_argument` for a field out of range (`max_width` 16 to 1920,
    /// default 640; `max_height` 16 to 1080, default 360; `fps` 1 to 30, default 15), a
    /// codec named twice, a `channel` this session already has open (`channel`), a
    /// seventeenth open stream, more than one of `scene_id`, `source` and `source_id` (the
    /// later field), or a `source`
    /// whose kind is not a capture kind or whose settings its kind refuses
    /// (`source.kind`, `source.settings.<param>`); with `not_found` for a `scene_id` or a
    /// `source_id` the collection doesn't have; and with `action_failed` when the session cannot receive binary frames (an
    /// MCP session) or none of `codecs` can be encoded here.
    #[serde(rename = "preview.subscribe")]
    PreviewSubscribe {
        /// The number this stream's binary frames carry, chosen by the client. Not an
        /// ID: it only tells this session's streams apart, and can be reused once the
        /// stream has ended.
        channel: u32,
        /// The widest the picture may be, in pixels. Default 640.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_width: Option<u32>,
        /// The tallest the picture may be, in pixels. Default 360.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_height: Option<u32>,
        /// Pictures per second, at most. Default 15.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fps: Option<u32>,
        /// The codecs the client can decode, most preferred first. Empty or omitted
        /// means `["h264", "jpeg"]`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        codecs: Vec<PreviewCodec>,
        /// The scene to show. Omitted for the program mix.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scene_id: Option<SceneId>,
        /// A source to show alone instead, one the collection doesn't hold (since 0.49).
        /// Omitted for the program mix or `scene_id`'s scene.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<PreviewDraft>,
        /// A source of the collection to show alone (since 0.56), as a projector of it
        /// does. Omitted for the program mix, `scene_id`'s scene or `source`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_id: Option<SourceId>,
    },
    /// Closes one of this session's preview streams. No end frame follows; frames
    /// already sent may still arrive. Succeeds for a channel that is not open.
    #[serde(rename = "preview.unsubscribe")]
    PreviewUnsubscribe {
        /// The channel the stream was opened on.
        channel: u32,
    },
    /// Reports what one of this session's views of preview streams has done since it
    /// opened (0.103, ADR-0132): a client that shows preview pictures (a canvas, a
    /// projector) sends it about once a second while its counts change, so
    /// `profile.get`'s `views` shows what a person saw. `view` is the client's own label
    /// for it, 1 to 64 characters; `counts` are running totals over every stream the view
    /// has shown, and a report whose counts fall below the view's last one starts it
    /// again. Serves only the session that sends it, like `preview.subscribe`, and can't
    /// be bound to a hotkey. Fails with `invalid_argument` on `view` for a bad label, or
    /// for a 17th view of one session.
    #[serde(rename = "preview.report")]
    PreviewReport {
        /// The view's label, the client's own.
        view: String,
        /// Its running totals.
        counts: crate::preview::ViewCounts,
    },
    /// Puts a scene on program, with a transition (ADR-0029): the preset `preset_id`
    /// names, else the scene's `transition_in`, else the current program scene's
    /// `transition_out`, else the default preset. A `cut`, a zero duration, or a preset
    /// whose kind the engine doesn't have shows the scene from the next frame. A switch
    /// during a transition starts from what is on screen. The program scene is runtime
    /// state, not collection state (ADR-0030): the switch changes no `rev`, is never
    /// undone, and emits `studio.changed` naming the preset it used, which `studio.get`
    /// reads back. Switching to the scene already on program succeeds and emits nothing.
    /// Fails with `not_found` for a scene or a preset not in the collection.
    #[serde(rename = "scene.switch")]
    SceneSwitch {
        /// The scene to put on program.
        scene_id: SceneId,
        /// The preset to use, overriding the scenes' and the default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset_id: Option<TransitionPresetId>,
    },
    /// `scene.switch`, then a frame sheet of the program mix from the switch on (since
    /// 0.82, #1061), as `video.frames` takes one: so an agent sees a transition, a
    /// stinger's cover or a keyframe move as it plays, from one call. The switch is made
    /// as `scene.switch` makes it, and emits what it emits; the answer comes once the
    /// last picture is drawn, as a `scene.switch_and_watch` result. Switching to the
    /// scene already on program still takes the sheet. Fails as `scene.switch` does,
    /// before switching with `invalid_argument` past the [`FrameStrip`] limits.
    #[serde(rename = "scene.switch_and_watch")]
    SceneSwitchAndWatch {
        /// The scene to put on program.
        scene_id: SceneId,
        /// The preset to use, overriding the scenes' and the default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset_id: Option<TransitionPresetId>,
        /// When to take the pictures, counted from the switch.
        #[serde(flatten)]
        strip: FrameStrip,
    },
    /// Puts a scene on preview, the scene studio mode prepares before it goes on program
    /// with `scene.switch`. Preview is runtime state like program: it changes no `rev`,
    /// is never undone, and emits `studio.changed`. It never affects what an output
    /// shows unless the output's mix is the preview mix. Setting the scene already on
    /// preview succeeds and emits nothing. Fails with `not_found` for a scene not in
    /// the collection.
    #[serde(rename = "studio.set_preview")]
    StudioSetPreview {
        /// The scene to put on preview.
        scene_id: SceneId,
    },
    /// Sets how many scenes besides the program the studio keeps warm (0.115, ADR-0117
    /// §7): their sources keep running, so showing one starts nothing. The Preview scene
    /// is always warm, even past `scenes`; the rest are the scenes most recently on
    /// program, the most recent first. An application setting, not collection state: it
    /// changes no `rev`, is never undone, emits no event, holds whatever collection is
    /// open, and is kept across restarts when the engine's host names a file for it.
    /// `studio.get` reports it. Fails with `invalid_argument` on `scenes` above
    /// [`MAX_WARM_SCENES`].
    #[serde(rename = "studio.set_warm")]
    StudioSetWarm {
        /// The number of warm scenes, 0 to [`MAX_WARM_SCENES`]. The default is
        /// [`DEFAULT_WARM_SCENES`].
        scenes: u32,
    },
    /// Shows an item (0.80, ADR-0101) with its own appear effect, the way a hotkey shows
    /// one: the placement's `visible` becomes `true` as `item.set_visible` would make it,
    /// with the same `scene_id` rules and the same errors (`not_found` for an item or
    /// scene not in the collection, `invalid_argument` on `scene_id` for a scene whose
    /// placement it can't change). It is collection state: `rev` moves on by one,
    /// `state.changed` follows and the collection saves it. It is an action, so it is
    /// never a step of the sender's undo history (ADR-0024 §5); another client's undo that
    /// would overwrite it fails with `undo_conflict`, as after a batch. Showing an item
    /// already shown succeeds and changes nothing.
    ///
    /// When the program scene draws the item, it appears with its `appear` effect, after
    /// its `delay_ms` and with its easing, over the default preset's duration. `inherit`
    /// is the default preset's `unmatched`, or no animation when that preset is a `cut`
    /// or another kind without one. A source the program shows again this way restarts
    /// as on a scene switch: a `media` or `browser` source with `restart_on_show` starts
    /// from the beginning. A running transition continues from what is on screen.
    #[serde(rename = "item.show")]
    ItemShow {
        /// The item to show.
        item_id: ItemId,
        /// The scene whose placement of the item to change. See [`Edit::ItemSetVisible`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scene_id: Option<SceneId>,
    },
    /// Hides an item with its own `disappear` effect (0.80, ADR-0101): `item.show`'s
    /// opposite, with the same rules. Hiding an item already hidden succeeds and changes
    /// nothing.
    #[serde(rename = "item.hide")]
    ItemHide {
        /// The item to hide.
        item_id: ItemId,
        /// The scene whose placement of the item to change. See [`Edit::ItemSetVisible`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scene_id: Option<SceneId>,
    },
    /// Shows the item if that placement hides it, else hides it (0.80, ADR-0101), as
    /// `item.show` and `item.hide` do. In a keyframe scene, the placement's visibility is
    /// the keyframe's override if it has one, else the item's own. An action takes no
    /// `expected_rev` or idempotency key, so a retried toggle flips twice: a caller that
    /// may retry sends `item.show` or `item.hide`, which are safe to repeat.
    #[serde(rename = "item.toggle")]
    ItemToggle {
        /// The item to show or hide.
        item_id: ItemId,
        /// The scene whose placement of the item to change. See [`Edit::ItemSetVisible`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scene_id: Option<SceneId>,
    },
    /// Drives a PTZ camera (ADR-0146 §3): pans, tilts and zooms it at signed speeds in
    /// VISCA steps until the next drive, where 0 stops that axis. `pan` is -24 to 24
    /// (negative left), `tilt` -23 to 23 (negative down), `zoom` -8 to 8 (negative
    /// wide). Not an edit: nothing is stored or undone. A drive queued behind another
    /// replaces it. The action returns once the command is queued; `ptz.list` reports a
    /// head that didn't answer. Fails with `invalid_argument` on `camera` for a camera
    /// the machine's PTZ list doesn't name, and on a speed out of range, and with
    /// `action_failed` when the machine has no PTZ camera.
    #[serde(rename = "ptz.drive")]
    PtzDrive {
        /// The camera's name.
        camera: String,
        /// Pan speed: negative left, positive right, 0 stops.
        #[serde(default)]
        pan: i8,
        /// Tilt speed: negative down, positive up, 0 stops.
        #[serde(default)]
        tilt: i8,
        /// Zoom speed: negative wide, positive tele, 0 stops.
        #[serde(default)]
        zoom: i8,
    },
    /// Moves a PTZ camera to an absolute pose in its own counts (ADR-0146 §3, §5): pan
    /// and tilt together, zoom and focus each optional, at least one. `speed` (1 to 24,
    /// the fastest by default) paces pan and tilt; the tilt step is held to 23. Fails
    /// as `ptz.drive` does, and with `invalid_argument` for a pose `PtzPose::problem`
    /// refuses or an empty one.
    #[serde(rename = "ptz.aim")]
    PtzAim {
        /// The camera's name.
        camera: String,
        /// Where to move.
        #[serde(flatten)]
        pose: crate::ptz::PtzPose,
        /// The pan and tilt speed, 1 to 24. Omitted for the fastest.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        speed: Option<u8>,
    },
    /// Sends a PTZ camera to its home position (ADR-0146 §3). Fails as `ptz.drive` does.
    #[serde(rename = "ptz.home")]
    PtzHome {
        /// The camera's name.
        camera: String,
    },
    /// Recalls a preset stored in a PTZ camera (ADR-0146 §3), 0 to 127. Fails as
    /// `ptz.drive` does, and with `invalid_argument` on `preset` past 127.
    #[serde(rename = "ptz.recall")]
    PtzRecall {
        /// The camera's name.
        camera: String,
        /// The preset number.
        preset: u8,
    },
    /// Stores a PTZ camera's current pose as one of its presets (ADR-0146 §3), 0 to
    /// 127, in the head's own memory: nothing is stored in the collection or undone, and
    /// it replaces what the head held there. Fails as `ptz.recall` does.
    #[serde(rename = "ptz.store")]
    PtzStore {
        /// The camera's name.
        camera: String,
        /// The preset number.
        preset: u8,
    },
    /// Sets a PTZ camera's focus mode, focuses once, or drives the focus (ADR-0146 §3).
    /// `speed` (1 to 8, the head's standard speed when omitted) paces `near` and `far`.
    /// Fails as `ptz.drive` does.
    #[serde(rename = "ptz.focus")]
    PtzFocus {
        /// The camera's name.
        camera: String,
        /// What to do.
        focus: crate::ptz::PtzFocus,
        /// The drive speed for `near` and `far`, 1 to 8.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        speed: Option<u8>,
    },
    /// Zooms a scene's follow camera (ADR-0113 §4, since 0.85): multiplies its live zoom by 1.25 per
    /// step, a negative step zooming out, held to 1 to 4, and smoothed over 350 ms. Not an
    /// edit: nothing is stored or undone, and the zoom starts again at the follow camera's
    /// own `zoom` when it changes or the engine restarts. One step by default, so a hotkey
    /// can run it (ADR-0112). `not_found` for a scene that doesn't exist,
    /// `action_failed` for one without a camera that follows, and
    /// `invalid_argument` for `steps` outside -16..=16.
    #[serde(rename = "scene.zoom_camera")]
    SceneZoomCamera {
        /// The scene whose camera to zoom: a keyframe scene zooms its family's.
        scene_id: SceneId,
        /// How many steps in; negative zooms out. Default 1.
        #[serde(default = "one_step")]
        steps: i32,
    },
    /// Fires an item's motion modifiers on the `triggered` clock (ADR-0099, #1107),
    /// since 0.84: the one `modifier_id` names, or every one on the item when it is
    /// omitted. Each starts again from its beginning, so firing one while it runs
    /// restarts it. The fire is runtime state like the program scene: it changes no
    /// `rev`, is never undone and emits nothing; the item moves on every mix that draws
    /// it. A modifier on a group's item moves the whole group. Firing a modifier the
    /// item doesn't hold, or one on another clock, does nothing, since a keyframe scene
    /// can override the list it fires. Fails with `not_found` for an item not in the
    /// collection, and with `invalid_argument` for a malformed `modifier_id`.
    #[serde(rename = "item.trigger_motion")]
    ItemTriggerMotion {
        /// The item whose modifiers it fires.
        item_id: ItemId,
        /// The modifier to fire. Omitted for every one on the `triggered` clock.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        modifier_id: Option<String>,
    },
    /// Sets the keyframe a scene is drawn through wherever another scene places it
    /// (ADR-0116, since 0.88): keyframe tracks. A scene with keyframe scenes then switches
    /// between them on its own, independently of the scene placing it and of every other
    /// placed scene, so independent states (a background, a panel layout, an effect)
    /// don't multiply into one keyframe each combination. Where the placing scene is
    /// drawn through a keyframe of its own that overrides an item in this scene's tree,
    /// that keyframe draws it instead. What each placed scene draws through is studio
    /// state like the program scene: it changes no `rev`, is never undone or saved, and
    /// `studio.get` reports it. When program draws the scene and the preset is a `move`
    /// or `fade`, the change plays as its Move from what is on screen, pairing each item
    /// with itself, as `item.show` does; any other kind switches at once. Emits `scene.keyframe_changed`. Fails with `not_found` for a
    /// scene, keyframe or preset the collection doesn't have, and with
    /// `invalid_argument` when `scene_id` is itself a keyframe scene or `keyframe_id`
    /// isn't one of its keyframe scenes.
    #[serde(rename = "scene.set_keyframe")]
    SceneSetKeyframe {
        /// The scene whose placements switch: not a keyframe scene.
        scene_id: SceneId,
        /// One of its keyframe scenes. Omitted for the scene's own values.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        keyframe_id: Option<SceneId>,
        /// The preset whose timing the Move takes. Omitted for the default preset; one
        /// with no length (a cut) switches at once.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset_id: Option<TransitionPresetId>,
    },
    /// Plays a timeline from its start (ADR-0115, since 0.87), or from its start again if
    /// it is playing. Its cues fire at their times on the animation clock (ADR-0099
    /// decision 2), on every pass of a loop, until it ends or is stopped. Playing is
    /// studio state like the program scene: it changes no `rev`, is never undone, and
    /// emits `timeline.changed` (`started`); each cue emits `timeline.cue_fired`. A cue
    /// runs its action as the call would run, with the effects that call has (a
    /// `scene.switch` emits `studio.changed`, an `item.show` moves `rev`), credited to the
    /// client that started the timeline. A transition a cue starts takes the cue's exact
    /// time as its start, so a late tick doesn't delay what is seen. It plays whatever
    /// scene is on program, since each cue names its own target. Fails with `not_found`
    /// for a timeline the collection doesn't hold.
    #[serde(rename = "timeline.play")]
    TimelinePlay {
        /// The timeline to play.
        timeline_id: TimelineId,
    },
    /// Stops a playing timeline (ADR-0115, since 0.87), where it is: what its cues did
    /// stays. Emits `timeline.changed` (`stopped`). Stopping a timeline that isn't playing
    /// succeeds and emits nothing. Fails with `not_found` for a timeline the collection
    /// doesn't hold.
    #[serde(rename = "timeline.stop")]
    TimelineStop {
        /// The timeline to stop.
        timeline_id: TimelineId,
    },
    /// Plays a timeline from `at_ms` on its first pass (ADR-0115, since 0.87), whether or
    /// not it was playing, so the scene shows what it would if the timeline had played
    /// there: first the cues before `at_ms` that set a state run at once, only the last
    /// one for each thing they set (the program scene, an item's visibility, a shared
    /// value), then the timeline plays on from `at_ms`. Cues that are events (a toggle, a
    /// restart, a fired motion, another timeline's play, stop or seek) don't run again.
    /// Emits `timeline.changed` (`sought`). Fails with `not_found` for a timeline the
    /// collection doesn't hold, and with `invalid_argument` on `at_ms` past its
    /// `duration_ms`, or for a timeline that repeats, at or past where it jumps back.
    #[serde(rename = "timeline.seek")]
    TimelineSeek {
        /// The timeline.
        timeline_id: TimelineId,
        /// Where to play it from, in milliseconds from its start.
        at_ms: u32,
    },
    /// Restarts a source from its beginning (ADR-0115, since 0.87): a `browser` page
    /// loads again, and a `media` file plays from its start. Other kinds have nothing to
    /// restart, so for them it succeeds and does nothing, as it does for a source no mix
    /// shows yet. Runtime state like a scene switch's restart: it changes no `rev` and is
    /// never undone. Fails with `not_found` for a source the collection doesn't hold.
    #[serde(rename = "source.restart")]
    SourceRestart {
        /// The source to restart.
        source_id: SourceId,
    },
    /// Opens pairing for one more device (ADR-0044): the server draws a one-time
    /// pairing code and answers with it, to this client only (a `result` of type
    /// `pairing.start`). A device that posts the code with a name to `POST
    /// /api/v1/pair` within 2 minutes gets its own device token. One code is open at a
    /// time: a new `pairing.start` replaces the one open. A code is single-use and void
    /// after 5 wrong attempts. The code never appears in a log, an event or any other
    /// answer (R-SEC-001). Answered by the server that admits clients, not the engine:
    /// a `Control` with no server in front fails with `unsupported`. Fails with
    /// `action_failed` when the server listens beyond loopback, since a device token
    /// travels only over loopback or TLS and the server has no TLS yet.
    #[serde(rename = "pairing.start")]
    PairingStart,
    /// Revokes a paired device: its token stops working at once, and its open
    /// connections are closed. Fails with `not_found` for a device that isn't paired
    /// (or whose token lapsed after 30 days unused). Answered by the server, like
    /// `pairing.start`.
    #[serde(rename = "device.revoke")]
    DeviceRevoke {
        /// The device to revoke, as `device.list` reports it.
        device_id: DeviceId,
    },
    /// Answers a held client's request to connect (0.108, ADR-0131): `allow_once` lets it
    /// in, `always_allow` also adds it to the approved list, and `deny` turns it away
    /// with `not_allowed`. Every client gets `agent.approval_resolved`. Any admitted
    /// client may answer. Fails with `not_found` for a request that isn't open (answered,
    /// timed out, or the client went away). Answered by the server, like `device.revoke`.
    #[serde(rename = "agent.approve")]
    AgentApprove {
        /// The request, as `agent.approval_requested` or `agent.approved` names it.
        approval_id: String,
        /// The answer.
        decision: crate::ApprovalDecision,
    },
    /// Removes a row from the approved list (0.108, ADR-0131): that client's next
    /// connection asks again. An open connection stays open. Fails with `not_found` for
    /// a row the list doesn't hold. Answered by the server, like `device.revoke`.
    #[serde(rename = "agent.forget")]
    AgentForget {
        /// The client name, as `agent.approved` lists it.
        client_name: String,
        /// How it connects, as `agent.approved` lists it.
        transport: crate::AgentTransport,
    },
    /// Keeps a UI setting on the engine's machine (0.52, ADR-0084): an opaque JSON value
    /// the UI owns, under `key` (the web UI keeps its dock under `workspace.dock`), so
    /// every client of the engine, the desktop app and each browser, reads the same one
    /// back with `ui_settings.get`. A `value` of `null`, or none, removes the key. Not
    /// collection state: it changes no `rev`, is never undone, emits no event, and is
    /// kept across restarts in the data folder (ADR-0020), whatever collection is open.
    /// Answered by the server that admits clients, not the engine: a `Control` with no
    /// server in front fails with `unsupported`. Fails with `invalid_argument` on `key`
    /// for a key that isn't 1 to 64 of `a`-`z`, `0`-`9`, `.`, `_` and `-`, on `value`
    /// for a value over 64 KiB as compact JSON, and on `key` for a 65th key;
    /// `action_failed` when the settings file can't be written (the value is then kept
    /// until the server stops).
    #[serde(rename = "ui_settings.set")]
    UiSettingsSet {
        /// The setting's key.
        key: String,
        /// Its new value, any JSON; `null` or none removes the key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<serde_json::Value>,
    },
    /// Installs a WASM automation plugin from the package at `path` on the engine's
    /// machine, with what the person granted it after seeing its manifest (0.35,
    /// ADR-0069). It starts enabled, and `plugin.state_changed` reports it. For a person
    /// only: no MCP tool carries it, and no plugin is ever granted it. Fails with
    /// `already_exists` for a plugin ID in use, `invalid_argument` for a grant whose
    /// `problem` names a field (`grant.hosts.0`), a grant that gives more than the
    /// manifest asks for (`grant.classes.0`, `grant.hosts.0`, `grant.folders.0`), or a
    /// package that isn't a plugin, `action_failed` for a module whose `start` function
    /// traps or runs past its limits, and `unsupported` from an engine that hosts no
    /// plugins.
    #[serde(rename = "plugin.install")]
    PluginInstall {
        /// The ID to install it under.
        plugin_id: PluginId,
        /// The plugin's package: its manifest and module.
        path: String,
        /// The classes, hosts and folders the person granted it.
        grant: PluginGrant,
    },
    /// Replaces an installed plugin's grant, widening or narrowing it (0.35), never past
    /// what its manifest asks for (`invalid_argument`, as for `plugin.install`). The
    /// person grants it knowing what `plugin.list` last showed their client the manifest
    /// asks for (what it asks for now, if their client never listed it), so a plugin
    /// disabled because an update asks for more (`needs_grant`) can then be re-enabled.
    /// An update loaded after that list isn't acknowledged, and one that asks for more
    /// still waits as `needs_grant`. For a person only, like `plugin.install`. Fails with
    /// `not_found` (entity `plugin`) for a plugin that isn't installed.
    #[serde(rename = "plugin.grant")]
    PluginGrant {
        /// The plugin.
        plugin_id: PluginId,
        /// Its new grant.
        grant: PluginGrant,
    },
    /// Re-enables a disabled plugin (0.35). The engine disables a plugin that runs past a
    /// limit or traps, and never re-enables it by itself. Succeeds for one already
    /// enabled. Fails with `not_found` for a plugin that isn't installed, and
    /// `action_failed` for one whose update asks for more than its grant. For a person
    /// only, like `plugin.install`.
    #[serde(rename = "plugin.enable")]
    PluginEnable {
        /// The plugin.
        plugin_id: PluginId,
    },
    /// Re-enables an effect-pack kind the engine quarantined after a GPU device loss was
    /// blamed on it (0.61, #349; `gpu.device_lost`) or after it ran too long on the GPU
    /// (0.102, `gpu.effect_slow`), and the whole pack when the pack
    /// was quarantined. Its filters draw again from the next frame. The engine never
    /// lifts a quarantine by itself, nor when the pack's files change. Succeeds for a
    /// kind that isn't quarantined. Fails with `invalid_argument` on `kind_id`, as
    /// `kind.describe` does, for a kind that no loaded pack has and no quarantine names. For a person only, like
    /// `plugin.enable`.
    #[serde(rename = "effect.enable")]
    EffectEnable {
        /// The kind, `<pack>/<effect>`.
        kind_id: String,
    },
    /// Scans the CLAP search folders again (0.106, ADR-0035), so a plugin installed since
    /// the engine started is offered without a restart. It answers once the scan ends,
    /// which takes a moment per plugin file; `plugin.clap_scan` then reports it. Each
    /// effect found becomes its `clap/<plugin id>` filter kind; a kind an earlier scan
    /// found stays until the engine restarts, so a filter that uses it keeps its
    /// settings. A scan already running is waited for, then this one runs. For a person
    /// only, like `plugin.install`: no MCP tool carries it, no plugin is granted it and no
    /// hotkey binds it. Fails with `unsupported` when this engine hosts no CLAP plugins.
    #[serde(rename = "plugin.clap_rescan")]
    PluginClapRescan,
    /// Downloads a codec library `codec.downloads` lists, from its maker's server, and
    /// checks it against what the engine pins (0.69, ADR-0061): on Linux, Cisco's
    /// OpenH264. Send it only when a person asks, after showing the download's `notice`
    /// and a link to its `license_url`. It answers once the library is in place, which
    /// takes as long as the download; outputs started afterwards can encode with it.
    /// Succeeds for one already installed, downloading it again. For a person only: no
    /// MCP tool carries it, no plugin is granted it and no hotkey binds it. Fails with
    /// `invalid_argument` on `download_id` for an ID `codec.downloads` doesn't list, and
    /// `action_failed` when the download fails or doesn't match the pin (nothing is kept
    /// then), with the reason.
    #[serde(rename = "codec.download")]
    CodecDownload {
        /// The download, as `codec.downloads` lists it.
        download_id: String,
    },
    /// Deletes a downloaded codec library (0.69, ADR-0061). `codec.downloads` then
    /// lists it as not installed, and outputs started afterwards can't encode with it;
    /// one running keeps its encoder. Succeeds for one that isn't installed. For a
    /// person only, like `codec.download`. Fails with `invalid_argument` on
    /// `download_id` for an ID `codec.downloads` doesn't list, and `action_failed` when
    /// the file can't be deleted.
    #[serde(rename = "codec.delete")]
    CodecDelete {
        /// The download, as `codec.downloads` lists it.
        download_id: String,
    },
    /// Writes one effect pack (0.72, ADR-0097): replaces the pack `pack` whole with
    /// `files`, or adds it, and loads it at once. The pack is always the folder named
    /// `pack` in the packs folder, whose `effect.toml` must say `id = "<pack>"`. Answers
    /// with the pack as loaded. Fails, writing nothing and keeping the old version, with
    /// `too_large` for `files` over 3 MiB as sent (`MAX_PACK_WRITE_BYTES`); with
    /// `invalid_argument` for more than 64 files, an image over 2 MiB, a pack ID or file
    /// path outside the rules, a manifest that names another ID, an ID another folder
    /// holds, a folder `pack` that holds anything but that pack, or a pack that doesn't
    /// load (the first problem's file, line and message); with `internal` when the disk
    /// refuses the swap; and with `unsupported` from an engine that loads no packs. Not
    /// an edit: undo doesn't reach the disk. Agents may call it; no plugin is granted it.
    #[serde(rename = "pack.write")]
    PackWrite {
        /// The pack ID: lower-case ASCII letters, digits and `-`, starting with a letter
        /// or digit.
        pack: String,
        /// Every file of the pack.
        files: Vec<crate::PackFile>,
    },
    /// Removes one effect pack (0.72, ADR-0097): the folder whose `effect.toml` names
    /// pack ID `pack`, whatever its name and whether or not it loads. Its kinds leave
    /// `kind.list`, and filters and sources of them draw nothing, as before a pack loads.
    /// Fails with `not_found` (entity `pack`) when no folder holds that pack, `internal`
    /// when the disk refuses, and `unsupported` from an engine that loads no packs. Not
    /// an edit: undo doesn't reach the disk. Agents may call it; no plugin is granted it.
    #[serde(rename = "pack.delete")]
    PackDelete {
        /// The pack ID.
        pack: String,
    },
    /// Sets one shared value (0.78, ADR-0102 decision 7), or removes it when `value` is
    /// `null` or left out. Shared values are named JSON values the engine holds for pages and agents
    /// to read: never saved, never `rev`, and not an edit, so a value that changes often
    /// fills no undo history. Every client gets `values.changed` when it changes; setting
    /// the value a name already has, or removing a name that has none, changes nothing
    /// and sends nothing. Fails with `invalid_argument` at `name` for a name outside the
    /// rules (dotted segments of `a`-`z`, `0`-`9` and `_`, at most 64 bytes), at `value`
    /// for a value over 16 KiB as compact JSON, and at `name` when the store already
    /// holds 256 names and this is a new one.
    #[serde(rename = "values.set")]
    ValuesSet {
        /// The value's name, such as `status.text`.
        name: String,
        /// The value. `null`, or leaving it out, removes the value. Inside a value, `null`
        /// is an ordinary JSON value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<serde_json::Value>,
    },
    /// Opens the project (collection file) at `path` on the engine's machine and saves
    /// there from then on (0.94, ADR-0121). The file is loaded and checked first; a file
    /// the engine can't read or load (a newer `schema_version`, a broken file) fails with
    /// `action_failed` and its reason, and the open project stays open with nothing
    /// stopped, as it does when the open project can't be saved first. Then every
    /// output that isn't stopped stops, and the collection is
    /// replaced: `rev` becomes the file's or one past the current one, whichever is
    /// greater, every client's undo history and idempotency keys are cleared, open
    /// gestures are cancelled, timelines stop, and the first scene goes on program and
    /// preview. Every client gets `state.changed`, `studio.changed` and
    /// `collection.file_changed`. Fails with `invalid_argument` on `path` for a path
    /// that breaks the `absolute` asset path rules, and `unsupported` when the host
    /// named no collection file. Not an edit: undo can't take it back.
    #[serde(rename = "collection.open")]
    CollectionOpen {
        /// The absolute path of the collection file.
        path: String,
    },
    /// Creates a new, empty project at `path` (the four sample labels, the default
    /// canvas and frame rate) and opens it as `collection.open` would (0.94, ADR-0121).
    /// It never replaces a file: fails with `already_exists` when anything is at `path`,
    /// `action_failed` when it can't be written, and otherwise as `collection.open`.
    #[serde(rename = "collection.new")]
    CollectionNew {
        /// The absolute path of the new collection file.
        path: String,
    },
    /// Writes the current project to a new file at `path` and saves there from then on
    /// (0.94, ADR-0121). Every client gets `collection.file_changed`. When the new folder
    /// differs, each `collection` asset reference is rewritten to keep naming the same
    /// file: relative to the new folder when the file is under it, otherwise
    /// `absolute`; an `absolute` reference to a file under the new folder becomes
    /// relative to it. A rewrite is a new `rev` (`state.changed`) and clears every client's
    /// undo history. Nothing stops. Each output's stream key is copied to the new file.
    /// It never replaces a file: fails with `already_exists` when anything is at `path`,
    /// the open file included, `invalid_argument` on `path` for a path that breaks the
    /// `absolute` asset path rules, `action_failed` when the file can't be written (the
    /// open file stays the one saved to), and `unsupported` when the host named no
    /// collection file.
    #[serde(rename = "collection.save_as")]
    CollectionSaveAs {
        /// The absolute path of the new collection file.
        path: String,
    },
    /// **Deprecated** (ADR-0080), removed in 1.0. JStream has no privacy
    /// screen: the engine refuses this with `unsupported` and covers nothing.
    #[serde(rename = "privacy.set")]
    PrivacySet {
        /// Ignored: JStream has no privacy screen.
        on: bool,
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
            Action::LiveSet { .. } => "live.set",
            Action::LiveCancel { .. } => "live.cancel",
            Action::YoutubeSignIn => "youtube.sign_in",
            Action::YoutubeSignOut => "youtube.sign_out",
            Action::YoutubeCreateBroadcast { .. } => "youtube.create_broadcast",
            Action::YoutubeBindBroadcast { .. } => "youtube.bind_broadcast",
            Action::YoutubeTransitionBroadcast { .. } => "youtube.transition_broadcast",
            Action::AudioResetLoudness {} => "audio.reset_loudness",
            Action::PreviewSubscribe { .. } => "preview.subscribe",
            Action::PreviewUnsubscribe { .. } => "preview.unsubscribe",
            Action::PreviewReport { .. } => "preview.report",
            Action::SceneSwitch { .. } => "scene.switch",
            Action::SceneSwitchAndWatch { .. } => "scene.switch_and_watch",
            Action::PairingStart => "pairing.start",
            Action::DeviceRevoke { .. } => "device.revoke",
            Action::AgentApprove { .. } => "agent.approve",
            Action::AgentForget { .. } => "agent.forget",
            Action::UiSettingsSet { .. } => "ui_settings.set",
            Action::StudioSetPreview { .. } => "studio.set_preview",
            Action::StudioSetWarm { .. } => "studio.set_warm",
            Action::ItemShow { .. } => "item.show",
            Action::ItemHide { .. } => "item.hide",
            Action::ItemToggle { .. } => "item.toggle",
            Action::SceneZoomCamera { .. } => "scene.zoom_camera",
            Action::PtzDrive { .. } => "ptz.drive",
            Action::PtzAim { .. } => "ptz.aim",
            Action::PtzHome { .. } => "ptz.home",
            Action::PtzRecall { .. } => "ptz.recall",
            Action::PtzStore { .. } => "ptz.store",
            Action::PtzFocus { .. } => "ptz.focus",
            Action::SceneSetKeyframe { .. } => "scene.set_keyframe",
            Action::ItemTriggerMotion { .. } => "item.trigger_motion",
            Action::TimelinePlay { .. } => "timeline.play",
            Action::TimelineStop { .. } => "timeline.stop",
            Action::TimelineSeek { .. } => "timeline.seek",
            Action::SourceRestart { .. } => "source.restart",
            Action::PrivacySet { .. } => "privacy.set",
            Action::PluginInstall { .. } => "plugin.install",
            Action::PluginGrant { .. } => "plugin.grant",
            Action::PluginEnable { .. } => "plugin.enable",
            Action::PluginClapRescan => "plugin.clap_rescan",
            Action::EffectEnable { .. } => "effect.enable",
            Action::CodecDownload { .. } => "codec.download",
            Action::CodecDelete { .. } => "codec.delete",
            Action::PackWrite { .. } => "pack.write",
            Action::PackDelete { .. } => "pack.delete",
            Action::ValuesSet { .. } => "values.set",
            Action::CollectionOpen { .. } => "collection.open",
            Action::CollectionNew { .. } => "collection.new",
            Action::CollectionSaveAs { .. } => "collection.save_as",
        }
    }

    /// The collection objects this action names, by kind and ID: what a hotkey binding
    /// bound to it refers to (ADR-0040 §3). Deleting one of them removes the binding.
    pub fn named_objects(&self) -> Vec<(EntityKind, &str)> {
        match self {
            Action::OutputStart { output_id }
            | Action::OutputStop { output_id }
            | Action::OutputSetKey { output_id, .. }
            | Action::OutputClearKey { output_id } => {
                vec![(EntityKind::Output, output_id.as_str())]
            }
            Action::SceneSwitch {
                scene_id,
                preset_id,
            }
            | Action::SceneSwitchAndWatch {
                scene_id,
                preset_id,
                ..
            } => {
                let mut named = vec![(EntityKind::Scene, scene_id.as_str())];
                if let Some(preset_id) = preset_id {
                    named.push((EntityKind::Transition, preset_id.as_str()));
                }
                named
            }
            Action::StudioSetPreview { scene_id } | Action::SceneZoomCamera { scene_id, .. } => {
                vec![(EntityKind::Scene, scene_id.as_str())]
            }
            Action::ItemShow { item_id, scene_id }
            | Action::ItemHide { item_id, scene_id }
            | Action::ItemToggle { item_id, scene_id } => {
                let mut named = vec![(EntityKind::Item, item_id.as_str())];
                if let Some(scene_id) = scene_id {
                    named.push((EntityKind::Scene, scene_id.as_str()));
                }
                named
            }
            Action::ItemTriggerMotion { item_id, .. } => {
                vec![(EntityKind::Item, item_id.as_str())]
            }
            Action::SceneSetKeyframe {
                scene_id,
                keyframe_id,
                preset_id,
            } => {
                let mut named = vec![(EntityKind::Scene, scene_id.as_str())];
                if let Some(keyframe_id) = keyframe_id {
                    named.push((EntityKind::Scene, keyframe_id.as_str()));
                }
                if let Some(preset_id) = preset_id {
                    named.push((EntityKind::Transition, preset_id.as_str()));
                }
                named
            }
            Action::TimelinePlay { timeline_id }
            | Action::TimelineStop { timeline_id }
            | Action::TimelineSeek { timeline_id, .. } => {
                vec![(EntityKind::Timeline, timeline_id.as_str())]
            }
            Action::SourceRestart { source_id } => vec![(EntityKind::Source, source_id.as_str())],
            // Broadcasts, preview channels and devices are not collection objects, and a
            // live action can't be bound (`bind_problem`).
            // A PTZ camera belongs to the machine, not the collection (ADR-0146 §1).
            Action::PtzDrive { .. }
            | Action::PtzAim { .. }
            | Action::PtzHome { .. }
            | Action::PtzRecall { .. }
            | Action::PtzStore { .. }
            | Action::PtzFocus { .. }
            | Action::AudioResetLoudness {}
            | Action::LiveSet { .. }
            | Action::LiveCancel { .. }
            | Action::YoutubeSignIn
            | Action::YoutubeSignOut
            | Action::YoutubeCreateBroadcast { .. }
            | Action::YoutubeBindBroadcast { .. }
            | Action::YoutubeTransitionBroadcast { .. }
            | Action::PreviewSubscribe { .. }
            | Action::PreviewUnsubscribe { .. }
            | Action::PreviewReport { .. }
            | Action::PairingStart
            | Action::DeviceRevoke { .. }
            | Action::AgentApprove { .. }
            | Action::AgentForget { .. }
            | Action::UiSettingsSet { .. }
            | Action::StudioSetWarm { .. }
            | Action::PrivacySet { .. }
            | Action::PluginInstall { .. }
            | Action::PluginGrant { .. }
            | Action::PluginEnable { .. }
            | Action::PluginClapRescan
            | Action::EffectEnable { .. }
            | Action::CodecDownload { .. }
            | Action::CodecDelete { .. }
            | Action::PackWrite { .. }
            | Action::PackDelete { .. }
            | Action::ValuesSet { .. }
            | Action::CollectionOpen { .. }
            | Action::CollectionNew { .. }
            | Action::CollectionSaveAs { .. } => vec![],
        }
    }

    /// Why this action can't be bound to a hotkey, as the field within the action and the
    /// reason, or `None` if it can. The engine rejects `hotkey.set` with it, so every
    /// implementation gives the same one. Every action is listed, so a new one has to be
    /// classified here: one that carries a secret can't be bound, and neither can one
    /// that answers or serves only the session that sends it, or that runs an account,
    /// device or YouTube flow a person drives through the UI.
    pub fn bind_problem(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Action::OutputSetKey { .. } => Some((
                "type",
                "`output.set_key` can't be bound: a stream key is never stored in the \
                 collection (R-SEC-001)",
            )),
            Action::PreviewSubscribe { .. }
            | Action::PreviewUnsubscribe { .. }
            | Action::PreviewReport { .. } => Some((
                "type",
                "a preview action can't be bound: it serves only the session that sends it",
            )),
            Action::SceneSwitchAndWatch { .. } => Some((
                "type",
                "`scene.switch_and_watch` can't be bound: its frame sheet answers only the \
                 session that sends it; bind `scene.switch`",
            )),
            Action::LiveSet { .. } | Action::LiveCancel { .. } => Some((
                "type",
                "a live action can't be bound: its gesture belongs to the session that sends \
                 it (ADR-0032)",
            )),
            Action::PairingStart | Action::DeviceRevoke { .. } => Some((
                "type",
                "a pairing or device action can't be bound: the server answers it for the \
                 client that asks",
            )),
            Action::AgentApprove { .. } | Action::AgentForget { .. } => Some((
                "type",
                "an agent approval can't be bound: a person answers who may connect \
                 through the UI (ADR-0131)",
            )),
            Action::StudioSetWarm { .. } => Some((
                "type",
                "`studio.set_warm` can't be bound: it's an application setting, changed in \
                 Settings",
            )),
            Action::UiSettingsSet { .. } => Some((
                "type",
                "`ui_settings.set` can't be bound: the server answers it, and a UI keeps \
                 its own settings",
            )),
            Action::PluginInstall { .. }
            | Action::PluginGrant { .. }
            | Action::PluginEnable { .. } => Some((
                "type",
                "a plugin action can't be bound: a person installs, grants and re-enables \
                 plugins through the UI (ADR-0069)",
            )),
            Action::PtzDrive { .. } => Some((
                "type",
                "`ptz.drive` can't be bound: a binding fires only on press, so nothing would \
                 stop the move; bind `ptz.aim`, `ptz.recall` or `ptz.home` (ADR-0146 §3)",
            )),
            Action::PluginClapRescan => Some((
                "type",
                "`plugin.clap_rescan` can't be bound: a person rescans the CLAP plugins \
                 through the UI (ADR-0035)",
            )),
            Action::EffectEnable { .. } => Some((
                "type",
                "`effect.enable` can't be bound: a person re-enables a quarantined effect \
                 through the UI (#349)",
            )),
            Action::CodecDownload { .. } | Action::CodecDelete { .. } => Some((
                "type",
                "a codec action can't be bound: a person downloads or deletes a codec \
                 through the UI, after reading its licence (ADR-0061)",
            )),
            Action::CollectionOpen { .. }
            | Action::CollectionNew { .. }
            | Action::CollectionSaveAs { .. } => Some((
                "type",
                "a collection action can't be bound: it replaces or moves the project a key \
                 press would run in (ADR-0121)",
            )),
            Action::PackWrite { .. } | Action::PackDelete { .. } => Some((
                "type",
                "a pack action can't be bound: it writes GPU code to disk, which a key \
                 press shouldn't (ADR-0097)",
            )),
            Action::YoutubeSignIn
            | Action::YoutubeSignOut
            | Action::YoutubeCreateBroadcast { .. }
            | Action::YoutubeBindBroadcast { .. }
            | Action::YoutubeTransitionBroadcast { .. } => Some((
                "type",
                "a YouTube action can't be bound: YouTube is driven through the UI (ADR-0058)",
            )),
            Action::OutputStart { .. }
            | Action::OutputStop { .. }
            | Action::OutputClearKey { .. }
            | Action::AudioResetLoudness {}
            | Action::SceneSwitch { .. }
            | Action::StudioSetPreview { .. }
            | Action::ItemShow { .. }
            | Action::ItemHide { .. }
            | Action::ItemToggle { .. }
            | Action::SceneZoomCamera { .. }
            | Action::PtzAim { .. }
            | Action::PtzHome { .. }
            | Action::PtzRecall { .. }
            | Action::PtzStore { .. }
            | Action::PtzFocus { .. }
            | Action::SceneSetKeyframe { .. }
            | Action::ItemTriggerMotion { .. }
            | Action::TimelinePlay { .. }
            | Action::TimelineStop { .. }
            | Action::TimelineSeek { .. }
            | Action::SourceRestart { .. }
            | Action::PrivacySet { .. } => None,
            // Bindable, but checked as the call would be, so a saved binding never fails
            // every press on its name or size.
            Action::ValuesSet { name, value } => {
                if crate::check_value_name(name, "name").is_err() {
                    Some((
                        "name",
                        "a value's name is dotted segments of a-z, 0-9 and `_`, at most 64 bytes",
                    ))
                } else if value
                    .as_ref()
                    .is_some_and(|value| crate::check_value_size(value, "value").is_err())
                {
                    Some(("value", "a value is at most 16 KiB as JSON"))
                } else {
                    None
                }
            }
        }
    }

    /// Whether the action takes a step, so a wheel chord can run it once per notch
    /// (ADR-0112 §3, 0.104): `scene.zoom_camera`, whose `steps` is what one notch zooms.
    /// Every action is listed, so a new one has to be classified here.
    pub fn takes_steps(&self) -> bool {
        match self {
            Action::SceneZoomCamera { .. } => true,
            Action::PtzDrive { .. }
            | Action::PtzAim { .. }
            | Action::PtzHome { .. }
            | Action::PtzRecall { .. }
            | Action::PtzStore { .. }
            | Action::PtzFocus { .. }
            | Action::OutputStart { .. }
            | Action::PreviewReport { .. }
            | Action::OutputStop { .. }
            | Action::OutputSetKey { .. }
            | Action::OutputClearKey { .. }
            | Action::LiveSet { .. }
            | Action::LiveCancel { .. }
            | Action::YoutubeSignIn
            | Action::YoutubeSignOut
            | Action::YoutubeCreateBroadcast { .. }
            | Action::YoutubeBindBroadcast { .. }
            | Action::YoutubeTransitionBroadcast { .. }
            | Action::AudioResetLoudness {}
            | Action::PreviewSubscribe { .. }
            | Action::PreviewUnsubscribe { .. }
            | Action::SceneSwitch { .. }
            | Action::SceneSwitchAndWatch { .. }
            | Action::StudioSetPreview { .. }
            | Action::ItemShow { .. }
            | Action::ItemHide { .. }
            | Action::ItemToggle { .. }
            | Action::ItemTriggerMotion { .. }
            | Action::SceneSetKeyframe { .. }
            | Action::TimelinePlay { .. }
            | Action::TimelineStop { .. }
            | Action::TimelineSeek { .. }
            | Action::SourceRestart { .. }
            | Action::PairingStart
            | Action::DeviceRevoke { .. }
            | Action::UiSettingsSet { .. }
            | Action::StudioSetWarm { .. }
            | Action::PluginInstall { .. }
            | Action::PluginClapRescan
            | Action::AgentApprove { .. }
            | Action::AgentForget { .. }
            | Action::PluginGrant { .. }
            | Action::PluginEnable { .. }
            | Action::EffectEnable { .. }
            | Action::CodecDownload { .. }
            | Action::CodecDelete { .. }
            | Action::PackWrite { .. }
            | Action::PackDelete { .. }
            | Action::ValuesSet { .. }
            | Action::CollectionOpen { .. }
            | Action::CollectionNew { .. }
            | Action::CollectionSaveAs { .. }
            | Action::PrivacySet { .. } => false,
        }
    }
}

impl Action {
    /// Why this action can't be a timeline cue, as the field within the action and the
    /// reason, or `None` if it can (ADR-0099 decision 4, ADR-0115). A cue runs one of a
    /// closed set: `scene.switch`, `scene.set_keyframe` (ADR-0116), `item.show`,
    /// `item.hide`, `item.toggle`, `item.trigger_motion`, `values.set`,
    /// `source.restart`, and `timeline.play`,
    /// `timeline.stop` and `timeline.seek`. Each is an action a hotkey can run too, and
    /// none is an edit, so playing a timeline adds nothing to an undo history. The
    /// `values.set` checks are `bind_problem`'s.
    pub fn cue_problem(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Action::SceneSwitch { .. }
            | Action::SceneSetKeyframe { .. }
            | Action::ItemShow { .. }
            | Action::ItemHide { .. }
            | Action::ItemToggle { .. }
            | Action::ItemTriggerMotion { .. }
            | Action::SourceRestart { .. }
            | Action::TimelinePlay { .. }
            | Action::TimelineStop { .. }
            | Action::TimelineSeek { .. } => None,
            Action::ValuesSet { .. } => self.bind_problem(),
            _ => Some((
                "type",
                "a cue runs scene.switch, scene.set_keyframe, item.show, item.hide, \
                 item.toggle, item.trigger_motion, values.set, source.restart, timeline.play, \
                 timeline.stop or timeline.seek",
            )),
        }
    }

    /// What a cue running this action sets, for `timeline.seek`, or `None` for an
    /// event: seeking runs only the last cue before the seek for each key (ADR-0099
    /// decision 4). The program scene, the keyframe one scene's placements draw through,
    /// one placement's visibility (an item in the scene a show or hide names), and one
    /// shared value are states; a toggle, a fired motion,
    /// a restart and a timeline's play, stop or seek are events.
    pub fn cue_state(&self) -> Option<String> {
        match self {
            Action::SceneSwitch { .. } => Some("program".into()),
            Action::SceneSetKeyframe { scene_id, .. } => Some(format!("keyframe of {scene_id}")),
            Action::ItemShow { item_id, scene_id } | Action::ItemHide { item_id, scene_id } => {
                Some(format!(
                    "item {item_id} in {}",
                    scene_id.as_ref().map_or("", SceneId::as_str)
                ))
            }
            Action::ValuesSet { name, .. } => Some(format!("value {name}")),
            _ => None,
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
    /// Commits one of the client's open gestures (ADR-0032): when the batch applies, the
    /// gesture ends in the same step, and its live values give way to the committed
    /// state. A gesture the client has not open is `invalid_argument` (field
    /// `ends_gesture`), and nothing is applied. A batch that fails leaves the gesture
    /// open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_gesture: Option<GestureId>,
}

impl Batch {
    /// A batch of one edit, with no `expected_rev`, no idempotency key and no gesture.
    pub fn single(edit: Edit) -> Self {
        Batch {
            edits: vec![edit],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        }
    }

    /// Sets the gesture the batch commits.
    #[must_use]
    pub fn ending_gesture(mut self, gesture_id: GestureId) -> Self {
        self.ends_gesture = Some(gesture_id);
        self
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

    /// Checks that it is within bounds where any source draws it (#1130): each position
    /// coordinate within [`MAX_DRAWN_PX`](crate::MAX_DRAWN_PX), and each scale within
    /// ±[`MAX_LEGACY_SCALE`]. A source is at most `u32::MAX` pixels a side, so every
    /// corner then stays far inside f32's range. The bound is enough, not needed: a
    /// transform past it may still draw, and `item.set_transform` and `item.add` don't
    /// check it (ADR-0022 keeps their 0.3 meaning). Motion keeps a moved transform within
    /// it. Returns what is wrong if it isn't.
    pub fn check_drawable(&self) -> Result<(), &'static str> {
        let near = |v: Finite32| v.get().abs() <= crate::MAX_DRAWN_PX;
        if !(near(self.position.x) && near(self.position.y)) {
            return Err("its position must be within 1e15 pixels of the origin");
        }
        let scale = |v: Finite32| v.get().abs() <= MAX_LEGACY_SCALE;
        if !(scale(self.scale.x) && scale(self.scale.y)) {
            return Err("its scale must be within -100000..=100000");
        }
        Ok(())
    }
}

/// The largest scale, either way, within [`ItemTransform::check_drawable`]'s bounds. No
/// edit checks it: `item.set_transform` stores any finite scale (ADR-0022).
pub const MAX_LEGACY_SCALE: f32 = 100_000.0;

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

/// The highest item opacity `item.set_opacity`, `item.add` and a keyframe override
/// accept (since 0.83, #1101). Above 1 an opacity is a gain on the item's light.
pub const MAX_OPACITY: f32 = 4.0;

/// Where an item's effects run (ADR-0106, since 0.76).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EffectsSpace {
    /// Over the item's cropped, fitted box, in its own frame, before it is placed
    /// (ADR-0065 §5): an effect turns, scales and flips with its item.
    #[default]
    Box,
    /// Over the canvas, after the item is placed: the item is drawn where it is on a
    /// transparent canvas-sized layer, the effects run over that layer, and the layer is
    /// drawn over the scene with the item's opacity and blend. A canvas-sized mask
    /// lines up with the canvas whatever the item's transform.
    Canvas,
}

impl EffectsSpace {
    /// Whether this is the default, `box`.
    pub fn is_box(&self) -> bool {
        *self == EffectsSpace::Box
    }

    /// Its wire name (`box`).
    pub fn as_str(&self) -> &'static str {
        match self {
            EffectsSpace::Box => "box",
            EffectsSpace::Canvas => "canvas",
        }
    }
}

/// How an item composites onto what is below it (ADR-0023 §1, register D13).
///
/// The list is the modes the UI offers, and the compositor draws every one (ADR-0082, since
/// 0.47; before it the engine refused all but `normal`). The project's [`Blending`]
/// picks the space every mode mixes in (ADR-0126). `normal_srgb` and `additive_srgb`
/// (0.75 to 0.100, ADR-0105) still parse, as `normal` and `additive`; the engine never
/// sends them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BlendMode {
    /// Source-over: the item covers what is below it, by its alpha.
    #[default]
    #[serde(alias = "normal_srgb")]
    Normal,
    /// Adds the item's colour to what is below it.
    #[serde(alias = "additive_srgb")]
    Additive,
    /// Multiplies the colours, which darkens.
    Multiply,
    /// The inverse of multiplying the inverses, which lightens.
    Screen,
    /// Multiply or screen, depending on the colour below.
    Overlay,
    /// The darker of the two colours, per channel.
    Darken,
    /// The lighter of the two colours, per channel.
    Lighten,
    /// The absolute difference of the colours.
    Difference,
    /// Like difference, with lower contrast.
    Exclusion,
}

impl BlendMode {
    /// Every mode, in the order the UI lists them.
    pub const ALL: [BlendMode; 9] = [
        BlendMode::Normal,
        BlendMode::Additive,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::Difference,
        BlendMode::Exclusion,
    ];

    /// The mode with this wire name, or the mode an ADR-0105 name now reads as.
    pub fn from_name(name: &str) -> Option<BlendMode> {
        match name {
            "normal_srgb" => Some(BlendMode::Normal),
            "additive_srgb" => Some(BlendMode::Additive),
            _ => BlendMode::ALL
                .into_iter()
                .find(|mode| mode.as_str() == name),
        }
    }

    /// Whether this is the default mode, `normal`.
    pub fn is_normal(&self) -> bool {
        *self == BlendMode::Normal
    }

    /// The mode's wire name (`normal`).
    pub fn as_str(&self) -> &'static str {
        match self {
            BlendMode::Normal => "normal",
            BlendMode::Additive => "additive",
            BlendMode::Multiply => "multiply",
            BlendMode::Screen => "screen",
            BlendMode::Overlay => "overlay",
            BlendMode::Darken => "darken",
            BlendMode::Lighten => "lighten",
            BlendMode::Difference => "difference",
            BlendMode::Exclusion => "exclusion",
        }
    }
}

/// The colour space every blend mode of a project mixes in (ADR-0126, since 0.101).
///
/// A new collection is [`Blending::Srgb`]. A collection saved before it gets one when it
/// loads: `srgb` if its items used only the old `_srgb` modes, or none, and `linear`
/// otherwise, so its scenes keep their look.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Blending {
    /// Browser-style: colours mix in sRGB-encoded values, as CSS, a browser and most
    /// design tools mix them, so a design made there keeps its look.
    #[default]
    Srgb,
    /// Linear light: colours mix as light adds (ADR-0082 §2), so a glow or a fade of
    /// camera footage stays physically even. Overlay, difference and exclusion still mix
    /// encoded values.
    Linear,
}

impl Blending {
    /// Both spaces, in the order the UI lists them.
    pub const ALL: [Blending; 2] = [Blending::Srgb, Blending::Linear];

    /// The space's wire name (`srgb`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Blending::Srgb => "srgb",
            Blending::Linear => "linear",
        }
    }

    /// The space with this wire name.
    pub fn from_name(name: &str) -> Option<Blending> {
        Blending::ALL.into_iter().find(|b| b.as_str() == name)
    }
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

/// `scene.zoom_camera`'s default: one step in.
fn one_step() -> i32 {
    1
}

#[cfg(test)]
mod tests {
    use super::{Action, Batch, BlendMode, Command, CommandClass, Edit, SceneRole};
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

    /// `as_str` is the wire name serde writes, for every mode, and `from_name` reads it.
    #[test]
    fn blend_mode_names_match_the_wire() {
        for mode in BlendMode::ALL {
            assert_eq!(serde_json::to_value(mode).ok(), Some(json!(mode.as_str())));
            assert_eq!(BlendMode::from_name(mode.as_str()), Some(mode));
        }
        assert_eq!(BlendMode::from_name("dissolve"), None);
        for blending in super::Blending::ALL {
            assert_eq!(
                serde_json::to_value(blending).ok(),
                Some(json!(blending.as_str()))
            );
            assert_eq!(
                super::Blending::from_name(blending.as_str()),
                Some(blending)
            );
        }
    }

    /// ADR-0105's two modes still parse, as the modes they are now (ADR-0126 §1).
    #[test]
    fn the_old_srgb_mode_names_read_as_their_plain_modes() {
        for (old, now) in [
            ("normal_srgb", BlendMode::Normal),
            ("additive_srgb", BlendMode::Additive),
        ] {
            assert_eq!(
                serde_json::from_value::<BlendMode>(json!(old)).ok(),
                Some(now)
            );
            assert_eq!(BlendMode::from_name(old), Some(now));
        }
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
