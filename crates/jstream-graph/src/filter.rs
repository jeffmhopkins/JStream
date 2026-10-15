//! Filter chains: the `filter.*` edits (ADR-0026, docs/adr/0026-source-filter-chains.md).
//!
//! A source has a video chain and an audio chain. The master stage and each output that
//! carries audio have an audio chain (ADR-0027); their video chains wait for video mixes.
//! A filter's kind is a filter kind of its chain, and its settings are checked and
//! completed against the kind's descriptor exactly as a source's are (ADR-0025). Scene
//! and source references in a source's filter settings are edges of that source
//! ([`crate::deps`]), so the edits that set them run the cycle check. The master's and
//! outputs' filters take no references yet: nothing would keep what they name from
//! being deleted.

use crate::apply::{
    Kinds, already_exists, check_id, check_index, check_value, complete_settings, invalid,
    not_found, wire_index,
};
use crate::deps::{self, Node};
use crate::model::{Chains, Filter, MAX_CHAIN_FILTERS, Output, Source, State};
use jstream_protocol::{
    Edit, EntityKind, FilterChain, FilterId, FilterOwner, KindCategory, KindDescriptor,
    ProtocolError, Reference, Settings, SourceKind,
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

/// `owner`'s chains, or `not_found` for a source or output that doesn't exist.
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
/// The master stage and an output that carries audio have an audio chain; their video
/// chains wait for video mixes (ADR-0027).
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
        (FilterOwner::Master | FilterOwner::Output { .. }, FilterChain::Video) => missing(
            "the master stage's and outputs' video chains are not available yet; only \
             their audio chains hold filters"
                .into(),
        ),
        (FilterOwner::Output { output_id }, FilterChain::Audio) => match state.output(output_id) {
            None => Err(not_found(EntityKind::Output, output_id.as_str())),
            Some(output) if output.has_audio() => Ok(()),
            Some(_) => missing(format!(
                "output `{output_id}` carries no audio, so it has no audio chain"
            )),
        },
        (FilterOwner::Master, FilterChain::Audio) => Ok(()),
        _ => Err(unknown_owner()),
    }
}

/// Refuses a scene or source reference in the settings of a master or output filter,
/// naming the parameter as `field_prefix.<id>`. Only a source's filters may refer to
/// something: the delete checks follow a source's edges, not the master's or an
/// output's.
fn check_no_references(
    owner: &FilterOwner,
    settings: &Settings,
    at: usize,
    field_prefix: &str,
) -> Result<(), ProtocolError> {
    if owner.source_id().is_some() {
        return Ok(());
    }
    match settings
        .iter()
        .find(|(_, value)| Reference::from_value(value).is_some())
    {
        Some((id, _)) => Err(invalid(
            at,
            &format!("{field_prefix}.{id}"),
            "a master or output filter can't refer to a scene or source yet",
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
/// cycle through its source, naming the parameter as `field_prefix.<id>`. Only a
/// source's filters may refer to anything ([`check_no_references`]).
fn check_cycles(
    state: &State,
    owner: &FilterOwner,
    filter_id: &FilterId,
    at: usize,
    field_prefix: &str,
) -> Result<(), ProtocolError> {
    let Some(source_id) = owner.source_id() else {
        return Ok(());
    };
    let Some((_, _, filter)) = state
        .source(source_id)
        .and_then(|s| s.filters.find(filter_id))
    else {
        return Ok(());
    };
    let from = Node::Source(source_id.clone());
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
