//! The mock's keyframe scenes (ADR-0024 §2, ADR-0040 §1, ADR-0049,
//! docs/adr/0049-keyframe-scene-model-and-evaluation.md): `scene.create_variant`, the
//! placement edits addressed to a keyframe scene, which write its overrides,
//! `item.clear_override`, and the overrides an item leaving its parent's tree drops, with
//! the checks, errors and inverses of `jstream-graph`'s `keyframe` module. As there, an
//! override names an item in the parent's tree (its own, or a child of a group placed in
//! it, at any depth), and is written and cleared one field at a time. `item.set_effects`
//! writes the effects field, a whole list (ADR-0088), with the graph's checks that need
//! no kind descriptor: the IDs, their uniqueness and the list's length. Like the rest of
//! the mock it stores the settings as given.

use super::{Collection, already_exists, check_id, check_name, invalid, not_found};
use jstream_protocol::{
    Blocker, Crop, Edit, EntityKind, FilterChains, FilterInfo, Finite32, ItemGeometry, ItemId,
    ItemOverride, ItemTransform, MAX_CHAIN_FILTERS, OverrideField, ProtocolError, SceneId,
    SceneRole, Settings, SourceKind,
};
use std::collections::{BTreeMap, BTreeSet};

/// An overridden placement, in the form the edit that wrote it used.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Placement {
    /// Written by `item.set_geometry`.
    Geometry(ItemGeometry),
    /// Written by `item.set_transform`, the 0.3 form.
    Legacy(ItemTransform),
}

/// What a keyframe scene overrides for one item: each field is absent while it follows
/// the parent. An override with no field set is never stored.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Override {
    geometry: Option<Placement>,
    crop: Option<Crop>,
    visible: Option<bool>,
    opacity: Option<Finite32>,
    /// The effects, a whole video chain (ADR-0088).
    effects: Option<FilterChains>,
}

impl Override {
    fn is_empty(&self) -> bool {
        *self == Override::default()
    }

    /// The placement edits addressed to `scene_id` that write this override, one per
    /// field it sets: what restores it.
    fn write_edits(&self, scene_id: &SceneId, item_id: &ItemId) -> Vec<Edit> {
        let scene_id = Some(scene_id.clone());
        let item_id = item_id.clone();
        let mut edits = Vec::new();
        match self.geometry {
            Some(Placement::Geometry(geometry)) => edits.push(Edit::ItemSetGeometry {
                item_id: item_id.clone(),
                scene_id: scene_id.clone(),
                geometry,
            }),
            Some(Placement::Legacy(transform)) => edits.push(Edit::ItemSetTransform {
                item_id: item_id.clone(),
                scene_id: scene_id.clone(),
                transform,
            }),
            None => {}
        }
        if let Some(crop) = self.crop {
            edits.push(Edit::ItemSetCrop {
                item_id: item_id.clone(),
                scene_id: scene_id.clone(),
                crop,
            });
        }
        if let Some(visible) = self.visible {
            edits.push(Edit::ItemSetVisible {
                item_id: item_id.clone(),
                scene_id: scene_id.clone(),
                visible,
            });
        }
        if let Some(opacity) = self.opacity {
            edits.push(Edit::ItemSetOpacity {
                item_id: item_id.clone(),
                scene_id: scene_id.clone(),
                opacity,
            });
        }
        if let (Some(effects), Some(scene_id)) = (&self.effects, scene_id) {
            edits.push(Edit::ItemSetEffects {
                scene_id,
                item_id,
                effects: effects.video.clone(),
            });
        }
        edits
    }

    /// The settings of every overridden effect, for the delete checks.
    pub(super) fn effect_settings(&self) -> impl Iterator<Item = &Settings> {
        self.effects
            .iter()
            .flat_map(|e| &e.video)
            .map(|f| &f.settings)
    }

    /// Only `field` of this override.
    fn only(&self, field: OverrideField) -> Override {
        let mut only = Override::default();
        match field {
            OverrideField::Geometry => only.geometry = self.geometry,
            OverrideField::Crop => only.crop = self.crop,
            OverrideField::Visible => only.visible = self.visible,
            OverrideField::Opacity => only.opacity = self.opacity,
            OverrideField::Effects => only.effects = self.effects.clone(),
        }
        only
    }

    /// Takes `field` out of this override.
    fn clear(&mut self, field: OverrideField) {
        match field {
            OverrideField::Geometry => self.geometry = None,
            OverrideField::Crop => self.crop = None,
            OverrideField::Visible => self.visible = None,
            OverrideField::Opacity => self.opacity = None,
            OverrideField::Effects => self.effects = None,
        }
    }

    /// The field `other` sets, written into this override.
    fn merge(&mut self, other: Override) {
        self.geometry = other.geometry.or(self.geometry);
        self.crop = other.crop.or(self.crop);
        self.visible = other.visible.or(self.visible);
        self.opacity = other.opacity.or(self.opacity);
        self.effects = other.effects.or(self.effects.take());
    }

    /// The field `edit` writes, as an override of that field alone, and the field.
    fn of_edit(edit: &Edit) -> Option<(Override, OverrideField)> {
        let mut o = Override::default();
        let field = match edit {
            Edit::ItemSetGeometry { geometry, .. } => {
                o.geometry = Some(Placement::Geometry(*geometry));
                OverrideField::Geometry
            }
            Edit::ItemSetTransform { transform, .. } => {
                o.geometry = Some(Placement::Legacy(*transform));
                OverrideField::Geometry
            }
            Edit::ItemSetCrop { crop, .. } => {
                o.crop = Some(*crop);
                OverrideField::Crop
            }
            Edit::ItemSetVisible { visible, .. } => {
                o.visible = Some(*visible);
                OverrideField::Visible
            }
            Edit::ItemSetOpacity { opacity, .. } => {
                o.opacity = Some(*opacity);
                OverrideField::Opacity
            }
            Edit::ItemSetEffects { effects, .. } => {
                o.effects = Some(FilterChains {
                    video: effects.clone(),
                    audio: Vec::new(),
                });
                OverrideField::Effects
            }
            _ => return None,
        };
        Some((o, field))
    }

    /// As `state.get` reports it.
    fn info(&self, item_id: &ItemId) -> ItemOverride {
        ItemOverride {
            item_id: item_id.clone(),
            geometry: match self.geometry {
                Some(Placement::Geometry(geometry)) => Some(geometry),
                _ => None,
            },
            transform: match self.geometry {
                Some(Placement::Legacy(transform)) => Some(transform),
                _ => None,
            },
            crop: self.crop,
            visible: self.visible,
            opacity: self.opacity,
            effects: self.effects.as_ref().map(|e| e.video.clone()),
        }
    }
}

/// A keyframe scene's overrides, by item.
pub(super) type Overrides = BTreeMap<ItemId, Override>;

/// The edit that undoes writing or clearing one field of `item_id`'s override in
/// `scene_id`, given what the field held before: the placement edit that writes it
/// back, or `item.clear_override` when it was absent.
fn inverse(
    scene_id: &SceneId,
    item_id: &ItemId,
    before: &Override,
    field: OverrideField,
) -> Vec<Edit> {
    let back = before.only(field).write_edits(scene_id, item_id);
    if back.is_empty() {
        vec![Edit::ItemClearOverride {
            scene_id: scene_id.clone(),
            item_id: item_id.clone(),
            field,
        }]
    } else {
        back
    }
}

impl Collection {
    /// Applies a keyframe edit, and a placement edit addressed to a keyframe scene, and
    /// returns the edits that undo it. `None` for any other edit, and for a placement
    /// edit that changes the item itself.
    pub(super) fn apply_keyframe(
        &mut self,
        edit: &Edit,
        at: usize,
    ) -> Option<Result<Vec<Edit>, ProtocolError>> {
        match edit {
            Edit::SceneCreateVariant {
                scene_id,
                variant_of,
                name,
            } => Some(self.create_variant(scene_id, variant_of, name, at)),
            Edit::ItemClearOverride {
                scene_id,
                item_id,
                field,
            } => Some(self.clear_override(scene_id, item_id, *field, at)),
            Edit::ItemSetGeometry {
                item_id, scene_id, ..
            }
            | Edit::ItemSetTransform {
                item_id, scene_id, ..
            }
            | Edit::ItemSetCrop {
                item_id, scene_id, ..
            }
            | Edit::ItemSetVisible {
                item_id, scene_id, ..
            }
            | Edit::ItemSetOpacity {
                item_id, scene_id, ..
            } => match self.variant_target(item_id, scene_id.as_ref(), at) {
                Ok(Some(vi)) => Some(self.write_override(vi, item_id, edit, at)),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            },
            // Only a keyframe scene: an item's own effects take `filter.*` (ADR-0088).
            Edit::ItemSetEffects {
                scene_id, item_id, ..
            } => Some(match self.variant_target(item_id, Some(scene_id), at) {
                // Another scene that isn't a keyframe, with the graph's reason.
                Err(_)
                    if self
                        .items
                        .get(item_id)
                        .is_some_and(|i| &i.scene_id != scene_id)
                        && self
                            .scenes
                            .iter()
                            .any(|s| &s.id == scene_id && s.variant_of.is_none()) =>
                {
                    Err(invalid(
                        at,
                        "scene_id",
                        format!(
                            "scene `{scene_id}` is not a keyframe scene: name a keyframe \
                             scene whose tree holds item `{item_id}`"
                        ),
                    ))
                }
                Ok(Some(vi)) => self.write_override(vi, item_id, edit, at),
                Ok(None) => Err(invalid(
                    at,
                    "scene_id",
                    format!(
                        "scene `{scene_id}` is item `{item_id}`'s own scene: edit its own \
                             effects with `filter.*`, and name a keyframe scene here"
                    ),
                )),
                Err(error) => Err(error),
            }),
            _ => None,
        }
    }

    fn create_variant(
        &mut self,
        scene_id: &SceneId,
        variant_of: &SceneId,
        name: &str,
        at: usize,
    ) -> Result<Vec<Edit>, ProtocolError> {
        check_id(scene_id.as_str(), at, "scene_id")?;
        check_name(name, at)?;
        if self.scene_index(scene_id).is_some() {
            return Err(already_exists(EntityKind::Scene, scene_id.as_str()));
        }
        let parent = self.existing_scene(variant_of)?;
        if let Some(grandparent) = &self.scenes[parent].variant_of {
            return Err(invalid(
                at,
                "variant_of",
                format!(
                    "scene `{variant_of}` is itself a keyframe scene of `{grandparent}`; make \
                     the keyframe on `{grandparent}`"
                ),
            ));
        }
        self.scenes.push(super::Scene {
            id: scene_id.clone(),
            name: name.to_owned(),
            role: jstream_protocol::SceneRole::Scene,
            items: Vec::new(),
            folder: None,
            meta: Default::default(),
            filters: Default::default(),
            variant_of: Some(variant_of.clone()),
            overrides: Overrides::new(),
        });
        Ok(vec![Edit::SceneDelete {
            scene_id: scene_id.clone(),
        }])
    }

    fn clear_override(
        &mut self,
        scene_id: &SceneId,
        item_id: &ItemId,
        field: OverrideField,
        at: usize,
    ) -> Result<Vec<Edit>, ProtocolError> {
        let vi = self.existing_scene(scene_id)?;
        if !self.items.contains_key(item_id) {
            return Err(not_found(EntityKind::Item, item_id.as_str()));
        }
        let Some(parent) = self.scenes[vi].variant_of.clone() else {
            return Err(invalid(
                at,
                "scene_id",
                format!("scene `{scene_id}` is not a keyframe scene"),
            ));
        };
        self.check_parent_item(&parent, item_id, at, "item_id")?;
        let overrides = &mut self.scenes[vi].overrides;
        let Some(o) = overrides.get_mut(item_id) else {
            return Ok(Vec::new());
        };
        let before = o.clone();
        if before.only(field).is_empty() {
            return Ok(Vec::new());
        }
        o.clear(field);
        if o.is_empty() {
            overrides.remove(item_id);
        }
        Ok(before.only(field).write_edits(scene_id, item_id))
    }

    /// The keyframe scene a placement edit with `scene_id` writes an override in, as its
    /// index, or `None` when the edit changes the item itself (`scene_id` absent or the
    /// item's own scene). A scene that is neither is `invalid_argument`, and a missing
    /// one `not_found`.
    fn variant_target(
        &self,
        item_id: &ItemId,
        scene_id: Option<&SceneId>,
        at: usize,
    ) -> Result<Option<usize>, ProtocolError> {
        let own = &self
            .items
            .get(item_id)
            .ok_or_else(|| not_found(EntityKind::Item, item_id.as_str()))?
            .scene_id;
        let Some(scene_id) = scene_id.filter(|id| *id != own) else {
            return Ok(None);
        };
        let vi = self.existing_scene(scene_id)?;
        match &self.scenes[vi].variant_of {
            Some(parent) => {
                self.check_parent_item(parent, item_id, at, "scene_id")?;
                Ok(Some(vi))
            }
            None => Err(invalid(
                at,
                "scene_id",
                format!(
                    "item `{item_id}` is in scene `{own}`; name that scene or one of its \
                     keyframe scenes"
                ),
            )),
        }
    }

    /// Refuses an override of `item_id` in a keyframe scene of `parent` unless the item
    /// is in `parent`'s tree.
    fn check_parent_item(
        &self,
        parent: &SceneId,
        item_id: &ItemId,
        at: usize,
        name: &str,
    ) -> Result<(), ProtocolError> {
        if self.tree_items(parent).contains(item_id) {
            return Ok(());
        }
        Err(invalid(
            at,
            name,
            format!(
                "item `{item_id}` is neither an item of scene `{parent}`, the keyframe scene's \
                 parent, nor of a group placed in it"
            ),
        ))
    }

    /// The items in the tree of scene `root`: its own, and the items of every group
    /// (role `group`) placed in it through a `scene` source, at any depth.
    fn tree_items(&self, root: &SceneId) -> BTreeSet<ItemId> {
        let mut items = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut todo = vec![root.clone()];
        while let Some(scene_id) = todo.pop() {
            if !seen.insert(scene_id.clone()) {
                continue;
            }
            let Some(si) = self.scene_index(&scene_id) else {
                continue;
            };
            for item_id in &self.scenes[si].items {
                items.insert(item_id.clone());
                let group = self
                    .items
                    .get(item_id)
                    .and_then(|item| self.sources.get(&item.source_id))
                    .and_then(|source| match &source.kind {
                        SourceKind::Scene { scene_id } => Some(scene_id),
                        _ => None,
                    })
                    .filter(|id| {
                        self.scene_index(id)
                            .is_some_and(|gi| self.scenes[gi].role == SceneRole::Group)
                    });
                todo.extend(group.cloned());
            }
        }
        items
    }

    /// Writes the field `edit` sets into `item_id`'s override in the keyframe scene at
    /// `vi`, checked as the item's own field is, and returns the edit that undoes it.
    fn write_override(
        &mut self,
        vi: usize,
        item_id: &ItemId,
        edit: &Edit,
        at: usize,
    ) -> Result<Vec<Edit>, ProtocolError> {
        match edit {
            Edit::ItemSetGeometry { geometry, .. } => {
                super::geometry_view(geometry, at)?;
            }
            Edit::ItemSetOpacity { opacity, .. } => super::check_opacity(*opacity, at)?,
            Edit::ItemSetEffects { effects, .. } => check_effects(effects, at)?,
            _ => {}
        }
        let Some((written, field)) = Override::of_edit(edit) else {
            return Ok(Vec::new());
        };
        let scene = &mut self.scenes[vi];
        let o = scene.overrides.entry(item_id.clone()).or_default();
        let before = o.clone();
        o.merge(written);
        Ok(inverse(&scene.id, item_id, &before, field))
    }

    /// Drops every override whose item is no longer in its keyframe scene's tree
    /// (ADR-0040 §1), and returns the edits that write them back, to follow the edit's
    /// own inverse. The batch result doesn't report them in `dropped_overrides`: the
    /// mock leaves that empty.
    pub(super) fn drop_orphans(&mut self) -> Vec<Edit> {
        let trees: Vec<Option<BTreeSet<ItemId>>> = self
            .scenes
            .iter()
            .map(|scene| {
                let parent = scene.variant_of.as_ref()?;
                (!scene.overrides.is_empty()).then(|| self.tree_items(parent))
            })
            .collect();
        let mut restore = Vec::new();
        for (scene, tree) in self.scenes.iter_mut().zip(trees) {
            let Some(tree) = tree else {
                continue;
            };
            let gone: Vec<ItemId> = scene
                .overrides
                .keys()
                .filter(|item| !tree.contains(*item))
                .cloned()
                .collect();
            for item_id in gone {
                if let Some(o) = scene.overrides.remove(&item_id) {
                    restore.extend(o.write_edits(&scene.id, &item_id));
                }
            }
        }
        restore
    }

    /// Checks the `keyframe` filter owner as the graph does (ADR-0088), and while the
    /// keyframe scene follows the item's own effects copies them into its override.
    /// Returns whether it copied them: the edit's inverse is then `item.clear_override`.
    pub(super) fn copy_effects(
        &mut self,
        scene_id: &SceneId,
        item_id: &ItemId,
        at: usize,
    ) -> Result<bool, ProtocolError> {
        let vi = self.existing_scene(scene_id)?;
        let Some(item) = self.items.get(item_id) else {
            return Err(not_found(EntityKind::Item, item_id.as_str()));
        };
        let own = item.effects.clone();
        let Some(parent) = self.scenes[vi].variant_of.clone() else {
            return Err(invalid(
                at,
                "owner",
                format!(
                    "scene `{scene_id}` is not a keyframe scene: an item's own effects take \
                     the `item` owner"
                ),
            ));
        };
        self.check_parent_item(&parent, item_id, at, "owner")?;
        let o = self.scenes[vi]
            .overrides
            .entry(item_id.clone())
            .or_default();
        if o.effects.is_some() {
            return Ok(false);
        }
        o.effects = Some(own);
        Ok(true)
    }

    /// A keyframe scene's override of an item's effects, which [`Self::copy_effects`]
    /// made, to edit.
    pub(super) fn keyframe_effects(
        &mut self,
        scene_id: &SceneId,
        item_id: &ItemId,
    ) -> Option<&mut FilterChains> {
        self.scenes
            .iter_mut()
            .find(|s| s.id == *scene_id)?
            .overrides
            .get_mut(item_id)?
            .effects
            .as_mut()
    }

    /// The keyframe scenes of `scene_id`, as the blockers of its delete.
    pub(super) fn variant_blockers(&self, scene_id: &SceneId) -> Vec<Blocker> {
        self.scenes
            .iter()
            .filter(|s| s.variant_of.as_ref() == Some(scene_id))
            .map(|s| Blocker {
                entity: EntityKind::Scene,
                id: s.id.to_string(),
                field: "variant_of".into(),
            })
            .collect()
    }

    /// Refuses an edit that would give the keyframe scene at `si` items of its own.
    pub(super) fn check_not_variant(&self, si: usize, at: usize) -> Result<(), ProtocolError> {
        match &self.scenes[si].variant_of {
            Some(parent) => Err(invalid(
                at,
                "scene_id",
                format!(
                    "scene `{}` is a keyframe scene of `{parent}`: it shows its parent's \
                     items and has none of its own",
                    self.scenes[si].id
                ),
            )),
            None => Ok(()),
        }
    }

    /// Whether `scene_id` names a keyframe scene.
    pub(super) fn is_variant(&self, scene_id: &SceneId) -> bool {
        self.scene_index(scene_id)
            .is_some_and(|si| self.scenes[si].variant_of.is_some())
    }
}

/// The checks of an overridden effects list that need no kind descriptor, with the
/// graph's fields: its length, and each entry's ID, non-empty and unique.
fn check_effects(effects: &[FilterInfo], at: usize) -> Result<(), ProtocolError> {
    if effects.len() > MAX_CHAIN_FILTERS {
        return Err(invalid(
            at,
            "effects",
            format!("a chain holds at most {MAX_CHAIN_FILTERS} filters"),
        ));
    }
    let mut ids = BTreeSet::new();
    for (n, entry) in effects.iter().enumerate() {
        let field = format!("effects.{n}.filter_id");
        check_id(entry.filter_id.as_str(), at, &field)?;
        if !ids.insert(&entry.filter_id) {
            return Err(invalid(
                at,
                &field,
                format!("filter `{}` is in the list twice", entry.filter_id),
            ));
        }
    }
    Ok(())
}

/// A keyframe scene's overrides as `state.get` reports them, in item ID order.
pub(super) fn override_infos(overrides: &Overrides) -> Vec<ItemOverride> {
    overrides
        .iter()
        .map(|(item_id, o)| o.info(item_id))
        .collect()
}

/// The edits that write a deleted keyframe scene's overrides back, for its delete's
/// inverse.
pub(super) fn restore_overrides(scene_id: &SceneId, overrides: &Overrides) -> Vec<Edit> {
    overrides
        .iter()
        .flat_map(|(item_id, o)| o.write_edits(scene_id, item_id))
        .collect()
}
