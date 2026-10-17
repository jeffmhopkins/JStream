//! Filter chains (ADR-0026): the `filter.*` edits, their checks and inverses, duplicate
//! and delete, references in filter settings, and the collection file; and the master's
//! and outputs' audio chains (ADR-0027).

use super::{
    add, apply_err, apply_ok, blocker, blockers, builtin, fixture, graph, invalid_reason, kinds,
    plan, scene_ref, settings, tone,
};
use crate::Graph;
use jstream_protocol::{
    DeleteTarget, Edit, EntityKind, FilterChain, FilterId, FilterOwner, KindCategory,
    KindDescriptor, OutputId, OutputKind, ParamDescriptor, ParamType, ProtocolError, QueryResult,
    RecordContainer, SceneId, Settings, SourceId, SourceKind,
};
use serde_json::{Value, json};

fn owner(source: &str) -> FilterOwner {
    FilterOwner::Source {
        source_id: SourceId::new(source),
    }
}

fn filter_add(source: &str, chain: FilterChain, id: &str, kind: &str) -> Edit {
    Edit::FilterAdd {
        owner: owner(source),
        chain,
        filter_id: FilterId::new(id),
        kind: kind.into(),
        settings: Settings::new(),
        enabled: None,
        index: None,
    }
}

fn tint(source: &str, id: &str) -> Edit {
    filter_add(source, FilterChain::Video, id, "tint")
}

fn gain(source: &str, id: &str) -> Edit {
    filter_add(source, FilterChain::Audio, id, "gain")
}

fn with(edit: Edit, entries: &[(&str, Value)], index: Option<u32>) -> Edit {
    let Edit::FilterAdd {
        owner,
        chain,
        filter_id,
        kind,
        enabled,
        ..
    } = edit
    else {
        panic!("not a filter.add: {edit:?}");
    };
    Edit::FilterAdd {
        owner,
        chain,
        filter_id,
        kind,
        settings: settings(entries),
        enabled,
        index,
    }
}

fn filter_settings(source: &str, id: &str, patch: &[(&str, Value)]) -> Edit {
    Edit::FilterSetSettings {
        owner: owner(source),
        filter_id: FilterId::new(id),
        patch: settings(patch),
    }
}

fn reorder(source: &str, id: &str, index: u32) -> Edit {
    Edit::FilterReorder {
        owner: owner(source),
        filter_id: FilterId::new(id),
        index,
    }
}

fn set_enabled(source: &str, id: &str, enabled: bool) -> Edit {
    Edit::FilterSetEnabled {
        owner: owner(source),
        filter_id: FilterId::new(id),
        enabled,
    }
}

fn remove(source: &str, id: &str) -> Edit {
    Edit::FilterRemove {
        owner: owner(source),
        filter_id: FilterId::new(id),
    }
}

/// The IDs on `chain` of `source`, in order.
fn chain_ids(graph: &Graph, source: &str, chain: FilterChain) -> Vec<String> {
    graph
        .state()
        .source(&SourceId::new(source))
        .map(|s| {
            s.filters()
                .chain(chain)
                .iter()
                .map(|f| f.id().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Applies `edits`, then their inverse, and checks the state is back exactly.
fn undoes_exactly(graph: &mut Graph, edits: Vec<Edit>) -> crate::Applied {
    let before = graph.state().clone();
    let applied = apply_ok(graph, edits);
    let after = graph.state().clone();
    let undone = apply_ok(graph, applied.inverse.edits.clone());
    assert_eq!(graph.state(), &before, "the inverse restores the state");
    apply_ok(graph, undone.inverse.edits);
    assert_eq!(graph.state(), &after, "redo restores it again");
    applied
}

/// Every filter edit applies, is reported, and undoes exactly: a stored filter has every
/// parameter of its kind, a disabled one keeps its settings, and reorder and remove put
/// it back where it was.
#[test]
fn filter_edits_apply_report_and_undo_exactly() {
    let mut g = fixture();
    apply_ok(&mut g, vec![tone("mic2")]);
    let added = undoes_exactly(
        &mut g,
        vec![
            tint("cam", "warm"),
            with(tint("cam", "cold"), &[("amount", json!(0.5))], Some(0)),
            gain("mic2", "boost"),
        ],
    );
    assert_eq!(added.inverse.edits.len(), 3);
    assert_eq!(chain_ids(&g, "cam", FilterChain::Video), ["cold", "warm"]);
    let info = g
        .state()
        .source(&SourceId::new("cam"))
        .expect("exists")
        .to_info(g.state());
    assert_eq!(info.filters.video[0].kind, "tint");
    assert_eq!(
        info.filters.video[0].settings,
        settings(&[("amount", json!(0.5)), ("mask", Value::Null)])
    );
    assert_eq!(
        info.filters.video[1].settings.get("amount"),
        Some(&json!(1.0))
    );
    assert!(info.filters.audio.is_empty());
    let mic = g
        .state()
        .source(&SourceId::new("mic2"))
        .expect("exists")
        .to_info(g.state());
    assert_eq!(mic.filters.audio[0].filter_id, FilterId::new("boost"));

    let applied = undoes_exactly(&mut g, vec![set_enabled("cam", "warm", false)]);
    assert_eq!(
        applied.inverse.edits,
        vec![set_enabled("cam", "warm", true)]
    );
    let applied = undoes_exactly(
        &mut g,
        vec![filter_settings("cam", "warm", &[("amount", json!(0.25))])],
    );
    assert_eq!(
        applied.inverse.edits,
        vec![filter_settings("cam", "warm", &[("amount", json!(1.0))])]
    );
    // `null` resets to the default.
    apply_ok(
        &mut g,
        vec![filter_settings("cam", "warm", &[("amount", json!(0.25))])],
    );
    apply_ok(
        &mut g,
        vec![filter_settings("cam", "warm", &[("amount", Value::Null)])],
    );
    let warm = |g: &Graph| {
        g.state()
            .source(&SourceId::new("cam"))
            .and_then(|s| s.filters().find(&FilterId::new("warm")))
            .map(|(_, _, f)| (f.enabled(), f.settings().get("amount").cloned()))
    };
    assert_eq!(warm(&g), Some((false, Some(json!(1.0)))));

    let applied = undoes_exactly(&mut g, vec![reorder("cam", "warm", 0)]);
    assert_eq!(applied.inverse.edits, vec![reorder("cam", "warm", 1)]);
    // Removing the first filter re-adds it at index 0, disabled, with its settings.
    apply_ok(&mut g, vec![reorder("cam", "warm", 0)]);
    let applied = undoes_exactly(&mut g, vec![remove("cam", "warm")]);
    let Edit::FilterAdd {
        enabled,
        index,
        settings: restored,
        ..
    } = &applied.inverse.edits[0]
    else {
        panic!("not a filter.add: {:?}", applied.inverse.edits);
    };
    assert_eq!((*enabled, *index), (Some(false), Some(0)));
    assert_eq!(restored.get("amount"), Some(&json!(1.0)));
    // The last one comes back without an index.
    let applied = apply_ok(&mut g, vec![remove("cam", "cold")]);
    assert!(matches!(
        applied.inverse.edits[0],
        Edit::FilterAdd { index: None, .. }
    ));
}

/// Each check names its field, in the order the edit runs them.
#[test]
fn filter_edits_are_validated() {
    let mut g = fixture();
    apply_ok(&mut g, vec![tone("mic2"), tint("cam", "t")]);
    let field_of = |g: &mut Graph, edit: Edit| invalid_reason(apply_err(g, vec![edit]));

    assert!(matches!(
        apply_err(&mut g, vec![tint("ghost", "t")]),
        ProtocolError::NotFound {
            entity: EntityKind::Source,
            ..
        }
    ));
    assert_eq!(field_of(&mut g, tint("cam", "")).0, "edits.0.filter_id");
    assert!(matches!(
        apply_err(&mut g, vec![tint("cam", "t")]),
        ProtocolError::AlreadyExists {
            entity: EntityKind::Filter,
            ..
        }
    ));
    // IDs are per source: another source may use the same one.
    apply_ok(&mut g, vec![tint("mic", "t")]);
    // Chains the source doesn't have: audio on a video kind, video on an audio kind, and
    // audio on a scene source.
    for edit in [gain("cam", "g"), tint("mic2", "v"), gain("sa", "g")] {
        let (field, reason) = field_of(&mut g, edit);
        assert_eq!(field, "edits.0.chain");
        assert!(reason.contains("has no"), "{reason}");
    }
    // A scene source has a video chain.
    apply_ok(&mut g, vec![tint("sa", "v")]);
    // The kind must be a filter kind of that chain.
    for kind in ["gain", "color", "nope"] {
        let (field, _) = field_of(&mut g, filter_add("cam", FilterChain::Video, "x", kind));
        assert_eq!(field, "edits.0.kind", "{kind}");
    }
    let (field, _) = field_of(
        &mut g,
        with(tint("cam", "x"), &[("amount", json!(2))], None),
    );
    assert_eq!(field, "edits.0.settings.amount");
    let (field, _) = field_of(&mut g, with(tint("cam", "x"), &[("bogus", json!(1))], None));
    assert_eq!(field, "edits.0.settings.bogus");
    let (field, _) = field_of(&mut g, with(tint("cam", "x"), &[], Some(2)));
    assert_eq!(field, "edits.0.index");
    // A filter kind is not a source kind.
    let (field, reason) = field_of(
        &mut g,
        Edit::SourceCreate {
            source_id: SourceId::new("f"),
            name: "F".into(),
            kind: SourceKind::Builtin {
                kind: "tint".into(),
            },
            settings: Settings::new(),
            duplicated_from: None,
        },
    );
    assert_eq!(field, "edits.0.kind.kind");
    assert!(reason.contains("filter kind"), "{reason}");

    // The other edits find the filter, and check what they set.
    for edit in [
        remove("cam", "x"),
        set_enabled("cam", "x", false),
        filter_settings("cam", "x", &[]),
        reorder("cam", "x", 0),
    ] {
        assert!(matches!(
            apply_err(&mut g, vec![edit]),
            ProtocolError::NotFound {
                entity: EntityKind::Filter,
                ..
            }
        ));
    }
    assert_eq!(field_of(&mut g, reorder("cam", "t", 1)).0, "edits.0.index");
    let (field, _) = field_of(
        &mut g,
        filter_settings("cam", "t", &[("amount", json!(-1))]),
    );
    assert_eq!(field, "edits.0.patch.amount");

    // A chain holds at most eight.
    let more: Vec<Edit> = (1..8).map(|i| tint("cam", &format!("t{i}"))).collect();
    apply_ok(&mut g, more);
    let (field, reason) = field_of(&mut g, tint("cam", "t8"));
    assert_eq!(field, "edits.0.chain");
    assert!(reason.contains('8'), "{reason}");
}

/// A duplicate copies both chains with their IDs; a deleted source comes back with its
/// filters, in order and with their switches.
#[test]
fn duplicate_copies_filters_and_delete_restores_them() {
    let mut g = fixture();
    apply_ok(
        &mut g,
        vec![
            tint("cam", "a"),
            with(tint("cam", "b"), &[("amount", json!(0.5))], None),
            set_enabled("cam", "a", false),
        ],
    );
    apply_ok(
        &mut g,
        vec![Edit::SourceDuplicate {
            source_id: SourceId::new("cam"),
            new_source_id: SourceId::new("cam2"),
            name: "Copy".into(),
        }],
    );
    let filters = |g: &Graph, id: &str| {
        g.state()
            .source(&SourceId::new(id))
            .map(|s| s.filters().clone())
    };
    assert_eq!(filters(&g, "cam2"), filters(&g, "cam"));
    // From then on they change independently.
    apply_ok(&mut g, vec![remove("cam2", "b")]);
    assert_eq!(chain_ids(&g, "cam", FilterChain::Video), ["a", "b"]);

    let before = g.state().clone();
    let deleted = apply_ok(
        &mut g,
        vec![Edit::SourceDelete {
            source_id: SourceId::new("cam"),
        }],
    );
    let names: Vec<&str> = deleted.inverse.edits.iter().map(Edit::name).collect();
    assert_eq!(names, ["source.create", "filter.add", "filter.add"]);
    apply_ok(&mut g, deleted.inverse.edits);
    assert_eq!(g.state(), &before);
}

/// A scene reference in a filter's settings is an edge of its source: it is
/// cycle-checked, it blocks deleting the scene, and `delete.plan` clears it.
#[test]
fn references_in_filter_settings_are_edges_of_the_source() {
    let mut g = fixture();
    apply_ok(&mut g, vec![add("a", "i", "cam")]);
    // `cam` is placed in `a`, so masking its filter with `a` is a cycle, on add and on set.
    let (field, reason) = invalid_reason(apply_err(
        &mut g,
        vec![with(tint("cam", "m"), &[("mask", scene_ref("a"))], None)],
    ));
    assert_eq!(field, "edits.0.settings.mask");
    assert!(
        reason.ends_with("source `cam` -> scene `a` -> source `cam`"),
        "{reason}"
    );
    apply_ok(&mut g, vec![tint("cam", "m")]);
    let (field, _) = invalid_reason(apply_err(
        &mut g,
        vec![filter_settings("cam", "m", &[("mask", scene_ref("a"))])],
    ));
    assert_eq!(field, "edits.0.patch.mask");
    // A missing scene is not found.
    assert!(matches!(
        apply_err(
            &mut g,
            vec![filter_settings("cam", "m", &[("mask", scene_ref("zz"))])]
        ),
        ProtocolError::NotFound {
            entity: EntityKind::Scene,
            ..
        }
    ));
    // The edge then stops placing `cam` in the scene it masks with.
    apply_ok(
        &mut g,
        vec![filter_settings("cam", "m", &[("mask", scene_ref("c"))])],
    );
    let (field, _) = invalid_reason(apply_err(&mut g, vec![add("c", "ic", "cam")]));
    assert_eq!(field, "edits.0.source_id");

    let delete = Edit::SceneDelete {
        scene_id: SceneId::new("c"),
    };
    let error = apply_err(&mut g, vec![delete]);
    assert_eq!(
        blockers(error),
        vec![
            blocker(EntityKind::Source, "sc", "kind.scene_id"),
            blocker(EntityKind::Source, "cam", "filters.m.mask"),
        ]
    );
    let edits = plan(
        &g,
        DeleteTarget::Scene {
            scene_id: SceneId::new("c"),
        },
    );
    assert!(
        edits.contains(&filter_settings("cam", "m", &[("mask", Value::Null)])),
        "{edits:?}"
    );
    let before = g.state().clone();
    let applied = apply_ok(&mut g, edits);
    assert!(g.state().scene(&SceneId::new("c")).is_none());
    apply_ok(&mut g, applied.inverse.edits);
    assert_eq!(g.state(), &before);
}

/// Filters survive a save and load, disabled ones too, and a filter saved by another
/// version of its kind is refused like a source's settings.
#[test]
fn filters_round_trip_through_the_collection_file() {
    let mut g = graph();
    apply_ok(
        &mut g,
        vec![
            builtin("cam"),
            tone("mic"),
            with(tint("cam", "a"), &[("amount", json!(0.5))], None),
            tint("cam", "b"),
            set_enabled("cam", "b", false),
            gain("mic", "g"),
        ],
    );
    let json = g.to_collection_json();
    let loaded = Graph::new().with_kinds(kinds()).with_collection_json(&json);
    assert_eq!(loaded.as_ref().map(Graph::state), Ok(g.state()));

    let mut file: Value = serde_json::from_str(&json).expect("valid JSON");
    let source = file["sources"]
        .as_array_mut()
        .and_then(|s| s.iter_mut().find(|s| s["id"] == "cam"))
        .expect("cam is saved");
    assert_eq!(source["filters"]["video"][1]["enabled"], json!(false));
    assert!(source["filters"]["video"][0].get("enabled").is_none());
    source["filters"]["video"][0]["kind_version"] = json!(9);
    let refused = Graph::new()
        .with_kinds(kinds())
        .with_collection_json(&file.to_string());
    assert!(
        matches!(
            refused,
            Err(crate::LoadError::KindVersion { ref object, saved: 9, .. })
                if object == "filter `a` of source `cam`"
        ),
        "{refused:?}"
    );
}

/// A filter of `kind` on `owner`'s `chain`.
fn owned(owner: FilterOwner, chain: FilterChain, id: &str, kind: &str) -> Edit {
    Edit::FilterAdd {
        owner,
        chain,
        filter_id: FilterId::new(id),
        kind: kind.into(),
        settings: Settings::new(),
        enabled: None,
        index: None,
    }
}

/// A `gain` on `owner`'s audio chain.
fn owned_gain(owner: FilterOwner, id: &str) -> Edit {
    owned(owner, FilterChain::Audio, id, "gain")
}

fn output(id: &str) -> FilterOwner {
    FilterOwner::Output {
        output_id: OutputId::new(id),
    }
}

/// The master stage and an output have audio chains of their own (ADR-0027): their
/// filters apply, report and undo like a source's, keep IDs per owner, survive a save,
/// and come back with a deleted output.
#[test]
fn the_master_and_outputs_have_audio_chains() {
    let mut g = fixture();
    apply_ok(
        &mut g,
        vec![Edit::OutputCreate {
            output_id: OutputId::new("rec"),
            kind: OutputKind::Record {
                path: "/home/me/show.mp4".into(),
                container: RecordContainer::Mp4,
            },
        }],
    );
    // The same ID on two owners is two filters.
    undoes_exactly(
        &mut g,
        vec![
            owned_gain(FilterOwner::Master, "g"),
            owned_gain(output("rec"), "g"),
            with(
                owned_gain(output("rec"), "first"),
                &[("gain_db", json!(-6.0))],
                Some(0),
            ),
        ],
    );
    let QueryResult::StateGet {
        master, outputs, ..
    } = g.state_get()
    else {
        panic!("a state.get result");
    };
    assert_eq!(master.filters.audio[0].filter_id, FilterId::new("g"));
    assert_eq!(
        master.filters.audio[0].settings,
        settings(&[("gain_db", json!(0.0))])
    );
    let ids: Vec<_> = outputs[0]
        .filters
        .audio
        .iter()
        .map(|f| f.filter_id.to_string())
        .collect();
    assert_eq!(ids, ["first", "g"]);

    let json = g.to_collection_json();
    let loaded = Graph::new().with_kinds(kinds()).with_collection_json(&json);
    assert_eq!(loaded.as_ref().map(Graph::state), Ok(g.state()));

    let before = g.state().clone();
    let deleted = apply_ok(
        &mut g,
        vec![Edit::OutputDelete {
            output_id: OutputId::new("rec"),
        }],
    );
    let names: Vec<&str> = deleted.inverse.edits.iter().map(Edit::name).collect();
    assert_eq!(names, ["output.create", "filter.add", "filter.add"]);
    apply_ok(&mut g, deleted.inverse.edits);
    assert_eq!(g.state(), &before);
}

/// The master's and outputs' video chains wait for video mixes, an output must exist,
/// and their filters can't hold a reference yet.
#[test]
fn master_and_output_chains_are_validated() {
    let mut kinds = kinds();
    kinds.push(KindDescriptor {
        kind_id: "duck".into(),
        category: KindCategory::AudioFilter,
        name: "Duck".into(),
        version: 1,
        video: false,
        audio: true,
        params: vec![ParamDescriptor {
            id: "key".into(),
            ty: ParamType::Source { media: None },
            default: Value::Null,
            label: "Key".into(),
            description: None,
            group: None,
            advanced: false,
            visible_if: None,
            live: true,
        }],
    });
    let mut g = Graph::new().with_kinds(kinds);
    apply_ok(&mut g, vec![builtin("cam")]);
    let field_of = |g: &mut Graph, edit: Edit| invalid_reason(apply_err(g, vec![edit]));

    let video = owned(FilterOwner::Master, FilterChain::Video, "t", "tint");
    let (field, reason) = field_of(&mut g, video);
    assert_eq!(field, "edits.0.chain");
    assert!(reason.contains("video chains"), "{reason}");
    assert!(matches!(
        apply_err(&mut g, vec![owned_gain(output("ghost"), "g")]),
        ProtocolError::NotFound {
            entity: EntityKind::Output,
            ..
        }
    ));

    let duck = |owner: FilterOwner| {
        with(
            owned(owner, FilterChain::Audio, "d", "duck"),
            &[("key", json!({"type": "source", "source_id": "cam"}))],
            None,
        )
    };
    let (field, reason) = field_of(&mut g, duck(FilterOwner::Master));
    assert_eq!(field, "edits.0.settings.key");
    assert!(reason.contains("can't hold a reference"), "{reason}");
    // Without the reference it applies; setting one later is refused too.
    apply_ok(
        &mut g,
        vec![owned(FilterOwner::Master, FilterChain::Audio, "d", "duck")],
    );
    let patch = Edit::FilterSetSettings {
        owner: FilterOwner::Master,
        filter_id: FilterId::new("d"),
        patch: settings(&[("key", json!({"type": "source", "source_id": "cam"}))]),
    };
    let (field, _) = field_of(&mut g, patch);
    assert_eq!(field, "edits.0.patch.key");
}
