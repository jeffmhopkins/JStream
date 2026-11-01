//! The source and filter kinds the engine describes (`kind.list`, `kind.describe`) and validates
//! settings against (ADR-0025, docs/adr/0025-typed-settings-kind-descriptors.md): the
//! descriptors of `jstream-sources`' built-in registry, the audio filter kinds of
//! [`crate::filters`], and `jstream-render`'s built-in video filter kinds, which
//! [`crate::chain`] runs. The effect packs' video filter kinds are added as their packs
//! load ([`crate::effect_packs`]), and the CLAP kinds after a scan.
//!
//! Frames still come from `Engine::set_source_frame`; rendering sources from the
//! registry is the next step.

use jstream_protocol::KindDescriptor;

/// Every built-in kind, ordered by ID: the source kinds, the audio filter kinds and the
/// video filter kinds (ADR-0026), merged.
pub(crate) fn builtin() -> Vec<KindDescriptor> {
    let mut kinds = jstream_sources::Registry::builtin().descriptors();
    kinds.extend(crate::filters::descriptors());
    kinds.extend(jstream_render::video_filter_descriptors());
    kinds.sort_by(|a, b| a.kind_id.cmp(&b.kind_id));
    kinds
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

    /// Every built-in kind's descriptor, as `kind.list` serves it, snapshot-tested
    /// (ADR-0025, "Kinds get their own snapshot"; #635): adding or changing a kind changes
    /// no protocol schema, so this diff is where the audio filter kinds' settings, which
    /// no other crate snapshots, are reviewed like an API. Regenerate it with
    /// `INSTA_UPDATE=always cargo test -p jstream-core --lib kinds`.
    #[test]
    fn builtin_kinds_snapshot() {
        let kinds = serde_json::to_string_pretty(&builtin()).expect("descriptors serialize");
        insta::with_settings!({
            snapshot_path => "../schema",
            prepend_module_to_snapshot => false,
            omit_expression => true,
        }, {
            insta::assert_snapshot!("kinds", kinds);
        });
    }
}
