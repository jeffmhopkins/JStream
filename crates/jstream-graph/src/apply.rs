//! Applying edit batches atomically, and computing their inverses for undo.
//!
//! Each edit is applied to a working copy of the state, against what the edits before it
//! left, and yields the edits that undo it, computed from the state *before* it. The
//! inverse of a batch is the inverses of its edits in reverse order. Only when every
//! edit succeeds does the working copy replace the state.

use crate::deps::{self, Node};
use crate::model::{Graph, Item, Output, Scene, Source, State};
use crate::target::rtmp_url_problem;
use jstream_protocol::{
    Batch, Crop, Edit, EntityKind, ItemId, ItemTransform, KindDescriptor, Media, OutputId,
    OutputKind, ParamDescriptor, ParamType, ProtocolError, Reference, Rev, RtmpTarget, SceneId,
    SceneRole, Settings, SourceId, SourceKind,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The kinds a graph can create, by ID.
type Kinds = BTreeMap<String, KindDescriptor>;

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
            undo.push(apply_edit(&mut work, &self.kinds, edit, at, running)?);
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

/// The item a placement edit changes. `scene_id` is the edit's optional addressing
/// (ADR-0024 §2): absent or the item's own scene is the item itself. Keyframe scenes,
/// whose overrides another scene would write, aren't built yet, so any other existing
/// scene is `invalid_argument` and a missing one `not_found`.
fn placement_mut<'a>(
    state: &'a mut State,
    item_id: &ItemId,
    scene_id: Option<&SceneId>,
    at: usize,
) -> Result<&'a mut Item, ProtocolError> {
    let (si, ii) = find_item(state, item_id)?;
    if let Some(scene_id) = scene_id
        && scene_id != &state.scenes[si].id
    {
        find_scene(state, scene_id)?;
        return Err(invalid(
            at,
            "scene_id",
            format!(
                "item `{item_id}` is in scene `{}`, and only its own scene can be named \
                 until keyframe scenes exist",
                state.scenes[si].id
            ),
        ));
    }
    item_mut(state, (si, ii))
}

/// Applies `item.move` and returns its inverse: `item.move` back.
fn move_item(
    state: &mut State,
    item_id: &ItemId,
    scene_id: &SceneId,
    index: u32,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let (from_scene, from_index) = find_item(state, item_id)?;
    let to_scene = find_scene(state, scene_id)?;
    let old_scene_id = state.scenes[from_scene].id.clone();
    // The item's final position: one past the top is allowed in another scene.
    let slots = state.scenes[to_scene].items.len() + usize::from(to_scene != from_scene);
    let to_index = check_index(index, slots, at, "positions in the scene")?;
    let source_id = state.scenes[from_scene].items[from_index].source_id.clone();
    // The scene -> source edge already exists from the old scene; it points into the
    // source, so it can't be part of a path from the source back to the new scene.
    if let Some(cycle) = deps::cycle(
        state,
        &Node::Scene(scene_id.clone()),
        &Node::Source(source_id.clone()),
    ) {
        return Err(invalid(
            at,
            "scene_id",
            format!(
                "moving item `{item_id}` (source `{source_id}`) into scene `{scene_id}` \
                 would create a cycle: {cycle}"
            ),
        ));
    }
    // No keyframe overrides exist yet, so a move can't orphan any (ADR-0040 §1).
    let item = state.scenes[from_scene].items.remove(from_index);
    state.scenes[to_scene].items.insert(to_index, item);
    Ok(vec![Edit::ItemMove {
        item_id: item_id.clone(),
        scene_id: old_scene_id,
        index: wire_index(from_index)?,
    }])
}

/// Applies one edit to `state` and returns the edits that undo it.
fn apply_edit(
    state: &mut State,
    kinds: &Kinds,
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
                role: SceneRole::Scene,
                items: Vec::new(),
            });
            Ok(vec![Edit::SceneDelete {
                scene_id: scene_id.clone(),
            }])
        }
        Edit::SceneSetRole { scene_id, role } => {
            let si = find_scene(state, scene_id)?;
            // No keyframe overrides exist yet, so a role change can't orphan any
            // (ADR-0040 §1); once they do, this edit drops and its inverse restores them.
            let old = std::mem::replace(&mut state.scenes[si].role, *role);
            Ok(vec![Edit::SceneSetRole {
                scene_id: scene_id.clone(),
                role: old,
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
            let referred_by = deps::referrers(state, &Node::Scene(scene_id.clone()));
            if !referred_by.is_empty() {
                return Err(invalid(
                    at,
                    "scene_id",
                    format!(
                        "scene `{scene_id}` is referred to by {}; clear those settings first",
                        referred_by.join(", ")
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
            if scene.role != SceneRole::Scene {
                inverse.push(Edit::SceneSetRole {
                    scene_id: scene.id.clone(),
                    role: scene.role,
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
            if state.source(source_id).is_none() {
                return Err(not_found(EntityKind::Source, source_id.as_str()));
            }
            if state.item_index(item_id).is_some() {
                return Err(already_exists(EntityKind::Item, item_id.as_str()));
            }
            let edge = (
                Node::Scene(scene_id.clone()),
                Node::Source(source_id.clone()),
            );
            if let Some(cycle) = deps::cycle(state, &edge.0, &edge.1) {
                return Err(invalid(
                    at,
                    "source_id",
                    format!(
                        "placing source `{source_id}` in scene `{scene_id}` would create a cycle: {cycle}"
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
        Edit::ItemMove {
            item_id,
            scene_id,
            index,
        } => move_item(state, item_id, scene_id, *index, at),
        Edit::ItemSetTransform {
            item_id,
            scene_id,
            transform,
        } => {
            let item = placement_mut(state, item_id, scene_id.as_ref(), at)?;
            let old = std::mem::replace(&mut item.transform, *transform);
            Ok(vec![Edit::ItemSetTransform {
                item_id: item_id.clone(),
                scene_id: scene_id.clone(),
                transform: old,
            }])
        }
        Edit::ItemSetCrop {
            item_id,
            scene_id,
            crop,
        } => {
            let item = placement_mut(state, item_id, scene_id.as_ref(), at)?;
            let old = std::mem::replace(&mut item.crop, *crop);
            Ok(vec![Edit::ItemSetCrop {
                item_id: item_id.clone(),
                scene_id: scene_id.clone(),
                crop: old,
            }])
        }
        Edit::ItemSetVisible {
            item_id,
            scene_id,
            visible,
        } => {
            let item = placement_mut(state, item_id, scene_id.as_ref(), at)?;
            let old = std::mem::replace(&mut item.visible, *visible);
            Ok(vec![Edit::ItemSetVisible {
                item_id: item_id.clone(),
                scene_id: scene_id.clone(),
                visible: old,
            }])
        }
        Edit::SourceCreate {
            source_id,
            name,
            kind,
            settings,
        } => {
            check_id(source_id.as_str(), at, "source_id")?;
            check_name(name, at)?;
            let settings = match kind {
                SourceKind::Scene { scene_id } => {
                    find_scene(state, scene_id)?;
                    if !settings.is_empty() {
                        return Err(invalid(at, "settings", "a `scene` source has no settings"));
                    }
                    Settings::new()
                }
                SourceKind::Builtin { kind } => {
                    if kind.is_empty() {
                        return Err(invalid(at, "kind.kind", "must not be empty"));
                    }
                    let descriptor = kinds.get(kind).ok_or_else(|| {
                        invalid(
                            at,
                            "kind.kind",
                            format!("there is no kind `{kind}`; `kind.list` names them"),
                        )
                    })?;
                    complete_settings(state, kinds, descriptor, source_id, settings, at)?
                }
                _ => {
                    return Err(unsupported(at, "kind", "this source kind"));
                }
            };
            let inverse = create_source(state, source_id, name, kind, settings)?;
            check_reference_cycles(state, source_id, at, "settings")?;
            Ok(inverse)
        }
        Edit::SourceDuplicate {
            source_id,
            new_source_id,
            name,
        } => {
            let original = state
                .source(source_id)
                .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str()))?;
            let (kind, settings) = (original.kind.clone(), original.settings.clone());
            check_id(new_source_id.as_str(), at, "new_source_id")?;
            check_name(name, at)?;
            // The copy has the original's outgoing edges and none coming in, so it can't
            // close a cycle.
            create_source(state, new_source_id, name, &kind, settings)
        }
        Edit::SourceRename { source_id, name } => {
            check_name(name, at)?;
            let source = state
                .sources
                .get_mut(source_id)
                .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str()))?;
            let old = std::mem::replace(&mut source.name, name.clone());
            Ok(vec![Edit::SourceRename {
                source_id: source_id.clone(),
                name: old,
            }])
        }
        Edit::SourceSetSettings { source_id, patch } => {
            set_settings(state, kinds, source_id, patch, at)
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
            let referred_by = deps::referrers(state, &Node::Source(source_id.clone()));
            if !referred_by.is_empty() {
                return Err(invalid(
                    at,
                    "source_id",
                    format!(
                        "source `{source_id}` is referred to by {}; clear those settings first",
                        referred_by.join(", ")
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
                settings: source.settings,
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
    settings: Settings,
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
            settings,
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
            scene_id: None,
            crop: item.crop,
        });
    }
    if !item.visible {
        edits.push(Edit::ItemSetVisible {
            item_id: item.id.clone(),
            scene_id: None,
            visible: false,
        });
    }
    Ok(edits)
}

/// The complete settings of a new source of kind `descriptor`: `given`, checked, with
/// every parameter it leaves out (or sends as `null`, outside reference parameters) at
/// its default.
fn complete_settings(
    state: &State,
    kinds: &Kinds,
    descriptor: &KindDescriptor,
    source_id: &SourceId,
    given: &Settings,
    at: usize,
) -> Result<Settings, ProtocolError> {
    for (id, value) in given.iter() {
        let field = format!("settings.{id}");
        let param = descriptor.param(id).ok_or_else(|| {
            invalid(
                at,
                &field,
                format!("is not a parameter of kind `{}`", descriptor.kind_id),
            )
        })?;
        if !value.is_null() || param.is_reference() {
            check_value(state, kinds, source_id, param, value, at, &field)?;
        }
    }
    Ok(descriptor
        .params
        .iter()
        .map(|p| {
            let value = given
                .get(&p.id)
                .filter(|v| !v.is_null() || p.is_reference())
                .unwrap_or(&p.default);
            (p.id.clone(), value.clone())
        })
        .collect())
}

/// Applies `source.set_settings` and returns its inverse: the patched parameters'
/// previous values.
fn set_settings(
    state: &mut State,
    kinds: &Kinds,
    source_id: &SourceId,
    patch: &Settings,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let source = state
        .source(source_id)
        .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str()))?;
    let descriptor = match &source.kind {
        SourceKind::Builtin { kind } => kinds.get(kind).ok_or_else(|| {
            invalid(
                at,
                "source_id",
                format!("kind `{kind}` is not available, so its settings can't be checked"),
            )
        })?,
        _ if patch.is_empty() => {
            return Ok(vec![Edit::SourceSetSettings {
                source_id: source_id.clone(),
                patch: Settings::new(),
            }]);
        }
        _ => return Err(invalid(at, "patch", "a `scene` source has no settings")),
    };
    let mut new = Vec::new();
    for (id, value) in patch.iter() {
        let field = format!("patch.{id}");
        let param = descriptor.param(id).ok_or_else(|| {
            invalid(
                at,
                &field,
                format!("is not a parameter of kind `{}`", descriptor.kind_id),
            )
        })?;
        let value = if value.is_null() && !param.is_reference() {
            &param.default
        } else {
            check_value(state, kinds, source_id, param, value, at, &field)?;
            value
        };
        new.push((id.to_owned(), value.clone()));
    }
    let source = state
        .sources
        .get_mut(source_id)
        .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str()))?;
    let old: Settings = new
        .into_iter()
        .map(|(id, value)| {
            let old = source.settings.insert(id.clone(), value);
            (id, old.unwrap_or(Value::Null))
        })
        .collect();
    check_reference_cycles(state, source_id, at, "patch")?;
    Ok(vec![Edit::SourceSetSettings {
        source_id: source_id.clone(),
        patch: old,
    }])
}

/// Checks one settings value for `param` of source `source_id`: its type and
/// constraints, that a referenced scene or source exists (`not_found` if not), and that a
/// referenced source produces what the parameter needs. A source may name itself here;
/// [`check_reference_cycles`] rejects that as a cycle.
fn check_value(
    state: &State,
    kinds: &Kinds,
    source_id: &SourceId,
    param: &ParamDescriptor,
    value: &Value,
    at: usize,
    field: &str,
) -> Result<(), ProtocolError> {
    param
        .check(value)
        .map_err(|reason| invalid(at, field, reason))?;
    match Reference::from_value(value) {
        Some(Reference::Scene { scene_id }) => {
            find_scene(state, &scene_id)?;
        }
        Some(Reference::Source { source_id: target }) if &target != source_id => {
            let referenced = state
                .source(&target)
                .ok_or_else(|| not_found(EntityKind::Source, target.as_str()))?;
            if let ParamType::Source { media: Some(media) } = &param.ty {
                let (video, audio) = match &referenced.kind {
                    SourceKind::Builtin { kind } => kinds
                        .get(kind)
                        .map_or((false, false), |k| (k.video, k.audio)),
                    _ => (true, false),
                };
                let (ok, what) = match media {
                    Media::Video => (video, "video"),
                    Media::Audio => (audio, "audio"),
                    _ => (false, "that media"),
                };
                if !ok {
                    return Err(invalid(
                        at,
                        field,
                        format!("source `{target}` doesn't produce {what}"),
                    ));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Rejects a scene or source reference in `source_id`'s settings that closes a cycle,
/// naming the parameter in `field_prefix.<id>`.
fn check_reference_cycles(
    state: &State,
    source_id: &SourceId,
    at: usize,
    field_prefix: &str,
) -> Result<(), ProtocolError> {
    let Some(source) = state.source(source_id) else {
        return Ok(());
    };
    let from = Node::Source(source_id.clone());
    for (param, reference) in source.settings.references() {
        if let Some(to) = deps::reference_node(&reference)
            && let Some(cycle) = deps::cycle(state, &from, &to)
        {
            return Err(invalid(
                at,
                &format!("{field_prefix}.{param}"),
                format!("the reference to {to} would create a cycle: {cycle}"),
            ));
        }
    }
    Ok(())
}
