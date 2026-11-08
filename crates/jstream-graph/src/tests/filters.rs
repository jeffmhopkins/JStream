//! Filter chains (ADR-0026): the `filter.*` edits, their checks and inverses, duplicate
//! and delete, references in filter settings, and the collection file; and the master's
//! and outputs' audio chains (ADR-0080).

use super::{
    add, apply_err, apply_ok, blocker, blockers, builtin, fixture, graph, invalid_reason, kinds,
    plan, scene_create, scene_ref, settings, tone,
};
use crate::Graph;
use jstream_protocol::{
    DeleteTarget, Edit, EntityKind, FilterChain, FilterId, FilterOwner, ItemId, KindCategory,
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

/// The master stage and an output have audio chains of their own (ADR-0080): their
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

/// The master stage and every output, the virtual camera included, have a video chain
/// (ADR-0080): its filters apply, undo and survive a save, an output with one gets its
/// own encoder session even while every filter is off, and a deleted output brings its
/// video filters back with it.
#[test]
fn the_master_and_outputs_have_video_chains() {
    let mut g = fixture();
    apply_ok(
        &mut g,
        vec![
            Edit::OutputCreate {
                output_id: OutputId::new("cam"),
                kind: OutputKind::VirtualCam,
            },
            Edit::OutputCreate {
                output_id: OutputId::new("rec"),
                kind: OutputKind::Record {
                    path: "/home/me/show.mp4".into(),
                    container: RecordContainer::Mp4,
                },
            },
        ],
    );
    let has_chain = |g: &Graph, id: &str| {
        g.state()
            .output(&OutputId::new(id))
            .is_some_and(crate::Output::has_video_chain)
    };
    assert!(!has_chain(&g, "cam"));
    undoes_exactly(
        &mut g,
        vec![
            owned(FilterOwner::Master, FilterChain::Video, "t", "tint"),
            owned(output("cam"), FilterChain::Video, "t", "tint"),
            owned(output("rec"), FilterChain::Video, "t", "tint"),
        ],
    );
    assert!(has_chain(&g, "cam") && has_chain(&g, "rec"));
    apply_ok(
        &mut g,
        vec![Edit::FilterSetEnabled {
            owner: output("cam"),
            filter_id: FilterId::new("t"),
            enabled: false,
        }],
    );
    assert!(has_chain(&g, "cam"), "a disabled filter still counts");
    assert_eq!(g.state().master.chain(FilterChain::Video).len(), 1);

    let json = g.to_collection_json();
    let loaded = Graph::new().with_kinds(kinds()).with_collection_json(&json);
    assert_eq!(loaded.as_ref().map(Graph::state), Ok(g.state()));

    let before = g.state().clone();
    let deleted = apply_ok(
        &mut g,
        vec![Edit::OutputDelete {
            output_id: OutputId::new("cam"),
        }],
    );
    let names: Vec<&str> = deleted.inverse.edits.iter().map(Edit::name).collect();
    assert_eq!(names, ["output.create", "filter.add"]);
    apply_ok(&mut g, deleted.inverse.edits);
    assert_eq!(g.state(), &before);
}

/// The master's and an output's video chain take video filter kinds, an output must
/// exist, and their filters can't hold a scene, source or bus reference yet, but may
/// name a file.
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
        max_outset_px: None,
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
    kinds.push(KindDescriptor {
        kind_id: "look".into(),
        category: KindCategory::VideoFilter,
        name: "Look".into(),
        version: 1,
        video: true,
        audio: false,
        max_outset_px: None,
        params: vec![ParamDescriptor {
            id: "lut".into(),
            ty: ParamType::Asset {
                accept: vec!["cube".into()],
            },
            default: Value::Null,
            label: "LUT".into(),
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

    let audio_kind = owned(FilterOwner::Master, FilterChain::Video, "d", "duck");
    let (field, reason) = field_of(&mut g, audio_kind);
    assert_eq!(field, "edits.0.kind");
    assert!(reason.contains("for the video chain"), "{reason}");
    assert!(matches!(
        apply_err(
            &mut g,
            vec![owned(output("ghost"), FilterChain::Video, "t", "tint")]
        ),
        ProtocolError::NotFound {
            entity: EntityKind::Output,
            ..
        }
    ));
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
    assert!(
        reason.contains("can't hold a scene, source or bus reference"),
        "{reason}"
    );
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

    // A file is not an edge: the master's video chain may name one.
    let lut = json!({"type": "asset", "root": "app", "path": "looks/warm.cube"});
    apply_ok(
        &mut g,
        vec![with(
            owned(FilterOwner::Master, FilterChain::Video, "l", "look"),
            &[("lut", lut.clone())],
            None,
        )],
    );
    let saved = g.state().master().chain(FilterChain::Video);
    assert_eq!(saved[0].settings().get("lut"), Some(&lut));
}

/// A filter whose kind the graph doesn't describe (a CLAP plugin before the scan, say)
/// still loads, with its settings and kind version kept verbatim and saved back, and
/// the rest of its chain in order. `filter.add` of such a kind is still refused, and an
/// unchecked reference must still name something that exists.
#[test]
fn a_filter_of_a_missing_kind_loads_and_saves_as_it_was() {
    let mut g = graph();
    apply_ok(
        &mut g,
        vec![tone("mic"), gain("mic", "a"), gain("mic", "c")],
    );
    let mut file: Value = serde_json::from_str(&g.to_collection_json()).expect("valid JSON");
    let audio = file["sources"]
        .as_array_mut()
        .and_then(|s| s.iter_mut().find(|s| s["id"] == "mic"))
        .and_then(|s| s["filters"]["audio"].as_array_mut())
        .expect("mic's audio chain is saved");
    audio.insert(
        1,
        json!({
            "id": "b",
            "kind": "clap/com.example.verb",
            "kind_version": 3,
            "enabled": false,
            "settings": {"7": 0.25, "anything": "kept"},
        }),
    );
    let json = file.to_string();

    let loaded = Graph::new().with_kinds(kinds()).with_collection_json(&json);
    let loaded = loaded.expect("a missing kind doesn't stop the load");
    let chain: Vec<_> = loaded
        .state()
        .source(&SourceId::new("mic"))
        .map(|s| s.filters().audio.iter().map(|f| f.id().as_str()).collect())
        .unwrap_or_default();
    assert_eq!(chain, ["a", "b", "c"]);
    let saved: Value = serde_json::from_str(&loaded.to_collection_json()).expect("valid JSON");
    let reloaded: Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(saved["sources"], reloaded["sources"]);

    let mut edited = loaded.clone();
    assert_eq!(
        invalid_reason(apply_err(
            &mut edited,
            vec![filter_add(
                "mic",
                FilterChain::Audio,
                "d",
                "clap/com.example.verb"
            )]
        )),
        (
            "edits.0.kind".into(),
            "there is no kind `clap/com.example.verb`; `kind.list` names them".into()
        )
    );

    let mut dangling: Value = serde_json::from_str(&json).expect("valid JSON");
    dangling["sources"]
        .as_array_mut()
        .and_then(|s| s.iter_mut().find(|s| s["id"] == "mic"))
        .expect("mic is saved")["filters"]["audio"][1]["settings"]["side"] =
        json!({"type": "source", "source_id": "gone"});
    let refused = Graph::new()
        .with_kinds(kinds())
        .with_collection_json(&dangling.to_string());
    assert!(
        matches!(&refused, Err(crate::LoadError::Invalid { object, .. })
            if object == "filter `b` of source `mic`"),
        "{refused:?}"
    );

    // The master's chain keeps one too.
    let mut master: Value = serde_json::from_str(&json).expect("valid JSON");
    master["master"] = json!({"filters": {"audio": [
        {"id": "m", "kind": "clap/com.example.verb", "kind_version": 3, "settings": {"7": 1.0}}
    ]}});
    let loaded = Graph::new()
        .with_kinds(kinds())
        .with_collection_json(&master.to_string())
        .expect("a missing kind on the master loads");
    let saved: Value = serde_json::from_str(&loaded.to_collection_json()).expect("valid JSON");
    assert_eq!(saved["master"], master["master"]);

    // A scene's filter and an item's effect of a missing kind keep their kind version
    // (ADR-0065), and a keyframe scene still has none of its own, whatever the kind.
    let mut g = fixture();
    apply_ok(
        &mut g,
        vec![
            add("a", "i", "cam"),
            owned_tint(scene_owner("a"), "t", None),
            owned_tint(item_owner("i"), "e", None),
            Edit::SceneCreateVariant {
                scene_id: SceneId::new("k"),
                variant_of: SceneId::new("a"),
                name: "K".into(),
            },
        ],
    );
    let mut file: Value = serde_json::from_str(&g.to_collection_json()).expect("valid JSON");
    let scenes = file["scenes"].as_array_mut().expect("scenes are saved");
    let a = scenes
        .iter_mut()
        .find(|s| s["id"] == "a")
        .expect("a is saved");
    a["filters"]["video"][0]["kind"] = json!("effect/x");
    a["filters"]["video"][0]["kind_version"] = json!(3);
    a["items"][0]["effects"][0]["kind"] = json!("effect/y");
    a["items"][0]["effects"][0]["kind_version"] = json!(4);
    let json = file.to_string();
    let loaded = Graph::new()
        .with_kinds(kinds())
        .with_collection_json(&json)
        .expect("missing kinds on a scene and an item load");
    let saved: Value = serde_json::from_str(&loaded.to_collection_json()).expect("valid JSON");
    let reloaded: Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(saved["scenes"], reloaded["scenes"], "kind versions kept");

    let mut keyframe = reloaded.clone();
    keyframe["scenes"]
        .as_array_mut()
        .and_then(|s| s.iter_mut().find(|s| s["id"] == "k"))
        .expect("k is saved")["filters"] = json!({"video": [
        {"id": "own", "kind": "effect/x", "kind_version": 1, "settings": {}}
    ]});
    let refused = Graph::new()
        .with_kinds(kinds())
        .with_collection_json(&keyframe.to_string());
    assert!(
        matches!(&refused, Err(crate::LoadError::Invalid { error, .. })
            if error.to_string().contains("keyframe scene")),
        "a keyframe scene's own filter is refused: {refused:?}"
    );
}

fn scene_owner(scene: &str) -> FilterOwner {
    FilterOwner::Scene {
        scene_id: SceneId::new(scene),
    }
}

fn item_owner(item: &str) -> FilterOwner {
    FilterOwner::Item {
        item_id: ItemId::new(item),
    }
}

/// A `tint` on `owner`'s video chain, masked by scene `mask` if given.
fn owned_tint(owner: FilterOwner, id: &str, mask: Option<&str>) -> Edit {
    let edit = owned(owner, FilterChain::Video, id, "tint");
    match mask {
        Some(scene) => with(edit, &[("mask", scene_ref(scene))], None),
        None => edit,
    }
}

fn move_item(item: &str, scene: &str) -> Edit {
    Edit::ItemMove {
        item_id: ItemId::new(item),
        scene_id: SceneId::new(scene),
        index: 0,
    }
}

/// A scene's filters and an item's effects (ADR-0065): video chains that apply, report
/// and undo like a source's, refuse an audio chain, an audio kind and a keyframe scene,
/// and keep filter IDs per owner.
#[test]
fn scene_filters_and_item_effects_apply_report_and_undo() {
    let mut g = fixture();
    apply_ok(&mut g, vec![add("a", "i", "cam")]);
    undoes_exactly(
        &mut g,
        vec![
            owned_tint(scene_owner("a"), "t", None),
            owned_tint(item_owner("i"), "t", None),
            owned_tint(item_owner("i"), "u", None),
            Edit::FilterReorder {
                owner: item_owner("i"),
                filter_id: FilterId::new("u"),
                index: 0,
            },
            Edit::FilterSetEnabled {
                owner: scene_owner("a"),
                filter_id: FilterId::new("t"),
                enabled: false,
            },
        ],
    );
    let info = g.state().scene(&SceneId::new("a")).map(|s| s.to_info());
    assert_eq!(
        info.as_ref().map(|i| (
            i.filters.video.len(),
            i.filters.audio.len(),
            i.filters.video[0].enabled
        )),
        Some((1, 0, false))
    );
    let effects: Vec<String> = info
        .iter()
        .flat_map(|i| &i.items[0].effects)
        .map(|f| f.filter_id.to_string())
        .collect();
    assert_eq!(effects, ["u", "t"]);

    let field_of = |g: &mut Graph, edit: Edit| invalid_reason(apply_err(g, vec![edit]));
    let (field, reason) = field_of(&mut g, owned_gain(scene_owner("a"), "g"));
    assert_eq!(field, "edits.0.chain");
    assert!(reason.contains("no audio chain"), "{reason}");
    let (field, _) = field_of(&mut g, owned_gain(item_owner("i"), "g"));
    assert_eq!(field, "edits.0.chain");
    let (field, _) = field_of(
        &mut g,
        owned(item_owner("i"), FilterChain::Video, "v", "gain"),
    );
    assert_eq!(field, "edits.0.kind");
    assert!(matches!(
        apply_err(&mut g, vec![owned_tint(item_owner("ghost"), "t", None)]),
        ProtocolError::NotFound {
            entity: EntityKind::Item,
            ..
        }
    ));
    assert!(matches!(
        apply_err(&mut g, vec![owned_tint(scene_owner("a"), "t", None)]),
        ProtocolError::AlreadyExists {
            entity: EntityKind::Filter,
            ..
        }
    ));

    // A keyframe scene shows its parent's filters and has none of its own, for every
    // filter edit.
    apply_ok(
        &mut g,
        vec![Edit::SceneCreateVariant {
            scene_id: SceneId::new("a_wide"),
            variant_of: SceneId::new("a"),
            name: "A, wide".into(),
        }],
    );
    for edit in [
        owned_tint(scene_owner("a_wide"), "t", None),
        Edit::FilterRemove {
            owner: scene_owner("a_wide"),
            filter_id: FilterId::new("t"),
        },
    ] {
        let (field, reason) = field_of(&mut g, edit);
        assert_eq!(field, "edits.0.owner");
        assert!(reason.contains("keyframe scene of `a`"), "{reason}");
    }
}

/// An item's effects belong to the placement: `item.remove` and `scene.delete` put them
/// back, and `item.move` and `item.set_source` keep them.
#[test]
fn removing_and_moving_an_item_keeps_its_effects() {
    // `undoes_exactly` leaves its edits applied, so each case starts afresh.
    let setup = || {
        let mut g = fixture();
        apply_ok(
            &mut g,
            vec![
                scene_create("d"),
                add("d", "i", "cam"),
                add("d", "j", "mic"),
                owned_tint(scene_owner("d"), "s", Some("c")),
                owned_tint(item_owner("i"), "e", Some("c")),
            ],
        );
        g
    };
    undoes_exactly(
        &mut setup(),
        vec![Edit::ItemRemove {
            item_id: ItemId::new("i"),
        }],
    );
    undoes_exactly(
        &mut setup(),
        vec![Edit::SceneDelete {
            scene_id: SceneId::new("d"),
        }],
    );
    undoes_exactly(
        &mut setup(),
        vec![
            move_item("i", "b"),
            Edit::ItemSetSource {
                item_id: ItemId::new("i"),
                source_id: SourceId::new("mic"),
            },
        ],
    );
    let mut g = setup();
    apply_ok(&mut g, vec![move_item("i", "b")]);
    let effects: Vec<String> = g
        .state()
        .scenes()
        .iter()
        .flat_map(|s| s.items())
        .filter(|i| i.id().as_str() == "i")
        .flat_map(|i| i.effects())
        .map(|f| f.id().to_string())
        .collect();
    assert_eq!(effects, ["e"]);
    // Its mask is now an edge of `b`, so moving it into `c`, the scene it masks with,
    // is a cycle.
    let (field, reason) = invalid_reason(apply_err(&mut g, vec![move_item("i", "c")]));
    assert_eq!(field, "edits.0.scene_id");
    assert!(reason.contains("`effects.e.mask`"), "{reason}");
}

/// References in a scene's filters and an item's effects are edges of the scene: they
/// run the cycle check, block a delete with `filters.<id>.<param>` and
/// `effects.<id>.<param>`, and `delete.plan` clears them.
#[test]
fn references_in_scene_filters_and_effects_are_edges_of_the_scene() {
    let mut g = fixture();
    apply_ok(&mut g, vec![add("a", "i", "cam"), add("b", "k", "mic")]);
    // A scene masked with itself, and an item masked with its own scene, are cycles.
    let (field, reason) = invalid_reason(apply_err(
        &mut g,
        vec![owned_tint(scene_owner("a"), "s", Some("a"))],
    ));
    assert_eq!(field, "edits.0.settings.mask");
    assert!(reason.ends_with("scene `a` -> scene `a`"), "{reason}");
    let (field, _) = invalid_reason(apply_err(
        &mut g,
        vec![owned_tint(item_owner("i"), "e", Some("a"))],
    ));
    assert_eq!(field, "edits.0.settings.mask");

    apply_ok(
        &mut g,
        vec![
            owned_tint(scene_owner("a"), "s", Some("c")),
            owned_tint(item_owner("k"), "e", Some("c")),
        ],
    );
    // `a` and `b` now depend on `c`, so `c` can't place either.
    let (field, _) = invalid_reason(apply_err(&mut g, vec![add("c", "ic", "sa")]));
    assert_eq!(field, "edits.0.source_id");
    let (field, _) = invalid_reason(apply_err(
        &mut g,
        vec![Edit::FilterSetSettings {
            owner: item_owner("k"),
            filter_id: FilterId::new("e"),
            patch: settings(&[("mask", scene_ref("b"))]),
        }],
    ));
    assert_eq!(field, "edits.0.patch.mask");

    let target = || SceneId::new("c");
    let error = apply_err(&mut g, vec![Edit::SceneDelete { scene_id: target() }]);
    assert_eq!(
        blockers(error),
        vec![
            blocker(EntityKind::Source, "sc", "kind.scene_id"),
            blocker(EntityKind::Scene, "a", "filters.s.mask"),
            blocker(EntityKind::Item, "k", "effects.e.mask"),
        ]
    );
    let edits = plan(&g, DeleteTarget::Scene { scene_id: target() });
    let clear = |owner: FilterOwner, id: &str| Edit::FilterSetSettings {
        owner,
        filter_id: FilterId::new(id),
        patch: settings(&[("mask", Value::Null)]),
    };
    assert!(edits.contains(&clear(scene_owner("a"), "s")), "{edits:?}");
    assert!(edits.contains(&clear(item_owner("k"), "e")), "{edits:?}");
    let before = g.state().clone();
    let applied = apply_ok(&mut g, edits);
    assert!(g.state().scene(&target()).is_none());
    apply_ok(&mut g, applied.inverse.edits);
    assert_eq!(g.state(), &before);

    // An item the plan removes anyway needs no clearing: `isc` places `sc`, which the
    // plan deletes with `c`, and its own effect masks with `c`.
    apply_ok(
        &mut g,
        vec![
            add("a", "isc", "sc"),
            owned_tint(item_owner("isc"), "e", Some("c")),
        ],
    );
    let edits = plan(&g, DeleteTarget::Scene { scene_id: target() });
    assert!(
        edits.contains(&Edit::ItemRemove {
            item_id: ItemId::new("isc")
        }),
        "{edits:?}"
    );
    assert!(!edits.contains(&clear(item_owner("isc"), "e")), "{edits:?}");
    apply_ok(&mut g, edits);
}

/// A source's audio filter keyed on another source (ADR-0028's sidechain key, a
/// compressor's `key`) is an edge of its source: keying a source on itself, or two
/// sources on each other, is a cycle, and the key blocks deleting the source it names.
#[test]
fn a_sidechain_key_is_an_edge_of_its_source() {
    let mut kinds = kinds();
    kinds.push(KindDescriptor {
        kind_id: "duck".into(),
        category: KindCategory::AudioFilter,
        name: "Duck".into(),
        version: 1,
        video: false,
        audio: true,
        max_outset_px: None,
        params: vec![ParamDescriptor {
            id: "key".into(),
            ty: ParamType::Source { media: None },
            default: Value::Null,
            label: "Duck under".into(),
            description: None,
            group: None,
            advanced: false,
            visible_if: None,
            live: false,
        }],
    });
    let mut g = Graph::new().with_kinds(kinds);
    apply_ok(&mut g, vec![tone("music"), tone("mic")]);
    let key = |source: &str| json!({"type": "source", "source_id": source});
    let duck = |source: &str, on: &str| {
        with(
            filter_add(source, FilterChain::Audio, "d", "duck"),
            &[("key", key(on))],
            None,
        )
    };

    let (field, reason) = invalid_reason(apply_err(&mut g, vec![duck("music", "music")]));
    assert_eq!(field, "edits.0.settings.key");
    assert!(reason.contains("cycle"), "{reason}");
    apply_ok(&mut g, vec![duck("music", "mic")]);
    let (field, reason) = invalid_reason(apply_err(&mut g, vec![duck("mic", "music")]));
    assert_eq!(field, "edits.0.settings.key");
    assert!(
        reason.ends_with("source `mic` -> source `music` -> source `mic`"),
        "{reason}"
    );

    let delete = Edit::SourceDelete {
        source_id: SourceId::new("mic"),
    };
    assert_eq!(
        blockers(apply_err(&mut g, vec![delete])),
        vec![blocker(EntityKind::Source, "music", "filters.d.key")]
    );
}

/// A filter loaded while its kind was missing follows the kind's renames once the kind
/// is described, from the version the file recorded, and takes a default for what it
/// lacks, keeping a key the kind doesn't declare; one saved by a newer version stays as
/// it is (#1353, ADR-0025 › Versioning a kind).
#[test]
fn upgrade_moves_loaded_filters_through_renames() {
    let mut g = graph();
    apply_ok(
        &mut g,
        vec![
            builtin("cam"),
            with(tint("cam", "a"), &[("amount", json!(0.5))], None),
            tint("cam", "b"),
        ],
    );
    let mut file: Value = serde_json::from_str(&g.to_collection_json()).expect("valid JSON");
    let video = &mut file["sources"]
        .as_array_mut()
        .and_then(|s| s.iter_mut().find(|s| s["id"] == "cam"))
        .expect("cam is saved")["filters"]["video"];
    video[0]["settings"] = json!({ "amount": 0.75, "gone": 1 });
    video[1]["kind_version"] = json!(3);
    let without: Vec<KindDescriptor> = kinds()
        .into_iter()
        .filter(|k| k.kind_id != "tint")
        .collect();
    let loaded = Graph::new()
        .with_kinds(without)
        .with_collection_json(&file.to_string())
        .expect("loads with the kind missing");

    // Version 2 calls `amount` `strength`.
    let mut tint_v2 = kinds()
        .into_iter()
        .find(|k| k.kind_id == "tint")
        .expect("tint");
    tint_v2.version = 2;
    tint_v2.params[0].id = "strength".into();
    let mut loaded = loaded.with_kinds([tint_v2]);
    let migrate = |kind: &str, stored: &Settings, from: u32| -> Option<Settings> {
        assert_eq!(kind, "tint");
        (from <= 2).then(|| {
            stored
                .iter()
                .map(|(id, v)| (if id == "amount" { "strength" } else { id }, v.clone()))
                .collect()
        })
    };
    let before = loaded.rev();
    let changed = [("tint".to_owned(), None)].into_iter().collect();
    let upgraded = loaded.upgrade_kinds(&changed, &migrate).expect("a change");
    assert_eq!(upgraded.rev, before.next().expect("rev"));
    assert_eq!(loaded.rev(), upgraded.rev);
    let fitted = settings(&[
        ("strength", json!(0.75)),
        ("gone", json!(1)),
        ("mask", Value::Null),
    ]);
    assert_eq!(
        upgraded.set,
        [Edit::FilterSetSettings {
            owner: owner("cam"),
            filter_id: FilterId::new("a"),
            patch: settings(&[
                ("strength", json!(0.75)),
                ("mask", Value::Null),
                ("amount", Value::Null),
            ]),
        }],
        "only `a` changed: `b` came from a newer version"
    );
    let chain = loaded
        .state()
        .source(&SourceId::new("cam"))
        .expect("cam")
        .filters();
    let filters = chain.chain(FilterChain::Video);
    assert_eq!(filters[0].settings(), &fitted);
    assert_eq!(filters[1].settings().get("amount"), Some(&json!(1.0)));
    // Saved at the kind's version now, and nothing more to do.
    let saved: Value = serde_json::from_str(&loaded.to_collection_json()).expect("json");
    assert_eq!(
        saved["sources"][0]["filters"]["video"][0]["kind_version"],
        json!(2)
    );
    assert_eq!(loaded.upgrade_kinds(&changed, &migrate), None);
}

/// A `builtin` source whose kind is missing at load (a shader source whose pack isn't
/// loaded yet) is kept as the file has it, its kind version kept for the next save, and
/// is upgraded with the filters once the kind is described (#1412).
#[test]
fn a_source_of_a_missing_kind_loads_verbatim_and_is_upgraded() {
    let mut g = graph();
    apply_ok(&mut g, vec![builtin("bg")]);
    let mut file: Value = serde_json::from_str(&g.to_collection_json()).expect("valid JSON");
    let source = file["sources"]
        .as_array_mut()
        .and_then(|s| s.iter_mut().find(|s| s["id"] == "bg"))
        .expect("bg is saved");
    source["kind"]["kind"] = json!("wave/w");
    source["kind_version"] = json!(1);
    source["settings"] = json!({ "level": 0.25 });
    let mut loaded = Graph::new()
        .with_kinds(kinds())
        .with_collection_json(&file.to_string())
        .expect("loads with the kind missing");
    let bg = loaded.state().source(&SourceId::new("bg")).expect("bg");
    assert_eq!(bg.settings(), &settings(&[("level", json!(0.25))]));
    let saved: Value = serde_json::from_str(&loaded.to_collection_json()).expect("json");
    assert_eq!(saved["sources"][0]["kind_version"], json!(1));
    assert_eq!(saved["sources"][0]["settings"], json!({ "level": 0.25 }));

    // A reference in its settings must still name something that exists.
    let mut dangling = file.clone();
    dangling["sources"][0]["settings"] = json!({ "sky": scene_ref("nowhere") });
    assert!(
        Graph::new()
            .with_kinds(kinds())
            .with_collection_json(&dangling.to_string())
            .is_err()
    );

    // Version 2 calls `level` `gain`.
    let wave = KindDescriptor {
        kind_id: "wave/w".into(),
        category: KindCategory::Source,
        name: "Wave".into(),
        version: 2,
        video: true,
        audio: false,
        max_outset_px: None,
        params: vec![super::param(
            "gain",
            ParamType::Float {
                min: None,
                max: None,
                step: None,
                unit: None,
            },
            json!(1.0),
        )],
    };
    loaded = loaded.with_kinds([wave.clone()]);
    let migrate = |_: &str, stored: &Settings, _: u32| -> Option<Settings> {
        Some(
            stored
                .iter()
                .map(|(id, v)| (if id == "level" { "gain" } else { id }, v.clone()))
                .collect(),
        )
    };
    let changed = [("wave/w".to_owned(), None)].into_iter().collect();
    let upgraded = loaded.upgrade_kinds(&changed, &migrate).expect("a change");
    let gain = settings(&[("gain", json!(0.25))]);
    assert_eq!(
        upgraded.set,
        [Edit::SourceSetSettings {
            source_id: SourceId::new("bg"),
            patch: settings(&[("gain", json!(0.25)), ("level", Value::Null)]),
        }]
    );
    assert_eq!(
        loaded
            .state()
            .source(&SourceId::new("bg"))
            .expect("bg")
            .settings(),
        &gain
    );
    let saved: Value = serde_json::from_str(&loaded.to_collection_json()).expect("json");
    assert_eq!(saved["sources"][0]["kind_version"], json!(2));

    // Removed, it keeps the version its settings were stored by; a version 1 that can't
    // read them leaves them, and the save still says 2.
    let loaded = loaded.without_kinds(["wave/w"]);
    let mut wave_v1 = wave.clone();
    wave_v1.version = 1;
    wave_v1.params[0].id = "level".into();
    let mut loaded = loaded.with_kinds([wave_v1]);
    let newer = |_: &str, _: &Settings, from: u32| -> Option<Settings> {
        assert_eq!(from, 2);
        None
    };
    assert_eq!(loaded.upgrade_kinds(&changed, &newer), None);
    let saved: Value = serde_json::from_str(&loaded.to_collection_json()).expect("json");
    assert_eq!(saved["sources"][0]["kind_version"], json!(2));
    assert_eq!(saved["sources"][0]["settings"], json!({ "gain": 0.25 }));
}

/// Version 2 of `tint`, which calls `amount` `strength`, and its migration.
fn tint_v2() -> KindDescriptor {
    let mut tint = kinds()
        .into_iter()
        .find(|k| k.kind_id == "tint")
        .expect("tint");
    tint.version = 2;
    tint.params[0].id = "strength".into();
    tint
}

fn tint_v1() -> KindDescriptor {
    kinds()
        .into_iter()
        .find(|k| k.kind_id == "tint")
        .expect("tint")
}

fn migrate_tint(_: &str, stored: &Settings, from: u32) -> Option<Settings> {
    (from <= 2).then(|| {
        stored
            .iter()
            .map(|(id, v)| (if id == "amount" { "strength" } else { id }, v.clone()))
            .collect()
    })
}

fn cam_filter(g: &Graph, index: usize) -> &crate::Filter {
    &g.state()
        .source(&SourceId::new("cam"))
        .expect("cam")
        .filters()
        .chain(FilterChain::Video)[index]
}

fn saved_version(g: &Graph) -> Value {
    let saved: Value = serde_json::from_str(&g.to_collection_json()).expect("json");
    saved["sources"][0]["filters"]["video"][0]["kind_version"].clone()
}

/// A pack rewritten at the same version, without a parameter and then with it again,
/// leaves a filter's settings as they were: only a newer version moves them (#1353).
#[test]
fn a_same_version_reload_keeps_every_setting() {
    let mut g = graph();
    apply_ok(
        &mut g,
        vec![
            builtin("cam"),
            with(tint("cam", "a"), &[("amount", json!(0.5))], None),
        ],
    );
    let mut without_amount = tint_v1();
    without_amount.params.retain(|p| p.id != "amount");
    let mut g = g.with_kinds([without_amount]);
    let changed = [("tint".to_owned(), Some(1))].into_iter().collect();
    assert_eq!(g.upgrade_kinds(&changed, &migrate_tint), None);
    assert_eq!(
        cam_filter(&g, 0).settings().get("amount"),
        Some(&json!(0.5))
    );
    let mut g = g.with_kinds([tint_v1()]);
    assert_eq!(g.upgrade_kinds(&changed, &migrate_tint), None);
    assert_eq!(
        cam_filter(&g, 0).settings().get("amount"),
        Some(&json!(0.5))
    );
}

/// Settings a newer version stored keep that version through a downgrade, a save and a
/// reload, so the newer pack coming back finds them as it left them (#1353).
#[test]
fn a_downgrade_keeps_the_newer_version_across_a_save() {
    let mut g = graph().with_kinds([tint_v2()]);
    apply_ok(
        &mut g,
        vec![
            builtin("cam"),
            with(tint("cam", "a"), &[("strength", json!(0.75))], None),
        ],
    );
    // Version 1 knows no newer settings.
    let migrate_v1 = |_: &str, stored: &Settings, from: u32| (from <= 1).then(|| stored.clone());
    let mut g = g.with_kinds([tint_v1()]);
    let changed = [("tint".to_owned(), Some(2))].into_iter().collect();
    assert_eq!(g.upgrade_kinds(&changed, &migrate_v1), None);
    assert_eq!(saved_version(&g), json!(2), "saved by the newer version");

    let without: Vec<KindDescriptor> = kinds()
        .into_iter()
        .filter(|k| k.kind_id != "tint")
        .collect();
    let mut g = Graph::new()
        .with_kinds(without)
        .with_collection_json(&g.to_collection_json())
        .expect("loads with the kind missing")
        .with_kinds([tint_v1()]);
    let loaded = [("tint".to_owned(), None)].into_iter().collect();
    assert_eq!(g.upgrade_kinds(&loaded, &migrate_v1), None);
    assert_eq!(saved_version(&g), json!(2));

    let mut g = g.with_kinds([tint_v2()]);
    let upgraded = [("tint".to_owned(), Some(1))].into_iter().collect();
    assert_eq!(g.upgrade_kinds(&upgraded, &migrate_tint), None);
    assert_eq!(
        cam_filter(&g, 0).settings().get("strength"),
        Some(&json!(0.75))
    );
    assert_eq!(cam_filter(&g, 0).unknown_kind_version, None);
    assert_eq!(saved_version(&g), json!(2));
}

/// A kind removed and then added back at a newer version moves its filters from the
/// version they were stored by (#1353).
#[test]
fn a_removed_kind_coming_back_newer_moves_its_filters() {
    let mut g = graph();
    apply_ok(
        &mut g,
        vec![
            builtin("cam"),
            with(tint("cam", "a"), &[("amount", json!(0.5))], None),
        ],
    );
    let g = g.without_kinds(["tint"]);
    assert_eq!(cam_filter(&g, 0).unknown_kind_version, Some(1));
    assert_eq!(saved_version(&g), json!(1));
    let mut g = g.with_kinds([tint_v2()]);
    let added = [("tint".to_owned(), None)].into_iter().collect();
    assert!(g.upgrade_kinds(&added, &migrate_tint).is_some());
    assert_eq!(
        cam_filter(&g, 0).settings().get("strength"),
        Some(&json!(0.5))
    );
    assert_eq!(cam_filter(&g, 0).settings().get("amount"), None);
    assert_eq!(saved_version(&g), json!(2));
}
