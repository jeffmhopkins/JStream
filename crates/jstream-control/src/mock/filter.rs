//! The mock's filter chains (ADR-0026, docs/adr/0026-source-filter-chains.md): the
//! `filter.*` edits with the checks `jstream-graph` makes that need no kind descriptor,
//! in the same order and with the same errors. Like the rest of the mock it describes no
//! kinds, so it doesn't check a filter's kind, or whether a builtin source has the chain
//! (a `scene` source has no audio chain, and that it does check), and stores settings as
//! given.

use super::{
    Collection, already_exists, check_id, check_index, invalid, not_found, unimplemented_by_mock,
    wire_index,
};
use jstream_protocol::{
    Edit, EntityKind, FilterChain, FilterChains, FilterId, FilterInfo, FilterOwner,
    MAX_CHAIN_FILTERS, ProtocolError, Settings, SourceId, SourceKind,
};

impl Collection {
    /// Applies a `filter.*` edit and returns the edits that undo it.
    pub(super) fn apply_filter(
        &mut self,
        edit: Edit,
        at: usize,
    ) -> Result<Vec<Edit>, ProtocolError> {
        match edit {
            Edit::FilterAdd {
                owner,
                chain,
                filter_id,
                kind,
                settings,
                enabled,
                index,
            } => self.add_filter(
                owner,
                chain,
                FilterInfo {
                    filter_id,
                    kind,
                    enabled: enabled.unwrap_or(true),
                    settings,
                },
                index,
                at,
            ),
            Edit::FilterRemove { owner, filter_id } => self.remove_filter(&owner, &filter_id),
            Edit::FilterSetEnabled {
                owner,
                filter_id,
                enabled,
            } => self.find_filter(&owner, &filter_id).map(|filter| {
                let old = std::mem::replace(&mut filter.enabled, enabled);
                vec![Edit::FilterSetEnabled {
                    owner,
                    filter_id,
                    enabled: old,
                }]
            }),
            Edit::FilterSetSettings {
                owner,
                filter_id,
                patch,
            } => self.find_filter(&owner, &filter_id).map(|filter| {
                // As `source.set_settings`: `null` removes the entry.
                let mut old = Settings::new();
                for (id, value) in patch.0 {
                    let before = if value.is_null() {
                        filter.settings.0.remove(&id)
                    } else {
                        filter.settings.0.insert(id.clone(), value)
                    };
                    old.0.insert(id, before.unwrap_or_default());
                }
                vec![Edit::FilterSetSettings {
                    owner,
                    filter_id,
                    patch: old,
                }]
            }),
            Edit::FilterReorder {
                owner,
                filter_id,
                index,
            } => self.reorder_filter(owner, filter_id, index, at),
            other => Err(unimplemented_by_mock(other.name())),
        }
    }

    fn chains(&mut self, owner: &FilterOwner) -> Result<&mut FilterChains, ProtocolError> {
        let source_id = owner.source_id();
        self.sources
            .get_mut(source_id)
            .map(|source| &mut source.filters)
            .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str()))
    }

    fn add_filter(
        &mut self,
        owner: FilterOwner,
        chain: FilterChain,
        filter: FilterInfo,
        index: Option<u32>,
        at: usize,
    ) -> Result<Vec<Edit>, ProtocolError> {
        let source_id = owner.source_id().clone();
        let is_scene = self
            .sources
            .get(&source_id)
            .is_some_and(|s| matches!(s.kind, SourceKind::Scene { .. }));
        let chains = self.chains(&owner)?;
        check_id(filter.filter_id.as_str(), at, "filter_id")?;
        if position(chains, &filter.filter_id).is_some() {
            return Err(already_exists(
                EntityKind::Filter,
                filter.filter_id.as_str(),
            ));
        }
        if is_scene && chain == FilterChain::Audio {
            return Err(invalid(
                at,
                "chain",
                format!("source `{source_id}` has no audio chain"),
            ));
        }
        let filters = chain_mut(chains, chain);
        if filters.len() >= MAX_CHAIN_FILTERS {
            return Err(invalid(
                at,
                "chain",
                format!("a chain holds at most {MAX_CHAIN_FILTERS} filters"),
            ));
        }
        let at_index = match index {
            None => filters.len(),
            Some(index) => check_index(index, filters.len() + 1, at, "positions in the chain")?,
        };
        let filter_id = filter.filter_id.clone();
        filters.insert(at_index, filter);
        Ok(vec![Edit::FilterRemove { owner, filter_id }])
    }

    fn find_filter(
        &mut self,
        owner: &FilterOwner,
        filter_id: &FilterId,
    ) -> Result<&mut FilterInfo, ProtocolError> {
        let chains = self.chains(owner)?;
        let (chain, index) = position(chains, filter_id)
            .ok_or_else(|| not_found(EntityKind::Filter, filter_id.as_str()))?;
        Ok(&mut chain_mut(chains, chain)[index])
    }

    fn remove_filter(
        &mut self,
        owner: &FilterOwner,
        filter_id: &FilterId,
    ) -> Result<Vec<Edit>, ProtocolError> {
        let chains = self.chains(owner)?;
        let (chain, index) = position(chains, filter_id)
            .ok_or_else(|| not_found(EntityKind::Filter, filter_id.as_str()))?;
        let filters = chain_mut(chains, chain);
        let filter = filters.remove(index);
        let at = (index != filters.len())
            .then(|| wire_index(index))
            .transpose()?;
        Ok(vec![restore(owner.source_id(), chain, filter, at)])
    }

    fn reorder_filter(
        &mut self,
        owner: FilterOwner,
        filter_id: FilterId,
        index: u32,
        at: usize,
    ) -> Result<Vec<Edit>, ProtocolError> {
        let chains = self.chains(&owner)?;
        let (chain, from) = position(chains, &filter_id)
            .ok_or_else(|| not_found(EntityKind::Filter, filter_id.as_str()))?;
        let filters = chain_mut(chains, chain);
        let to = check_index(index, filters.len(), at, "filters in the chain")?;
        let filter = filters.remove(from);
        filters.insert(to, filter);
        Ok(vec![Edit::FilterReorder {
            owner,
            filter_id,
            index: wire_index(from)?,
        }])
    }
}

/// The edits that re-create a deleted source's filters after its `source.create`, video
/// chain first.
pub(super) fn restore_all(source_id: &SourceId, chains: FilterChains) -> Vec<Edit> {
    let video = chains
        .video
        .into_iter()
        .map(|f| restore(source_id, FilterChain::Video, f, None));
    let audio = chains
        .audio
        .into_iter()
        .map(|f| restore(source_id, FilterChain::Audio, f, None));
    video.chain(audio).collect()
}

fn restore(
    source_id: &SourceId,
    chain: FilterChain,
    filter: FilterInfo,
    index: Option<u32>,
) -> Edit {
    Edit::FilterAdd {
        owner: FilterOwner::Source {
            source_id: source_id.clone(),
        },
        chain,
        filter_id: filter.filter_id,
        kind: filter.kind,
        settings: filter.settings,
        enabled: (!filter.enabled).then_some(false),
        index,
    }
}

/// The chain and index of `filter_id`.
fn position(chains: &FilterChains, filter_id: &FilterId) -> Option<(FilterChain, usize)> {
    [FilterChain::Video, FilterChain::Audio]
        .into_iter()
        .find_map(|chain| {
            chains
                .chain(chain)
                .iter()
                .position(|f| f.filter_id == *filter_id)
                .map(|index| (chain, index))
        })
}

fn chain_mut(chains: &mut FilterChains, chain: FilterChain) -> &mut Vec<FilterInfo> {
    match chain {
        FilterChain::Video => &mut chains.video,
        FilterChain::Audio => &mut chains.audio,
    }
}
