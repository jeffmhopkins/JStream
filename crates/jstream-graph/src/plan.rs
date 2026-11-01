//! `delete.plan`: the batch that deletes a scene, source, bus, folder or label together
//! with everything that refers to it ([ADR-0025](../../../docs/adr/0025-typed-settings-kind-descriptors.md)).
//!
//! Rejection stays the only delete: `scene.delete`, `source.delete` and `bus.delete`
//! refuse while anything refers to their target. The plan spells out the removal as ordinary edits,
//! so the client can show it, and its undo is the batch's normal inverse.
//! Hotkey bindings are not blockers: the deletes remove those that name their targets
//! (ADR-0040 §3), and the plan lists them.

use crate::deps::{self, Node};
use crate::model::Graph;
use jstream_protocol::{
    DeleteTarget, Edit, EntityKind, FilterId, FilterOwner, HotkeyBinding, ItemId, MixInput,
    ProtocolError, QueryResult, SceneId, Settings, SourceId, SourceKind, VideoMix, VideoMixId,
};
use serde_json::Value;
use std::collections::BTreeMap;

impl Graph {
    /// The `delete.plan` result for `target`: the edits that delete it and its blockers,
    /// at the current revision. Applied as one batch with that `expected_rev`, they
    /// succeed. Fails with `not_found` if the target doesn't exist.
    ///
    /// Deleting a scene also deletes its keyframe scenes and the `scene` sources that show
    /// it or them. Deleting a source
    /// never deletes a scene, so the cascade stops there. A bus is held only by settings
    /// values, which the plan clears; `bus.delete` takes its sends itself. The edits are, in order:
    /// - `item.remove` for each item that places a deleted source;
    /// - one `source.set_settings` per remaining source whose settings refer to a deleted
    ///   object, setting each such parameter to `null`;
    /// - one `filter.set_settings` per remaining source filter whose settings do,
    ///   likewise, then one per remaining scene filter and item effect (ADR-0065 §3), in
    ///   scene order;
    /// - one `item.set_effects` per remaining keyframe scene's override whose effects do
    ///   (ADR-0088), in scene order and then item ID order, with each such parameter
    ///   `null`;
    /// - `source.delete` for each `scene` source that shows a deleted scene;
    /// - `video_mix.update` for each video mix that shows a deleted scene, keeping its
    ///   name, resolution and rate and showing the program instead;
    /// - `scene.delete` for each keyframe scene of a deleted scene;
    /// - the delete of the target.
    ///
    /// It also lists, ordered by chord, the hotkey bindings those deletes remove.
    ///
    /// A folder's plan moves the scenes and folders in it up to its parent
    /// (`scene.set_folder`, then `folder.set_parent`), then deletes it. A label's plan
    /// takes it off each source carrying it (`source.set_meta`), then deletes it. No
    /// action names a folder or label, so neither removes a hotkey binding.
    pub fn delete_plan(&self, target: &DeleteTarget) -> Result<QueryResult, ProtocolError> {
        let state = &self.state;
        let (target, last) = match target {
            DeleteTarget::Scene { scene_id } => {
                if state.scene(scene_id).is_none() {
                    return Err(not_found(EntityKind::Scene, scene_id.as_str()));
                }
                (
                    Node::Scene(scene_id.clone()),
                    Edit::SceneDelete {
                        scene_id: scene_id.clone(),
                    },
                )
            }
            DeleteTarget::Source { source_id } => {
                if state.source(source_id).is_none() {
                    return Err(not_found(EntityKind::Source, source_id.as_str()));
                }
                (
                    Node::Source(source_id.clone()),
                    Edit::SourceDelete {
                        source_id: source_id.clone(),
                    },
                )
            }
            DeleteTarget::Folder { folder_id } => {
                return Ok(QueryResult::DeletePlan {
                    rev: self.rev,
                    edits: crate::organise::folder_plan(state, folder_id)?,
                    hotkeys: Vec::new(),
                });
            }
            DeleteTarget::Label { label_id } => {
                return Ok(QueryResult::DeletePlan {
                    rev: self.rev,
                    edits: crate::organise::label_plan(state, label_id)?,
                    hotkeys: Vec::new(),
                });
            }
            DeleteTarget::Bus { bus_id } => {
                if state.bus(bus_id).is_none() {
                    return Err(not_found(EntityKind::Bus, bus_id.as_str()));
                }
                if bus_id.is_master() {
                    return Err(ProtocolError::invalid_argument(
                        "target.bus_id",
                        "the master bus can't be deleted",
                    ));
                }
                (
                    Node::Bus(bus_id.clone()),
                    Edit::BusDelete {
                        bus_id: bus_id.clone(),
                    },
                )
            }
            _ => {
                return Err(ProtocolError::invalid_argument(
                    "target",
                    "this kind of target can't be planned",
                ));
            }
        };
        // What the batch deletes: the target, and for a scene the `scene` sources that
        // show it. Those are leaves: deleting a source deletes nothing further.
        let mut deleted = vec![target.clone()];
        let mut shown_by = Vec::new();
        let mut variants = Vec::new();
        if let Node::Scene(scene_id) = &target {
            // A keyframe scene goes with its parent (ADR-0024 §2): it has no items of its
            // own, so it is a leaf like a `scene` source.
            variants.extend(state.variants_of(scene_id).map(|v| v.id.clone()));
            let scenes: Vec<SceneId> = std::iter::once(scene_id.clone())
                .chain(variants.iter().cloned())
                .collect();
            deleted.extend(variants.iter().cloned().map(Node::Scene));
            for source in state.sources() {
                if matches!(&source.kind, SourceKind::Scene { scene_id: s } if scenes.contains(s)) {
                    shown_by.push(source.id.clone());
                    deleted.push(Node::Source(source.id.clone()));
                }
            }
        }

        let mut removes: Vec<ItemId> = Vec::new();
        let mut repoints: Vec<VideoMixId> = Vec::new();
        let mut clears: BTreeMap<SourceId, Settings> = BTreeMap::new();
        let mut filter_clears: BTreeMap<(SourceId, FilterId), Settings> = BTreeMap::new();
        // A scene filter's or an item effect's reference (ADR-0065 §3), cleared after the
        // item removes, in blocker order.
        let mut owner_clears: Vec<(FilterOwner, FilterId, String)> = Vec::new();
        for node in &deleted {
            for blocker in deps::blockers(state, node) {
                let owned = match blocker.entity {
                    EntityKind::Scene => blocker.field.strip_prefix("filters."),
                    EntityKind::Item => blocker.field.strip_prefix("effects."),
                    _ => None,
                };
                if let Some((filter, param)) = owned.and_then(|rest| rest.rsplit_once('.')) {
                    owner_clears.push((
                        owner_of(&blocker),
                        FilterId::new(filter),
                        param.to_owned(),
                    ));
                    continue;
                }
                if blocker.entity == EntityKind::Item {
                    removes.push(ItemId::new(blocker.id));
                    continue;
                }
                if blocker.entity == EntityKind::VideoMix {
                    repoints.push(VideoMixId::new(blocker.id));
                    continue;
                }
                // A keyframe scene, already in `variants`.
                if blocker.entity == EntityKind::Scene {
                    continue;
                }
                let source_id = SourceId::new(blocker.id);
                // A source the batch deletes anyway needs no clearing.
                if deleted.contains(&Node::Source(source_id.clone())) {
                    continue;
                }
                if let Some(param) = blocker.field.strip_prefix("settings.") {
                    clears
                        .entry(source_id)
                        .or_default()
                        .insert(param.to_owned(), Value::Null);
                } else if let Some((filter, param)) = blocker
                    .field
                    .strip_prefix("filters.")
                    .and_then(|rest| rest.rsplit_once('.'))
                {
                    filter_clears
                        .entry((source_id, FilterId::new(filter)))
                        .or_default()
                        .insert(param.to_owned(), Value::Null);
                }
                // `kind.scene_id`: a `scene` source, already in `shown_by`.
            }
        }

        let mut edits: Vec<Edit> = removes
            .into_iter()
            .map(|item_id| Edit::ItemRemove { item_id })
            .collect();
        edits.extend(
            clears
                .into_iter()
                .map(|(source_id, patch)| Edit::SourceSetSettings { source_id, patch }),
        );
        edits.extend(
            filter_clears
                .into_iter()
                .map(|((source_id, filter_id), patch)| Edit::FilterSetSettings {
                    owner: FilterOwner::Source { source_id },
                    filter_id,
                    patch,
                }),
        );
        // A scene the batch deletes, or an item it removes, needs no clearing; nor does
        // an item in a scene it deletes.
        let gone_scenes: Vec<&SceneId> = deleted
            .iter()
            .filter_map(|node| match node {
                Node::Scene(id) => Some(id),
                _ => None,
            })
            .collect();
        let mut patches: Vec<(FilterOwner, FilterId, Settings)> = Vec::new();
        for (owner, filter_id, param) in owner_clears {
            let gone = match &owner {
                FilterOwner::Scene { scene_id } => gone_scenes.contains(&scene_id),
                FilterOwner::Item { item_id } => edits.iter().any(
                    |e| matches!(e, Edit::ItemRemove { item_id: removed } if removed == item_id),
                ) || state
                    .item_index(item_id)
                    .is_some_and(|(si, _)| gone_scenes.contains(&&state.scenes()[si].id)),
                _ => false,
            };
            if gone {
                continue;
            }
            match patches
                .iter_mut()
                .find(|(o, f, _)| *o == owner && *f == filter_id)
            {
                Some((_, _, patch)) => {
                    patch.insert(param, Value::Null);
                }
                None => {
                    let mut patch = Settings::new();
                    patch.insert(param, Value::Null);
                    patches.push((owner, filter_id, patch));
                }
            }
        }
        edits.extend(patches.into_iter().map(|(owner, filter_id, patch)| {
            Edit::FilterSetSettings {
                owner,
                filter_id,
                patch,
            }
        }));
        // A keyframe scene's overridden effects (ADR-0088): the whole list again, with
        // each reference to a deleted object `null`. Not for a keyframe scene the batch
        // deletes, nor an item it removes, whose overrides go with it.
        for scene in state.scenes() {
            if scene.variant_of.is_none() || variants.contains(&scene.id) {
                continue;
            }
            for (item_id, o) in scene.overrides.iter() {
                let Some(filters) = o.effects.as_ref().map(|chains| &chains.video) else {
                    continue;
                };
                let removed = edits
                    .iter()
                    .any(|e| matches!(e, Edit::ItemRemove { item_id: gone } if gone == item_id));
                if removed {
                    continue;
                }
                let mut changed = false;
                let mut effects = crate::keyframe::effects_info(filters);
                for effect in &mut effects {
                    let named: Vec<String> = effect
                        .settings
                        .references()
                        .filter(|(_, r)| {
                            deps::reference_node(r).is_some_and(|node| deleted.contains(&node))
                        })
                        .map(|(param, _)| param.to_owned())
                        .collect();
                    for param in named {
                        effect.settings.insert(param, Value::Null);
                        changed = true;
                    }
                }
                if changed {
                    edits.push(Edit::ItemSetEffects {
                        scene_id: scene.id.clone(),
                        item_id: item_id.clone(),
                        effects,
                    });
                }
            }
        }
        edits.extend(
            shown_by
                .into_iter()
                .map(|source_id| Edit::SourceDelete { source_id }),
        );
        edits.extend(repoints.into_iter().filter_map(|mix_id| {
            let current = state.video_mix(&mix_id)?;
            Some(Edit::VideoMixUpdate {
                name: current.name.clone(),
                mix: VideoMix {
                    input: MixInput::Program,
                    ..current.mix.clone()
                },
                mix_id,
            })
        }));
        edits.extend(
            variants
                .into_iter()
                .map(|scene_id| Edit::SceneDelete { scene_id }),
        );
        edits.push(last);
        let hotkeys = state
            .hotkeys()
            .filter(|(_, action)| {
                action.named_objects().iter().any(|&(entity, id)| {
                    deleted.iter().any(|node| match node {
                        Node::Scene(s) => entity == EntityKind::Scene && s.as_str() == id,
                        Node::Source(s) => entity == EntityKind::Source && s.as_str() == id,
                        // No action names a bus, so no binding can.
                        Node::Bus(_) => false,
                    })
                })
            })
            .map(|(chord, action)| HotkeyBinding::new(chord.clone(), action))
            .collect();
        Ok(QueryResult::DeletePlan {
            rev: self.rev,
            edits,
            hotkeys,
        })
    }
}

fn not_found(entity: EntityKind, id: &str) -> ProtocolError {
    ProtocolError::NotFound {
        entity,
        id: id.to_owned(),
    }
}

/// The filter owner a scene-filter or item-effect blocker names.
fn owner_of(blocker: &jstream_protocol::Blocker) -> FilterOwner {
    match blocker.entity {
        EntityKind::Scene => FilterOwner::Scene {
            scene_id: SceneId::new(&blocker.id),
        },
        _ => FilterOwner::Item {
            item_id: ItemId::new(&blocker.id),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_protocol::Rev;

    #[test]
    fn a_missing_target_is_not_found() {
        let graph = Graph::new();
        assert_eq!(
            graph.delete_plan(&DeleteTarget::Scene {
                scene_id: SceneId::new("nope"),
            }),
            Err(not_found(EntityKind::Scene, "nope"))
        );
        assert_eq!(
            graph.delete_plan(&DeleteTarget::Source {
                source_id: SourceId::new("nope"),
            }),
            Err(not_found(EntityKind::Source, "nope"))
        );
    }

    #[test]
    fn an_unreferenced_target_plans_just_its_delete() {
        let mut graph = Graph::new();
        graph
            .apply(
                &[Edit::SceneCreate {
                    scene_id: SceneId::new("a"),
                    name: "A".into(),
                    role: None,
                }],
                None,
            )
            .expect("applies");
        assert_eq!(
            graph.delete_plan(&DeleteTarget::Scene {
                scene_id: SceneId::new("a"),
            }),
            Ok(QueryResult::DeletePlan {
                rev: Rev(1),
                edits: vec![Edit::SceneDelete {
                    scene_id: SceneId::new("a"),
                }],
                hotkeys: vec![],
            })
        );
    }
}
