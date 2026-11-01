//! Keyframe scenes (ADR-0024 §2, ADR-0040 §1, ADR-0049): creating one, writing and
//! clearing its overrides with their inverses, and dropping the overrides of an item that
//! leaves its parent. The override set itself is `jstream-anim`'s [`Keyframe`].
//!
//! A keyframe scene (a variant) has no items of its own. Its overrides name items in its
//! parent's tree: the parent's own, or a child of a group placed in the parent, at any
//! depth (ADR-0024 §2). An edit that takes an item out of that tree drops its overrides
//! ([`drop_orphans`], ADR-0040 §1).
//!
//! Besides the placement fields, an override may hold the item's effects, a whole list
//! that the keyframe draws in place of the item's own, written by `item.set_effects`
//! (ADR-0088). Each entry is checked as `filter.add` checks an item effect, and its
//! references are edges of the keyframe scene, so the write runs the cycle check.
//!
//! Docs: docs/adr/0049-keyframe-scene-model-and-evaluation.md, docs/adr/0024-scene-structure.md, docs/adr/0088-keyframe-scenes-override-item-effects.md, crates/jstream-graph/README.md#purpose

use crate::apply::{
    Kinds, already_exists, check_id, check_name, complete_settings_in, invalid, not_found,
};
use crate::deps::{self, Node};
use crate::model::{Chains, Filter, MAX_CHAIN_FILTERS, Overrides, Placement, Scene, State};
use jstream_anim::{Field, Inverse, Override, Value};
use jstream_protocol::{
    Crop, DroppedOverride, Edit, EntityKind, FilterInfo, Finite32, ItemId, ItemOverride,
    KindCategory, OverrideField, ProtocolError, Reference, SceneId, SceneMeta, SceneRole,
};
use std::collections::BTreeSet;

/// The value a keyframe scene stores for one field.
pub(crate) type FieldValue = Value<Placement, Crop, Chains>;

/// One item's override in a keyframe scene.
pub(crate) type ItemOverrides = Override<Placement, Crop, Chains>;

/// Applies the keyframe edits; `None` for any other edit.
pub(crate) fn apply(
    state: &mut State,
    kinds: &Kinds,
    edit: &Edit,
    at: usize,
) -> Option<Result<Vec<Edit>, ProtocolError>> {
    match edit {
        Edit::SceneCreateVariant {
            scene_id,
            variant_of,
            name,
        } => Some(create(state, scene_id, variant_of, name, at)),
        Edit::ItemClearOverride {
            scene_id,
            item_id,
            field,
        } => Some(clear(state, scene_id, item_id, *field, at)),
        Edit::ItemSetEffects {
            scene_id,
            item_id,
            effects,
        } => Some(set_effects(
            state, kinds, scene_id, item_id, effects, None, at,
        )),
        _ => None,
    }
}

/// Applies `item.set_effects` (ADR-0088): checks `effects` as `filter.add` checks an
/// item effect, writes them whole into the keyframe scene's override, and runs the
/// cycle check from the keyframe scene. `versions`, only when loading a collection
/// file, gives each entry's saved kind version: an entry whose kind this graph doesn't
/// describe is then kept verbatim, as a load keeps an item's own effect
/// ([`crate::filter::add_unknown_kind`]).
pub(crate) fn set_effects(
    state: &mut State,
    kinds: &Kinds,
    scene_id: &SceneId,
    item_id: &ItemId,
    effects: &[FilterInfo],
    versions: Option<&[Option<u32>]>,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let Some(vi) = target(state, item_id, Some(scene_id), at)? else {
        return Err(invalid(
            at,
            "scene_id",
            format!(
                "scene `{scene_id}` is item `{item_id}`'s own scene: edit its own effects \
                 with `filter.*`, and name a keyframe scene here"
            ),
        ));
    };
    let video = check_effects(state, kinds, effects, versions, at)?;
    let chains = Chains {
        video,
        audio: Vec::new(),
    };
    let inverse = write(state, vi, item_id, Value::Effects(chains), at)?;
    let from = Node::Scene(scene_id.clone());
    for (n, entry) in effects.iter().enumerate() {
        let stored = state.scenes[vi]
            .overrides
            .get(item_id)
            .and_then(|o| o.effects.as_ref())
            .and_then(|chains| chains.video.get(n));
        for (param, reference) in stored.into_iter().flat_map(|f| f.settings.references()) {
            if let Some(to) = deps::reference_node(&reference)
                && let Some(cycle) = deps::cycle(state, &from, &to)
            {
                return Err(invalid(
                    at,
                    &format!("effects.{n}.settings.{param}"),
                    format!(
                        "the reference to {to} in `{}` would create a cycle: {cycle}",
                        entry.filter_id
                    ),
                ));
            }
        }
    }
    Ok(inverse)
}

/// `effects` as an override stores them: each entry's ID checked and unique, its kind a
/// video filter kind, and its settings checked and completed against the kind, with the
/// field `effects.<n>.<field>`. At most [`MAX_CHAIN_FILTERS`], as an item's own chain.
fn check_effects(
    state: &State,
    kinds: &Kinds,
    effects: &[FilterInfo],
    versions: Option<&[Option<u32>]>,
    at: usize,
) -> Result<Vec<Filter>, ProtocolError> {
    if effects.len() > MAX_CHAIN_FILTERS {
        return Err(invalid(
            at,
            "effects",
            format!("a chain holds at most {MAX_CHAIN_FILTERS} filters"),
        ));
    }
    let mut ids = BTreeSet::new();
    let mut out = Vec::with_capacity(effects.len());
    for (n, entry) in effects.iter().enumerate() {
        let field = |name: &str| format!("effects.{n}.{name}");
        check_id(entry.filter_id.as_str(), at, &field("filter_id"))?;
        if !ids.insert(&entry.filter_id) {
            return Err(invalid(
                at,
                &field("filter_id"),
                format!("filter `{}` is in the list twice", entry.filter_id),
            ));
        }
        let (settings, unknown_kind_version) = match kinds.get(&entry.kind) {
            Some(descriptor) if descriptor.category == KindCategory::VideoFilter => {
                let settings = complete_settings_in(
                    state,
                    kinds,
                    descriptor,
                    None,
                    &entry.settings,
                    at,
                    &field("settings"),
                )?;
                (settings, None)
            }
            Some(_) => {
                return Err(invalid(
                    at,
                    &field("kind"),
                    format!("kind `{}` is not a video filter kind", entry.kind),
                ));
            }
            // A load keeps a filter whose kind is missing, verbatim (ADR-0025).
            None if versions.is_some() => {
                check_references_exist(state, &entry.settings, at, &field("settings"))?;
                let version = versions.and_then(|v| v.get(n).copied().flatten());
                (entry.settings.clone(), version)
            }
            None => {
                return Err(invalid(
                    at,
                    &field("kind"),
                    format!("there is no kind `{}`; `kind.list` names them", entry.kind),
                ));
            }
        };
        out.push(Filter {
            id: entry.filter_id.clone(),
            kind: entry.kind.clone(),
            enabled: entry.enabled,
            settings,
            unknown_kind_version,
        });
    }
    Ok(out)
}

/// Refuses a reference in unchecked settings to something that doesn't exist, so
/// deletes and the cycle check see a whole graph, as a load of an item's own effect
/// does.
fn check_references_exist(
    state: &State,
    settings: &jstream_protocol::Settings,
    at: usize,
    prefix: &str,
) -> Result<(), ProtocolError> {
    for (param, reference) in settings.references() {
        let exists = match &reference {
            Reference::Scene { scene_id } => state.scene(scene_id).is_some(),
            Reference::Source { source_id } => state.source(source_id).is_some(),
            Reference::Bus { bus_id } => state.bus(bus_id).is_some(),
            _ => true,
        };
        if !exists {
            return Err(invalid(
                at,
                &format!("{prefix}.{param}"),
                "names something that doesn't exist",
            ));
        }
    }
    Ok(())
}

fn create(
    state: &mut State,
    scene_id: &SceneId,
    variant_of: &SceneId,
    name: &str,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    check_id(scene_id.as_str(), at, "scene_id")?;
    check_name(name, at)?;
    if state.scene_index(scene_id).is_some() {
        return Err(already_exists(EntityKind::Scene, scene_id.as_str()));
    }
    let parent = state
        .scene(variant_of)
        .ok_or_else(|| not_found(EntityKind::Scene, variant_of.as_str()))?;
    if let Some(grandparent) = &parent.variant_of {
        return Err(invalid(
            at,
            "variant_of",
            format!(
                "scene `{variant_of}` is itself a keyframe scene of `{grandparent}`; make the \
                 keyframe on `{grandparent}`"
            ),
        ));
    }
    state.scenes.push(Scene {
        id: scene_id.clone(),
        name: name.to_owned(),
        role: SceneRole::Scene,
        transition_in: None,
        transition_out: None,
        folder: None,
        meta: SceneMeta::default(),
        items: Vec::new(),
        variant_of: Some(variant_of.clone()),
        overrides: Overrides::new(),
        filters: Default::default(),
    });
    Ok(vec![Edit::SceneDelete {
        scene_id: scene_id.clone(),
    }])
}

fn clear(
    state: &mut State,
    scene_id: &SceneId,
    item_id: &ItemId,
    field: OverrideField,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let vi = state
        .scene_index(scene_id)
        .ok_or_else(|| not_found(EntityKind::Scene, scene_id.as_str()))?;
    if state.item_index(item_id).is_none() {
        return Err(not_found(EntityKind::Item, item_id.as_str()));
    }
    let Some(parent) = state.scenes[vi].variant_of.clone() else {
        return Err(invalid(
            at,
            "scene_id",
            format!("scene `{scene_id}` is not a keyframe scene"),
        ));
    };
    check_parent_item(state, &parent, item_id, at, "item_id")?;
    let inverse = state.scenes[vi].overrides.clear(item_id, field_of(field));
    Ok(inverse
        .into_iter()
        .map(|inverse| inverse_edit(scene_id, item_id, inverse))
        .collect())
}

/// The keyframe scene a placement edit with `scene_id` writes an override in, as its
/// index, or `None` when the edit changes the item itself (`scene_id` absent or the
/// item's own scene). A scene that is neither the item's own nor a keyframe scene whose
/// tree holds the item (its scene's, or one whose parent reaches the item's group, at
/// any depth) is `invalid_argument`, and a missing one `not_found`.
pub(crate) fn target(
    state: &State,
    item_id: &ItemId,
    scene_id: Option<&SceneId>,
    at: usize,
) -> Result<Option<usize>, ProtocolError> {
    let (si, _) = state
        .item_index(item_id)
        .ok_or_else(|| not_found(EntityKind::Item, item_id.as_str()))?;
    let Some(scene_id) = scene_id.filter(|id| **id != state.scenes[si].id) else {
        return Ok(None);
    };
    let vi = state
        .scene_index(scene_id)
        .ok_or_else(|| not_found(EntityKind::Scene, scene_id.as_str()))?;
    match &state.scenes[vi].variant_of {
        Some(parent) => {
            check_parent_item(state, parent, item_id, at, "scene_id")?;
            Ok(Some(vi))
        }
        None => Err(invalid(
            at,
            "scene_id",
            format!(
                "item `{item_id}` is in scene `{}`; name that scene or a keyframe scene \
                 whose tree holds it",
                state.scenes[si].id
            ),
        )),
    }
}

/// The keyframe scene, as its index, whose override of `item_id`'s effects a filter edit
/// on the `keyframe` owner addresses (ADR-0088): `not_found` for a missing scene or
/// item, and `invalid_argument` on `owner` for a scene that isn't a keyframe scene or
/// whose tree doesn't hold the item.
pub(crate) fn effects_target(
    state: &State,
    scene_id: &SceneId,
    item_id: &ItemId,
    at: usize,
) -> Result<usize, ProtocolError> {
    let vi = state
        .scene_index(scene_id)
        .ok_or_else(|| not_found(EntityKind::Scene, scene_id.as_str()))?;
    if state.item_index(item_id).is_none() {
        return Err(not_found(EntityKind::Item, item_id.as_str()));
    }
    let Some(parent) = state.scenes[vi].variant_of.clone() else {
        return Err(invalid(
            at,
            "owner",
            format!(
                "scene `{scene_id}` is not a keyframe scene: an item's own effects take the \
                 `item` owner"
            ),
        ));
    };
    check_parent_item(state, &parent, item_id, at, "owner")?;
    Ok(vi)
}

/// Refuses an override of `item_id` in a keyframe scene of `parent` unless the item is
/// in `parent`'s tree: one of its own, or a child of a group placed in it.
fn check_parent_item(
    state: &State,
    parent: &SceneId,
    item_id: &ItemId,
    at: usize,
    name: &str,
) -> Result<(), ProtocolError> {
    if state.tree_items(parent).contains(item_id) {
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

/// Writes `value` into the override of `item_id` in the keyframe scene at `vi`, and
/// returns the edit that undoes it (ADR-0049 §2).
pub(crate) fn write(
    state: &mut State,
    vi: usize,
    item_id: &ItemId,
    value: FieldValue,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let scene = &mut state.scenes[vi];
    let inverse = scene
        .overrides
        .write(item_id.clone(), value)
        .map_err(|problem| invalid(at, problem.field.name(), problem.reason))?;
    Ok(vec![inverse_edit(&scene.id, item_id, inverse)])
}

/// Drops every override that names an item no longer in its keyframe scene's tree: an
/// edit took the item, or the group holding it, out of the parent (ADR-0040 §1). An
/// override stays while any path still reaches its item. Reports each in `dropped` and
/// returns the edits that write them back, to follow the edit's own inverse.
pub(crate) fn drop_orphans(state: &mut State, dropped: &mut Vec<DroppedOverride>) -> Vec<Edit> {
    let orphaned: Vec<(usize, Vec<ItemId>)> = state
        .scenes
        .iter()
        .enumerate()
        .filter_map(|(vi, scene)| {
            let parent = scene.variant_of.as_ref()?;
            if scene.overrides.is_empty() {
                return None;
            }
            let tree = state.tree_items(parent);
            let gone: Vec<ItemId> = scene
                .overrides
                .iter()
                .map(|(item, _)| item)
                .filter(|item| !tree.contains(item))
                .cloned()
                .collect();
            (!gone.is_empty()).then_some((vi, gone))
        })
        .collect();
    let mut restore = Vec::new();
    for (vi, gone) in orphaned {
        let scene = &mut state.scenes[vi];
        for (item, o) in scene.overrides.drop_unless(|item| !gone.contains(item)) {
            restore.extend(write_edits(&scene.id, &item, &o));
            dropped.push(DroppedOverride {
                scene_id: scene.id.clone(),
                dropped: to_info(&item, &o),
            });
        }
    }
    restore
}

/// The edits addressed to `scene_id` that write `o`, one per field it sets, in
/// [`Field::ALL`] order: what restores it.
pub(crate) fn write_edits(scene_id: &SceneId, item_id: &ItemId, o: &ItemOverrides) -> Vec<Edit> {
    o.values()
        .map(|value| value_edit(scene_id, item_id, value))
        .collect()
}

/// `o` for `item_id`, as `state.get` reports it.
pub(crate) fn to_info(item_id: &ItemId, o: &ItemOverrides) -> ItemOverride {
    ItemOverride {
        item_id: item_id.clone(),
        geometry: o.geometry.and_then(|placement| match placement {
            Placement::Geometry(geometry) => Some(geometry),
            Placement::Legacy(_) => None,
        }),
        transform: o.geometry.and_then(|placement| match placement {
            Placement::Legacy(transform) => Some(transform),
            Placement::Geometry(_) => None,
        }),
        crop: o.crop,
        visible: o.visible,
        opacity: o.opacity.and_then(Finite32::new),
        effects: o.effects.as_ref().map(|chains| effects_info(&chains.video)),
    }
}

/// An overridden effects list as the wire reports it, and as `item.set_effects` writes
/// it back.
pub(crate) fn effects_info(filters: &[Filter]) -> Vec<FilterInfo> {
    filters
        .iter()
        .map(|filter| FilterInfo {
            filter_id: filter.id.clone(),
            kind: filter.kind.clone(),
            enabled: filter.enabled,
            settings: filter.settings.clone(),
        })
        .collect()
}

fn inverse_edit(
    scene_id: &SceneId,
    item_id: &ItemId,
    inverse: Inverse<Placement, Crop, Chains>,
) -> Edit {
    match inverse {
        Inverse::Write(value) => value_edit(scene_id, item_id, value),
        Inverse::Clear(field) => Edit::ItemClearOverride {
            scene_id: scene_id.clone(),
            item_id: item_id.clone(),
            field: wire_field(field),
        },
    }
}

/// The edit addressed to the keyframe scene `keyframe` that writes `value`: a placement
/// edit, or `item.set_effects`.
fn value_edit(keyframe: &SceneId, item_id: &ItemId, value: FieldValue) -> Edit {
    let item_id = item_id.clone();
    let scene_id = Some(keyframe.clone());
    match value {
        Value::Geometry(Placement::Geometry(geometry)) => Edit::ItemSetGeometry {
            item_id,
            scene_id,
            geometry,
        },
        Value::Geometry(Placement::Legacy(transform)) => Edit::ItemSetTransform {
            item_id,
            scene_id,
            transform,
        },
        Value::Crop(crop) => Edit::ItemSetCrop {
            item_id,
            scene_id,
            crop,
        },
        Value::Visible(visible) => Edit::ItemSetVisible {
            item_id,
            scene_id,
            visible,
        },
        Value::Opacity(opacity) => Edit::ItemSetOpacity {
            item_id,
            scene_id,
            // Stored only after the check that it is finite and in [0, 1].
            opacity: Finite32::new(opacity).unwrap_or(Finite32::ONE),
        },
        Value::Effects(chains) => Edit::ItemSetEffects {
            scene_id: keyframe.clone(),
            item_id,
            effects: effects_info(&chains.video),
        },
    }
}

fn field_of(field: OverrideField) -> Field {
    match field {
        OverrideField::Geometry => Field::Geometry,
        OverrideField::Crop => Field::Crop,
        OverrideField::Visible => Field::Visible,
        OverrideField::Opacity => Field::Opacity,
        OverrideField::Effects => Field::Effects,
    }
}

fn wire_field(field: Field) -> OverrideField {
    match field {
        Field::Geometry => OverrideField::Geometry,
        Field::Crop => OverrideField::Crop,
        Field::Visible => OverrideField::Visible,
        Field::Opacity => OverrideField::Opacity,
        Field::Effects => OverrideField::Effects,
    }
}
