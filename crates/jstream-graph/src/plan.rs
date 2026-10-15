//! `delete.plan`: the batch that deletes a scene, source, folder or label together with
//! everything that refers to it ([ADR-0025](../../../docs/adr/0025-typed-settings-kind-descriptors.md)).
//!
//! Rejection stays the only delete: `scene.delete` and `source.delete` refuse while
//! anything refers to their target. The plan spells out the removal as ordinary edits,
//! so the client can show it, and its undo is the batch's normal inverse.

use crate::deps::{self, Node};
use crate::model::Graph;
use jstream_protocol::{
    DeleteTarget, Edit, EntityKind, ItemId, ProtocolError, QueryResult, Settings, SourceId,
    SourceKind,
};
use serde_json::Value;
use std::collections::BTreeMap;

impl Graph {
    /// The `delete.plan` result for `target`: the edits that delete it and its blockers,
    /// at the current revision. Applied as one batch with that `expected_rev`, they
    /// succeed. Fails with `not_found` if the target doesn't exist.
    ///
    /// Deleting a scene also deletes the `scene` sources that show it. Deleting a source
    /// never deletes a scene, so the cascade stops there. The edits are, in order:
    /// - `item.remove` for each item that places a deleted source;
    /// - one `source.set_settings` per remaining source whose settings refer to a deleted
    ///   object, setting each such parameter to `null`;
    /// - `source.delete` for each `scene` source that shows a deleted scene;
    /// - the delete of the target.
    ///
    /// A folder's plan moves the scenes and folders in it up to its parent
    /// (`scene.set_folder`, then `folder.set_parent`), then deletes it. A label's plan
    /// takes it off each source carrying it (`source.set_meta`), then deletes it.
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
                });
            }
            DeleteTarget::Label { label_id } => {
                return Ok(QueryResult::DeletePlan {
                    rev: self.rev,
                    edits: crate::organise::label_plan(state, label_id)?,
                });
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
        if let Node::Scene(scene_id) = &target {
            for source in state.sources() {
                if matches!(&source.kind, SourceKind::Scene { scene_id: s } if s == scene_id) {
                    shown_by.push(source.id.clone());
                    deleted.push(Node::Source(source.id.clone()));
                }
            }
        }

        let mut removes: Vec<ItemId> = Vec::new();
        let mut clears: BTreeMap<SourceId, Settings> = BTreeMap::new();
        for node in &deleted {
            for blocker in deps::blockers(state, node) {
                match (blocker.entity, blocker.field.strip_prefix("settings.")) {
                    (EntityKind::Item, _) => removes.push(ItemId::new(blocker.id)),
                    (EntityKind::Source, Some(param)) => {
                        let source_id = SourceId::new(blocker.id);
                        // A source the batch deletes anyway needs no clearing.
                        if !deleted.contains(&Node::Source(source_id.clone())) {
                            let param = param.to_owned();
                            clears
                                .entry(source_id)
                                .or_default()
                                .insert(param, Value::Null);
                        }
                    }
                    // `kind.scene_id`: a `scene` source, already in `shown_by`.
                    _ => {}
                }
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
            shown_by
                .into_iter()
                .map(|source_id| Edit::SourceDelete { source_id }),
        );
        edits.push(last);
        Ok(QueryResult::DeletePlan {
            rev: self.rev,
            edits,
        })
    }
}

fn not_found(entity: EntityKind, id: &str) -> ProtocolError {
    ProtocolError::NotFound {
        entity,
        id: id.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_protocol::{Rev, SceneId};

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
            })
        );
    }
}
