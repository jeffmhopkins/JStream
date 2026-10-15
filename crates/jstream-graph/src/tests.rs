//! Unit tests for edit application, validation and inverses, and the undo property test.

use crate::{Graph, Placement};
use jstream_protocol::{
    AudioFollow, BusId, Canvas, Crop, DeleteTarget, Edit, EncoderProfile, EncoderProfileId,
    EntityKind, Finite32, FitMode, ItemGeometry, ItemId, ItemTransform, KindCategory,
    KindDescriptor, Media, OutputId, OutputInfo, OutputKind, ParamDescriptor, ParamType,
    PlacementForm, ProtocolError, Query, QueryResult, RecordContainer, Rev, RtmpTarget, SceneId,
    SceneRole, Settings, Size, SourceAudio, SourceId, SourceKind, Vec2,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn param(id: &str, ty: ParamType, default: Value) -> ParamDescriptor {
    ParamDescriptor {
        id: id.into(),
        ty,
        default,
        label: id.into(),
        description: None,
        group: None,
        advanced: false,
        visible_if: None,
        live: true,
    }
}

/// The kinds the tests create: `color`, a video kind with a scalar, a reference of each
/// sort, and an asset; and `tone`, an audio kind with no parameters.
fn kinds() -> Vec<KindDescriptor> {
    vec![
        KindDescriptor {
            kind_id: "color".into(),
            category: KindCategory::Source,
            name: "Colour".into(),
            version: 1,
            video: true,
            audio: false,
            params: vec![
                param(
                    "color",
                    ParamType::Color { alpha: true },
                    json!("#000000ff"),
                ),
                param(
                    "size",
                    ParamType::Int {
                        min: Some(1),
                        max: Some(100),
                        step: None,
                        unit: None,
                    },
                    json!(10),
                ),
                param("mask", ParamType::Scene, Value::Null),
                param("follow", ParamType::Source { media: None }, Value::Null),
                param(
                    "duck",
                    ParamType::Source {
                        media: Some(Media::Audio),
                    },
                    Value::Null,
                ),
                param("file", ParamType::Asset { accept: vec![] }, Value::Null),
            ],
        },
        KindDescriptor {
            kind_id: "tone".into(),
            category: KindCategory::Source,
            name: "Tone".into(),
            version: 1,
            video: false,
            audio: true,
            params: vec![],
        },
    ]
}

fn graph() -> Graph {
    Graph::new().with_kinds(kinds())
}

fn settings(entries: &[(&str, Value)]) -> Settings {
    entries.iter().map(|(k, v)| (*k, v.clone())).collect()
}

fn scene_ref(id: &str) -> Value {
    json!({"type": "scene", "scene_id": id})
}

fn source_ref(id: &str) -> Value {
    json!({"type": "source", "source_id": id})
}

fn set_settings(source: &str, patch: &[(&str, Value)]) -> Edit {
    Edit::SourceSetSettings {
        source_id: SourceId::new(source),
        patch: settings(patch),
    }
}

fn scene_create(id: &str) -> Edit {
    Edit::SceneCreate {
        scene_id: SceneId::new(id),
        name: format!("Scene {id}"),
    }
}

fn builtin(id: &str) -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: format!("Source {id}"),
        kind: SourceKind::Builtin {
            kind: "color".into(),
        },
        settings: Settings::new(),
        duplicated_from: None,
    }
}

fn scene_source(id: &str, scene: &str) -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: format!("Nested {scene}"),
        kind: SourceKind::Scene {
            scene_id: SceneId::new(scene),
        },
        settings: Settings::new(),
        duplicated_from: None,
    }
}

fn add(scene: &str, item: &str, source: &str) -> Edit {
    Edit::ItemAdd {
        scene_id: SceneId::new(scene),
        item_id: ItemId::new(item),
        source_id: SourceId::new(source),
        transform: None,
        geometry: None,
    }
}

fn target(url: &str) -> RtmpTarget {
    RtmpTarget::Generic { url: url.into() }
}

fn output_create(id: &str, url: &str) -> Edit {
    Edit::OutputCreate {
        output_id: OutputId::new(id),
        kind: OutputKind::Rtmp {
            target: target(url),
        },
    }
}

fn record_create(id: &str, path: &str) -> Edit {
    Edit::OutputCreate {
        output_id: OutputId::new(id),
        kind: OutputKind::Record {
            path: path.into(),
            container: RecordContainer::Mp4,
        },
    }
}

fn output_url(graph: &Graph, id: &str) -> Option<String> {
    match graph.state().output(&OutputId::new(id))?.kind() {
        OutputKind::Rtmp {
            target: RtmpTarget::Generic { url },
        } => Some(url.clone()),
        _ => None,
    }
}

/// Applies `edits` and checks the result, then that the inverse restores the state.
fn apply_ok(graph: &mut Graph, edits: Vec<Edit>) -> crate::Applied {
    let before = graph.clone();
    let applied = graph.apply(&edits, Some(before.rev())).expect("applies");
    assert_eq!(applied.rev, Rev(before.rev().0 + 1));
    assert_eq!(graph.rev(), applied.rev);
    let mut undone = graph.clone();
    undone
        .apply(&applied.inverse.edits, Some(applied.rev))
        .expect("inverse applies");
    assert_eq!(undone.state(), before.state(), "inverse of {edits:?}");
    applied
}

/// Applies `edits`, expects `error`, and checks nothing changed.
fn apply_err(graph: &mut Graph, edits: Vec<Edit>) -> ProtocolError {
    let before = graph.clone();
    let error = graph.apply(&edits, None).expect_err("is rejected");
    assert_eq!(graph, &before, "a failed batch changed the graph");
    error
}

fn invalid_reason(error: ProtocolError) -> (String, String) {
    match error {
        ProtocolError::InvalidArgument { field, reason, .. } => (field, reason),
        other => panic!("expected invalid_argument, got {other:?}"),
    }
}

/// Scenes a, b, c; builtin sources cam and mic; scene sources for each scene.
fn fixture() -> Graph {
    let mut graph = graph();
    apply_ok(
        &mut graph,
        vec![
            scene_create("a"),
            scene_create("b"),
            scene_create("c"),
            builtin("cam"),
            builtin("mic"),
            scene_source("sa", "a"),
            scene_source("sb", "b"),
            scene_source("sc", "c"),
        ],
    );
    graph
}

#[test]
fn starts_empty_at_rev_zero() {
    let graph = Graph::new();
    assert_eq!(graph.rev(), Rev(0));
    assert!(graph.state().scenes().is_empty());
    assert_eq!(graph.state().sources().len(), 0);
}

#[test]
fn a_batch_bumps_rev_by_one() {
    let mut graph = fixture();
    assert_eq!(graph.rev(), Rev(1));
    let applied = apply_ok(
        &mut graph,
        vec![add("a", "i1", "cam"), add("a", "i2", "cam")],
    );
    assert_eq!(applied.rev, Rev(2));
}

#[test]
fn stale_expected_rev_is_rejected() {
    let mut graph = fixture();
    let error = graph.apply(&[scene_create("d")], Some(Rev(0)));
    assert_eq!(
        error,
        Err(ProtocolError::RevConflict {
            expected: Rev(0),
            current: Rev(1)
        })
    );
    assert!(graph.state().scene(&SceneId::new("d")).is_none());
    assert_eq!(graph.rev(), Rev(1));
}

#[test]
fn empty_batch_is_rejected() {
    let mut graph = fixture();
    assert_eq!(apply_err(&mut graph, vec![]), ProtocolError::EmptyBatch);
}

#[test]
fn a_failing_edit_rolls_back_the_whole_batch() {
    let mut graph = fixture();
    let error = apply_err(
        &mut graph,
        vec![
            scene_create("d"),
            add("d", "i1", "cam"),
            add("zz", "i2", "cam"),
        ],
    );
    assert_eq!(
        error,
        ProtocolError::NotFound {
            entity: EntityKind::Scene,
            id: "zz".into()
        }
    );
}

#[test]
fn later_edits_see_earlier_ones() {
    let mut graph = graph();
    apply_ok(
        &mut graph,
        vec![scene_create("s"), builtin("cam"), add("s", "i", "cam")],
    );
    assert_eq!(graph.state().scenes()[0].items().len(), 1);
}

#[test]
fn duplicate_ids_are_rejected() {
    let mut graph = fixture();
    apply_ok(&mut graph, vec![add("a", "i1", "cam")]);
    let cases = [
        (scene_create("a"), EntityKind::Scene, "a"),
        (builtin("cam"), EntityKind::Source, "cam"),
        (add("b", "i1", "mic"), EntityKind::Item, "i1"),
        (
            Edit::SourceDuplicate {
                source_id: SourceId::new("cam"),
                new_source_id: SourceId::new("mic"),
                name: "x".into(),
            },
            EntityKind::Source,
            "mic",
        ),
    ];
    for (edit, entity, id) in cases {
        assert_eq!(
            apply_err(&mut graph, vec![edit]),
            ProtocolError::AlreadyExists {
                entity,
                id: id.into()
            }
        );
    }
}

#[test]
fn missing_objects_are_not_found() {
    let mut graph = fixture();
    let cases = [
        (
            Edit::SceneRename {
                scene_id: SceneId::new("x"),
                name: "n".into(),
            },
            EntityKind::Scene,
        ),
        (add("a", "i", "x"), EntityKind::Source),
        (
            Edit::ItemRemove {
                item_id: ItemId::new("x"),
            },
            EntityKind::Item,
        ),
        (
            Edit::SourceDelete {
                source_id: SourceId::new("x"),
            },
            EntityKind::Source,
        ),
        (scene_source("s", "x"), EntityKind::Scene),
    ];
    for (edit, entity) in cases {
        assert_eq!(
            apply_err(&mut graph, vec![edit]),
            ProtocolError::NotFound {
                entity,
                id: "x".into()
            }
        );
    }
}

#[test]
fn invalid_arguments_name_the_field() {
    let mut graph = fixture();
    let cases = [
        (
            Edit::SceneCreate {
                scene_id: SceneId::new(""),
                name: "n".into(),
            },
            "edits.0.scene_id",
        ),
        (
            Edit::SceneRename {
                scene_id: SceneId::new("a"),
                name: "  ".into(),
            },
            "edits.0.name",
        ),
        (add("a", "", "cam"), "edits.0.item_id"),
        (
            Edit::SceneReorder {
                scene_id: SceneId::new("a"),
                index: 3,
            },
            "edits.0.index",
        ),
    ];
    for (edit, field) in cases {
        assert_eq!(invalid_reason(apply_err(&mut graph, vec![edit])).0, field);
    }
}

#[test]
fn one_source_is_shared_by_every_placement() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![
            add("a", "i1", "cam"),
            add("a", "i2", "cam"),
            add("b", "i3", "cam"),
        ],
    );
    let cam = SourceId::new("cam");
    assert_eq!(graph.state().placements(&cam).count(), 3);
    // Removing a placement keeps the source.
    apply_ok(
        &mut graph,
        vec![Edit::ItemRemove {
            item_id: ItemId::new("i1"),
        }],
    );
    assert!(graph.state().source(&cam).is_some());
    assert_eq!(graph.state().placements(&cam).count(), 2);
}

#[test]
fn duplicate_makes_a_separate_source() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![Edit::SourceDuplicate {
            source_id: SourceId::new("cam"),
            new_source_id: SourceId::new("cam2"),
            name: "Camera copy".into(),
        }],
    );
    let state = graph.state();
    let copy = state.source(&SourceId::new("cam2")).expect("copied");
    assert_eq!(copy.name(), "Camera copy");
    assert_eq!(
        Some(copy.kind()),
        state.source(&SourceId::new("cam")).map(|s| s.kind())
    );
}

#[test]
fn a_placed_source_cannot_be_deleted() {
    let mut graph = fixture();
    apply_ok(&mut graph, vec![add("a", "i1", "cam")]);
    let delete = Edit::SourceDelete {
        source_id: SourceId::new("cam"),
    };
    let (field, reason) = invalid_reason(apply_err(&mut graph, vec![delete.clone()]));
    assert_eq!(field, "edits.0.source_id");
    assert!(reason.contains("i1"), "{reason}");
    // Removing the placement first, in the same batch, works and undoes exactly.
    apply_ok(
        &mut graph,
        vec![
            Edit::ItemRemove {
                item_id: ItemId::new("i1"),
            },
            delete,
        ],
    );
}

#[test]
fn a_scene_shown_by_a_scene_source_cannot_be_deleted() {
    let mut graph = fixture();
    let delete = Edit::SceneDelete {
        scene_id: SceneId::new("b"),
    };
    let (_, reason) = invalid_reason(apply_err(&mut graph, vec![delete.clone()]));
    assert!(reason.contains("sb"), "{reason}");
    apply_ok(
        &mut graph,
        vec![
            Edit::SourceDelete {
                source_id: SourceId::new("sb"),
            },
            delete,
        ],
    );
}

#[test]
fn a_scene_cannot_contain_itself() {
    let mut graph = fixture();
    let (field, reason) = invalid_reason(apply_err(&mut graph, vec![add("a", "i", "sa")]));
    assert_eq!(field, "edits.0.source_id");
    assert!(
        reason.ends_with("scene `a` -> source `sa` -> scene `a`"),
        "{reason}"
    );
}

#[test]
fn two_cycles_are_rejected() {
    let mut graph = fixture();
    apply_ok(&mut graph, vec![add("a", "ab", "sb")]);
    let (_, reason) = invalid_reason(apply_err(&mut graph, vec![add("b", "ba", "sa")]));
    assert!(
        reason.ends_with("scene `b` -> source `sa` -> scene `a` -> source `sb` -> scene `b`"),
        "{reason}"
    );
}

#[test]
fn three_cycles_are_rejected_even_within_one_batch() {
    let mut graph = fixture();
    let (field, reason) = invalid_reason(apply_err(
        &mut graph,
        vec![
            add("a", "ab", "sb"),
            add("b", "bc", "sc"),
            add("c", "ca", "sa"),
        ],
    ));
    assert_eq!(field, "edits.2.source_id");
    assert!(
        reason.ends_with(
            "scene `c` -> source `sa` -> scene `a` -> source `sb` -> scene `b` -> source `sc` -> scene `c`"
        ),
        "{reason}"
    );
}

#[test]
fn legal_nesting_is_accepted() {
    let mut graph = fixture();
    // A diamond: a shows b and c, b shows c, and c is placed twice in a.
    apply_ok(
        &mut graph,
        vec![
            add("a", "ab", "sb"),
            add("a", "ac", "sc"),
            add("b", "bc", "sc"),
            add("a", "ac2", "sc"),
            add("c", "cam", "cam"),
        ],
    );
    assert_eq!(
        graph
            .state()
            .scene(&SceneId::new("a"))
            .map(|s| s.items().len()),
        Some(3)
    );
}

fn ids(graph: &Graph, scene: &str) -> Vec<String> {
    graph
        .state()
        .scene(&SceneId::new(scene))
        .map(|s| s.items().iter().map(|i| i.id().to_string()).collect())
        .unwrap_or_default()
}

#[test]
fn outputs_are_created_retargeted_and_deleted_with_exact_inverses() {
    let mut graph = Graph::new();
    let created = apply_ok(
        &mut graph,
        vec![output_create("yt", "rtmp://a.rtmp.youtube.com/live2")],
    );
    assert_eq!(
        created.inverse.edits,
        vec![Edit::OutputDelete {
            output_id: OutputId::new("yt"),
        }]
    );
    assert_eq!(
        output_url(&graph, "yt").as_deref(),
        Some("rtmp://a.rtmp.youtube.com/live2")
    );

    let retarget = apply_ok(
        &mut graph,
        vec![Edit::OutputSetTarget {
            output_id: OutputId::new("yt"),
            target: target("rtmps://a.rtmps.youtube.com:443/live2"),
        }],
    );
    assert_eq!(
        retarget.inverse.edits,
        vec![Edit::OutputSetTarget {
            output_id: OutputId::new("yt"),
            target: target("rtmp://a.rtmp.youtube.com/live2"),
        }]
    );
    assert_eq!(
        output_url(&graph, "yt").as_deref(),
        Some("rtmps://a.rtmps.youtube.com:443/live2")
    );
    assert_eq!(
        graph.output_list(),
        QueryResult::OutputList {
            rev: Rev(2),
            outputs: vec![OutputInfo {
                output_id: OutputId::new("yt"),
                kind: OutputKind::Rtmp {
                    target: target("rtmps://a.rtmps.youtube.com:443/live2"),
                },
                encoder_profile: None,
            }],
        }
    );

    // Undo the retarget, then redo it: the inverse's inverse is the edit again.
    let undo = graph.apply_batch(&retarget.inverse).expect("undo applies");
    assert_eq!(
        output_url(&graph, "yt").as_deref(),
        Some("rtmp://a.rtmp.youtube.com/live2")
    );
    graph.apply_batch(&undo.inverse).expect("redo applies");
    assert_eq!(
        output_url(&graph, "yt").as_deref(),
        Some("rtmps://a.rtmps.youtube.com:443/live2")
    );

    let deleted = apply_ok(
        &mut graph,
        vec![Edit::OutputDelete {
            output_id: OutputId::new("yt"),
        }],
    );
    assert_eq!(
        deleted.inverse.edits,
        vec![output_create("yt", "rtmps://a.rtmps.youtube.com:443/live2")]
    );
    assert_eq!(graph.state().outputs().len(), 0);
    match graph.state_get() {
        QueryResult::StateGet { outputs, .. } => assert!(outputs.is_empty()),
        other => panic!("not state.get: {other:?}"),
    }
}

#[test]
fn a_recording_output_is_created_and_deleted_with_exact_inverses() {
    let mut graph = Graph::new();
    let path = "/home/me/Videos/show.mp4".to_owned();
    let created = apply_ok(&mut graph, vec![record_create("rec", &path)]);
    assert_eq!(
        created.inverse.edits,
        vec![Edit::OutputDelete {
            output_id: OutputId::new("rec"),
        }]
    );
    assert_eq!(
        graph.output_list(),
        QueryResult::OutputList {
            rev: Rev(1),
            outputs: vec![OutputInfo {
                output_id: OutputId::new("rec"),
                kind: OutputKind::Record {
                    path: path.clone(),
                    container: RecordContainer::Mp4,
                },
                encoder_profile: None,
            }],
        }
    );
    // A recording has no RTMP target to change.
    let (field, _) = invalid_reason(apply_err(
        &mut graph,
        vec![Edit::OutputSetTarget {
            output_id: OutputId::new("rec"),
            target: target("rtmp://host/live"),
        }],
    ));
    assert_eq!(field, "edits.0.output_id");
    let deleted = apply_ok(
        &mut graph,
        vec![Edit::OutputDelete {
            output_id: OutputId::new("rec"),
        }],
    );
    assert_eq!(deleted.inverse.edits, vec![record_create("rec", &path)]);
}

#[test]
fn recording_paths_are_validated() {
    let mut graph = Graph::new();
    for path in [
        String::new(),
        "show.mp4".into(),
        "/Videos/".into(),
        "/Videos/..".into(),
        "/Videos/show\n.mp4".into(),
    ] {
        let (field, reason) =
            invalid_reason(apply_err(&mut graph, vec![record_create("rec", &path)]));
        assert_eq!(field, "edits.0.kind.path", "{path:?}: {reason}");
    }
    // A kind, target or container from a newer client parses as `Unknown` and is
    // refused as an invalid argument, not stored.
    for (kind, field) in [
        (OutputKind::Unknown, "edits.0.kind.type"),
        (
            OutputKind::Rtmp {
                target: RtmpTarget::Unknown,
            },
            "edits.0.kind.target.type",
        ),
        (
            OutputKind::Record {
                path: "/Videos/show.mkv".into(),
                container: RecordContainer::Unknown,
            },
            "edits.0.kind.container",
        ),
    ] {
        let edit = Edit::OutputCreate {
            output_id: OutputId::new("rec"),
            kind,
        };
        let (got, reason) = invalid_reason(apply_err(&mut graph, vec![edit]));
        assert_eq!(got, field, "{reason}");
    }
    assert_eq!(graph.state().outputs().len(), 0);
}

#[test]
fn output_edits_are_validated() {
    let mut graph = Graph::new();
    apply_ok(&mut graph, vec![output_create("yt", "rtmp://host/live")]);

    let cases = [
        (output_create("", "rtmp://host/live"), "edits.0.output_id"),
        (
            output_create("o2", "rtmp://alice:key@host/live"),
            "edits.0.kind.target.url",
        ),
        (
            output_create("o2", "rtmp://host/live?key=abc"),
            "edits.0.kind.target.url",
        ),
        (
            output_create("o2", "http://host/live"),
            "edits.0.kind.target.url",
        ),
        (
            output_create("o2", "rtmp://host"),
            "edits.0.kind.target.url",
        ),
        (
            Edit::OutputSetTarget {
                output_id: OutputId::new("yt"),
                target: target("rtmp://host/live#key"),
            },
            "edits.0.target.url",
        ),
    ];
    for (edit, field) in cases {
        let (got, reason) = invalid_reason(apply_err(&mut graph, vec![edit.clone()]));
        assert_eq!(got, field, "{edit:?}: {reason}");
        assert!(
            !reason.contains("key=abc") && !reason.contains("alice"),
            "{reason}"
        );
    }

    assert_eq!(
        apply_err(&mut graph, vec![output_create("yt", "rtmp://other/live")]),
        ProtocolError::AlreadyExists {
            entity: EntityKind::Output,
            id: "yt".into(),
        }
    );
    for edit in [
        Edit::OutputSetTarget {
            output_id: OutputId::new("nope"),
            target: target("rtmp://host/live"),
        },
        Edit::OutputDelete {
            output_id: OutputId::new("nope"),
        },
    ] {
        assert_eq!(
            apply_err(&mut graph, vec![edit]),
            ProtocolError::NotFound {
                entity: EntityKind::Output,
                id: "nope".into(),
            }
        );
    }
    // A bad target is rejected even when the output is missing too: not_found first.
    assert!(matches!(
        apply_err(
            &mut graph,
            vec![Edit::OutputSetTarget {
                output_id: OutputId::new("nope"),
                target: target("bad"),
            }]
        ),
        ProtocolError::NotFound { .. }
    ));
}

#[test]
fn a_running_output_cannot_be_deleted() {
    let mut graph = Graph::new();
    apply_ok(
        &mut graph,
        vec![
            output_create("yt", "rtmp://host/live"),
            output_create("rec", "rtmp://host/rec"),
        ],
    );
    let running = BTreeSet::from([OutputId::new("yt")]);
    let before = graph.clone();
    let delete = |id: &str| Edit::OutputDelete {
        output_id: OutputId::new(id),
    };
    let error = graph
        .apply_with_running(&[delete("rec"), delete("yt")], None, &running)
        .expect_err("rejected");
    let (field, reason) = invalid_reason(error);
    assert_eq!(field, "edits.1.output_id");
    assert!(reason.contains("running"), "{reason}");
    assert_eq!(graph, before, "nothing applied");

    // An earlier failing edit is still reported first.
    let error = graph
        .apply_with_running(&[delete("nope"), delete("yt")], None, &running)
        .expect_err("rejected");
    assert!(matches!(error, ProtocolError::NotFound { .. }), "{error:?}");

    // Retargeting a running output is allowed; deleting a stopped one too.
    graph
        .apply_with_running(
            &[
                Edit::OutputSetTarget {
                    output_id: OutputId::new("yt"),
                    target: target("rtmp://backup/live"),
                },
                delete("rec"),
            ],
            None,
            &running,
        )
        .expect("applies");
}

#[test]
fn reorder_moves_one_and_keeps_the_rest_in_order() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![
            add("a", "i0", "cam"),
            add("a", "i1", "cam"),
            add("a", "i2", "cam"),
        ],
    );
    apply_ok(
        &mut graph,
        vec![Edit::ItemReorder {
            item_id: ItemId::new("i2"),
            index: 0,
        }],
    );
    assert_eq!(ids(&graph, "a"), ["i2", "i0", "i1"]);
    apply_ok(
        &mut graph,
        vec![Edit::SceneReorder {
            scene_id: SceneId::new("c"),
            index: 0,
        }],
    );
    let order: Vec<_> = graph
        .state()
        .scenes()
        .iter()
        .map(|s| s.id().to_string())
        .collect();
    assert_eq!(order, ["c", "a", "b"]);
}

#[test]
fn removing_a_middle_hidden_cropped_item_undoes_exactly() {
    let mut graph = fixture();
    let t = ItemTransform {
        position: Vec2::new(10.0, 20.0).expect("finite"),
        ..ItemTransform::IDENTITY
    };
    apply_ok(
        &mut graph,
        vec![
            add("a", "i0", "cam"),
            add("a", "i1", "mic"),
            add("a", "i2", "cam"),
            Edit::ItemSetTransform {
                item_id: ItemId::new("i1"),
                scene_id: None,
                transform: t,
            },
            Edit::ItemSetVisible {
                item_id: ItemId::new("i1"),
                scene_id: None,
                visible: false,
            },
            Edit::ItemSetCrop {
                item_id: ItemId::new("i1"),
                scene_id: None,
                crop: Crop {
                    left: 4,
                    ..Crop::NONE
                },
            },
        ],
    );
    let applied = apply_ok(
        &mut graph,
        vec![Edit::ItemRemove {
            item_id: ItemId::new("i1"),
        }],
    );
    let names: Vec<_> = applied.inverse.edits.iter().map(Edit::name).collect();
    assert_eq!(
        names,
        [
            "item.add",
            "item.reorder",
            "item.set_crop",
            "item.set_visible"
        ]
    );
}

#[test]
fn deleting_a_middle_scene_undoes_exactly() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![
            add("b", "i0", "cam"),
            add("b", "i1", "mic"),
            Edit::ItemSetVisible {
                item_id: ItemId::new("i0"),
                scene_id: None,
                visible: false,
            },
            Edit::SourceDelete {
                source_id: SourceId::new("sb"),
            },
        ],
    );
    let applied = apply_ok(
        &mut graph,
        vec![Edit::SceneDelete {
            scene_id: SceneId::new("b"),
        }],
    );
    let names: Vec<_> = applied.inverse.edits.iter().map(Edit::name).collect();
    assert_eq!(
        names,
        [
            "scene.create",
            "scene.reorder",
            "item.add",
            "item.set_visible",
            "item.add"
        ]
    );
    // The sources the scene's items placed are still there.
    assert!(graph.state().source(&SourceId::new("cam")).is_some());
}

#[test]
fn inverse_of_a_batch_runs_in_reverse_order() {
    let mut graph = fixture();
    let applied = apply_ok(
        &mut graph,
        vec![
            add("a", "i", "cam"),
            Edit::SceneRename {
                scene_id: SceneId::new("a"),
                name: "Renamed".into(),
            },
        ],
    );
    let names: Vec<_> = applied.inverse.edits.iter().map(Edit::name).collect();
    assert_eq!(names, ["scene.rename", "item.remove"]);
    assert_eq!(applied.inverse.expected_rev, None);
}

mod property {
    //! Apply a random batch, then its inverse: the state must be what it was. Then apply
    //! the inverse's inverse (redo): the state must be what the batch produced.
    //!
    //! A hand-rolled, seeded generator keeps the test deterministic and dependency-free.

    use super::{
        add, blockers, builtin, graph, output_create, plan, record_create, scene_create, scene_ref,
        scene_source, set_settings, source_ref, target,
    };
    use crate::Graph;
    use jstream_protocol::{
        AudioFollow, BusId, Crop, DeleteTarget, Edit, EncoderProfile, EncoderProfileId, Finite32,
        FitMode, ItemGeometry, ItemId, ItemTransform, OutputId, RateControl, Rev, SceneId,
        SceneRole, Settings, Size, SourceId, SourceKind, Vec2,
    };
    use serde_json::{Value, json};
    use std::collections::BTreeSet;

    /// xorshift64*: small, fast, and good enough to pick edits.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: usize) -> usize {
            usize::try_from(self.next() % (n.max(1) as u64)).expect("fits")
        }

        fn pick<T: Clone>(&mut self, from: &[T]) -> Option<T> {
            (!from.is_empty()).then(|| from[self.below(from.len())].clone())
        }
    }

    /// A random edit, mostly valid against `graph`'s state. Small ID pools make
    /// collisions, missing IDs and cycles common too.
    fn random_edit(rng: &mut Rng, graph: &Graph) -> Edit {
        let state = graph.state();
        let scenes: Vec<String> = state.scenes().iter().map(|s| s.id().to_string()).collect();
        let items: Vec<String> = state
            .scenes()
            .iter()
            .flat_map(|s| s.items().iter().map(|i| i.id().to_string()))
            .collect();
        let sources: Vec<String> = state.sources().map(|s| s.id().to_string()).collect();
        let outputs: Vec<String> = state.outputs().map(|o| o.id().to_string()).collect();
        let encoders: Vec<String> = state.encoders().map(|e| e.id().to_string()).collect();
        let buses: Vec<String> = state.buses().map(|b| b.id().to_string()).collect();
        let urls = [
            "rtmp://a.example/live",
            "rtmps://b.example:443/live",
            "rtmp://c.example/app/instance",
            "rtmp://bad.example/live?key=1",
        ];
        let pool = |rng: &mut Rng, prefix: &str| format!("{prefix}{}", rng.below(8));
        let scene = |rng: &mut Rng| rng.pick(&scenes).unwrap_or_else(|| pool(rng, "s"));
        let item = |rng: &mut Rng| rng.pick(&items).unwrap_or_else(|| pool(rng, "i"));
        let source = |rng: &mut Rng| rng.pick(&sources).unwrap_or_else(|| pool(rng, "src"));
        let output = |rng: &mut Rng| rng.pick(&outputs).unwrap_or_else(|| pool(rng, "out"));
        let url = |rng: &mut Rng| urls[rng.below(urls.len())];
        let encoder = |rng: &mut Rng| rng.pick(&encoders).unwrap_or_else(|| pool(rng, "enc"));
        let bus = |rng: &mut Rng| {
            BusId::new(match rng.below(5) {
                0 => pool(rng, "bus"),
                _ => rng.pick(&buses).unwrap_or_else(|| pool(rng, "bus")),
            })
        };
        // Out of the fader's range one time in eight.
        let gain = |rng: &mut Rng| {
            Finite32::new([-120.0, -60.0, -12.0, -6.0, -1.5, 0.0, 6.0, 24.0][rng.below(8)])
                .expect("finite")
        };
        let maybe = |rng: &mut Rng| rng.below(2) == 0;
        // Out of range one time in eight.
        let profile = |rng: &mut Rng| {
            let mut profile = EncoderProfile::DEFAULT;
            profile.video.rate_control = RateControl::Cbr {
                kbps: [50, 2500, 6000, 9000, 12_000, 20_000, 40_000, 80_000][rng.below(8)],
            };
            profile
        };
        let index = |rng: &mut Rng| u32::try_from(rng.below(6)).expect("fits");
        // Usually no addressing; sometimes the item's own scene; sometimes another.
        let address = |rng: &mut Rng, item: &str| match rng.below(4) {
            0 | 1 => None,
            2 => state
                .item(&ItemId::new(item))
                .map(|(scene, _)| scene.id().clone()),
            _ => Some(SceneId::new(scene(rng))),
        };
        let f = |rng: &mut Rng| f32::from(u16::try_from(rng.below(2000)).expect("fits")) - 1000.0;
        // Mostly valid geometries; one in six has a zero width or a tilt.
        let geometry = |rng: &mut Rng| {
            let (width, tilt) = match rng.below(6) {
                0 => (0.0, 0.0),
                1 => (64.0, 5.0),
                _ => (f(rng).abs() + 1.0, 0.0),
            };
            ItemGeometry {
                center: Vec2::new(f(rng), f(rng)).expect("finite"),
                size: Size {
                    width: Finite32::new(width).expect("finite"),
                    height: Finite32::new(f(rng).abs() + 1.0).expect("finite"),
                },
                fit_mode: [FitMode::Fit, FitMode::Fill, FitMode::Stretch][rng.below(3)],
                rotation_deg: Finite32::new(f(rng) / 10.0).expect("finite"),
                tilt_deg: Finite32::new(tilt).expect("finite"),
                ..ItemGeometry::covering_canvas(1, 1).expect("positive")
            }
        };
        match rng.below(39) {
            0 => scene_create(&pool(rng, "s")),
            1 => Edit::SceneRename {
                scene_id: SceneId::new(scene(rng)),
                name: pool(rng, "name "),
            },
            2 => Edit::SceneDelete {
                scene_id: SceneId::new(scene(rng)),
            },
            3 => Edit::SceneReorder {
                scene_id: SceneId::new(scene(rng)),
                index: index(rng),
            },
            4 | 5 => add(&scene(rng), &pool(rng, "i"), &source(rng)),
            6 => Edit::ItemRemove {
                item_id: ItemId::new(item(rng)),
            },
            7 => Edit::ItemReorder {
                item_id: ItemId::new(item(rng)),
                index: index(rng),
            },
            8 => {
                let item = item(rng);
                Edit::ItemSetTransform {
                    scene_id: address(rng, &item),
                    item_id: ItemId::new(item),
                    transform: ItemTransform {
                        position: Vec2::new(f(rng), f(rng)).expect("finite"),
                        ..ItemTransform::IDENTITY
                    },
                }
            }
            9 => {
                let item = item(rng);
                Edit::ItemSetVisible {
                    scene_id: address(rng, &item),
                    item_id: ItemId::new(item),
                    visible: rng.below(2) == 0,
                }
            }
            10 => Edit::ItemSetCrop {
                item_id: ItemId::new(item(rng)),
                scene_id: None,
                crop: Crop {
                    left: u32::try_from(rng.below(3)).expect("fits"),
                    bottom: u32::try_from(rng.below(3)).expect("fits"),
                    ..Crop::NONE
                },
            },
            11 => {
                if rng.below(2) == 0 {
                    builtin(&pool(rng, "src"))
                } else {
                    scene_source(&pool(rng, "src"), &scene(rng))
                }
            }
            12 => Edit::SourceDuplicate {
                source_id: SourceId::new(source(rng)),
                new_source_id: SourceId::new(pool(rng, "src")),
                name: "copy".into(),
            },
            13 => Edit::SourceRename {
                source_id: SourceId::new(source(rng)),
                name: pool(rng, "name "),
            },
            14 | 15 => {
                let value = |rng: &mut Rng| match rng.below(4) {
                    0 => Value::Null,
                    1 => scene_ref(&scene(rng)),
                    _ => source_ref(&source(rng)),
                };
                let size = json!(u64::try_from(rng.below(120)).expect("fits"));
                let patch: Vec<(&str, Value)> = match rng.below(4) {
                    0 => vec![("mask", value(rng))],
                    1 => vec![("follow", value(rng)), ("size", size)],
                    2 => vec![("color", json!("#ff00ff")), ("size", Value::Null)],
                    _ => vec![("mask", value(rng)), ("follow", value(rng))],
                };
                set_settings(&source(rng), &patch)
            }
            16 => Edit::SourceDelete {
                source_id: SourceId::new(source(rng)),
            },
            17 => {
                if rng.below(4) == 0 {
                    record_create(&pool(rng, "out"), "/Videos/show.mp4")
                } else {
                    output_create(&pool(rng, "out"), url(rng))
                }
            }
            18 => Edit::OutputSetTarget {
                output_id: OutputId::new(output(rng)),
                target: target(url(rng)),
            },
            19 => Edit::OutputDelete {
                output_id: OutputId::new(output(rng)),
            },
            20 => Edit::SceneSetRole {
                scene_id: SceneId::new(scene(rng)),
                role: if rng.below(2) == 0 {
                    SceneRole::Group
                } else {
                    SceneRole::Scene
                },
            },
            21 => Edit::ItemMove {
                item_id: ItemId::new(item(rng)),
                scene_id: SceneId::new(scene(rng)),
                index: index(rng),
            },
            22 => {
                let item = item(rng);
                Edit::ItemSetGeometry {
                    scene_id: address(rng, &item),
                    item_id: ItemId::new(item),
                    geometry: geometry(rng),
                }
            }
            23 => Edit::ItemAdd {
                scene_id: SceneId::new(scene(rng)),
                item_id: ItemId::new(pool(rng, "i")),
                source_id: SourceId::new(source(rng)),
                transform: None,
                geometry: Some(geometry(rng)),
            },
            24 => {
                let profile_id = EncoderProfileId::new(pool(rng, "enc"));
                let name = pool(rng, "name ");
                let profile = profile(rng);
                if rng.below(2) == 0 {
                    Edit::EncoderCreate {
                        profile_id,
                        name,
                        profile,
                    }
                } else {
                    Edit::EncoderUpdate {
                        profile_id: EncoderProfileId::new(encoder(rng)),
                        name,
                        profile,
                    }
                }
            }
            25 => Edit::OutputSetEncoder {
                output_id: OutputId::new(output(rng)),
                profile_id: (rng.below(3) != 0).then(|| EncoderProfileId::new(encoder(rng))),
            },
            26 => Edit::EncoderDelete {
                profile_id: EncoderProfileId::new(encoder(rng)),
            },
            27 => Edit::ItemSetSource {
                item_id: ItemId::new(item(rng)),
                source_id: SourceId::new(source(rng)),
            },
            28 => Edit::SourceCreate {
                source_id: SourceId::new(pool(rng, "src")),
                name: "linked".into(),
                kind: SourceKind::Builtin {
                    kind: "color".into(),
                },
                settings: Settings::new(),
                duplicated_from: Some(SourceId::new(source(rng))),
            },
            29 => Edit::SourceCreate {
                source_id: SourceId::new(pool(rng, "src")),
                name: "tone".into(),
                kind: SourceKind::Builtin {
                    kind: "tone".into(),
                },
                settings: Settings::new(),
                duplicated_from: None,
            },
            30 => Edit::SourceSetAudio {
                source_id: SourceId::new(source(rng)),
                enabled: maybe(rng).then(|| maybe(rng)),
                follow: maybe(rng).then(|| {
                    if maybe(rng) {
                        AudioFollow::Always
                    } else {
                        AudioFollow::WhenShown
                    }
                }),
                gain_db: maybe(rng).then(|| gain(rng)),
                muted: maybe(rng).then(|| maybe(rng)),
            },
            31 => Edit::BusCreate {
                bus_id: BusId::new(pool(rng, "bus")),
                name: pool(rng, "name "),
            },
            32 => Edit::BusRename {
                bus_id: bus(rng),
                name: pool(rng, "name "),
            },
            33 => Edit::BusSetFader {
                bus_id: bus(rng),
                gain_db: maybe(rng).then(|| gain(rng)),
                muted: maybe(rng).then(|| maybe(rng)),
            },
            34 => Edit::BusDelete { bus_id: bus(rng) },
            35 | 36 => Edit::SendSet {
                source_id: SourceId::new(source(rng)),
                bus_id: bus(rng),
                gain_db: gain(rng),
            },
            37 => Edit::SendRemove {
                source_id: SourceId::new(source(rng)),
                bus_id: bus(rng),
            },
            _ => Edit::CanvasSetSize {
                width: u32::try_from(rng.below(3000)).expect("fits"),
                height: 720,
            },
        }
    }

    /// A batch of 1–5 edits, built against a scratch copy so later edits see earlier ones.
    /// About one batch in eight gets a random edit that may well be invalid.
    fn random_batch(rng: &mut Rng, graph: &Graph) -> Vec<Edit> {
        let mut scratch = graph.clone();
        let mut edits = Vec::new();
        for _ in 0..=rng.below(5) {
            let edit = random_edit(rng, &scratch);
            let ok = scratch.apply(std::slice::from_ref(&edit), None).is_ok();
            if ok || rng.below(8) == 0 {
                edits.push(edit);
            }
        }
        edits
    }

    #[test]
    fn inverse_restores_the_state_exactly() {
        let mut applied_names = BTreeSet::new();
        let mut successes = 0;
        let mut failures = 0;
        let mut plans = 0;
        for seed in 1..=40u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let mut graph = graph();
            for _ in 0..150 {
                let edits = random_batch(&mut rng, &graph);
                let before = graph.clone();
                match graph.apply(&edits, Some(before.rev())) {
                    Ok(applied) => {
                        successes += 1;
                        applied_names.extend(edits.iter().map(Edit::name));
                        assert_eq!(applied.rev, Rev(before.rev().0 + 1));
                        // Every edge-adding edit is cycle checked, so no batch, whatever
                        // its edits, leaves a cycle behind.
                        assert_eq!(crate::deps::any_cycle(graph.state()), None, "{edits:?}");
                        let after = graph.clone();

                        let undo = graph
                            .apply(&applied.inverse.edits, Some(applied.rev))
                            .unwrap_or_else(|e| panic!("inverse of {edits:?} failed: {e}"));
                        assert_eq!(graph.state(), before.state(), "undo of {edits:?}");

                        graph
                            .apply(&undo.inverse.edits, Some(undo.rev))
                            .unwrap_or_else(|e| panic!("redo of {edits:?} failed: {e}"));
                        assert_eq!(graph.state(), after.state(), "redo of {edits:?}");
                        assert_eq!(graph.rev(), Rev(before.rev().0 + 3));
                    }
                    Err(_) => {
                        failures += 1;
                        assert_eq!(graph, before, "a failed batch changed the graph");
                    }
                }
                plans += check_delete_plans(&graph);
            }
        }
        // The generator must actually exercise every edit and both outcomes.
        let all: BTreeSet<&str> = [
            "scene.create",
            "scene.rename",
            "scene.delete",
            "scene.reorder",
            "scene.set_role",
            "item.add",
            "item.move",
            "item.remove",
            "item.reorder",
            "item.set_transform",
            "item.set_geometry",
            "item.set_visible",
            "item.set_crop",
            "item.set_source",
            "source.create",
            "source.duplicate",
            "source.delete",
            "source.rename",
            "source.set_settings",
            "output.create",
            "output.set_target",
            "output.set_encoder",
            "output.delete",
            "encoder.create",
            "encoder.update",
            "encoder.delete",
            "canvas.set_size",
            "source.set_audio",
            "bus.create",
            "bus.rename",
            "bus.set_fader",
            "bus.delete",
            "send.set",
            "send.remove",
        ]
        .into();
        assert_eq!(applied_names, all);
        assert!(
            successes > 1000 && failures > 100,
            "{successes} ok, {failures} failed"
        );
        assert!(plans > 1000, "only {plans} delete plans had blockers");
    }

    /// ADR-0025: for every scene and source, a plain delete is refused exactly when it
    /// has blockers, and then the error lists them; the plan always applies as one batch
    /// at the planned rev, deletes the target, and undoes exactly. Returns how many
    /// targets had blockers.
    fn check_delete_plans(graph: &Graph) -> usize {
        let state = graph.state();
        let scenes = state.scenes().iter().map(|s| {
            let id = s.id().clone();
            (
                DeleteTarget::Scene {
                    scene_id: id.clone(),
                },
                Edit::SceneDelete { scene_id: id },
            )
        });
        let sources = state.sources().map(|s| {
            let id = s.id().clone();
            (
                DeleteTarget::Source {
                    source_id: id.clone(),
                },
                Edit::SourceDelete { source_id: id },
            )
        });
        let mut blocked = 0;
        for (target, delete) in scenes.chain(sources).collect::<Vec<_>>() {
            let edits = plan(graph, target);
            let mut alone = graph.clone();
            match alone.apply(std::slice::from_ref(&delete), None) {
                Ok(_) => assert_eq!(edits, vec![delete.clone()], "unblocked plan"),
                Err(error) => {
                    blocked += 1;
                    assert!(!blockers(error).is_empty(), "refused without blockers");
                    assert!(edits.len() > 1, "blocked plan {edits:?}");
                }
            }
            assert_eq!(edits.last(), Some(&delete));
            let mut planned = graph.clone();
            let applied = planned
                .apply(&edits, Some(graph.rev()))
                .unwrap_or_else(|e| panic!("plan {edits:?} failed: {e}"));
            let gone = match &delete {
                Edit::SceneDelete { scene_id } => planned.state().scene(scene_id).is_none(),
                Edit::SourceDelete { source_id } => planned.state().source(source_id).is_none(),
                _ => false,
            };
            assert!(gone, "plan {edits:?} left its target");
            planned
                .apply(&applied.inverse.edits, Some(applied.rev))
                .unwrap_or_else(|e| panic!("undo of plan {edits:?} failed: {e}"));
            assert_eq!(planned.state(), graph.state(), "undo of plan {edits:?}");
        }
        blocked
    }
}

#[test]
fn a_new_source_stores_every_parameter_with_its_defaults() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![Edit::SourceCreate {
            source_id: SourceId::new("bg"),
            name: "Background".into(),
            kind: SourceKind::Builtin {
                kind: "color".into(),
            },
            settings: settings(&[("size", json!(5)), ("color", Value::Null)]),
            duplicated_from: None,
        }],
    );
    let bg = graph.state().source(&SourceId::new("bg")).expect("created");
    assert_eq!(
        bg.settings(),
        &settings(&[
            ("color", json!("#000000ff")),
            ("size", json!(5)),
            ("mask", Value::Null),
            ("follow", Value::Null),
            ("duck", Value::Null),
            ("file", Value::Null),
        ])
    );
    // A kind without parameters stores an empty map, and reads omit it.
    apply_ok(&mut graph, vec![tone("t")]);
    assert!(
        graph
            .state()
            .source(&SourceId::new("t"))
            .is_some_and(|s| s.settings().is_empty())
    );
}

fn tone(id: &str) -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: "Tone".into(),
        kind: SourceKind::Builtin {
            kind: "tone".into(),
        },
        settings: Settings::new(),
        duplicated_from: None,
    }
}

fn create_color(id: &str, entries: &[(&str, Value)]) -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: "Colour".into(),
        kind: SourceKind::Builtin {
            kind: "color".into(),
        },
        settings: settings(entries),
        duplicated_from: None,
    }
}

#[test]
fn settings_are_checked_against_the_descriptor() {
    let mut graph = fixture();
    let cases = [
        (
            Edit::SourceCreate {
                source_id: SourceId::new("x"),
                name: "x".into(),
                kind: SourceKind::Builtin {
                    kind: "hologram".into(),
                },
                settings: Settings::new(),
                duplicated_from: None,
            },
            "edits.0.kind.kind",
        ),
        (
            create_color("x", &[("glow", json!(1))]),
            "edits.0.settings.glow",
        ),
        (
            create_color("x", &[("size", json!(0))]),
            "edits.0.settings.size",
        ),
        (
            create_color("x", &[("size", json!("big"))]),
            "edits.0.settings.size",
        ),
        (
            create_color("x", &[("color", json!("red"))]),
            "edits.0.settings.color",
        ),
        (
            create_color("x", &[("mask", source_ref("cam"))]),
            "edits.0.settings.mask",
        ),
        (
            create_color(
                "x",
                &[(
                    "file",
                    json!({"type": "asset", "root": "app", "path": "../x.png"}),
                )],
            ),
            "edits.0.settings.file",
        ),
        (
            Edit::SourceCreate {
                source_id: SourceId::new("x"),
                name: "x".into(),
                kind: SourceKind::Scene {
                    scene_id: SceneId::new("a"),
                },
                settings: settings(&[("size", json!(3))]),
                duplicated_from: None,
            },
            "edits.0.settings",
        ),
        (
            set_settings("cam", &[("size", json!(101))]),
            "edits.0.patch.size",
        ),
        (
            set_settings("cam", &[("glow", json!(1))]),
            "edits.0.patch.glow",
        ),
        (set_settings("sa", &[("size", json!(3))]), "edits.0.patch"),
        // `duck` takes a source that produces audio; `cam` is video only.
        (
            set_settings("mic", &[("duck", source_ref("cam"))]),
            "edits.0.patch.duck",
        ),
    ];
    for (edit, field) in cases {
        assert_eq!(
            invalid_reason(apply_err(&mut graph, vec![edit.clone()])).0,
            field,
            "{edit:?}"
        );
    }
    // Missing reference targets are `not_found`, like any missing object.
    assert_eq!(
        apply_err(
            &mut graph,
            vec![set_settings("cam", &[("mask", scene_ref("zz"))])]
        ),
        ProtocolError::NotFound {
            entity: EntityKind::Scene,
            id: "zz".into()
        }
    );
    assert_eq!(
        apply_err(
            &mut graph,
            vec![create_color("x", &[("follow", source_ref("zz"))])]
        ),
        ProtocolError::NotFound {
            entity: EntityKind::Source,
            id: "zz".into()
        }
    );
    assert_eq!(
        invalid_reason(apply_err(
            &mut graph,
            vec![set_settings("mic", &[("duck", source_ref("cam"))])]
        ))
        .1,
        "source `cam` doesn't produce audio"
    );
    // An audio source satisfies `duck`.
    apply_ok(
        &mut graph,
        vec![tone("t"), set_settings("cam", &[("duck", source_ref("t"))])],
    );
}

#[test]
fn set_settings_patches_and_undoes_to_the_old_values() {
    let mut graph = fixture();
    let applied = apply_ok(
        &mut graph,
        vec![set_settings(
            "cam",
            &[("size", json!(42)), ("mask", scene_ref("a"))],
        )],
    );
    assert_eq!(
        applied.inverse.edits,
        vec![set_settings(
            "cam",
            &[("mask", Value::Null), ("size", json!(10))]
        )]
    );
    let cam = graph.state().source(&SourceId::new("cam")).expect("cam");
    assert_eq!(cam.settings().get("size"), Some(&json!(42)));
    assert_eq!(cam.settings().get("color"), Some(&json!("#000000ff")));
    // `null` on a scalar resets it to its default; on a reference, clears it.
    apply_ok(
        &mut graph,
        vec![set_settings(
            "cam",
            &[("size", Value::Null), ("mask", Value::Null)],
        )],
    );
    let cam = graph.state().source(&SourceId::new("cam")).expect("cam");
    assert_eq!(cam.settings().get("size"), Some(&json!(10)));
    assert_eq!(cam.settings().get("mask"), Some(&Value::Null));
}

#[test]
fn rename_changes_only_the_name() {
    let mut graph = fixture();
    let applied = apply_ok(
        &mut graph,
        vec![Edit::SourceRename {
            source_id: SourceId::new("cam"),
            name: "Webcam".into(),
        }],
    );
    assert_eq!(
        applied.inverse.edits,
        vec![Edit::SourceRename {
            source_id: SourceId::new("cam"),
            name: "Source cam".into(),
        }]
    );
    let blank = Edit::SourceRename {
        source_id: SourceId::new("cam"),
        name: " ".into(),
    };
    assert_eq!(
        invalid_reason(apply_err(&mut graph, vec![blank])).0,
        "edits.0.name"
    );
}

fn blockers(error: ProtocolError) -> Vec<(EntityKind, String, String)> {
    match error {
        ProtocolError::InvalidArgument { blockers, .. } => blockers
            .into_iter()
            .map(|b| (b.entity, b.id, b.field))
            .collect(),
        other => panic!("expected invalid_argument, got {other:?}"),
    }
}

fn blocker(entity: EntityKind, id: &str, field: &str) -> (EntityKind, String, String) {
    (entity, id.into(), field.into())
}

#[test]
fn referenced_objects_cannot_be_deleted() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![
            set_settings(
                "cam",
                &[("mask", scene_ref("c")), ("follow", source_ref("mic"))],
            ),
            add("a", "i1", "mic"),
        ],
    );
    // ADR-0025: the error lists every blocker, not just the first kind found.
    let delete_scene = Edit::SceneDelete {
        scene_id: SceneId::new("c"),
    };
    let error = apply_err(&mut graph, vec![delete_scene.clone()]);
    let (field, reason) = invalid_reason(error.clone());
    assert_eq!(field, "edits.0.scene_id");
    assert!(reason.contains("source `cam` (settings.mask)"), "{reason}");
    assert_eq!(
        blockers(error),
        vec![
            blocker(EntityKind::Source, "sc", "kind.scene_id"),
            blocker(EntityKind::Source, "cam", "settings.mask"),
        ]
    );
    let delete_mic = Edit::SourceDelete {
        source_id: SourceId::new("mic"),
    };
    let error = apply_err(&mut graph, vec![delete_mic.clone()]);
    assert_eq!(invalid_reason(error.clone()).0, "edits.0.source_id");
    assert_eq!(
        blockers(error),
        vec![
            blocker(EntityKind::Item, "i1", "source_id"),
            blocker(EntityKind::Source, "cam", "settings.follow"),
        ]
    );
    // Clearing the references first, in the same batch, works and undoes exactly; the
    // deleted source comes back with its settings.
    apply_ok(
        &mut graph,
        vec![
            Edit::ItemRemove {
                item_id: ItemId::new("i1"),
            },
            Edit::SourceDelete {
                source_id: SourceId::new("sc"),
            },
            set_settings("cam", &[("mask", Value::Null), ("follow", Value::Null)]),
            delete_scene,
            delete_mic,
        ],
    );
}

fn plan(graph: &Graph, target: DeleteTarget) -> Vec<Edit> {
    match graph.query(&Query::DeletePlan { target }) {
        Some(Ok(QueryResult::DeletePlan { rev, edits })) => {
            assert_eq!(rev, graph.rev());
            edits
        }
        other => panic!("expected a delete plan, got {other:?}"),
    }
}

#[test]
fn delete_plan_removes_a_scene_with_its_scene_sources_and_clears_references() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![
            // `sc` shows `c` and is placed twice; `cam` refers to both `c` and `sc`. The
            // scene's own item `i3` goes with the scene, so the plan doesn't list it.
            add("a", "i1", "sc"),
            add("b", "i2", "sc"),
            add("c", "i3", "mic"),
            set_settings(
                "cam",
                &[("mask", scene_ref("c")), ("follow", source_ref("sc"))],
            ),
            create_color("bg", &[("mask", scene_ref("c"))]),
        ],
    );
    let edits = plan(
        &graph,
        DeleteTarget::Scene {
            scene_id: SceneId::new("c"),
        },
    );
    assert_eq!(
        edits,
        vec![
            Edit::ItemRemove {
                item_id: ItemId::new("i1"),
            },
            Edit::ItemRemove {
                item_id: ItemId::new("i2"),
            },
            set_settings("bg", &[("mask", Value::Null)]),
            set_settings("cam", &[("follow", Value::Null), ("mask", Value::Null)]),
            Edit::SourceDelete {
                source_id: SourceId::new("sc"),
            },
            Edit::SceneDelete {
                scene_id: SceneId::new("c"),
            },
        ]
    );
    let before = graph.clone();
    let applied = graph
        .apply(&edits, Some(before.rev()))
        .expect("plan applies");
    let state = graph.state();
    assert!(state.scene(&SceneId::new("c")).is_none());
    assert!(state.source(&SourceId::new("sc")).is_none());
    // The sources holding the references stay.
    assert!(state.source(&SourceId::new("cam")).is_some());
    assert!(state.source(&SourceId::new("bg")).is_some());
    // Undo is the batch's ordinary inverse.
    graph
        .apply(&applied.inverse.edits, Some(applied.rev))
        .expect("undo applies");
    assert_eq!(graph.state(), before.state());
}

#[test]
fn delete_plan_removes_a_sources_placements_and_clears_references() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![
            add("a", "i1", "mic"),
            add("a", "i2", "mic"),
            set_settings("cam", &[("follow", source_ref("mic"))]),
        ],
    );
    let edits = plan(
        &graph,
        DeleteTarget::Source {
            source_id: SourceId::new("mic"),
        },
    );
    assert_eq!(
        edits,
        vec![
            Edit::ItemRemove {
                item_id: ItemId::new("i1"),
            },
            Edit::ItemRemove {
                item_id: ItemId::new("i2"),
            },
            set_settings("cam", &[("follow", Value::Null)]),
            Edit::SourceDelete {
                source_id: SourceId::new("mic"),
            },
        ]
    );
    apply_ok(&mut graph, edits);
    assert!(graph.state().source(&SourceId::new("mic")).is_none());
}

#[test]
fn delete_plan_of_a_missing_target_is_not_found() {
    let graph = fixture();
    assert_eq!(
        graph.query(&Query::DeletePlan {
            target: DeleteTarget::Scene {
                scene_id: SceneId::new("nope"),
            },
        }),
        Some(Err(ProtocolError::NotFound {
            entity: EntityKind::Scene,
            id: "nope".into(),
        }))
    );
}

#[test]
fn a_deleted_source_comes_back_with_its_settings() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![create_color(
            "bg",
            &[("size", json!(7)), ("mask", scene_ref("a"))],
        )],
    );
    let applied = apply_ok(
        &mut graph,
        vec![Edit::SourceDelete {
            source_id: SourceId::new("bg"),
        }],
    );
    let Some(Edit::SourceCreate { settings, .. }) = applied.inverse.edits.first() else {
        panic!("inverse: {:?}", applied.inverse);
    };
    assert_eq!(settings.get("size"), Some(&json!(7)));
}

/// ADR-0025: every edit that can add a dependency edge runs the cycle check: `item.add`
/// and `item.set_source` (scene to source, see `set_source_rejects_missing_objects_and_cycles`), `source.create` and `source.set_settings` (source to the scenes
/// and sources its settings refer to). `source.duplicate` copies outgoing edges onto a
/// source nothing depends on yet, so it can't close a cycle.
#[test]
fn every_edge_adding_edit_is_cycle_checked() {
    // item.add
    let mut g = fixture();
    let (field, reason) = invalid_reason(apply_err(&mut g, vec![add("a", "i", "sa")]));
    assert_eq!(field, "edits.0.source_id");
    assert!(reason.contains("cycle"), "{reason}");

    // source.create naming itself
    let (field, reason) = invalid_reason(apply_err(
        &mut g,
        vec![create_color("x", &[("follow", source_ref("x"))])],
    ));
    assert_eq!(field, "edits.0.settings.follow");
    assert!(reason.ends_with("source `x` -> source `x`"), "{reason}");

    // source.set_settings: `cam` is placed in `a`; masking it with `a` is a cycle.
    apply_ok(&mut g, vec![add("a", "i1", "cam")]);
    let (field, reason) = invalid_reason(apply_err(
        &mut g,
        vec![set_settings("cam", &[("mask", scene_ref("a"))])],
    ));
    assert_eq!(field, "edits.0.patch.mask");
    assert!(
        reason.ends_with("source `cam` -> scene `a` -> source `cam`"),
        "{reason}"
    );
    // ... and through a nested scene: `b` is shown in `a`, so a source placed in `b`
    // can't be masked with `a`.
    let mut g = fixture();
    apply_ok(&mut g, vec![add("a", "ab", "sb")]);
    let (field, reason) = invalid_reason(apply_err(
        &mut g,
        vec![
            set_settings("mic", &[("mask", scene_ref("a"))]),
            add("b", "im", "mic"),
        ],
    ));
    assert_eq!(field, "edits.1.source_id");
    assert!(
        reason.ends_with("scene `b` -> source `mic` -> scene `a` -> source `sb` -> scene `b`"),
        "{reason}"
    );
    // Source to source: cam follows mic, so mic can't follow cam.
    apply_ok(
        &mut g,
        vec![set_settings("cam", &[("follow", source_ref("mic"))])],
    );
    let (field, _) = invalid_reason(apply_err(
        &mut g,
        vec![set_settings("mic", &[("follow", source_ref("cam"))])],
    ));
    assert_eq!(field, "edits.0.patch.follow");
    // A settings edge then blocks placing: `mic` masks with `c`, so `c` can't place `mic`.
    apply_ok(
        &mut g,
        vec![set_settings("mic", &[("mask", scene_ref("c"))])],
    );
    let (field, _) = invalid_reason(apply_err(&mut g, vec![add("c", "ic", "mic")]));
    assert_eq!(field, "edits.0.source_id");
    // A legal reference: masking with an unrelated scene.
    apply_ok(
        &mut g,
        vec![set_settings("cam", &[("mask", scene_ref("c"))])],
    );
}

fn encoder_create(id: &str) -> Edit {
    Edit::EncoderCreate {
        profile_id: EncoderProfileId::new(id),
        name: format!("Profile {id}"),
        profile: EncoderProfile::DEFAULT,
    }
}

fn set_encoder(output: &str, profile: Option<&str>) -> Edit {
    Edit::OutputSetEncoder {
        output_id: OutputId::new(output),
        profile_id: profile.map(EncoderProfileId::new),
    }
}

/// Every encoder edit undoes exactly, and deleting an output that uses a profile
/// restores its choice on undo.
#[test]
fn encoder_edits_invert() {
    let mut graph = graph();
    apply_ok(
        &mut graph,
        vec![
            output_create("yt", "rtmp://a.rtmp.youtube.com/live2"),
            encoder_create("hq"),
        ],
    );
    apply_ok(&mut graph, vec![set_encoder("yt", Some("hq"))]);
    assert_eq!(
        graph
            .state()
            .output(&OutputId::new("yt"))
            .and_then(|o| o.encoder_profile())
            .map(EncoderProfileId::as_str),
        Some("hq")
    );
    let mut faster = EncoderProfile::DEFAULT;
    faster.video.keyframe_interval_s = 1;
    apply_ok(
        &mut graph,
        vec![Edit::EncoderUpdate {
            profile_id: EncoderProfileId::new("hq"),
            name: "Fast keyframes".into(),
            profile: faster,
        }],
    );
    assert_eq!(
        graph.state().encoding_of(&OutputId::new("yt")),
        Some(faster)
    );
    let deleted = apply_ok(
        &mut graph,
        vec![Edit::OutputDelete {
            output_id: OutputId::new("yt"),
        }],
    );
    assert_eq!(deleted.inverse.edits.len(), 2, "{:?}", deleted.inverse);
    apply_ok(
        &mut graph,
        vec![Edit::EncoderDelete {
            profile_id: EncoderProfileId::new("hq"),
        }],
    );
}

/// A profile in use can be deleted in the batch that moves its last user off it.
#[test]
fn a_profile_is_freed_within_one_batch() {
    let mut graph = graph();
    apply_ok(
        &mut graph,
        vec![
            output_create("yt", "rtmp://a.rtmp.youtube.com/live2"),
            encoder_create("hq"),
            set_encoder("yt", Some("hq")),
        ],
    );
    let (field, _) = invalid_reason(apply_err(
        &mut graph,
        vec![Edit::EncoderDelete {
            profile_id: EncoderProfileId::new("hq"),
        }],
    ));
    assert_eq!(field, "edits.0.profile_id");
    apply_ok(
        &mut graph,
        vec![
            set_encoder("yt", None),
            Edit::EncoderDelete {
                profile_id: EncoderProfileId::new("hq"),
            },
        ],
    );
    assert_eq!(
        graph.state().encoding_of(&OutputId::new("yt")),
        Some(EncoderProfile::DEFAULT)
    );
}

fn item_move(item: &str, scene: &str, index: u32) -> Edit {
    Edit::ItemMove {
        item_id: ItemId::new(item),
        scene_id: SceneId::new(scene),
        index,
    }
}

fn set_role(scene: &str, role: SceneRole) -> Edit {
    Edit::SceneSetRole {
        scene_id: SceneId::new(scene),
        role,
    }
}

fn role(graph: &Graph, scene: &str) -> SceneRole {
    graph
        .state()
        .scene(&SceneId::new(scene))
        .expect("scene exists")
        .role()
}

#[test]
fn set_role_changes_only_the_role_and_is_reported() {
    let mut g = fixture();
    assert_eq!(role(&g, "b"), SceneRole::Scene);
    let applied = apply_ok(&mut g, vec![set_role("b", SceneRole::Group)]);
    assert_eq!(applied.inverse.edits, vec![set_role("b", SceneRole::Scene)]);
    assert_eq!(role(&g, "b"), SceneRole::Group);
    let QueryResult::SceneList { scenes, .. } = g.scene_list() else {
        panic!("not a scene list");
    };
    let roles: Vec<SceneRole> = scenes.iter().map(|s| s.role).collect();
    assert_eq!(
        roles,
        [SceneRole::Scene, SceneRole::Group, SceneRole::Scene]
    );
    let QueryResult::StateGet { scenes, .. } = g.state_get() else {
        panic!("not a state");
    };
    assert_eq!(scenes[1].role, SceneRole::Group);
    // Setting the same role again changes nothing, and is still undoable.
    apply_ok(&mut g, vec![set_role("b", SceneRole::Group)]);
    assert!(matches!(
        apply_err(&mut g, vec![set_role("nope", SceneRole::Group)]),
        ProtocolError::NotFound {
            entity: EntityKind::Scene,
            ..
        }
    ));
}

#[test]
fn deleting_a_group_restores_its_role() {
    let mut g = fixture();
    apply_ok(
        &mut g,
        vec![
            scene_create("g"),
            set_role("g", SceneRole::Group),
            add("g", "gi", "cam"),
        ],
    );
    let applied = apply_ok(
        &mut g,
        vec![Edit::SceneDelete {
            scene_id: SceneId::new("g"),
        }],
    );
    assert!(
        applied
            .inverse
            .edits
            .contains(&set_role("g", SceneRole::Group))
    );
}

#[test]
fn move_keeps_the_item_and_undoes_to_its_old_place() {
    let mut g = fixture();
    apply_ok(
        &mut g,
        vec![
            add("a", "a0", "cam"),
            add("a", "a1", "mic"),
            add("a", "a2", "cam"),
            add("b", "b0", "mic"),
            Edit::ItemSetVisible {
                item_id: ItemId::new("a1"),
                scene_id: None,
                visible: false,
            },
            Edit::ItemSetCrop {
                item_id: ItemId::new("a1"),
                scene_id: None,
                crop: Crop {
                    left: 4,
                    ..Crop::NONE
                },
            },
        ],
    );
    let before = g
        .state()
        .item(&ItemId::new("a1"))
        .expect("exists")
        .1
        .clone();
    // To the bottom of `b`.
    let applied = apply_ok(&mut g, vec![item_move("a1", "b", 0)]);
    assert_eq!(applied.inverse.edits, vec![item_move("a1", "a", 1)]);
    assert_eq!(ids(&g, "a"), ["a0", "a2"]);
    assert_eq!(ids(&g, "b"), ["a1", "b0"]);
    let (scene, item) = g.state().item(&ItemId::new("a1")).expect("exists");
    assert_eq!(scene.id().as_str(), "b");
    assert_eq!(item, &before, "the move keeps every property");
    // One past the top of another scene is allowed.
    apply_ok(&mut g, vec![item_move("a0", "b", 2)]);
    assert_eq!(ids(&g, "b"), ["a1", "b0", "a0"]);
    // Within its own scene it is a reorder, and the top is the last index.
    apply_ok(&mut g, vec![item_move("a0", "b", 0)]);
    assert_eq!(ids(&g, "b"), ["a0", "a1", "b0"]);
    let (field, _) = invalid_reason(apply_err(&mut g, vec![item_move("a0", "b", 3)]));
    assert_eq!(field, "edits.0.index");
    let (field, _) = invalid_reason(apply_err(&mut g, vec![item_move("a2", "b", 4)]));
    assert_eq!(field, "edits.0.index");
    // Into an empty scene, only index 0.
    let (field, _) = invalid_reason(apply_err(&mut g, vec![item_move("a2", "c", 1)]));
    assert_eq!(field, "edits.0.index");
    apply_ok(&mut g, vec![item_move("a2", "c", 0)]);
    assert_eq!(ids(&g, "c"), ["a2"]);
    assert!(matches!(
        apply_err(&mut g, vec![item_move("nope", "c", 0)]),
        ProtocolError::NotFound {
            entity: EntityKind::Item,
            ..
        }
    ));
    assert!(matches!(
        apply_err(&mut g, vec![item_move("a2", "nope", 0)]),
        ProtocolError::NotFound {
            entity: EntityKind::Scene,
            ..
        }
    ));
}

#[test]
fn move_is_cycle_checked() {
    let mut g = fixture();
    // `a` shows `b`, and `c` holds an item showing `a`. Moving that item into `b` would
    // make `b` show `a`, which shows `b`.
    apply_ok(&mut g, vec![add("a", "ab", "sb"), add("c", "ca", "sa")]);
    let (field, reason) = invalid_reason(apply_err(&mut g, vec![item_move("ca", "b", 0)]));
    assert_eq!(field, "edits.0.scene_id");
    assert!(
        reason.ends_with("scene `b` -> source `sa` -> scene `a` -> source `sb` -> scene `b`"),
        "{reason}"
    );
    // A scene can't take an item that shows the scene itself.
    let (field, _) = invalid_reason(apply_err(&mut g, vec![item_move("ca", "a", 0)]));
    assert_eq!(field, "edits.0.scene_id");
    // Within its own scene a move adds no edge.
    apply_ok(&mut g, vec![add("c", "c1", "cam"), item_move("ca", "c", 1)]);
    assert_eq!(ids(&g, "c"), ["c1", "ca"]);
}

/// ADR-0024 §1's grouping batch and the ungrouping batch keep every item ID, and each
/// undoes exactly.
#[test]
fn grouping_and_ungrouping_keep_item_ids() {
    let mut g = fixture();
    apply_ok(
        &mut g,
        vec![
            add("a", "bottom", "cam"),
            add("a", "x", "mic"),
            add("a", "y", "cam"),
            add("a", "top", "mic"),
        ],
    );
    let before = g.state().clone();
    // Group `x` and `y`: the placement goes at the lowest selected index, then the
    // selected items move in, bottom first.
    apply_ok(
        &mut g,
        vec![
            scene_create("grp"),
            set_role("grp", SceneRole::Group),
            scene_source("sgrp", "grp"),
            add("a", "place", "sgrp"),
            item_move("place", "a", 1),
            item_move("x", "grp", 0),
            item_move("y", "grp", 1),
        ],
    );
    assert_eq!(ids(&g, "a"), ["bottom", "place", "top"]);
    assert_eq!(ids(&g, "grp"), ["x", "y"]);
    assert_eq!(role(&g, "grp"), SceneRole::Group);
    // A group can be placed twice, like any scene (ADR-0040 §2).
    apply_ok(&mut g, vec![add("b", "again", "sgrp")]);
    // Ungroup: move the children back above the placement, then remove the group.
    apply_ok(
        &mut g,
        vec![
            Edit::ItemRemove {
                item_id: ItemId::new("again"),
            },
            item_move("x", "a", 2),
            item_move("y", "a", 3),
            Edit::ItemRemove {
                item_id: ItemId::new("place"),
            },
            Edit::SourceDelete {
                source_id: SourceId::new("sgrp"),
            },
            Edit::SceneDelete {
                scene_id: SceneId::new("grp"),
            },
        ],
    );
    assert_eq!(g.state(), &before);
}

#[test]
fn placement_edits_take_only_the_items_own_scene() {
    let mut g = fixture();
    apply_ok(&mut g, vec![add("a", "i", "cam")]);
    let visible = |scene: Option<&str>| Edit::ItemSetVisible {
        item_id: ItemId::new("i"),
        scene_id: scene.map(SceneId::new),
        visible: false,
    };
    // Its own scene, named or not, changes the item, and the inverse keeps the address.
    let applied = apply_ok(&mut g, vec![visible(Some("a"))]);
    assert_eq!(
        applied.inverse.edits,
        vec![Edit::ItemSetVisible {
            item_id: ItemId::new("i"),
            scene_id: Some(SceneId::new("a")),
            visible: true,
        }]
    );
    assert!(
        !g.state()
            .item(&ItemId::new("i"))
            .expect("exists")
            .1
            .visible()
    );
    apply_ok(&mut g, vec![visible(None)]);
    // Any other scene is rejected until keyframe scenes exist.
    let (field, reason) = invalid_reason(apply_err(&mut g, vec![visible(Some("b"))]));
    assert_eq!(field, "edits.0.scene_id");
    assert!(reason.contains("keyframe"), "{reason}");
    for edit in [
        Edit::ItemSetTransform {
            item_id: ItemId::new("i"),
            scene_id: Some(SceneId::new("b")),
            transform: ItemTransform::IDENTITY,
        },
        Edit::ItemSetCrop {
            item_id: ItemId::new("i"),
            scene_id: Some(SceneId::new("c")),
            crop: Crop::NONE,
        },
        Edit::ItemSetGeometry {
            item_id: ItemId::new("i"),
            scene_id: Some(SceneId::new("b")),
            geometry: boxed(),
        },
    ] {
        let (field, _) = invalid_reason(apply_err(&mut g, vec![edit]));
        assert_eq!(field, "edits.0.scene_id");
    }
    assert!(matches!(
        apply_err(&mut g, vec![visible(Some("nope"))]),
        ProtocolError::NotFound {
            entity: EntityKind::Scene,
            ..
        }
    ));
}

/// A 200×100 box centred at (300, 200), rotated 90° clockwise and filled.
fn boxed() -> ItemGeometry {
    ItemGeometry {
        fit_mode: FitMode::Fill,
        rotation_deg: Finite32::new(90.0).expect("finite"),
        ..ItemGeometry::try_new(
            Vec2::new(300.0, 200.0).expect("finite"),
            Size::try_new(200.0, 100.0).expect("positive"),
        )
        .expect("positive")
    }
}

fn set_geometry(item: &str, geometry: ItemGeometry) -> Edit {
    Edit::ItemSetGeometry {
        item_id: ItemId::new(item),
        scene_id: None,
        geometry,
    }
}

fn placement(graph: &Graph, item: &str) -> Placement {
    *graph
        .state()
        .item(&ItemId::new(item))
        .expect("item exists")
        .1
        .placement()
}

#[test]
fn set_geometry_converts_a_legacy_item_and_undo_restores_the_transform() {
    let mut graph = fixture();
    let moved = ItemTransform {
        position: Vec2::new(5.0, 6.0).expect("finite"),
        ..ItemTransform::IDENTITY
    };
    apply_ok(
        &mut graph,
        vec![
            add("a", "i1", "cam"),
            Edit::ItemSetTransform {
                item_id: ItemId::new("i1"),
                scene_id: None,
                transform: moved,
            },
        ],
    );
    let applied = apply_ok(&mut graph, vec![set_geometry("i1", boxed())]);
    assert_eq!(placement(&graph, "i1"), Placement::Geometry(boxed()));
    assert_eq!(
        applied.inverse.edits,
        vec![Edit::ItemSetTransform {
            item_id: ItemId::new("i1"),
            scene_id: None,
            transform: moved,
        }]
    );
    // Writing a transform back makes it legacy again; its inverse restores the geometry.
    let applied = apply_ok(
        &mut graph,
        vec![Edit::ItemSetTransform {
            item_id: ItemId::new("i1"),
            scene_id: None,
            transform: moved,
        }],
    );
    assert_eq!(placement(&graph, "i1"), Placement::Legacy(moved));
    assert_eq!(applied.inverse.edits, vec![set_geometry("i1", boxed())]);
}

#[test]
fn removing_a_geometry_item_re_adds_it_with_its_geometry() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![Edit::ItemAdd {
            scene_id: SceneId::new("a"),
            item_id: ItemId::new("i1"),
            source_id: SourceId::new("cam"),
            transform: None,
            geometry: Some(boxed()),
        }],
    );
    let applied = apply_ok(
        &mut graph,
        vec![Edit::ItemRemove {
            item_id: ItemId::new("i1"),
        }],
    );
    assert_eq!(
        applied.inverse.edits,
        vec![Edit::ItemAdd {
            scene_id: SceneId::new("a"),
            item_id: ItemId::new("i1"),
            source_id: SourceId::new("cam"),
            transform: None,
            geometry: Some(boxed()),
        }]
    );
}

#[test]
fn a_geometry_item_reads_its_geometry_and_a_transform_view() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![
            add("a", "legacy", "cam"),
            Edit::ItemAdd {
                scene_id: SceneId::new("a"),
                item_id: ItemId::new("boxed"),
                source_id: SourceId::new("cam"),
                transform: None,
                geometry: Some(boxed()),
            },
        ],
    );
    let Some(Ok(QueryResult::ItemList { items, .. })) = graph.query(&Query::ItemList {
        scene_id: SceneId::new("a"),
    }) else {
        panic!("item.list");
    };
    assert_eq!(items[0].placement, PlacementForm::Legacy);
    assert_eq!(items[0].geometry, None);
    assert_eq!(items[0].transform, ItemTransform::IDENTITY);
    assert_eq!(items[1].placement, PlacementForm::Geometry);
    assert_eq!(items[1].geometry, Some(boxed()));
    // Rotated 90° clockwise about (300, 200), the top-left corner (-100, -50) lands at
    // (300 + 50, 200 - 100).
    let view = items[1].transform;
    assert!((view.position.x.get() - 350.0).abs() < 1e-3, "{view:?}");
    assert!((view.position.y.get() - 100.0).abs() < 1e-3, "{view:?}");
    assert_eq!(view.scale, ItemTransform::IDENTITY.scale);
    assert_eq!(view.rotation_deg, boxed().rotation_deg);
}

#[test]
fn invalid_geometries_name_the_field() {
    let mut graph = fixture();
    apply_ok(&mut graph, vec![add("a", "i1", "cam")]);
    let finite = |v: f32| Finite32::new(v).expect("finite");
    let cases = [
        (
            ItemGeometry {
                size: Size {
                    width: finite(-1.0),
                    height: finite(10.0),
                },
                ..boxed()
            },
            "edits.0.geometry.size",
        ),
        (
            ItemGeometry {
                perspective_px: Some(finite(0.0)),
                ..boxed()
            },
            "edits.0.geometry.perspective_px",
        ),
        (
            ItemGeometry {
                tilt_deg: finite(1.0),
                ..boxed()
            },
            "edits.0.geometry.tilt_deg",
        ),
        (
            ItemGeometry {
                turn_deg: finite(1.0),
                ..boxed()
            },
            "edits.0.geometry.turn_deg",
        ),
        (
            ItemGeometry {
                depth_px: finite(1.0),
                ..boxed()
            },
            "edits.0.geometry.depth_px",
        ),
        (
            ItemGeometry {
                perspective_px: Some(finite(800.0)),
                ..boxed()
            },
            "edits.0.geometry.perspective_px",
        ),
        (
            ItemGeometry {
                center: Vec2::new(f32::MAX, 0.0).expect("finite"),
                size: Size {
                    width: finite(f32::MAX),
                    height: finite(10.0),
                },
                rotation_deg: finite(180.0),
                ..boxed()
            },
            "edits.0.geometry",
        ),
    ];
    for (geometry, field) in cases {
        let (got, _) = invalid_reason(apply_err(&mut graph, vec![set_geometry("i1", geometry)]));
        assert_eq!(got, field, "{geometry:?}");
    }
    let both = Edit::ItemAdd {
        scene_id: SceneId::new("a"),
        item_id: ItemId::new("i2"),
        source_id: SourceId::new("cam"),
        transform: Some(ItemTransform::IDENTITY),
        geometry: Some(boxed()),
    };
    let (field, _) = invalid_reason(apply_err(&mut graph, vec![both]));
    assert_eq!(field, "edits.0.geometry");
}

#[test]
fn canvas_set_size_is_checked_and_undone() {
    let mut graph = fixture();
    assert_eq!(graph.state().canvas(), Canvas::DEFAULT);
    let applied = apply_ok(
        &mut graph,
        vec![Edit::CanvasSetSize {
            width: 1080,
            height: 1920,
        }],
    );
    assert_eq!(
        graph.state().canvas(),
        Canvas {
            width: 1080,
            height: 1920
        }
    );
    assert_eq!(
        applied.inverse.edits,
        vec![Edit::CanvasSetSize {
            width: 1920,
            height: 1080,
        }]
    );
    for (width, height, field) in [
        (0, 1080, "edits.0.width"),
        (1920, 0, "edits.0.height"),
        (crate::MAX_CANVAS_EDGE + 1, 1080, "edits.0.width"),
    ] {
        let (got, _) = invalid_reason(apply_err(
            &mut graph,
            vec![Edit::CanvasSetSize { width, height }],
        ));
        assert_eq!(got, field);
    }
    apply_ok(
        &mut graph,
        vec![Edit::CanvasSetSize {
            width: crate::MAX_CANVAS_EDGE,
            height: crate::MAX_CANVAS_EDGE,
        }],
    );
}

fn set_source(item: &str, source: &str) -> Edit {
    Edit::ItemSetSource {
        item_id: ItemId::new(item),
        source_id: SourceId::new(source),
    }
}

fn duplicate(source: &str, copy: &str) -> Edit {
    Edit::SourceDuplicate {
        source_id: SourceId::new(source),
        new_source_id: SourceId::new(copy),
        name: format!("Copy of {source}"),
    }
}

/// The `duplicated_from` that `source.list` reports for `id`.
fn reported_original(graph: &Graph, id: &str) -> Option<String> {
    let Some(QueryResult::SourceList { sources, .. }) =
        graph.query(&Query::SourceList).and_then(Result::ok)
    else {
        panic!("source.list answers");
    };
    sources
        .into_iter()
        .find(|s| s.source_id.as_str() == id)
        .expect("listed")
        .duplicated_from
        .map(|o| o.to_string())
}

/// ADR-0023 §4: `item.set_source` changes only which source an item places.
#[test]
fn set_source_keeps_the_item_and_its_placement() {
    let mut g = fixture();
    let moved = ItemTransform {
        position: Vec2::new(5.0, 6.0).expect("finite"),
        ..ItemTransform::IDENTITY
    };
    apply_ok(
        &mut g,
        vec![
            add("a", "i1", "cam"),
            add("a", "i2", "mic"),
            add("a", "i3", "cam"),
            Edit::ItemSetTransform {
                item_id: ItemId::new("i1"),
                scene_id: None,
                transform: moved,
            },
            Edit::ItemSetCrop {
                item_id: ItemId::new("i1"),
                scene_id: None,
                crop: Crop {
                    left: 4,
                    ..Crop::NONE
                },
            },
            Edit::ItemSetVisible {
                item_id: ItemId::new("i1"),
                scene_id: None,
                visible: false,
            },
        ],
    );
    let before = g.state().item(&ItemId::new("i1")).expect("i1").1.clone();
    let applied = apply_ok(&mut g, vec![set_source("i1", "sb")]);
    assert_eq!(applied.inverse.edits, vec![set_source("i1", "cam")]);
    let (scene, item) = g.state().item(&ItemId::new("i1")).expect("i1");
    assert_eq!(scene.id().as_str(), "a");
    assert_eq!(item.source_id().as_str(), "sb");
    assert_eq!(item.placement(), before.placement());
    assert_eq!(item.crop(), before.crop());
    assert!(!item.visible());
    assert_eq!(ids(&g, "a"), ["i1", "i2", "i3"]);
    // The other placement of `cam` is untouched.
    assert_eq!(g.state().placements(&SourceId::new("cam")).count(), 1);
    // Setting the source it already places is allowed and undoes to itself.
    let applied = apply_ok(&mut g, vec![set_source("i1", "sb")]);
    assert_eq!(applied.inverse.edits, vec![set_source("i1", "sb")]);
}

#[test]
fn set_source_rejects_missing_objects_and_cycles() {
    let mut g = fixture();
    apply_ok(&mut g, vec![add("a", "i1", "cam"), add("b", "ib", "sa")]);
    assert_eq!(
        apply_err(&mut g, vec![set_source("ghost", "cam")]),
        ProtocolError::NotFound {
            entity: EntityKind::Item,
            id: "ghost".into(),
        }
    );
    assert_eq!(
        apply_err(&mut g, vec![set_source("i1", "ghost")]),
        ProtocolError::NotFound {
            entity: EntityKind::Source,
            id: "ghost".into(),
        }
    );
    // `a` can't show itself.
    let (field, reason) = invalid_reason(apply_err(&mut g, vec![set_source("i1", "sa")]));
    assert_eq!(field, "edits.0.source_id");
    assert!(
        reason.ends_with("scene `a` -> source `sa` -> scene `a`"),
        "{reason}"
    );
    // `b` shows `a`, so `a` can't show `b`, even in a later edit of the batch.
    let (field, reason) = invalid_reason(apply_err(
        &mut g,
        vec![add("c", "ic", "cam"), set_source("i1", "sb")],
    ));
    assert_eq!(field, "edits.1.source_id");
    assert!(
        reason.ends_with("scene `a` -> source `sb` -> scene `b` -> source `sa` -> scene `a`"),
        "{reason}"
    );
    // Through a settings reference: `mic` masks with `a`, so `a` can't place `mic`.
    apply_ok(
        &mut g,
        vec![set_settings("mic", &[("mask", scene_ref("a"))])],
    );
    let (field, _) = invalid_reason(apply_err(&mut g, vec![set_source("i1", "mic")]));
    assert_eq!(field, "edits.0.source_id");
    // Retargeting `ib` away from `sa` first frees `a` to show `b`, in one batch.
    apply_ok(
        &mut g,
        vec![set_source("ib", "cam"), set_source("i1", "sb")],
    );
}

/// ADR-0023 §4: `source.duplicate` records the original, the link is reported only while
/// the original exists, and every inverse keeps it.
#[test]
fn duplicated_from_is_a_plain_link() {
    let mut g = fixture();
    apply_ok(
        &mut g,
        vec![duplicate("cam", "cam2"), duplicate("cam2", "cam3")],
    );
    let state = g.state();
    let link = |id: &str| {
        state
            .source(&SourceId::new(id))
            .and_then(|s| s.duplicated_from())
            .map(ToString::to_string)
    };
    assert_eq!(link("cam2").as_deref(), Some("cam"));
    // A copy of a copy names the copy it was made from.
    assert_eq!(link("cam3").as_deref(), Some("cam2"));
    assert_eq!(link("cam"), None);
    assert_eq!(reported_original(&g, "cam2").as_deref(), Some("cam"));

    // Deleting the original is allowed; the link then reads as absent, and undo brings
    // it back.
    let applied = apply_ok(
        &mut g,
        vec![Edit::SourceDelete {
            source_id: SourceId::new("cam"),
        }],
    );
    assert_eq!(reported_original(&g, "cam2"), None);
    g.apply(&applied.inverse.edits, None).expect("undo");
    assert_eq!(reported_original(&g, "cam2").as_deref(), Some("cam"));

    // Deleting the copy undoes to a `source.create` that carries the link, even when the
    // original went first in the same batch.
    let applied = apply_ok(
        &mut g,
        vec![
            Edit::SourceDelete {
                source_id: SourceId::new("cam"),
            },
            Edit::SourceDelete {
                source_id: SourceId::new("cam3"),
            },
            Edit::SourceDelete {
                source_id: SourceId::new("cam2"),
            },
        ],
    );
    let Some(Edit::SourceCreate {
        source_id,
        duplicated_from,
        ..
    }) = applied.inverse.edits.first()
    else {
        panic!("inverse starts with source.create");
    };
    assert_eq!(source_id.as_str(), "cam2");
    assert_eq!(duplicated_from.as_ref().map(SourceId::as_str), Some("cam"));

    // `source.create` may name a source that doesn't exist, but not itself.
    let linked = |id: &str, original: &str| Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: "Linked".into(),
        kind: SourceKind::Builtin {
            kind: "color".into(),
        },
        settings: Settings::new(),
        duplicated_from: Some(SourceId::new(original)),
    };
    apply_ok(&mut g, vec![linked("x", "gone")]);
    assert_eq!(reported_original(&g, "x"), None);
    let (field, reason) = invalid_reason(apply_err(&mut g, vec![linked("y", "y")]));
    assert_eq!(
        (field.as_str(), reason.as_str()),
        ("edits.0.duplicated_from", "must not name the source itself")
    );
    // The link is not a dependency: it blocks no delete and closes no cycle.
    apply_ok(&mut g, vec![add("a", "ix", "x"), linked("sa2", "sa")]);
}

/// ADR-0023 §4: converting between reference and clone is one batch each way, and keeps
/// the item.
#[test]
fn reference_and_clone_convert_in_one_batch_each() {
    let mut g = fixture();
    apply_ok(&mut g, vec![add("a", "i1", "cam"), add("b", "i2", "cam")]);
    // Reference -> clone.
    apply_ok(
        &mut g,
        vec![duplicate("cam", "cam_c"), set_source("i1", "cam_c")],
    );
    assert_eq!(g.state().placements(&SourceId::new("cam")).count(), 1);
    assert_eq!(reported_original(&g, "cam_c").as_deref(), Some("cam"));
    // The clone changes independently of the original.
    apply_ok(
        &mut g,
        vec![set_settings("cam_c", &[("color", json!("#ff0000ff"))])],
    );
    assert_ne!(
        g.state()
            .source(&SourceId::new("cam_c"))
            .map(|s| s.settings()),
        g.state()
            .source(&SourceId::new("cam"))
            .map(|s| s.settings())
    );
    // Clone -> reference: back to `duplicated_from`, then drop the unplaced clone.
    let applied = apply_ok(
        &mut g,
        vec![
            set_source("i1", "cam"),
            Edit::SourceDelete {
                source_id: SourceId::new("cam_c"),
            },
        ],
    );
    assert_eq!(g.state().placements(&SourceId::new("cam")).count(), 2);
    assert!(g.state().source(&SourceId::new("cam_c")).is_none());
    assert_eq!(ids(&g, "a"), ["i1"]);
    // Undo restores the clone with its own settings and its link.
    g.apply(&applied.inverse.edits, None).expect("undo");
    assert_eq!(reported_original(&g, "cam_c").as_deref(), Some("cam"));
    assert_eq!(
        g.state()
            .item(&ItemId::new("i1"))
            .map(|(_, i)| i.source_id().as_str()),
        Some("cam_c")
    );
}

fn tone_create(id: &str) -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: format!("Tone {id}"),
        kind: SourceKind::Builtin {
            kind: "tone".into(),
        },
        settings: Settings::new(),
        duplicated_from: None,
    }
}

fn db(value: f32) -> Finite32 {
    Finite32::new(value).expect("finite")
}

/// ADR-0028's defaults: an audio input follows `always` and sends to the master at
/// 0 dB, a nested scene follows `when_shown` and sends nowhere, and a kind without
/// audio has no audio settings.
#[test]
fn new_sources_get_their_kinds_audio_defaults() {
    let mut graph = fixture();
    apply_ok(&mut graph, vec![tone_create("t")]);
    let state = graph.state();
    let audio = |id: &str| {
        state
            .source(&SourceId::new(id))
            .and_then(|s| s.audio().copied())
    };
    assert_eq!(audio("t"), Some(SourceAudio::new(AudioFollow::Always)));
    assert_eq!(audio("sa"), Some(SourceAudio::new(AudioFollow::WhenShown)));
    assert_eq!(audio("cam"), None);
    let sends: Vec<_> = state
        .sends()
        .map(|(source, bus, gain)| (source.as_str(), bus.as_str(), gain))
        .collect();
    assert_eq!(sends, [("t", "master", Finite32::ZERO)]);
}

/// A copy takes the original's audio settings and sends; deleting a source takes its
/// sends with it and its inverse puts back exactly what the delete removed.
#[test]
fn duplicate_copies_audio_and_delete_restores_it() {
    let mut graph = fixture();
    apply_ok(
        &mut graph,
        vec![
            tone_create("t"),
            Edit::BusCreate {
                bus_id: BusId::new("vod"),
                name: "VOD".into(),
            },
            Edit::SendSet {
                source_id: SourceId::new("t"),
                bus_id: BusId::new("vod"),
                gain_db: db(-3.0),
            },
            Edit::SendRemove {
                source_id: SourceId::new("t"),
                bus_id: BusId::master(),
            },
            Edit::SourceSetAudio {
                source_id: SourceId::new("t"),
                enabled: Some(false),
                follow: None,
                gain_db: Some(db(-6.0)),
                muted: None,
            },
            Edit::SourceDuplicate {
                source_id: SourceId::new("t"),
                new_source_id: SourceId::new("t2"),
                name: "Copy".into(),
            },
        ],
    );
    let state = graph.state();
    let (t, t2) = (SourceId::new("t"), SourceId::new("t2"));
    assert_eq!(
        state.source(&t2).and_then(|s| s.audio().copied()),
        state.source(&t).and_then(|s| s.audio().copied())
    );
    assert_eq!(state.send(&t2, &BusId::new("vod")), Some(db(-3.0)));
    assert_eq!(state.send(&t2, &BusId::master()), None);

    let applied = apply_ok(&mut graph, vec![Edit::SourceDelete { source_id: t2 }]);
    assert_eq!(
        applied.inverse.edits[1..],
        [
            Edit::SendRemove {
                source_id: SourceId::new("t2"),
                bus_id: BusId::master(),
            },
            Edit::SendSet {
                source_id: SourceId::new("t2"),
                bus_id: BusId::new("vod"),
                gain_db: db(-3.0),
            },
            Edit::SourceSetAudio {
                source_id: SourceId::new("t2"),
                enabled: Some(false),
                follow: None,
                gain_db: Some(db(-6.0)),
                muted: None,
            },
        ]
    );
}

/// Deleting a bus deletes the sends into it, and its inverse restores the bus, its
/// fader and the sends.
#[test]
fn deleting_a_bus_takes_its_sends_and_undoes_exactly() {
    let mut graph = fixture();
    let vod = BusId::new("vod");
    apply_ok(
        &mut graph,
        vec![
            tone_create("t"),
            Edit::BusCreate {
                bus_id: vod.clone(),
                name: "VOD".into(),
            },
            Edit::BusSetFader {
                bus_id: vod.clone(),
                gain_db: None,
                muted: Some(true),
            },
            Edit::SendSet {
                source_id: SourceId::new("t"),
                bus_id: vod.clone(),
                gain_db: db(-3.0),
            },
        ],
    );
    let applied = apply_ok(
        &mut graph,
        vec![Edit::BusDelete {
            bus_id: vod.clone(),
        }],
    );
    assert_eq!(graph.state().bus(&vod), None);
    assert_eq!(graph.state().send(&SourceId::new("t"), &vod), None);
    assert_eq!(applied.inverse.edits.len(), 3);
    let error = apply_err(
        &mut graph,
        vec![Edit::BusDelete {
            bus_id: BusId::master(),
        }],
    );
    assert_eq!(
        invalid_reason(error),
        (
            "edits.0.bus_id".to_owned(),
            "the master bus can't be deleted".to_owned()
        )
    );
}
