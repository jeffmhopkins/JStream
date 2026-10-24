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
use crate::error::EntityKind;
use crate::filter::{FilterChain, FilterOwner};
use crate::finite::Finite32;
use crate::geometry::ItemGeometry;
use crate::hotkey::Chord;
use crate::id::{
    BusId, DeviceId, EncoderProfileId, FilterId, FolderId, GestureId, IdempotencyKey, ItemId,
    LabelId, OutputId, Rev, SceneId, SourceId, TransitionPresetId, VideoMixId,
};
use crate::mix::VideoMix;
use crate::organise::{SceneMeta, SourceMeta};
use crate::output::{OutputKind, RtmpTarget, SecretString};
use crate::preview::PreviewCodec;
use crate::settings::Settings;
use crate::transition::{Easing, ItemTransition};
use crate::youtube::{BroadcastPrivacy, BroadcastTransition};
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
    /// `invalid_argument` while a `scene` source or a video mix shows it, or any source's
    /// settings refer to it: delete those sources, point those mixes elsewhere, or clear
    /// those references, first (in the same batch if needed). Hotkey bindings whose
    /// action names the scene are not blockers: they are removed with it (ADR-0040 §3).
    /// Inverse: `scene.create` with its role (omitted for `scene`), then `scene.reorder`
    /// to its old display position (unless it was last), `scene.set_transitions` if it
    /// had an override, `scene.set_folder` and `scene.set_meta` if it had a folder or
    /// metadata, then each deleted item's `item.remove` inverse, from bottom to top, then
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
    /// Removes an item from its scene. The source itself stays. Inverse: `item.add` with
    /// the removed item's scene, ID and source, its placement in the field matching its
    /// form (`transform` or `geometry`), and its opacity and blend where they differ from
    /// the defaults, then `item.reorder` to its old stacking position (unless it was on
    /// top), `item.set_crop` if it was cropped, `item.set_visible` if it was hidden, and
    /// `item.set_transition` if its transition settings were not the default.
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
        /// takes it (ADR-0024 §2). Omitted, or the item's own scene, changes the item.
        /// Any other scene is `invalid_argument` until keyframe scenes exist, when naming
        /// a keyframe scene of the item's scene will write that scene's override.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scene_id: Option<SceneId>,
        /// Whether the item is drawn. Items are visible when added.
        visible: bool,
    },
    /// Changes how transparent an item is drawn (ADR-0023 §1): its alpha is multiplied by
    /// `opacity` after crop and fit. A value outside 0 to 1 is `invalid_argument`. Items
    /// are added at 1. Inverse: `item.set_opacity` with the previous value.
    #[serde(rename = "item.set_opacity")]
    ItemSetOpacity {
        /// The item to change.
        item_id: ItemId,
        /// The scene whose placement of the item to change. See [`Edit::ItemSetVisible`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scene_id: Option<SceneId>,
        /// The alpha multiplier, from 0 (invisible) to 1 (as the source draws it).
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
    /// Replaces a scene's organisation metadata: its icon, colour, favourite flag and
    /// whether a multiview shows it. Every field left out takes its default. An icon
    /// that is empty or longer than 64 bytes, or a colour that isn't
    /// `#rrggbb`, is `invalid_argument`. Inverse: `scene.set_meta` with the previous
    /// metadata.
    #[serde(rename = "scene.set_meta")]
    SceneSetMeta {
        /// The scene to change.
        scene_id: SceneId,
        /// The new metadata.
        #[serde(flatten)]
        meta: SceneMeta,
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
    /// Deletes a label. Rejected with `invalid_argument` while a source carries it,
    /// listing them as `blockers`: take it off those sources first, or send the batch
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
    /// Deletes a bus and every send into it. The master bus can't be deleted
    /// (`invalid_argument`), nor (since 0.24) a bus a source's settings refer to: that
    /// `invalid_argument` lists the `blockers`, and `delete.plan` clears them. Inverse: `bus.create` with its name, `bus.set_fader` if
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
            Edit::VideoMixCreate { .. } => "video_mix.create",
            Edit::VideoMixUpdate { .. } => "video_mix.update",
            Edit::VideoMixDelete { .. } => "video_mix.delete",
            Edit::OutputSetVideoMix { .. } => "output.set_video_mix",
            Edit::FolderCreate { .. } => "folder.create",
            Edit::FolderRename { .. } => "folder.rename",
            Edit::FolderSetParent { .. } => "folder.set_parent",
            Edit::FolderReorder { .. } => "folder.reorder",
            Edit::FolderDelete { .. } => "folder.delete",
            Edit::SceneSetFolder { .. } => "scene.set_folder",
            Edit::SceneSetMeta { .. } => "scene.set_meta",
            Edit::LabelCreate { .. } => "label.create",
            Edit::LabelUpdate { .. } => "label.update",
            Edit::LabelReorder { .. } => "label.reorder",
            Edit::LabelDelete { .. } => "label.delete",
            Edit::SourceSetMeta { .. } => "source.set_meta",
            Edit::HotkeySet { .. } => "hotkey.set",
            Edit::HotkeyClear { .. } => "hotkey.clear",
            Edit::TransitionCreate { .. } => "transition.create",
            Edit::TransitionUpdate { .. } => "transition.update",
            Edit::TransitionDelete { .. } => "transition.delete",
            Edit::TransitionSetDefault { .. } => "transition.set_default",
            Edit::SceneSetTransitions { .. } => "scene.set_transitions",
            Edit::ItemSetTransition { .. } => "item.set_transition",
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
    /// setter (`item.set_transform`, `item.set_geometry`, `item.set_crop`), an empty or
    /// over-256 list, and a 17th open gesture are `invalid_argument`. Nothing in a
    /// rejected message is applied. The gesture ends with a batch carrying
    /// `ends_gesture`, with `live.cancel`, or when its client's session ends.
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
    /// Opens a preview stream of the program mix for this session: low-resolution
    /// pictures sent as binary frames on `channel`, only to this session (ADR-0045).
    /// The picture is the canvas scaled down to fit `max_width` × `max_height`, with
    /// even sides; it follows the canvas when that changes. A picture is sent when what
    /// the program mix draws changes, at most `fps` times a second, and at least once
    /// every 2 s (a keyframe), so a client that lost frames recovers. The engine picks
    /// the first of `codecs` it can encode, and every frame names its codec. The stream
    /// shows the mix before any master stage. It lasts until `preview.unsubscribe`, the
    /// session's end, or an end frame (the engine could not go on; the frame says why).
    ///
    /// Fails with `invalid_argument` for a field out of range (`max_width` 16 to 1920,
    /// default 640; `max_height` 16 to 1080, default 360; `fps` 1 to 30, default 15), a
    /// codec named twice, a `channel` this session already has open (`channel`), or a
    /// fifth open stream; and with `action_failed` when the session cannot receive
    /// binary frames (an MCP session) or none of `codecs` can be encoded here.
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
    },
    /// Closes one of this session's preview streams. No end frame follows; frames
    /// already sent may still arrive. Succeeds for a channel that is not open.
    #[serde(rename = "preview.unsubscribe")]
    PreviewUnsubscribe {
        /// The channel the stream was opened on.
        channel: u32,
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
    /// Turns the privacy screen on or off (ADR-0027 §4). While it is on, every output's
    /// video is covered from the next frame: a heavy blur of the outgoing picture, or
    /// black when the blur can't be drawn, never the picture itself. It is runtime
    /// state, not collection state: it changes no `rev`, is never undone, goes to the
    /// audit log, and the engine starts with it off. A change emits `privacy.changed`,
    /// which `privacy.get` reads back; setting the state it already has succeeds and
    /// emits nothing.
    #[serde(rename = "privacy.set")]
    PrivacySet {
        /// Whether the privacy screen covers the outputs.
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
            Action::SceneSwitch { .. } => "scene.switch",
            Action::PairingStart => "pairing.start",
            Action::DeviceRevoke { .. } => "device.revoke",
            Action::StudioSetPreview { .. } => "studio.set_preview",
            Action::PrivacySet { .. } => "privacy.set",
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
            } => {
                let mut named = vec![(EntityKind::Scene, scene_id.as_str())];
                if let Some(preset_id) = preset_id {
                    named.push((EntityKind::Transition, preset_id.as_str()));
                }
                named
            }
            Action::StudioSetPreview { scene_id } => {
                vec![(EntityKind::Scene, scene_id.as_str())]
            }
            // Broadcasts, preview channels and devices are not collection objects, and a
            // live action can't be bound (`bind_problem`).
            Action::AudioResetLoudness {}
            | Action::LiveSet { .. }
            | Action::LiveCancel { .. }
            | Action::YoutubeSignIn
            | Action::YoutubeSignOut
            | Action::YoutubeCreateBroadcast { .. }
            | Action::YoutubeBindBroadcast { .. }
            | Action::YoutubeTransitionBroadcast { .. }
            | Action::PreviewSubscribe { .. }
            | Action::PreviewUnsubscribe { .. }
            | Action::PairingStart
            | Action::DeviceRevoke { .. }
            | Action::PrivacySet { .. } => vec![],
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
            Action::PreviewSubscribe { .. } | Action::PreviewUnsubscribe { .. } => Some((
                "type",
                "a preview action can't be bound: it serves only the session that sends it",
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
            | Action::PrivacySet { .. } => None,
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

/// How an item composites onto what is below it (ADR-0023 §1, register D13).
///
/// The list is the modes the UI offers. The compositor blends in linear light and draws
/// `normal` only so far; the engine rejects every other mode with `invalid_argument`
/// until it draws it, so a client can read the list without the engine claiming it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BlendMode {
    /// Source-over: the item covers what is below it, by its alpha.
    #[default]
    Normal,
    /// Adds the item's colour to what is below it.
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

    /// The mode with this wire name.
    pub fn from_name(name: &str) -> Option<BlendMode> {
        BlendMode::ALL
            .into_iter()
            .find(|mode| mode.as_str() == name)
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
