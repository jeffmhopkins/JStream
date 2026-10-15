//! The source kinds the engine describes (`kind.list`, `kind.describe`) and validates
//! settings against (ADR-0025, docs/adr/0025-typed-settings-kind-descriptors.md): the
//! descriptors of `jstream-sources`' built-in registry.
//!
//! Frames still come from `Engine::set_source_frame`; rendering sources from the
//! registry is the next step.

use jstream_protocol::KindDescriptor;

/// Every built-in source kind, ordered by ID.
pub(crate) fn builtin() -> Vec<KindDescriptor> {
    jstream_sources::Registry::builtin().descriptors()
}

#[cfg(test)]
mod tests {
    use super::builtin;
    use jstream_protocol::parity;
    use std::collections::BTreeSet;

    #[test]
    fn ids_are_sorted_and_unique_and_defaults_are_valid() {
        let kinds = builtin();
        let ids: Vec<&str> = kinds.iter().map(|k| k.kind_id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(ids, sorted);
        for kind in &kinds {
            let params: BTreeSet<&str> = kind.params.iter().map(|p| p.id.as_str()).collect();
            assert_eq!(params.len(), kind.params.len(), "{}", kind.kind_id);
            for p in &kind.params {
                assert_eq!(p.check(&p.default), Ok(()), "{}.{}", kind.kind_id, p.id);
            }
        }
    }

    /// The shared parity collection needs its kinds described exactly as the fixture
    /// does, so every implementation reports the same settings. Its audio kind, `mic`, is
    /// not a built-in kind; the engine's parity tests add it themselves.
    #[test]
    fn parity_kinds_are_builtin() {
        let kinds = builtin();
        for kind in parity::kinds().into_iter().filter(|k| !k.audio) {
            assert!(kinds.contains(&kind), "{}", kind.kind_id);
        }
    }
}
