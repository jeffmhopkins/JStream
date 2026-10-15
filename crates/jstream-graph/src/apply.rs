//! Applying edit batches atomically, and computing their inverses for undo.
//!
//! Each edit is applied to a working copy of the state, against what the edits before it
//! left, and yields the edits that undo it, computed from the state *before* it. The
//! inverse of a batch is the inverses of its edits in reverse order. Only when every
//! edit succeeds does the working copy replace the state.

use crate::model::{Graph, Item, Output, Scene, Source, State};
use crate::target::rtmp_url_problem;
use jstream_protocol::{
    Batch, Crop, Edit, EntityKind, ItemId, ItemTransform, OutputId, OutputKind, ProtocolError, Rev,
    RtmpTarget, SceneId, SourceId, SourceKind,
};
use std::collections::BTreeSet;

/// The result of a successful [`Graph::apply`].
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    /// The revision the batch produced: the previous one plus one.
    pub rev: Rev,
    /// The edits that undo the batch, as a batch with no `expected_rev` or idempotency
    /// key. Applied to the state the batch produced, it restores the state before the
    /// batch exactly. The caller (the engine's undo history) decides which `expected_rev`
    /// to send it with.
    pub inverse: Batch,
}

impl Graph {
    /// Applies `edits` atomically, as one batch.
    ///
    /// Fails, changing nothing, with:
    /// - `rev_conflict` if `expected_rev` is set and isn't the current revision;
    /// - `empty_batch` if there are no edits;
    /// - `not_found`, `already_exists` or `invalid_argument` (with the field as
    ///   `edits.<n>.<field>`) for the first edit that can't apply to the state the edits
    ///   before it left;
    /// - `internal` if the revision would overflow, or for an edit this crate doesn't
    ///   implement.
    ///
    /// On success the revision increases by exactly one, and the result carries the
    /// inverse batch.
    pub fn apply(
        &mut self,
        edits: &[Edit],
        expected_rev: Option<Rev>,
    ) -> Result<Applied, ProtocolError> {
        self.apply_with_running(edits, expected_rev, &BTreeSet::new())
    }

    /// [`apply`](Self::apply), for an engine whose outputs in `running` are running: an
    /// `output.delete` of one of them fails with `invalid_argument` (field
    /// `edits.<n>.output_id`), in edit order like any other failure. Whether an output
    /// runs is runtime state, which the graph doesn't hold, so the engine passes it.
    pub fn apply_with_running(
        &mut self,
        edits: &[Edit],
        expected_rev: Option<Rev>,
        running: &BTreeSet<OutputId>,
    ) -> Result<Applied, ProtocolError> {
        if let Some(expected) = expected_rev
            && expected != self.rev
        {
            return Err(ProtocolError::RevConflict {
                expected,
                current: self.rev,
            });
        }
        if edits.is_empty() {
            return Err(ProtocolError::EmptyBatch);
        }
        let rev = self.rev.next().ok_or_else(|| ProtocolError::Internal {
            reason: "the revision counter overflowed".into(),
        })?;
        let mut work = self.state.clone();
        let mut undo: Vec<Vec<Edit>> = Vec::with_capacity(edits.len());
        for (at, edit) in edits.iter().enumerate() {
            undo.push(apply_edit(&mut work, edit, at, running)?);
        }
        self.state = work;
        self.rev = rev;
        Ok(Applied {
            rev,
            inverse: Batch {
                edits: undo.into_iter().rev().flatten().collect(),
                expected_rev: None,
                idempotency_key: None,
            },
        })
    }

    /// Applies a protocol [`Batch`], using its `expected_rev`. The idempotency key is the
    /// engine's concern and is ignored here.
    pub fn apply_batch(&mut self, batch: &Batch) -> Result<Applied, ProtocolError> {
        self.apply(&batch.edits, batch.expected_rev)
    }
}

/// `edits.<at>.<name>`, the field path errors carry.
fn field(at: usize, name: &str) -> String {
    format!("edits.{at}.{name}")
}

fn invalid(at: usize, name: &str, reason: impl Into<String>) -> ProtocolError {
    ProtocolError::InvalidArgument {
        field: field(at, name),
        reason: reason.into(),
    }
}

fn not_found(entity: EntityKind, id: &str) -> ProtocolError {
    ProtocolError::NotFound {
        entity,
        id: id.to_owned(),
    }
}

fn already_exists(entity: EntityKind, id: &str) -> ProtocolError {
    ProtocolError::AlreadyExists {
        entity,
        id: id.to_owned(),
    }
}

fn check_id(id: &str, at: usize, name: &str) -> Result<(), ProtocolError> {
    if id.is_empty() {
        return Err(invalid(at, name, "must not be empty"));
    }
    Ok(())
}

fn check_name(value: &str, at: usize) -> Result<(), ProtocolError> {
    if value.trim().is_empty() {
        return Err(invalid(at, "name", "must not be empty"));
    }
    Ok(())
}

/// Converts a wire index into a position below `len`.
fn check_index(index: u32, len: usize, at: usize, what: &str) -> Result<usize, ProtocolError> {
    match usize::try_from(index) {
        Ok(i) if i < len => Ok(i),
        _ => Err(invalid(
            at,
            "index",
            format!("must be less than the number of {what} ({len})"),
        )),
    }
}

fn wire_index(index: usize) -> Result<u32, ProtocolError> {
    u32::try_from(index).map_err(|_| ProtocolError::Internal {
        reason: format!("position {index} does not fit the protocol's u32 index"),
    })
}

fn find_scene(state: &State, id: &SceneId) -> Result<usize, ProtocolError> {
    state
        .scene_index(id)
        .ok_or_else(|| not_found(EntityKind::Scene, id.as_str()))
}

fn find_item(state: &State, id: &ItemId) -> Result<(usize, usize), ProtocolError> {
    state
        .item_index(id)
        .ok_or_else(|| not_found(EntityKind::Item, id.as_str()))
}

/// The item at `(si, ii)`, which `find_item` just returned.
fn item_mut(state: &mut State, (si, ii): (usize, usize)) -> Result<&mut Item, ProtocolError> {
    state
        .scenes
        .get_mut(si)
        .and_then(|s| s.items.get_mut(ii))
        .ok_or_else(|| ProtocolError::Internal {
            reason: "invariant: an item index found a moment ago is valid".into(),
        })
}

/// Applies one edit to `state` and returns the edits that undo it.
fn apply_edit(
    state: &mut State,
    edit: &Edit,
    at: usize,
    running: &BTreeSet<OutputId>,
) -> Result<Vec<Edit>, ProtocolError> {
    match edit {
        Edit::SceneCreate { scene_id, name } => {
            check_id(scene_id.as_str(), at, "scene_id")?;
            check_name(name, at)?;
            if state.scene_index(scene_id).is_some() {
                return Err(already_exists(EntityKind::Scene, scene_id.as_str()));
            }
            state.scenes.push(Scene {
                id: scene_id.clone(),
                name: name.clone(),
                items: Vec::new(),
            });
            Ok(vec![Edit::SceneDelete {
                scene_id: scene_id.clone(),
            }])
        }
        Edit::SceneRename { scene_id, name } => {
            let si = find_scene(state, scene_id)?;
            check_name(name, at)?;
            let scene = &mut state.scenes[si];
            let old = std::mem::replace(&mut scene.name, name.clone());
            Ok(vec![Edit::SceneRename {
                scene_id: scene_id.clone(),
                name: old,
            }])
        }
        Edit::SceneDelete { scene_id } => {
            let si = find_scene(state, scene_id)?;
            let shown_by: Vec<&str> = state
                .sources()
                .filter(|s| matches!(&s.kind, SourceKind::Scene { scene_id: t } if t == scene_id))
                .map(|s| s.id.as_str())
                .collect();
            if !shown_by.is_empty() {
                return Err(invalid(
                    at,
                    "scene_id",
                    format!(
                        "scene `{scene_id}` is shown by scene sources {}; delete them first",
                        shown_by.join(", ")
                    ),
                ));
            }
            let scene = state.scenes.remove(si);
            let mut inverse = vec![Edit::SceneCreate {
                scene_id: scene.id.clone(),
                name: scene.name.clone(),
            }];
            // Re-created scenes are appended, so only a scene that wasn't last moves back.
            if si != state.scenes.len() {
                inverse.push(Edit::SceneReorder {
                    scene_id: scene.id.clone(),
                    index: wire_index(si)?,
                });
            }
            // Re-adding bottom to top puts each item back at its own index.
            for (ii, item) in scene.items.iter().enumerate() {
                inverse.extend(restore_item(&scene.id, item, ii, ii)?);
            }
            Ok(inverse)
        }
        Edit::SceneReorder { scene_id, index } => {
            let from = find_scene(state, scene_id)?;
            let to = check_index(*index, state.scenes.len(), at, "scenes")?;
            let scene = state.scenes.remove(from);
            state.scenes.insert(to, scene);
            Ok(vec![Edit::SceneReorder {
                scene_id: scene_id.clone(),
                index: wire_index(from)?,
            }])
        }
        Edit::ItemAdd {
            scene_id,
            item_id,
            source_id,
            transform,
        } => {
            check_id(item_id.as_str(), at, "item_id")?;
            let si = find_scene(state, scene_id)?;
            let source = state
                .source(source_id)
                .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str()))?;
            if state.item_index(item_id).is_some() {
                return Err(already_exists(EntityKind::Item, item_id.as_str()));
            }
            if let SourceKind::Scene { scene_id: nested } = &source.kind
                && let Some(path) = scene_path(state, nested, scene_id)
            {
                let mut cycle = vec![scene_id.as_str()];
                cycle.extend(path.iter().map(SceneId::as_str));
                return Err(invalid(
                    at,
                    "source_id",
                    format!(
                        "placing scene `{nested}` in scene `{scene_id}` would create a cycle: {}",
                        cycle.join(" -> ")
                    ),
                ));
            }
            state.scenes[si].items.push(Item {
                id: item_id.clone(),
                source_id: source_id.clone(),
                transform: transform.unwrap_or(ItemTransform::IDENTITY),
                crop: Crop::NONE,
                visible: true,
            });
            Ok(vec![Edit::ItemRemove {
                item_id: item_id.clone(),
            }])
        }
        Edit::ItemRemove { item_id } => {
            let (si, ii) = find_item(state, item_id)?;
            let scene = &mut state.scenes[si];
            let item = scene.items.remove(ii);
            // `item.add` puts it on top, which is index `len` now that it is gone.
            restore_item(&scene.id, &item, ii, scene.items.len())
        }
        Edit::ItemReorder { item_id, index } => {
            let (si, from) = find_item(state, item_id)?;
            let items = &mut state.scenes[si].items;
            let to = check_index(*index, items.len(), at, "items in the scene")?;
            let item = items.remove(from);
            items.insert(to, item);
            Ok(vec![Edit::ItemReorder {
                item_id: item_id.clone(),
                index: wire_index(from)?,
            }])
        }
        Edit::ItemSetTransform { item_id, transform } => {
            let item = item_mut(state, find_item(state, item_id)?)?;
            let old = std::mem::replace(&mut item.transform, *transform);
            Ok(vec![Edit::ItemSetTransform {
                item_id: item_id.clone(),
                transform: old,
            }])
        }
        Edit::ItemSetCrop { item_id, crop } => {
            let item = item_mut(state, find_item(state, item_id)?)?;
            let old = std::mem::replace(&mut item.crop, *crop);
            Ok(vec![Edit::ItemSetCrop {
                item_id: item_id.clone(),
                crop: old,
            }])
        }
        Edit::ItemSetVisible { item_id, visible } => {
            let item = item_mut(state, find_item(state, item_id)?)?;
            let old = std::mem::replace(&mut item.visible, *visible);
            Ok(vec![Edit::ItemSetVisible {
                item_id: item_id.clone(),
                visible: old,
            }])
        }
        Edit::SourceCreate {
            source_id,
            name,
            kind,
        } => {
            check_id(source_id.as_str(), at, "source_id")?;
            check_name(name, at)?;
            match kind {
                SourceKind::Scene { scene_id } => {
                    find_scene(state, scene_id)?;
                }
                SourceKind::Builtin { kind } => {
                    if kind.is_empty() {
                        return Err(invalid(at, "kind.kind", "must not be empty"));
                    }
                }
                _ => {
                    return Err(unsupported(at, "kind", "this source kind"));
                }
            }
            create_source(state, source_id, name, kind)
        }
        Edit::SourceDuplicate {
            source_id,
            new_source_id,
            name,
        } => {
            let kind = state
                .source(source_id)
                .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str()))?
                .kind
                .clone();
            check_id(new_source_id.as_str(), at, "new_source_id")?;
            check_name(name, at)?;
            create_source(state, new_source_id, name, &kind)
        }
        Edit::SourceDelete { source_id } => {
            if state.source(source_id).is_none() {
                return Err(not_found(EntityKind::Source, source_id.as_str()));
            }
            let used_by: Vec<&str> = state.placements(source_id).map(|i| i.id.as_str()).collect();
            if !used_by.is_empty() {
                return Err(invalid(
                    at,
                    "source_id",
                    format!(
                        "source `{source_id}` is placed by items {}; remove them first",
                        used_by.join(", ")
                    ),
                ));
            }
            let source = state
                .sources
                .remove(source_id)
                .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str()))?;
            Ok(vec![Edit::SourceCreate {
                source_id: source.id,
                name: source.name,
                kind: source.kind,
            }])
        }
        Edit::OutputCreate { output_id, kind } => {
            check_id(output_id.as_str(), at, "output_id")?;
            if state.outputs.contains_key(output_id) {
                return Err(already_exists(EntityKind::Output, output_id.as_str()));
            }
            match kind {
                OutputKind::Rtmp { target } => check_target(target, at, "kind.target")?,
                _ => return Err(unsupported(at, "kind", "this output kind")),
            }
            state.outputs.insert(
                output_id.clone(),
                Output {
                    id: output_id.clone(),
                    kind: kind.clone(),
                },
            );
            Ok(vec![Edit::OutputDelete {
                output_id: output_id.clone(),
            }])
        }
        Edit::OutputSetTarget { output_id, target } => {
            let output = state
                .outputs
                .get_mut(output_id)
                .ok_or_else(|| not_found(EntityKind::Output, output_id.as_str()))?;
            check_target(target, at, "target")?;
            let OutputKind::Rtmp { target: current } = &mut output.kind else {
                return Err(invalid(at, "output_id", "the output is not an RTMP output"));
            };
            let old = std::mem::replace(current, target.clone());
            Ok(vec![Edit::OutputSetTarget {
                output_id: output_id.clone(),
                target: old,
            }])
        }
        Edit::OutputDelete { output_id } => {
            if running.contains(output_id) {
                return Err(invalid(
                    at,
                    "output_id",
                    format!("output `{output_id}` is running; stop it first"),
                ));
            }
            let output = state
                .outputs
                .remove(output_id)
                .ok_or_else(|| not_found(EntityKind::Output, output_id.as_str()))?;
            Ok(vec![Edit::OutputCreate {
                output_id: output.id,
                kind: output.kind,
            }])
        }
        other => Err(unsupported(at, "type", &format!("`{}`", other.name()))),
    }
}

/// Checks an RTMP target; `name` is its field path within the edit (`kind.target`).
/// The reason never repeats the URL, which might hold a key.
fn check_target(target: &RtmpTarget, at: usize, name: &str) -> Result<(), ProtocolError> {
    match target {
        RtmpTarget::Generic { url } => match rtmp_url_problem(url) {
            Some(problem) => Err(invalid(at, &format!("{name}.url"), problem)),
            None => Ok(()),
        },
        _ => Err(unsupported(at, name, "this RTMP target")),
    }
}

fn unsupported(at: usize, name: &str, what: &str) -> ProtocolError {
    ProtocolError::Internal {
        reason: format!(
            "{}: {what} is not implemented by jstream-graph",
            field(at, name)
        ),
    }
}

fn create_source(
    state: &mut State,
    id: &SourceId,
    name: &str,
    kind: &SourceKind,
) -> Result<Vec<Edit>, ProtocolError> {
    if state.sources.contains_key(id) {
        return Err(already_exists(EntityKind::Source, id.as_str()));
    }
    state.sources.insert(
        id.clone(),
        Source {
            id: id.clone(),
            name: name.to_owned(),
            kind: kind.clone(),
        },
    );
    Ok(vec![Edit::SourceDelete {
        source_id: id.clone(),
    }])
}

/// The edits that put a removed `item` back in scene `scene_id` at stacking position
/// `index`, given that `item.add` would place it at `top`.
fn restore_item(
    scene_id: &SceneId,
    item: &Item,
    index: usize,
    top: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let mut edits = vec![Edit::ItemAdd {
        scene_id: scene_id.clone(),
        item_id: item.id.clone(),
        source_id: item.source_id.clone(),
        transform: Some(item.transform),
    }];
    if index != top {
        edits.push(Edit::ItemReorder {
            item_id: item.id.clone(),
            index: wire_index(index)?,
        });
    }
    if item.crop != Crop::NONE {
        edits.push(Edit::ItemSetCrop {
            item_id: item.id.clone(),
            crop: item.crop,
        });
    }
    if !item.visible {
        edits.push(Edit::ItemSetVisible {
            item_id: item.id.clone(),
            visible: false,
        });
    }
    Ok(edits)
}

/// The scenes nested in `scene`: those shown by the scene sources its items place.
fn nested_scenes<'a>(state: &'a State, scene: &'a Scene) -> impl Iterator<Item = &'a SceneId> {
    scene.items.iter().filter_map(
        move |i| match state.sources.get(&i.source_id).map(|s| &s.kind) {
            Some(SourceKind::Scene { scene_id }) => Some(scene_id),
            _ => None,
        },
    )
}

/// A chain of nesting from `from` down to `to` (`[from, …, to]`), if `to` is `from` or is
/// nested in it at any depth.
fn scene_path(state: &State, from: &SceneId, to: &SceneId) -> Option<Vec<SceneId>> {
    let mut visited = BTreeSet::new();
    let mut path = Vec::new();
    if dfs(state, from, to, &mut visited, &mut path) {
        Some(path)
    } else {
        None
    }
}

fn dfs(
    state: &State,
    at: &SceneId,
    to: &SceneId,
    visited: &mut BTreeSet<SceneId>,
    path: &mut Vec<SceneId>,
) -> bool {
    path.push(at.clone());
    if at == to {
        return true;
    }
    if visited.insert(at.clone())
        && let Some(scene) = state.scene(at)
    {
        for next in nested_scenes(state, scene) {
            if dfs(state, next, to, visited, path) {
                return true;
            }
        }
    }
    path.pop();
    false
}
