//! The collection file: the v1 fixture, round trips, migrations and refused files.

use super::{LoadError, Migration, SCHEMA_VERSION, migrate_with};
use crate::Graph;
use jstream_protocol::{
    Edit, EncoderProfile, EncoderProfileId, KindCategory, KindDescriptor, OutputId, OutputKind,
    ParamDescriptor, ParamType, ProtocolError, Rev, SceneId, SceneRole, SourceId, SourceKind,
    parity,
};
use serde_json::{Value, json};

/// A collection file at schema version 1. It must keep loading for as long as version 1
/// is supported (R-API-003): when the schema changes, this file stays as it is, a
/// migration upgrades it, and a fixture of the new version is added beside it.
const V1: &str = include_str!("fixtures/v1.json");

/// The parity fixture's `webcam`, and `color`, whose `follow` parameter refers to
/// another source.
fn kinds() -> Vec<KindDescriptor> {
    let mut kinds = parity::kinds();
    kinds.push(KindDescriptor {
        kind_id: "color".into(),
        category: KindCategory::Source,
        name: "Colour".into(),
        version: 2,
        video: true,
        audio: false,
        params: vec![
            ParamDescriptor {
                id: "color".into(),
                ty: ParamType::Color { alpha: true },
                default: json!("#000000ff"),
                label: "Colour".into(),
                description: None,
                group: None,
                advanced: false,
                visible_if: None,
                live: true,
            },
            ParamDescriptor {
                id: "follow".into(),
                ty: ParamType::Source { media: None },
                default: Value::Null,
                label: "Follow".into(),
                description: None,
                group: None,
                advanced: false,
                visible_if: None,
                live: true,
            },
        ],
    });
    kinds
}

fn color(id: &str, follow: Option<&str>) -> Edit {
    let follow = follow.map_or(Value::Null, |id| json!({"type": "source", "source_id": id}));
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: id.into(),
        kind: SourceKind::Builtin {
            kind: "color".into(),
        },
        settings: [("follow", follow)].into_iter().collect(),
        duplicated_from: None,
    }
}

/// The parity collection, plus `a_tint`, whose settings refer to `z_base`, which sorts
/// after it, and scene `b` made a group, in one more batch: rev 2. That batch also
/// deletes the virtual camera `vcam`, which the parity collection gained after the
/// fixture was written; `a_virtual_camera_round_trips` covers it.
fn fixture_graph() -> Graph {
    let mut graph = Graph::new().with_kinds(kinds());
    let applied = graph.apply_batch(&parity::collection());
    assert!(applied.is_ok(), "{applied:?}");
    let applied = graph.apply(
        &[
            color("z_base", None),
            color("a_tint", Some("z_base")),
            Edit::SceneSetRole {
                scene_id: SceneId::new("b"),
                role: SceneRole::Group,
            },
            Edit::OutputDelete {
                output_id: OutputId::new("vcam"),
            },
        ],
        None,
    );
    assert!(applied.is_ok(), "{applied:?}");
    graph
}

fn load(json: &str) -> Result<Graph, LoadError> {
    Graph::new().with_kinds(kinds()).with_collection_json(json)
}

fn edit_file(edit: impl FnOnce(&mut Value)) -> String {
    let mut value: Value = serde_json::from_str(V1).unwrap_or_default();
    edit(&mut value);
    value.to_string()
}

#[test]
fn the_fixture_is_what_the_collection_saves() {
    assert_eq!(fixture_graph().to_collection_json(), V1);
}

#[test]
fn the_v1_fixture_loads_exactly() {
    let expected = fixture_graph();
    let loaded = load(V1);
    assert_eq!(loaded.as_ref().map(Graph::rev), Ok(Rev(2)));
    assert_eq!(loaded.as_ref().map(Graph::state), Ok(expected.state()));
    assert_eq!(loaded.map(|g| g.kinds), Ok(expected.kinds));
}

#[test]
fn an_empty_collection_round_trips() {
    let graph = Graph::new().with_kinds(kinds());
    let json = graph.to_collection_json();
    assert_eq!(
        serde_json::from_str::<Value>(&json).ok(),
        Some(json!({
            "schema_version": 1,
            "rev": 0,
            "canvas": {"width": 1920, "height": 1080},
            "scenes": [],
            "sources": [],
            "outputs": []
        }))
    );
    assert_eq!(load(&json), Ok(graph));
}

/// A virtual camera output saves as its bare kind and loads back the same.
#[test]
fn a_virtual_camera_round_trips() {
    let mut graph = fixture_graph();
    let applied = graph.apply(
        &[Edit::OutputCreate {
            output_id: OutputId::new("cam"),
            kind: OutputKind::VirtualCam,
        }],
        None,
    );
    assert!(applied.is_ok(), "{applied:?}");
    let json = graph.to_collection_json();
    let saved: Value = serde_json::from_str(&json).unwrap_or_default();
    assert!(
        saved["outputs"].as_array().is_some_and(
            |outputs| outputs.contains(&json!({"id": "cam", "kind": {"type": "virtual_cam"}}))
        ),
        "{json}"
    );
    assert_eq!(load(&json), Ok(graph));
}

/// A copy's `duplicated_from` survives a save and load, also once its original is gone,
/// and a source saved without one loads with none.
#[test]
fn clone_links_round_trip() {
    let mut graph = Graph::new().with_kinds(kinds());
    let duplicate = Edit::SourceDuplicate {
        source_id: SourceId::new("orig"),
        new_source_id: SourceId::new("copy"),
        name: "copy".into(),
    };
    let delete = Edit::SourceDelete {
        source_id: SourceId::new("orig"),
    };
    let applied = graph.apply(
        &[color("orig", None), color("plain", None), duplicate],
        None,
    );
    assert!(applied.is_ok(), "{applied:?}");
    let applied = graph.apply(&[delete], None);
    assert!(applied.is_ok(), "{applied:?}");
    let loaded = load(&graph.to_collection_json());
    assert_eq!(loaded.as_ref(), Ok(&graph));
    let link = |id: &str| {
        loaded
            .as_ref()
            .ok()
            .and_then(|g| g.state().source(&SourceId::new(id)))
            .map(|s| s.duplicated_from().cloned())
    };
    assert_eq!(link("copy"), Some(Some(SourceId::new("orig"))));
    assert_eq!(link("plain"), Some(None));
}

#[test]
fn encoder_profiles_and_an_outputs_profile_round_trip() {
    let mut graph = fixture_graph();
    let mut profile = EncoderProfile::DEFAULT;
    profile.video.keyframe_interval_s = 4;
    let applied = graph.apply(
        &[
            Edit::EncoderCreate {
                profile_id: EncoderProfileId::new("mid"),
                name: "Middle".into(),
                profile,
            },
            Edit::OutputSetEncoder {
                output_id: OutputId::new("tw"),
                profile_id: Some(EncoderProfileId::new("mid")),
            },
        ],
        None,
    );
    assert!(applied.is_ok(), "{applied:?}");
    let json = graph.to_collection_json();
    let value: Value = serde_json::from_str(&json).unwrap_or_default();
    assert_eq!(
        // Ordered by ID, after the fixture's `hq` and `low`.
        value["encoders"][2],
        json!({"id": "mid", "name": "Middle", "profile": serde_json::to_value(profile).ok()})
    );
    assert_eq!(value["outputs"][1]["encoder_profile"], json!("mid"));
    assert_eq!(load(&json), Ok(graph));
}

/// A file saved before encoder profiles existed has neither `encoders` nor any
/// output's `encoder_profile`. It loads with no profiles, every output on the default.
#[test]
fn a_file_without_encoder_profiles_loads() {
    let file = edit_file(|v| {
        v.as_object_mut().map(|o| o.remove("encoders"));
        for output in v["outputs"].as_array_mut().into_iter().flatten() {
            output.as_object_mut().map(|o| o.remove("encoder_profile"));
        }
    });
    let Ok(graph) = load(&file) else {
        panic!("the file did not load");
    };
    assert_eq!(graph.state().encoders().len(), 0);
    assert!(
        graph
            .state()
            .outputs()
            .all(|o| o.encoder_profile().is_none())
    );
}

#[test]
fn an_output_naming_a_missing_profile_is_refused() {
    let file = edit_file(|v| v["outputs"][0]["encoder_profile"] = json!("gone"));
    assert!(
        matches!(load(&file), Err(LoadError::Invalid { ref object, .. }) if object == "output `rec`"),
        "{:?}",
        load(&file)
    );
}

#[test]
fn a_loaded_collection_keeps_editing_from_its_rev() {
    let Ok(mut graph) = load(V1) else {
        panic!("the fixture did not load");
    };
    let applied = graph.apply(
        &[Edit::SourceDelete {
            source_id: SourceId::new("a_tint"),
        }],
        Some(Rev(2)),
    );
    assert_eq!(applied.map(|a| a.rev), Ok(Rev(3)));
}

#[test]
fn unsupported_versions_are_refused() {
    for (version, found) in [
        (Value::Null, 0),
        (json!(0), 0),
        (json!("1"), 0),
        (json!(SCHEMA_VERSION + 1), u64::from(SCHEMA_VERSION) + 1),
        (json!(u64::MAX), u64::MAX),
    ] {
        let file = edit_file(|v| v["schema_version"] = version.clone());
        assert_eq!(load(&file), Err(LoadError::Version { found }), "{version}");
    }
    let file = edit_file(|v| {
        v.as_object_mut().map(|o| o.remove("schema_version"));
    });
    assert_eq!(load(&file), Err(LoadError::Version { found: 0 }));
}

#[test]
fn malformed_files_are_refused() {
    let unknown = edit_file(|v| v["scenes"][0]["opacity"] = json!(1));
    let missing = edit_file(|v| {
        v["scenes"][0].as_object_mut().map(|o| o.remove("items"));
    });
    for file in ["", "{", "[]", "null", unknown.as_str(), missing.as_str()] {
        assert!(
            matches!(
                load(file),
                Err(LoadError::Parse(_) | LoadError::Version { .. })
            ),
            "{file}"
        );
    }
}

#[test]
fn a_collection_an_edit_would_refuse_is_refused() {
    let dangling = edit_file(|v| v["scenes"][1]["items"][0]["source_id"] = json!("gone"));
    assert!(
        matches!(
            load(&dangling),
            Err(LoadError::Invalid { ref object, ref error })
                if object.starts_with("item `")
                    && matches!(**error, ProtocolError::NotFound { .. })
        ),
        "{:?}",
        load(&dangling)
    );

    // `z_base` follows `a_tint`, which follows `z_base`.
    let cycle = edit_file(|v| {
        v["sources"][3]["settings"]["follow"] = json!({"type": "source", "source_id": "a_tint"});
    });
    assert!(
        matches!(load(&cycle), Err(LoadError::Invalid { .. })),
        "{:?}",
        load(&cycle)
    );

    let unknown_kind = edit_file(|v| v["sources"][0]["kind"]["kind"] = json!("hologram"));
    assert!(
        matches!(load(&unknown_kind), Err(LoadError::Invalid { .. })),
        "{:?}",
        load(&unknown_kind)
    );

    // Output `tw`.
    let bad_url = edit_file(|v| {
        v["outputs"][1]["kind"]["target"]["url"] = json!("rtmp://user:key@host/live");
    });
    assert!(
        matches!(load(&bad_url), Err(LoadError::Invalid { ref object, .. }) if object.starts_with("output `")),
        "{:?}",
        load(&bad_url)
    );
}

#[test]
fn a_source_saved_by_another_kind_version_is_refused() {
    let file = edit_file(|v| v["sources"][0]["kind_version"] = json!(1));
    assert_eq!(
        load(&file),
        Err(LoadError::KindVersion {
            object: "source `a_tint`".into(),
            kind: "color".into(),
            saved: 1,
            current: 2,
        })
    );
}

#[test]
fn errors_name_the_object_but_not_its_values() {
    let file = edit_file(|v| {
        v["outputs"][1]["kind"]["target"]["url"] = json!("rtmp://user:secret-key@host/live");
    });
    let message = load(&file).err().map(|e| e.to_string()).unwrap_or_default();
    assert!(message.starts_with("output `"), "{message}");
    assert!(!message.contains("secret"), "{message}");
}

/// The migration machinery, with stand-ins for versions 1 to 3: an old file runs every
/// later migration in order, a current one runs none, and a failure names its version.
#[test]
fn migrations_run_in_order_from_the_file_version() {
    let to_2: Migration = |v| {
        v["log"] = json!("2");
        Ok(())
    };
    let to_3: Migration = |v| match v["log"].as_str() {
        Some(log) => {
            v["log"] = json!(format!("{log}3"));
            Ok(())
        }
        None => Err("no log".into()),
    };
    let migrations = [to_2, to_3];
    let mut old = json!({"schema_version": 1});
    assert_eq!(migrate_with(&mut old, &migrations), Ok(()));
    assert_eq!(old, json!({"schema_version": 3, "log": "23"}));

    let mut current = json!({"schema_version": 3, "log": "x"});
    assert_eq!(migrate_with(&mut current, &migrations), Ok(()));
    assert_eq!(current, json!({"schema_version": 3, "log": "x"}));

    let mut broken = json!({"schema_version": 2});
    assert_eq!(
        migrate_with(&mut broken, &migrations),
        Err(LoadError::Migration {
            from: 2,
            reason: "no log".into()
        })
    );

    let mut newer = json!({"schema_version": 4});
    assert_eq!(
        migrate_with(&mut newer, &migrations),
        Err(LoadError::Version { found: 4 })
    );
}

/// Each item has exactly one placement form, and the canvas and a geometry pass the
/// same checks as their edits.
#[test]
fn placements_and_the_canvas_are_checked() {
    // `i4` in scene `b` has a geometry; `i3` in scene `a` a legacy transform.
    let both = edit_file(|v| {
        v["scenes"][0]["items"][0]["transform"] = v["scenes"][1]["items"][0]["transform"].clone();
    });
    let neither = edit_file(|v| {
        v["scenes"][0]["items"][0]
            .as_object_mut()
            .map(|o| o.remove("geometry"));
    });
    for file in [both, neither] {
        assert_eq!(
            load(&file),
            Err(LoadError::Parse(
                "item `i4`: exactly one of `transform` and `geometry` must be set".into()
            ))
        );
    }

    let flat = edit_file(|v| v["scenes"][0]["items"][0]["geometry"]["size"]["height"] = json!(0.0));
    assert!(
        matches!(load(&flat), Err(LoadError::Invalid { ref object, .. }) if object == "item `i4`"),
        "{:?}",
        load(&flat)
    );

    let canvas = edit_file(|v| v["canvas"]["width"] = json!(0));
    assert!(
        matches!(load(&canvas), Err(LoadError::Invalid { ref object, .. }) if object == "the canvas"),
        "{:?}",
        load(&canvas)
    );
}

/// R-SEC-001: a malformed file's error never quotes a value from it, so it is safe to
/// log even when a credential sits where a number or a known field should be.
#[test]
fn parse_errors_quote_nothing_from_the_file() {
    let secret = "rtmp://user:passSECRET@host/live";
    let wrong_type = edit_file(|v| v["canvas"]["width"] = json!(secret));
    let unknown_key = edit_file(|v| v["scenes"][0][secret] = json!(1));
    // An unknown container parses (as `Unknown`) and is refused by the edit check.
    let unknown_variant = edit_file(|v| v["outputs"][0]["kind"]["container"] = json!(secret));
    let unknown_tag = edit_file(|v| v["sources"][1]["kind"]["type"] = json!(secret));
    let syntax = format!("{{\"schema_version\": 1, \"rev\": {secret}}}");
    for file in [
        wrong_type,
        unknown_key,
        unknown_variant,
        unknown_tag,
        syntax,
    ] {
        let error = load(&file);
        assert!(error.is_err(), "{file}");
        let message = error.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(!message.contains("SECRET"), "{message}");
    }
    let missing = edit_file(|v| {
        v.as_object_mut().map(|o| o.remove("canvas"));
    });
    assert_eq!(
        load(&missing),
        Err(LoadError::Parse("missing field `canvas`".into()))
    );
}
