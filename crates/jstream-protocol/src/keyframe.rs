//! Keyframe scenes on the wire ([ADR-0024](../../../docs/adr/0024-scene-structure.md) §2,
//! [ADR-0049](../../../docs/adr/0049-keyframe-scene-model-and-evaluation.md)).
//!
//! A keyframe scene (a **variant**) is created by `scene.create_variant` and names its
//! parent in `SceneInfo.variant_of`. It has no items of its own: it shows its parent's
//! items, and stores per item the fields it overrides ([`ItemOverride`]). A field it
//! doesn't override follows the parent's current value. The placement edits write an
//! override when their `scene_id` names a variant of the item's scene, and
//! `item.clear_override` removes one field ([`OverrideField`]).

use crate::command::{Crop, ItemTransform};
use crate::finite::Finite32;
use crate::geometry::ItemGeometry;
use crate::id::{ItemId, SceneId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One of the four fields a keyframe scene can override. Each is overridden whole: a
/// variant that overrides `geometry` overrides all of it.
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
}

/// What a keyframe scene overrides for one of its parent's items, as `state.get` reports
/// it. An omitted field follows the parent. At least one field is set: an item with no
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
}

/// Overrides a batch dropped because their item left the keyframe scene's parent
/// (ADR-0040 §1): `item.remove` of the item, or `item.move` of it to another scene.
/// Undoing the batch restores them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DroppedOverride {
    /// The keyframe scene that held the override.
    pub scene_id: SceneId,
    /// The override, whole.
    #[serde(flatten)]
    pub dropped: ItemOverride,
}
