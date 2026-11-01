//! Keyframe scenes on the wire ([ADR-0024](../../../docs/adr/0024-scene-structure.md) §2,
//! [ADR-0049](../../../docs/adr/0049-keyframe-scene-model-and-evaluation.md)).
//!
//! A keyframe scene (a **variant**) is created by `scene.create_variant` and names its
//! parent in `SceneInfo.variant_of`. It has no items of its own: it shows its parent's
//! items, and stores per item the fields it overrides ([`ItemOverride`]). A field it
//! doesn't override follows the parent's current value. An override names an item in
//! the parent's **tree**: one of the parent's own, or a child of a group (a scene with
//! the role `group`) the parent places, at any depth (since 0.55, #846). The placement edits write
//! an override when their `scene_id` names a variant whose tree holds the item, and
//! `item.clear_override` removes one field ([`OverrideField`]). Since 0.63 a variant also
//! overrides an item's effects, whole, with `item.set_effects`
//! ([ADR-0088](../../../docs/adr/0088-keyframe-scenes-override-item-effects.md)).

use crate::command::{Crop, ItemTransform};
use crate::filter::FilterInfo;
use crate::finite::Finite32;
use crate::geometry::ItemGeometry;
use crate::id::{ItemId, SceneId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One of the fields a keyframe scene can override. Each is overridden whole: a variant
/// that overrides `geometry` overrides all of it, and one that overrides `effects` the
/// whole list.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OverrideField {
    /// The item's geometry (`item.set_geometry`, or `item.set_transform` while it lasts).
    Geometry,
    /// Its crop (`item.set_crop`).
    Crop,
    /// Whether it is drawn (`item.set_visible`).
    Visible,
    /// Its opacity (`item.set_opacity`).
    Opacity,
    /// Its effects, the whole list (`item.set_effects`, since 0.63, ADR-0088).
    Effects,
}

/// What a keyframe scene overrides for one item in its parent's tree, as `state.get`
/// reports it. An omitted field follows the parent. At least one field is set: an item with no
/// override has no entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ItemOverride {
    /// The parent's item.
    pub item_id: ItemId,
    /// The overridden geometry, as `item.set_geometry` wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<ItemGeometry>,
    /// The overridden geometry in the deprecated 0.3 form, as `item.set_transform` wrote
    /// it. Never set together with `geometry`: both are the one `geometry` field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<ItemTransform>,
    /// The overridden crop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<Crop>,
    /// The overridden visibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// The overridden opacity, in `0..=1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<Finite32>,
    /// The overridden effects, first first, which the keyframe draws in place of the
    /// item's own (since 0.63, ADR-0088). An empty list is an override too: no effects
    /// there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<Vec<FilterInfo>>,
}

/// Overrides a batch dropped because their item left the keyframe scene's tree
/// (ADR-0040 §1): `item.remove` of the item, `item.move` of it out of the tree, or an
/// edit that takes the group holding it out (`scene.set_role` of the group, or removing
/// or retargeting the last item that places it). Undoing the batch restores them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DroppedOverride {
    /// The keyframe scene that held the override.
    pub scene_id: SceneId,
    /// The override, whole.
    #[serde(flatten)]
    pub dropped: ItemOverride,
}
