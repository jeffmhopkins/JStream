//! Filter chains on sources: the `filter.*` edits (ADR-0026,
//! docs/adr/0026-source-filter-chains.md).
//!
//! A source has a video chain and an audio chain. A filter's kind is a filter kind of its
//! chain, and its settings are checked and completed against the kind's descriptor
//! exactly as a source's are (ADR-0025). Scene and source references in a filter's
//! settings are edges of the filter's source ([`crate::deps`]), so the edits that set
//! them run the cycle check.

use crate::apply::{
    Kinds, already_exists, check_id, check_index, check_value, complete_settings, invalid,
    not_found, wire_index,
};
use crate::deps::{self, Node};
use crate::model::{Filter, MAX_CHAIN_FILTERS, Source, State};
use jstream_protocol::{
    Edit, EntityKind, FilterChain, FilterId, FilterOwner, KindCategory, KindDescriptor,
    ProtocolError, Settings, SourceId, SourceKind,
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
    source
        .filters
        .all()
        .map(|(chain, filter)| restore(&source.id, chain, filter, None))
        .collect()
}

/// The `filter.add` that puts `filter` back on `chain` of `source_id`, at `index` (`None`
/// for last).
fn restore(source_id: &SourceId, chain: FilterChain, filter: &Filter, index: Option<u32>) -> Edit {
    Edit::FilterAdd {
        owner: FilterOwner::Source {
            source_id: source_id.clone(),
        },
        chain,
        filter_id: filter.id.clone(),
        kind: filter.kind.clone(),
        settings: filter.settings.clone(),
        enabled: (!filter.enabled).then_some(false),
        index,
    }
}

fn owner_source<'a>(state: &'a State, owner: &FilterOwner) -> Result<&'a Source, ProtocolError> {
    let source_id = owner.source_id();
    state
        .source(source_id)
        .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str()))
}

/// Whether `source` has `chain`: video if its kind produces video, audio if it produces
/// audio and is not a `scene` source (ADR-0026 §1). A kind the graph doesn't describe has
/// neither.
fn has_chain(kinds: &Kinds, source: &Source, chain: FilterChain) -> bool {
    let (video, audio) = match &source.kind {
        SourceKind::Builtin { kind } => kinds
            .get(kind)
            .map_or((false, false), |k| (k.video, k.audio)),
        _ => (true, false),
    };
    match chain {
        FilterChain::Video => video,
        FilterChain::Audio => audio,
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
    let source = owner_source(state, owner)?;
    let source_id = source.id.clone();
    check_id(filter_id.as_str(), at, "filter_id")?;
    if source.filters.find(filter_id).is_some() {
        return Err(already_exists(EntityKind::Filter, filter_id.as_str()));
    }
    if !has_chain(kinds, source, chain) {
        return Err(invalid(
            at,
            "chain",
            format!("source `{source_id}` has no {} chain", chain.as_str()),
        ));
    }
    let len = source.filters.chain(chain).len();
    if len >= MAX_CHAIN_FILTERS {
        return Err(invalid(
            at,
            "chain",
            format!("a chain holds at most {MAX_CHAIN_FILTERS} filters"),
        ));
    }
    let descriptor = filter_kind(kinds, kind, chain, at)?;
    let settings = complete_settings(state, kinds, descriptor, &source_id, settings, at)?;
    let position = match index {
        None => len,
        Some(index) => check_index(index, len + 1, at, "positions in the chain")?,
    };
    source_mut(state, &source_id)?
        .filters
        .chain_mut(chain)
        .insert(
            position,
            Filter {
                id: filter_id.clone(),
                kind: kind.clone(),
                enabled,
                settings,
            },
        );
    check_cycles(state, &source_id, filter_id, at, "settings")?;
    Ok(vec![Edit::FilterRemove {
        owner: owner.clone(),
        filter_id: filter_id.clone(),
    }])
}

fn source_mut<'a>(state: &'a mut State, id: &SourceId) -> Result<&'a mut Source, ProtocolError> {
    state
        .sources
        .get_mut(id)
        .ok_or_else(|| not_found(EntityKind::Source, id.as_str()))
}

/// The chain and index of `filter_id` on `owner`'s source, or `not_found`.
fn find(
    state: &State,
    owner: &FilterOwner,
    filter_id: &FilterId,
) -> Result<(FilterChain, usize), ProtocolError> {
    let source = owner_source(state, owner)?;
    source
        .filters
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
    let filters = source_mut(state, owner.source_id())?
        .filters
        .chain_mut(chain);
    let filter = filters.remove(index);
    // `filter.add` without an index puts it last, which is `len` now that it is gone.
    let at = (index != filters.len())
        .then(|| wire_index(index))
        .transpose()?;
    Ok(vec![restore(owner.source_id(), chain, &filter, at)])
}

fn set_enabled(
    state: &mut State,
    owner: &FilterOwner,
    filter_id: &FilterId,
    enabled: bool,
) -> Result<Vec<Edit>, ProtocolError> {
    let (chain, index) = find(state, owner, filter_id)?;
    let filter = &mut source_mut(state, owner.source_id())?
        .filters
        .chain_mut(chain)[index];
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
    let kind = &owner_source(state, owner)?.filters.chain(chain)[index].kind;
    let descriptor = kinds.get(kind).ok_or_else(|| {
        invalid(
            at,
            "filter_id",
            format!("kind `{kind}` is not available, so its settings can't be checked"),
        )
    })?;
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
    let filter = &mut source_mut(state, source_id)?.filters.chain_mut(chain)[index];
    let old: Settings = new
        .into_iter()
        .map(|(id, value)| {
            let old = filter.settings.insert(id.clone(), value);
            (id, old.unwrap_or(Value::Null))
        })
        .collect();
    check_cycles(state, source_id, filter_id, at, "patch")?;
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
    let filters = source_mut(state, owner.source_id())?
        .filters
        .chain_mut(chain);
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
/// cycle through its source, naming the parameter as `field_prefix.<id>`.
fn check_cycles(
    state: &State,
    source_id: &SourceId,
    filter_id: &FilterId,
    at: usize,
    field_prefix: &str,
) -> Result<(), ProtocolError> {
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
