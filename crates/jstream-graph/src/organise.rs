//! Folders, labels and organisation metadata (ADR-0024 §3 and §4): the `folder.*` and
//! `label.*` edits, `scene.set_folder`, `scene.set_meta` and `source.set_meta`.
//!
//! None of it changes rendering, switching or nesting, so none of it is a dependency
//! edge. A folder or label can't be deleted while something sits in or carries it; the
//! blockers are listed here, for the delete check and for `delete.plan`.

use crate::apply::{
    already_exists, blocked, check_id, check_index, check_name, invalid, not_found, wire_index,
};
use crate::model::{Folder, Label, Source, State};
use jstream_protocol::{
    Blocker, Edit, EntityKind, FolderId, LabelId, ProtocolError, SceneId, SceneMeta, SourceId,
    SourceMeta, check_color, check_icon,
};
use std::collections::BTreeSet;

/// Applies the organisation edits and returns the edits that undo them. `None` for any
/// other edit.
pub(crate) fn apply(
    state: &mut State,
    edit: &Edit,
    at: usize,
) -> Option<Result<Vec<Edit>, ProtocolError>> {
    Some(match edit {
        Edit::FolderCreate {
            folder_id,
            name,
            parent,
        } => create_folder(state, folder_id, name, parent.as_ref(), at),
        Edit::FolderRename { folder_id, name } => rename_folder(state, folder_id, name, at),
        Edit::FolderSetParent { folder_id, parent } => {
            set_parent(state, folder_id, parent.as_ref(), at)
        }
        Edit::FolderReorder { folder_id, index } => reorder(
            &mut state.folders,
            |f| &f.id == folder_id,
            *index,
            at,
            "folders",
            || not_found(EntityKind::Folder, folder_id.as_str()),
        )
        .map(|from| {
            vec![Edit::FolderReorder {
                folder_id: folder_id.clone(),
                index: from,
            }]
        }),
        Edit::FolderDelete { folder_id } => delete_folder(state, folder_id, at),
        Edit::SceneSetFolder {
            scene_id,
            folder_id,
        } => set_folder(state, scene_id, folder_id.as_ref()),
        Edit::SceneSetMeta { scene_id, meta } => set_scene_meta(state, scene_id, meta, at),
        Edit::LabelCreate {
            label_id,
            name,
            color,
            icon,
        } => create_label(state, label_id, name, color, icon.as_deref(), at),
        Edit::LabelUpdate {
            label_id,
            name,
            color,
            icon,
        } => update_label(state, label_id, name, color, icon.as_deref(), at),
        Edit::LabelReorder { label_id, index } => reorder(
            &mut state.labels,
            |l| &l.id == label_id,
            *index,
            at,
            "labels",
            || not_found(EntityKind::Label, label_id.as_str()),
        )
        .map(|from| {
            vec![Edit::LabelReorder {
                label_id: label_id.clone(),
                index: from,
            }]
        }),
        Edit::LabelDelete { label_id } => delete_label(state, label_id, at),
        Edit::SourceSetMeta { source_id, meta } => set_source_meta(state, source_id, meta, at),
        _ => return None,
    })
}

/// A source's metadata in its protocol form.
pub(crate) fn source_meta(source: &Source) -> SourceMeta {
    SourceMeta {
        labels: source.labels.iter().cloned().collect(),
        favourite: source.favourite,
    }
}

/// Everything that stops deleting folder `id`: the scenes filed in it (display order),
/// then the folders inside it (folder order).
pub(crate) fn folder_blockers(state: &State, id: &FolderId) -> Vec<Blocker> {
    let scenes = state
        .scenes
        .iter()
        .filter(|s| s.folder.as_ref() == Some(id))
        .map(|s| Blocker {
            entity: EntityKind::Scene,
            id: s.id.as_str().to_owned(),
            field: "folder".into(),
        });
    let folders = state
        .folders
        .iter()
        .filter(|f| f.parent.as_ref() == Some(id))
        .map(|f| Blocker {
            entity: EntityKind::Folder,
            id: f.id.as_str().to_owned(),
            field: "parent".into(),
        });
    scenes.chain(folders).collect()
}

/// Everything that stops deleting label `id`: the sources carrying it, by ID.
pub(crate) fn label_blockers(state: &State, id: &LabelId) -> Vec<Blocker> {
    state
        .sources()
        .filter(|s| s.labels.contains(id))
        .map(|s| Blocker {
            entity: EntityKind::Source,
            id: s.id.as_str().to_owned(),
            field: "labels".into(),
        })
        .collect()
}

/// The `delete.plan` edits for folder `id`: what sits in it moves up to its parent,
/// scenes first, then the folder's delete. `not_found` if there is no such folder.
pub(crate) fn folder_plan(state: &State, id: &FolderId) -> Result<Vec<Edit>, ProtocolError> {
    let parent = state
        .folder(id)
        .ok_or_else(|| not_found(EntityKind::Folder, id.as_str()))?
        .parent
        .clone();
    let mut edits: Vec<Edit> = folder_blockers(state, id)
        .into_iter()
        .map(|blocker| match blocker.entity {
            EntityKind::Scene => Edit::SceneSetFolder {
                scene_id: SceneId::new(blocker.id),
                folder_id: parent.clone(),
            },
            _ => Edit::FolderSetParent {
                folder_id: FolderId::new(blocker.id),
                parent: parent.clone(),
            },
        })
        .collect();
    edits.push(Edit::FolderDelete {
        folder_id: id.clone(),
    });
    Ok(edits)
}

/// The `delete.plan` edits for label `id`: each source carrying it keeps its other
/// metadata without it, then the label's delete. `not_found` if there is no such label.
pub(crate) fn label_plan(state: &State, id: &LabelId) -> Result<Vec<Edit>, ProtocolError> {
    label_index(state, id)?;
    let mut edits: Vec<Edit> = state
        .sources()
        .filter(|s| s.labels.contains(id))
        .map(|source| {
            let mut meta = source_meta(source);
            meta.labels.retain(|l| l != id);
            Edit::SourceSetMeta {
                source_id: source.id.clone(),
                meta,
            }
        })
        .collect();
    edits.push(Edit::LabelDelete {
        label_id: id.clone(),
    });
    Ok(edits)
}

/// Moves the entry matching `is` to `index` in `list` and returns its old index, or
/// `missing()` if no entry matches.
fn reorder<T>(
    list: &mut Vec<T>,
    is: impl Fn(&T) -> bool,
    index: u32,
    at: usize,
    what: &str,
    missing: impl FnOnce() -> ProtocolError,
) -> Result<u32, ProtocolError> {
    let from = list.iter().position(is).ok_or_else(missing)?;
    let to = check_index(index, list.len(), at, what)?;
    let entry = list.remove(from);
    list.insert(to, entry);
    wire_index(from)
}

fn folder_index(state: &State, id: &FolderId) -> Result<usize, ProtocolError> {
    state
        .folders
        .iter()
        .position(|f| &f.id == id)
        .ok_or_else(|| not_found(EntityKind::Folder, id.as_str()))
}

fn label_index(state: &State, id: &LabelId) -> Result<usize, ProtocolError> {
    state
        .labels
        .iter()
        .position(|l| &l.id == id)
        .ok_or_else(|| not_found(EntityKind::Label, id.as_str()))
}

fn create_folder(
    state: &mut State,
    id: &FolderId,
    name: &str,
    parent: Option<&FolderId>,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    check_id(id.as_str(), at, "folder_id")?;
    check_name(name, at)?;
    if state.folder(id).is_some() {
        return Err(already_exists(EntityKind::Folder, id.as_str()));
    }
    if let Some(parent) = parent {
        folder_index(state, parent)?;
    }
    state.folders.push(Folder {
        id: id.clone(),
        name: name.to_owned(),
        parent: parent.cloned(),
    });
    Ok(vec![Edit::FolderDelete {
        folder_id: id.clone(),
    }])
}

fn rename_folder(
    state: &mut State,
    id: &FolderId,
    name: &str,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let fi = folder_index(state, id)?;
    check_name(name, at)?;
    let old = std::mem::replace(&mut state.folders[fi].name, name.to_owned());
    Ok(vec![Edit::FolderRename {
        folder_id: id.clone(),
        name: old,
    }])
}

fn set_parent(
    state: &mut State,
    id: &FolderId,
    parent: Option<&FolderId>,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let fi = folder_index(state, id)?;
    if let Some(parent) = parent {
        folder_index(state, parent)?;
        // Walk up from the new parent: reaching the folder means it would sit inside
        // itself. Parents always exist and never loop, so the walk ends.
        let mut path = vec![parent.clone()];
        let mut at_folder = Some(parent.clone());
        while let Some(current) = at_folder {
            if &current == id {
                let chain: Vec<String> = path.iter().rev().map(|f| format!("`{f}`")).collect();
                return Err(invalid(
                    at,
                    "parent",
                    format!(
                        "folder `{id}` would sit inside itself: {} -> `{id}`",
                        chain.join(" -> ")
                    ),
                ));
            }
            at_folder = state.folder(&current).and_then(|f| f.parent.clone());
            if let Some(next) = &at_folder {
                path.push(next.clone());
            }
        }
    }
    let old = std::mem::replace(&mut state.folders[fi].parent, parent.cloned());
    Ok(vec![Edit::FolderSetParent {
        folder_id: id.clone(),
        parent: old,
    }])
}

fn delete_folder(state: &mut State, id: &FolderId, at: usize) -> Result<Vec<Edit>, ProtocolError> {
    let fi = folder_index(state, id)?;
    let blockers = folder_blockers(state, id);
    if !blockers.is_empty() {
        return Err(blocked(
            at,
            "folder_id",
            &format!("folder `{id}`"),
            blockers,
        ));
    }
    let folder = state.folders.remove(fi);
    let mut inverse = vec![Edit::FolderCreate {
        folder_id: folder.id.clone(),
        name: folder.name,
        parent: folder.parent,
    }];
    // Re-created folders are appended, so only a folder that wasn't last moves back.
    if fi != state.folders.len() {
        inverse.push(Edit::FolderReorder {
            folder_id: folder.id,
            index: wire_index(fi)?,
        });
    }
    Ok(inverse)
}

fn set_folder(
    state: &mut State,
    scene_id: &SceneId,
    folder_id: Option<&FolderId>,
) -> Result<Vec<Edit>, ProtocolError> {
    let si = state
        .scene_index(scene_id)
        .ok_or_else(|| not_found(EntityKind::Scene, scene_id.as_str()))?;
    if let Some(folder_id) = folder_id {
        folder_index(state, folder_id)?;
    }
    // Variants, which sit under their parent and never in a folder (ADR-0024 §3), don't
    // exist yet; once they do, this refuses them.
    let old = std::mem::replace(&mut state.scenes[si].folder, folder_id.cloned());
    Ok(vec![Edit::SceneSetFolder {
        scene_id: scene_id.clone(),
        folder_id: old,
    }])
}

fn set_scene_meta(
    state: &mut State,
    scene_id: &SceneId,
    meta: &SceneMeta,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let si = state
        .scene_index(scene_id)
        .ok_or_else(|| not_found(EntityKind::Scene, scene_id.as_str()))?;
    if let Some((name, reason)) = meta.problem() {
        return Err(invalid(at, name, reason));
    }
    let old = std::mem::replace(&mut state.scenes[si].meta, meta.clone());
    Ok(vec![Edit::SceneSetMeta {
        scene_id: scene_id.clone(),
        meta: old,
    }])
}

/// Checks a label's name, colour and icon.
fn check_label(
    name: &str,
    color: &str,
    icon: Option<&str>,
    at: usize,
) -> Result<(), ProtocolError> {
    check_name(name, at)?;
    check_color(color).map_err(|reason| invalid(at, "color", reason))?;
    if let Some(icon) = icon {
        check_icon(icon).map_err(|reason| invalid(at, "icon", reason))?;
    }
    Ok(())
}

fn create_label(
    state: &mut State,
    id: &LabelId,
    name: &str,
    color: &str,
    icon: Option<&str>,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    check_id(id.as_str(), at, "label_id")?;
    if state.label(id).is_some() {
        return Err(already_exists(EntityKind::Label, id.as_str()));
    }
    check_label(name, color, icon, at)?;
    state.labels.push(Label {
        id: id.clone(),
        name: name.to_owned(),
        color: color.to_owned(),
        icon: icon.map(str::to_owned),
    });
    Ok(vec![Edit::LabelDelete {
        label_id: id.clone(),
    }])
}

fn update_label(
    state: &mut State,
    id: &LabelId,
    name: &str,
    color: &str,
    icon: Option<&str>,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let li = label_index(state, id)?;
    check_label(name, color, icon, at)?;
    let label = &mut state.labels[li];
    let old = Edit::LabelUpdate {
        label_id: id.clone(),
        name: std::mem::replace(&mut label.name, name.to_owned()),
        color: std::mem::replace(&mut label.color, color.to_owned()),
        icon: std::mem::replace(&mut label.icon, icon.map(str::to_owned)),
    };
    Ok(vec![old])
}

fn delete_label(state: &mut State, id: &LabelId, at: usize) -> Result<Vec<Edit>, ProtocolError> {
    let li = label_index(state, id)?;
    let blockers = label_blockers(state, id);
    if !blockers.is_empty() {
        return Err(blocked(at, "label_id", &format!("label `{id}`"), blockers));
    }
    let label = state.labels.remove(li);
    let mut inverse = vec![Edit::LabelCreate {
        label_id: label.id.clone(),
        name: label.name,
        color: label.color,
        icon: label.icon,
    }];
    if li != state.labels.len() {
        inverse.push(Edit::LabelReorder {
            label_id: label.id,
            index: wire_index(li)?,
        });
    }
    Ok(inverse)
}

fn set_source_meta(
    state: &mut State,
    source_id: &SourceId,
    meta: &SourceMeta,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    if state.source(source_id).is_none() {
        return Err(not_found(EntityKind::Source, source_id.as_str()));
    }
    let mut labels = BTreeSet::new();
    for label in &meta.labels {
        label_index(state, label)?;
        if !labels.insert(label.clone()) {
            return Err(invalid(
                at,
                "labels",
                format!("label `{label}` is listed more than once"),
            ));
        }
    }
    let source = state
        .sources
        .get_mut(source_id)
        .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str()))?;
    let old = source_meta(source);
    source.labels = labels;
    source.favourite = meta.favourite;
    Ok(vec![Edit::SourceSetMeta {
        source_id: source_id.clone(),
        meta: old,
    }])
}

#[cfg(test)]
mod tests {
    use crate::Graph;
    use jstream_protocol::{
        DeleteTarget, Edit, EntityKind, FolderId, LabelId, ProtocolError, Query, QueryResult,
        SceneId, SceneMeta, SourceId, SourceKind, SourceMeta,
    };

    fn folder(id: &str, parent: Option<&str>) -> Edit {
        Edit::FolderCreate {
            folder_id: FolderId::new(id),
            name: format!("Folder {id}"),
            parent: parent.map(FolderId::new),
        }
    }

    fn label(id: &str) -> Edit {
        Edit::LabelCreate {
            label_id: LabelId::new(id),
            name: format!("Label {id}"),
            color: "#ff8800".into(),
            icon: None,
        }
    }

    fn scene(id: &str) -> Edit {
        Edit::SceneCreate {
            scene_id: SceneId::new(id),
            name: format!("Scene {id}"),
            role: None,
        }
    }

    fn file(scene: &str, folder: Option<&str>) -> Edit {
        Edit::SceneSetFolder {
            scene_id: SceneId::new(scene),
            folder_id: folder.map(FolderId::new),
        }
    }

    fn scene_source(id: &str, scene: &str) -> Edit {
        Edit::SourceCreate {
            source_id: SourceId::new(id),
            name: format!("Source {id}"),
            kind: SourceKind::Scene {
                scene_id: SceneId::new(scene),
            },
            settings: Default::default(),
            duplicated_from: None,
        }
    }

    fn tag(source: &str, labels: &[&str], favourite: bool) -> Edit {
        Edit::SourceSetMeta {
            source_id: SourceId::new(source),
            meta: SourceMeta {
                labels: labels.iter().map(|l| LabelId::new(*l)).collect(),
                favourite,
            },
        }
    }

    /// Applies `edits` as one batch and checks that its inverse restores the state.
    fn apply_ok(graph: &mut Graph, edits: Vec<Edit>) {
        let before = graph.clone();
        let applied = graph.apply(&edits, None).expect("applies");
        let mut undone = graph.clone();
        undone
            .apply(&applied.inverse.edits, None)
            .expect("inverse applies");
        assert_eq!(undone.state(), before.state(), "inverse of {edits:?}");
    }

    fn apply_err(graph: &mut Graph, edits: Vec<Edit>) -> ProtocolError {
        let before = graph.clone();
        let error = graph.apply(&edits, None).expect_err("is refused");
        assert_eq!(graph, &before, "a refused batch changed the graph");
        error
    }

    fn field(error: ProtocolError) -> String {
        match error {
            ProtocolError::InvalidArgument { field, .. } => field,
            other => panic!("expected invalid_argument, got {other:?}"),
        }
    }

    /// Folders `top` > `mid` > `leaf`, and `side`; scenes `a` in `mid` and `b` in none;
    /// labels `red` then `blue`; scene source `sa` carrying both.
    fn fixture() -> Graph {
        let mut graph = Graph::new();
        apply_ok(
            &mut graph,
            vec![
                folder("top", None),
                folder("mid", Some("top")),
                folder("leaf", Some("mid")),
                folder("side", None),
                scene("a"),
                scene("b"),
                file("a", Some("mid")),
                label("red"),
                label("blue"),
                scene_source("sa", "a"),
                tag("sa", &["red", "blue"], true),
            ],
        );
        graph
    }

    #[test]
    fn state_get_reports_folders_labels_and_metadata() {
        let graph = fixture();
        let Some(Ok(QueryResult::StateGet {
            folders,
            labels,
            scenes,
            sources,
            ..
        })) = graph.query(&Query::StateGet)
        else {
            panic!("state.get answers");
        };
        let order: Vec<_> = folders.iter().map(|f| f.folder_id.as_str()).collect();
        assert_eq!(order, ["top", "mid", "leaf", "side"]);
        assert_eq!(folders[2].parent, Some(FolderId::new("mid")));
        let order: Vec<_> = labels.iter().map(|l| l.label_id.as_str()).collect();
        assert_eq!(order, ["red", "blue"]);
        assert_eq!(scenes[0].folder, Some(FolderId::new("mid")));
        assert_eq!(scenes[1].folder, None);
        // Labels are stored ordered by ID, whatever order they were sent in.
        assert_eq!(
            sources[0].meta.labels,
            [LabelId::new("blue"), LabelId::new("red")]
        );
        assert!(sources[0].meta.favourite);
    }

    #[test]
    fn a_folder_cannot_sit_inside_itself() {
        let mut graph = fixture();
        for parent in ["top", "mid", "leaf"] {
            let error = apply_err(
                &mut graph,
                vec![Edit::FolderSetParent {
                    folder_id: FolderId::new("top"),
                    parent: Some(FolderId::new(parent)),
                }],
            );
            assert_eq!(field(error), "edits.0.parent", "{parent}");
        }
        // Moving a subtree elsewhere, or to the top level, is fine and undoes exactly.
        apply_ok(
            &mut graph,
            vec![
                Edit::FolderSetParent {
                    folder_id: FolderId::new("mid"),
                    parent: Some(FolderId::new("side")),
                },
                Edit::FolderSetParent {
                    folder_id: FolderId::new("leaf"),
                    parent: None,
                },
            ],
        );
    }

    #[test]
    fn missing_folders_and_labels_are_not_found() {
        let mut graph = fixture();
        let missing = |entity, id: &str| ProtocolError::NotFound {
            entity,
            id: id.into(),
        };
        assert_eq!(
            apply_err(&mut graph, vec![folder("x", Some("nope"))]),
            missing(EntityKind::Folder, "nope")
        );
        assert_eq!(
            apply_err(&mut graph, vec![file("a", Some("nope"))]),
            missing(EntityKind::Folder, "nope")
        );
        assert_eq!(
            apply_err(&mut graph, vec![tag("sa", &["nope"], false)]),
            missing(EntityKind::Label, "nope")
        );
        assert_eq!(
            apply_err(
                &mut graph,
                vec![Edit::LabelDelete {
                    label_id: LabelId::new("nope"),
                }]
            ),
            missing(EntityKind::Label, "nope")
        );
        assert_eq!(
            apply_err(&mut graph, vec![folder("top", None)]),
            ProtocolError::AlreadyExists {
                entity: EntityKind::Folder,
                id: "top".into(),
            }
        );
    }

    #[test]
    fn values_are_checked() {
        let mut graph = fixture();
        let bad_label = Edit::LabelCreate {
            label_id: LabelId::new("x"),
            name: "X".into(),
            color: "orange".into(),
            icon: None,
        };
        assert_eq!(
            field(apply_err(&mut graph, vec![bad_label])),
            "edits.0.color"
        );
        let bad_icon = Edit::LabelUpdate {
            label_id: LabelId::new("red"),
            name: "Red".into(),
            color: "#ff0000".into(),
            icon: Some("two words".into()),
        };
        assert_eq!(field(apply_err(&mut graph, vec![bad_icon])), "edits.0.icon");
        let bad_meta = Edit::SceneSetMeta {
            scene_id: SceneId::new("a"),
            meta: SceneMeta {
                color: Some("#12345".into()),
                ..SceneMeta::default()
            },
        };
        assert_eq!(
            field(apply_err(&mut graph, vec![bad_meta])),
            "edits.0.color"
        );
        assert_eq!(
            field(apply_err(
                &mut graph,
                vec![tag("sa", &["red", "red"], false)]
            )),
            "edits.0.labels"
        );
        assert_eq!(
            field(apply_err(
                &mut graph,
                vec![folder("x", None), folder("", None)]
            )),
            "edits.1.folder_id"
        );
    }

    #[test]
    fn a_folder_or_label_in_use_cannot_be_deleted_and_the_plan_clears_it() {
        let mut graph = fixture();
        let delete_mid = Edit::FolderDelete {
            folder_id: FolderId::new("mid"),
        };
        match apply_err(&mut graph, vec![delete_mid.clone()]) {
            ProtocolError::InvalidArgument { blockers, .. } => {
                let found: Vec<_> = blockers
                    .iter()
                    .map(|b| (b.entity, b.id.as_str(), b.field.as_str()))
                    .collect();
                assert_eq!(
                    found,
                    [
                        (EntityKind::Scene, "a", "folder"),
                        (EntityKind::Folder, "leaf", "parent")
                    ]
                );
            }
            other => panic!("expected blockers, got {other:?}"),
        }
        let Ok(QueryResult::DeletePlan { edits, .. }) = graph.delete_plan(&DeleteTarget::Folder {
            folder_id: FolderId::new("mid"),
        }) else {
            panic!("plans");
        };
        assert_eq!(
            edits,
            [
                file("a", Some("top")),
                Edit::FolderSetParent {
                    folder_id: FolderId::new("leaf"),
                    parent: Some(FolderId::new("top")),
                },
                delete_mid,
            ]
        );
        apply_ok(&mut graph, edits);

        let Ok(QueryResult::DeletePlan { edits, .. }) = graph.delete_plan(&DeleteTarget::Label {
            label_id: LabelId::new("red"),
        }) else {
            panic!("plans");
        };
        assert_eq!(
            edits,
            [
                tag("sa", &["blue"], true),
                Edit::LabelDelete {
                    label_id: LabelId::new("red"),
                },
            ]
        );
        apply_ok(&mut graph, edits);
    }

    #[test]
    fn deleting_a_filed_scene_or_tagged_source_undoes_exactly() {
        let mut graph = fixture();
        apply_ok(
            &mut graph,
            vec![Edit::SceneSetMeta {
                scene_id: SceneId::new("a"),
                meta: SceneMeta {
                    icon: Some("star".into()),
                    color: Some("#00aa00".into()),
                    favourite: true,
                    show_in_multiview: false,
                },
            }],
        );
        // `apply_ok` checks each inverse restores the folder, metadata and labels.
        apply_ok(
            &mut graph,
            vec![
                Edit::SourceDelete {
                    source_id: SourceId::new("sa"),
                },
                Edit::SceneDelete {
                    scene_id: SceneId::new("a"),
                },
            ],
        );
        // Deleting the middle folder and label puts them back in place.
        apply_ok(
            &mut graph,
            vec![
                Edit::FolderDelete {
                    folder_id: FolderId::new("side"),
                },
                Edit::LabelReorder {
                    label_id: LabelId::new("blue"),
                    index: 0,
                },
            ],
        );
    }

    #[test]
    fn a_duplicate_copies_the_labels_and_favourite() {
        let mut graph = fixture();
        apply_ok(
            &mut graph,
            vec![Edit::SourceDuplicate {
                source_id: SourceId::new("sa"),
                new_source_id: SourceId::new("sa2"),
                name: "Copy".into(),
            }],
        );
        let copy = graph.state().source(&SourceId::new("sa2")).expect("copied");
        assert_eq!(copy.labels().len(), 2);
        assert!(copy.favourite());
    }
}
