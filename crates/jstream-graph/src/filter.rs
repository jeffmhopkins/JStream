//! Filter chains: the `filter.*` edits (ADR-0026, docs/adr/0026-source-filter-chains.md).
//!
//! A source has a video chain and an audio chain. The master stage and every output have
//! a video chain, and the master stage and each output that carries audio an audio
//! chain (ADR-0080).
//! A filter's kind is a filter kind of its chain, and its settings are checked and
//! completed against the kind's descriptor exactly as a source's are (ADR-0025). Scene
//! and source references in a source's filter settings are edges of that source
//! ([`crate::deps`]), so the edits that set them run the cycle check. The master's and
//! outputs' filters take no references yet: nothing would keep what they name from
//! being deleted.
//!
//! A scene and an item own a video chain each (ADR-0065): a scene's filters and an
//! item's effects. A keyframe scene owns none, since it shows its parent's. References in
//! them are edges of the scene, or of the item's scene, so they run the cycle check and
//! block deletes as a source's do.

use crate::apply::{
    Kinds, already_exists, check_id, check_index, check_value, complete_settings, invalid,
    not_found, wire_index,
};
use crate::deps::{self, Node};
use crate::model::{Chains, Filter, Item, MAX_CHAIN_FILTERS, Output, Scene, Source, State};
use jstream_protocol::{
    Edit, EntityKind, FilterChain, FilterId, FilterOwner, KindCategory, KindDescriptor,
    ProtocolError, Reference, SceneId, Settings, SourceKind,
};
use serde_json::Value;

/// Applies a `filter.*` edit and returns the edits that undo it. `None` for any other
/// edit.
pub(crate) fn apply(
    state: &mut State,
    kinds: &Kinds,
    edit: &Edit,
    at: usize,
) -> Option<Result<Vec<Edit>, ProtocolError>> {
    if let Some(owner) = edit_owner(edit)
        && let Err(error) = check_not_variant(state, owner, at)
    {
        return Some(Err(error));
    }
    Some(match edit {
        Edit::FilterAdd { .. } => add(state, kinds, edit, at),
        Edit::FilterRemove { owner, filter_id } => remove(state, owner, filter_id),
        Edit::FilterSetEnabled {
            owner,
            filter_id,
            enabled,
        } => set_enabled(state, owner, filter_id, *enabled),
        Edit::FilterSetSettings {
            owner,
            filter_id,
            patch,
        } => set_settings(state, kinds, owner, filter_id, patch, at),
        Edit::FilterReorder {
            owner,
            filter_id,
            index,
        } => reorder(state, owner, filter_id, *index, at),
        _ => return None,
    })
}

/// The owner a `filter.*` edit addresses, or `None` for any other edit.
fn edit_owner(edit: &Edit) -> Option<&FilterOwner> {
    match edit {
        Edit::FilterAdd { owner, .. }
        | Edit::FilterRemove { owner, .. }
        | Edit::FilterSetEnabled { owner, .. }
        | Edit::FilterSetSettings { owner, .. }
        | Edit::FilterReorder { owner, .. } => Some(owner),
        _ => None,
    }
}

/// Refuses a keyframe scene as the owner, for every filter edit, with `invalid_argument`
/// on `owner`: it shows its parent's filters and has none of its own (ADR-0065 §3).
fn check_not_variant(state: &State, owner: &FilterOwner, at: usize) -> Result<(), ProtocolError> {
    if let FilterOwner::Scene { scene_id } = owner
        && let Some(parent) = state.scene(scene_id).and_then(|s| s.variant_of.as_ref())
    {
        return Err(invalid(
            at,
            "owner",
            format!(
                "scene `{scene_id}` is a keyframe scene of `{parent}`: it shows its \
                 parent's filters and has none of its own"
            ),
        ));
    }
    Ok(())
}

/// The edits that re-create `scene`'s filters after `scene.create`: what
/// `scene.delete`'s inverse appends.
pub(crate) fn restore_scene(scene: &Scene) -> Vec<Edit> {
    let owner = FilterOwner::Scene {
        scene_id: scene.id.clone(),
    };
    restore_chains(&owner, &scene.filters)
}

/// The edits that re-create `item`'s effects after `item.add`: what `item.remove`'s
/// inverse appends.
pub(crate) fn restore_effects(item: &Item) -> Vec<Edit> {
    let owner = FilterOwner::Item {
        item_id: item.id.clone(),
    };
    restore_chains(&owner, &item.effects)
}

/// The scene whose edges a filter on `owner` adds to: a scene's own, an item's scene;
/// `None` for any other owner.
fn owner_scene<'a>(state: &'a State, owner: &'a FilterOwner) -> Option<&'a SceneId> {
    match owner {
        FilterOwner::Scene { scene_id } => Some(scene_id),
        FilterOwner::Item { item_id } => state
            .item_index(item_id)
            .map(|(si, _)| &state.scenes[si].id),
        _ => None,
    }
}

/// The first reference in `item`'s effects that would close a cycle if the item sat in
/// scene `scene_id`, as the parameter (`effects.<filter_id>.<param>`) and the cycle: what
/// `item.move` checks, since its effects' references become edges of the new scene.
pub(crate) fn effects_cycle(
    state: &State,
    item: &Item,
    scene_id: &SceneId,
) -> Option<(String, String)> {
    let from = Node::Scene(scene_id.clone());
    item.effects.all().find_map(|(_, filter)| {
        filter.settings.references().find_map(|(param, reference)| {
            let to = deps::reference_node(&reference)?;
            let cycle = deps::cycle(state, &from, &to)?;
            Some((format!("effects.{}.{param}", filter.id), cycle))
        })
    })
}

/// The edits that re-create `source`'s filters after `source.create`, video chain first:
/// what `source.delete`'s inverse appends.
pub(crate) fn restore_all(source: &Source) -> Vec<Edit> {
    let owner = FilterOwner::Source {
        source_id: source.id.clone(),
    };
    restore_chains(&owner, &source.filters)
}

/// The edits that re-create `output`'s filters after `output.create`: what
/// `output.delete`'s inverse appends.
pub(crate) fn restore_output(output: &Output) -> Vec<Edit> {
    let owner = FilterOwner::Output {
        output_id: output.id.clone(),
    };
    restore_chains(&owner, &output.filters)
}

/// The edits that re-create `chains` on `owner`, video chain first.
fn restore_chains(owner: &FilterOwner, chains: &Chains) -> Vec<Edit> {
    chains
        .all()
        .map(|(chain, filter)| restore(owner, chain, filter, None))
        .collect()
}

/// The `filter.add` that puts `filter` back on `chain` of `owner`, at `index` (`None` for
/// last).
fn restore(owner: &FilterOwner, chain: FilterChain, filter: &Filter, index: Option<u32>) -> Edit {
    Edit::FilterAdd {
        owner: owner.clone(),
        chain,
        filter_id: filter.id.clone(),
        kind: filter.kind.clone(),
        settings: filter.settings.clone(),
        enabled: (!filter.enabled).then_some(false),
        index,
    }
}

/// `owner`'s chains, or `not_found` for a source, output, scene or item that doesn't
/// exist.
fn chains<'a>(state: &'a State, owner: &FilterOwner) -> Result<&'a Chains, ProtocolError> {
    match owner {
        FilterOwner::Source { source_id } => state
            .source(source_id)
            .map(|source| &source.filters)
            .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str())),
        FilterOwner::Output { output_id } => state
            .output(output_id)
            .map(|output| &output.filters)
            .ok_or_else(|| not_found(EntityKind::Output, output_id.as_str())),
        FilterOwner::Master => Ok(&state.master),
        FilterOwner::Scene { scene_id } => state
            .scene(scene_id)
            .map(|scene| &scene.filters)
            .ok_or_else(|| not_found(EntityKind::Scene, scene_id.as_str())),
        FilterOwner::Item { item_id } => state
            .item_index(item_id)
            .map(|(si, ii)| &state.scenes[si].items[ii].effects)
            .ok_or_else(|| not_found(EntityKind::Item, item_id.as_str())),
        _ => Err(unknown_owner()),
    }
}

fn chains_mut<'a>(
    state: &'a mut State,
    owner: &FilterOwner,
) -> Result<&'a mut Chains, ProtocolError> {
    match owner {
        FilterOwner::Source { source_id } => state
            .sources
            .get_mut(source_id)
            .map(|source| &mut source.filters)
            .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str())),
        FilterOwner::Output { output_id } => state
            .outputs
            .get_mut(output_id)
            .map(|output| &mut output.filters)
            .ok_or_else(|| not_found(EntityKind::Output, output_id.as_str())),
        FilterOwner::Master => Ok(&mut state.master),
        FilterOwner::Scene { scene_id } => match state.scene_index(scene_id) {
            Some(si) => Ok(&mut state.scenes[si].filters),
            None => Err(not_found(EntityKind::Scene, scene_id.as_str())),
        },
        FilterOwner::Item { item_id } => match state.item_index(item_id) {
            Some((si, ii)) => Ok(&mut state.scenes[si].items[ii].effects),
            None => Err(not_found(EntityKind::Item, item_id.as_str())),
        },
        _ => Err(unknown_owner()),
    }
}

/// An owner added to the protocol after this graph was built.
fn unknown_owner() -> ProtocolError {
    ProtocolError::Internal {
        reason: "a filter owner this graph doesn't know".into(),
    }
}

/// Checks that `owner` has `chain`, or `invalid_argument` on `chain`. A source has a
/// video chain if its kind produces video, and an audio chain if it produces audio and
/// is not a `scene` source (ADR-0026 §1); a kind the graph doesn't describe has neither.
/// The master stage and every output have a video chain, and the master stage and an
/// output that carries audio an audio chain (ADR-0080). A scene and an item have a
/// video chain only (ADR-0065 §1).
fn check_chain(
    state: &State,
    kinds: &Kinds,
    owner: &FilterOwner,
    chain: FilterChain,
    at: usize,
) -> Result<(), ProtocolError> {
    let missing = |what: String| Err(invalid(at, "chain", what));
    match (owner, chain) {
        (FilterOwner::Source { source_id }, _) => {
            let Some(source) = state.source(source_id) else {
                return Err(not_found(EntityKind::Source, source_id.as_str()));
            };
            let (video, audio) = match &source.kind {
                SourceKind::Builtin { kind } => kinds
                    .get(kind)
                    .map_or((false, false), |k| (k.video, k.audio)),
                _ => (true, false),
            };
            let has = match chain {
                FilterChain::Video => video,
                FilterChain::Audio => audio,
            };
            if has {
                Ok(())
            } else {
                missing(format!(
                    "source `{source_id}` has no {} chain",
                    chain.as_str()
                ))
            }
        }
        (FilterOwner::Output { output_id }, FilterChain::Video) => match state.output(output_id) {
            None => Err(not_found(EntityKind::Output, output_id.as_str())),
            Some(_) => Ok(()),
        },
        (FilterOwner::Master, FilterChain::Video) => Ok(()),
        (FilterOwner::Output { output_id }, FilterChain::Audio) => match state.output(output_id) {
            None => Err(not_found(EntityKind::Output, output_id.as_str())),
            Some(output) if output.has_audio() => Ok(()),
            Some(_) => missing(format!(
                "output `{output_id}` carries no audio, so it has no audio chain"
            )),
        },
        (FilterOwner::Master, FilterChain::Audio) => Ok(()),
        (FilterOwner::Scene { .. } | FilterOwner::Item { .. }, FilterChain::Video) => Ok(()),
        (FilterOwner::Scene { scene_id }, FilterChain::Audio) => missing(format!(
            "scene `{scene_id}` has no audio chain: a scene's audio is its bus's"
        )),
        (FilterOwner::Item { item_id }, FilterChain::Audio) => missing(format!(
            "item `{item_id}` has no audio chain: its effects are video only"
        )),
        _ => Err(unknown_owner()),
    }
}

/// Refuses any reference (a scene, source, bus or asset) in the settings of a master
/// or output filter, naming the parameter as `field_prefix.<id>`. Only a source's,
/// scene's or item's filters may refer to something: the delete checks follow their
/// edges, not the master's or an output's.
fn check_no_references(
    owner: &FilterOwner,
    settings: &Settings,
    at: usize,
    field_prefix: &str,
) -> Result<(), ProtocolError> {
    if matches!(
        owner,
        FilterOwner::Source { .. } | FilterOwner::Scene { .. } | FilterOwner::Item { .. }
    ) {
        return Ok(());
    }
    match settings
        .iter()
        .find(|(_, value)| Reference::from_value(value).is_some())
    {
        Some((id, _)) => Err(invalid(
            at,
            &format!("{field_prefix}.{id}"),
            "a master or output filter can't hold a reference yet",
        )),
        None => Ok(()),
    }
}

/// The filter kind `kind` for `chain`, or `invalid_argument` on `kind`.
fn filter_kind<'a>(
    kinds: &'a Kinds,
    kind: &str,
    chain: FilterChain,
    at: usize,
) -> Result<&'a KindDescriptor, ProtocolError> {
    let wanted = match chain {
        FilterChain::Video => KindCategory::VideoFilter,
        FilterChain::Audio => KindCategory::AudioFilter,
    };
    match kinds.get(kind) {
        Some(descriptor) if descriptor.category == wanted => Ok(descriptor),
        Some(_) => Err(invalid(
            at,
            "kind",
            format!(
                "kind `{kind}` is not a filter kind for the {} chain",
                chain.as_str()
            ),
        )),
        None => Err(invalid(
            at,
            "kind",
            format!("there is no kind `{kind}`; `kind.list` names them"),
        )),
    }
}

/// Puts a filter whose kind this graph doesn't describe on its chain, as a collection
/// file saved it: a pack kind that isn't installed, or a CLAP kind before the scan that
/// describes it (ADR-0035). Its settings are kept verbatim and unchecked, and
/// `kind_version` is kept for the next save, so loading in a build without the kind
/// destroys nothing (ADR-0025 › Versioning a kind). The engine reports such a filter as
/// an error and bypasses it until its kind appears. Only a load does this: `filter.add`
/// refuses a kind it can't check.
pub(crate) fn add_unknown_kind(
    state: &mut State,
    kinds: &Kinds,
    edit: &Edit,
    kind_version: Option<u32>,
) -> Result<(), ProtocolError> {
    let Edit::FilterAdd {
        owner,
        chain,
        filter_id,
        kind,
        settings,
        enabled,
        index: None,
    } = edit
    else {
        return Err(ProtocolError::Internal {
            reason: "invariant: a load adds filters with `filter.add`, last".into(),
        });
    };
    // As `filter.*` does: a keyframe scene shows its parent's filters (ADR-0065 §1).
    check_not_variant(state, owner, 0)?;
    let filters = chains(state, owner)?;
    check_id(filter_id.as_str(), 0, "filter_id")?;
    if filters.find(filter_id).is_some() {
        return Err(already_exists(EntityKind::Filter, filter_id.as_str()));
    }
    check_chain(state, kinds, owner, *chain, 0)?;
    if filters.chain(*chain).len() >= MAX_CHAIN_FILTERS {
        return Err(invalid(
            0,
            "chain",
            format!("a chain holds at most {MAX_CHAIN_FILTERS} filters"),
        ));
    }
    check_no_references(owner, settings, 0, "settings")?;
    // Unchecked settings may still name objects: each must exist, as a checked
    // reference's must, so deletes and the cycle check see a whole graph.
    for (param, reference) in settings.references() {
        if let Some(node) = deps::reference_node(&reference) {
            let exists = match &node {
                Node::Scene(id) => state.scene(id).is_some(),
                Node::Source(id) => state.source(id).is_some(),
                Node::Bus(id) => state.bus(id).is_some(),
            };
            if !exists {
                return Err(invalid(
                    0,
                    &format!("settings.{param}"),
                    format!("{node} doesn't exist"),
                ));
            }
        }
    }
    chains_mut(state, owner)?.chain_mut(*chain).push(Filter {
        id: filter_id.clone(),
        kind: kind.clone(),
        enabled: enabled.unwrap_or(true),
        settings: settings.clone(),
        unknown_kind_version: kind_version,
    });
    check_cycles(state, owner, filter_id, 0, "settings")
}

/// Applies `filter.add` (`edit`) and returns its inverse, `filter.remove`.
fn add(
    state: &mut State,
    kinds: &Kinds,
    edit: &Edit,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let Edit::FilterAdd {
        owner,
        chain,
        filter_id,
        kind,
        settings,
        enabled,
        index,
    } = edit
    else {
        return Err(ProtocolError::Internal {
            reason: "invariant: `add` is only called with `filter.add`".into(),
        });
    };
    let (chain, enabled, index) = (*chain, enabled.unwrap_or(true), *index);
    let filters = chains(state, owner)?;
    check_id(filter_id.as_str(), at, "filter_id")?;
    if filters.find(filter_id).is_some() {
        return Err(already_exists(EntityKind::Filter, filter_id.as_str()));
    }
    check_chain(state, kinds, owner, chain, at)?;
    let len = filters.chain(chain).len();
    if len >= MAX_CHAIN_FILTERS {
        return Err(invalid(
            at,
            "chain",
            format!("a chain holds at most {MAX_CHAIN_FILTERS} filters"),
        ));
    }
    let descriptor = filter_kind(kinds, kind, chain, at)?;
    check_no_references(owner, settings, at, "settings")?;
    let settings = complete_settings(state, kinds, descriptor, owner.source_id(), settings, at)?;
    let position = match index {
        None => len,
        Some(index) => check_index(index, len + 1, at, "positions in the chain")?,
    };
    chains_mut(state, owner)?.chain_mut(chain).insert(
        position,
        Filter {
            id: filter_id.clone(),
            kind: kind.clone(),
            enabled,
            settings,
            unknown_kind_version: None,
        },
    );
    check_cycles(state, owner, filter_id, at, "settings")?;
    Ok(vec![Edit::FilterRemove {
        owner: owner.clone(),
        filter_id: filter_id.clone(),
    }])
}

/// The chain and index of `filter_id` on `owner`, or `not_found`.
fn find(
    state: &State,
    owner: &FilterOwner,
    filter_id: &FilterId,
) -> Result<(FilterChain, usize), ProtocolError> {
    chains(state, owner)?
        .find(filter_id)
        .map(|(chain, index, _)| (chain, index))
        .ok_or_else(|| not_found(EntityKind::Filter, filter_id.as_str()))
}

fn remove(
    state: &mut State,
    owner: &FilterOwner,
    filter_id: &FilterId,
) -> Result<Vec<Edit>, ProtocolError> {
    let (chain, index) = find(state, owner, filter_id)?;
    let filters = chains_mut(state, owner)?.chain_mut(chain);
    let filter = filters.remove(index);
    // `filter.add` without an index puts it last, which is `len` now that it is gone.
    let at = (index != filters.len())
        .then(|| wire_index(index))
        .transpose()?;
    Ok(vec![restore(owner, chain, &filter, at)])
}

fn set_enabled(
    state: &mut State,
    owner: &FilterOwner,
    filter_id: &FilterId,
    enabled: bool,
) -> Result<Vec<Edit>, ProtocolError> {
    let (chain, index) = find(state, owner, filter_id)?;
    let filter = &mut chains_mut(state, owner)?.chain_mut(chain)[index];
    let old = std::mem::replace(&mut filter.enabled, enabled);
    Ok(vec![Edit::FilterSetEnabled {
        owner: owner.clone(),
        filter_id: filter_id.clone(),
        enabled: old,
    }])
}

/// Applies `filter.set_settings` with `source.set_settings`'s rules and returns its
/// inverse: the patched parameters' previous values.
fn set_settings(
    state: &mut State,
    kinds: &Kinds,
    owner: &FilterOwner,
    filter_id: &FilterId,
    patch: &Settings,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let (chain, index) = find(state, owner, filter_id)?;
    let source_id = owner.source_id();
    let kind = &chains(state, owner)?.chain(chain)[index].kind;
    let descriptor = kinds.get(kind).ok_or_else(|| {
        invalid(
            at,
            "filter_id",
            format!("kind `{kind}` is not available, so its settings can't be checked"),
        )
    })?;
    check_no_references(owner, patch, at, "patch")?;
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
    let filter = &mut chains_mut(state, owner)?.chain_mut(chain)[index];
    let old: Settings = new
        .into_iter()
        .map(|(id, value)| {
            let old = filter.settings.insert(id.clone(), value);
            (id, old.unwrap_or(Value::Null))
        })
        .collect();
    check_cycles(state, owner, filter_id, at, "patch")?;
    Ok(vec![Edit::FilterSetSettings {
        owner: owner.clone(),
        filter_id: filter_id.clone(),
        patch: old,
    }])
}

fn reorder(
    state: &mut State,
    owner: &FilterOwner,
    filter_id: &FilterId,
    index: u32,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let (chain, from) = find(state, owner, filter_id)?;
    let filters = chains_mut(state, owner)?.chain_mut(chain);
    let to = check_index(index, filters.len(), at, "filters in the chain")?;
    let filter = filters.remove(from);
    filters.insert(to, filter);
    Ok(vec![Edit::FilterReorder {
        owner: owner.clone(),
        filter_id: filter_id.clone(),
        index: wire_index(from)?,
    }])
}

/// Rejects a scene or source reference in filter `filter_id`'s settings that closes a
/// cycle through its owner's node, naming the parameter as `field_prefix.<id>`: the
/// source, or for a scene filter or an item effect the scene. Only those owners'
/// filters may refer to anything ([`check_no_references`]).
fn check_cycles(
    state: &State,
    owner: &FilterOwner,
    filter_id: &FilterId,
    at: usize,
    field_prefix: &str,
) -> Result<(), ProtocolError> {
    let from = match (owner.source_id(), owner_scene(state, owner)) {
        (Some(source_id), _) => Node::Source(source_id.clone()),
        (None, Some(scene_id)) => Node::Scene(scene_id.clone()),
        (None, None) => return Ok(()),
    };
    let Some((_, _, filter)) = chains(state, owner)
        .ok()
        .and_then(|chains| chains.find(filter_id))
    else {
        return Ok(());
    };
    for (param, reference) in filter.settings.references() {
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
