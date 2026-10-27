//! The collection file: the v1 fixture, round trips, migrations and refused files.

use super::{LoadError, Migration, SCHEMA_VERSION, migrate_with};
use crate::Graph;
use jstream_protocol::{
    Action, BusId, Chord, Edit, EncoderProfile, EncoderProfileId, FilterChain, FilterId,
    FilterOwner, FolderId, ItemId, KindCategory, KindDescriptor, LabelId, OutputId, OutputKind,
    ParamDescriptor, ParamType, ProtocolError, Rev, SceneId, SceneMeta, SceneRole, SourceId,
    SourceKind, SourceMeta, parity,
};
use serde_json::{Value, json};

/// A collection file at schema version 1. It must keep loading for as long as version 1
/// is supported (R-API-003): when the schema changes, this file stays as it is, a
/// migration upgrades it, and a fixture of the new version is added beside it.
const V1: &str = include_str!("fixtures/v1.json");

/// A collection file at schema version 2, the current one: version 1 with scene
/// filters and item effects (ADR-0065). The same rule applies once version 3 exists.
const V2: &str = include_str!("fixtures/v2.json");

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
        max_outset_px: None,
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
    kinds.push(KindDescriptor {
        kind_id: "scope".into(),
        category: KindCategory::Source,
        name: "Scope".into(),
        version: 1,
        video: true,
        audio: false,
        max_outset_px: None,
        params: vec![ParamDescriptor {
            id: "audio".into(),
            ty: ParamType::Bus,
            default: Value::Null,
            label: "Audio".into(),
            description: None,
            group: None,
            advanced: false,
            visible_if: None,
            live: false,
        }],
    });
    kinds.push(KindDescriptor {
        kind_id: "frame".into(),
        category: KindCategory::VideoFilter,
        name: "Frame".into(),
        version: 1,
        video: true,
        audio: false,
        max_outset_px: Some(4),
        params: vec![ParamDescriptor {
            id: "tint".into(),
            ty: ParamType::Source { media: None },
            default: Value::Null,
            label: "Tint".into(),
            description: None,
            group: None,
            advanced: false,
            visible_if: None,
            live: false,
        }],
    });
    kinds
}

/// A `frame` filter tinted by source `z_base`, which sorts after every scene and item
/// that holds one, so a load must add filters after every source.
fn frame(owner: FilterOwner, id: &str, enabled: Option<bool>) -> Edit {
    Edit::FilterAdd {
        owner,
        chain: FilterChain::Video,
        filter_id: FilterId::new(id),
        kind: "frame".into(),
        settings: [("tint", json!({"type": "source", "source_id": "z_base"}))]
            .into_iter()
            .collect(),
        enabled,
        index: None,
    }
}

/// The v1 fixture's collection, plus a disabled scene filter on scene `a` and an effect
/// on item `i4` (ADR-0065), in one more batch: rev 3.
fn v2_graph() -> Graph {
    let mut graph = fixture_graph();
    let applied = graph.apply(
        &[
            frame(
                FilterOwner::Scene {
                    scene_id: SceneId::new("a"),
                },
                "grade",
                Some(false),
            ),
            frame(
                FilterOwner::Item {
                    item_id: ItemId::new("i4"),
                },
                "edge",
                None,
            ),
        ],
        None,
    );
    assert!(applied.is_ok(), "{applied:?}");
    graph
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
/// deletes the virtual camera `vcam` and clears the hotkeys, which the parity collection
/// gained after the fixture was written; `a_virtual_camera_round_trips` and
/// `hotkeys_round_trip` cover them.
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
            Edit::HotkeyClear {
                chord: Chord::new("F1"),
            },
            Edit::HotkeyClear {
                chord: Chord::new("Ctrl+Digit1"),
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
    assert_eq!(v2_graph().to_collection_json(), V2);
}

#[test]
fn the_v2_fixture_loads_exactly() {
    let expected = v2_graph();
    let loaded = load(V2);
    assert_eq!(loaded.as_ref().map(Graph::rev), Ok(Rev(3)));
    assert_eq!(loaded.as_ref().map(Graph::state), Ok(expected.state()));
}

/// Version 1 upgrades unchanged: it has no scene filters, and its items' `effects`
/// could only be empty.
#[test]
fn the_v1_fixture_loads_exactly() {
    let expected = fixture_graph();
    let loaded = load(V1);
    assert_eq!(loaded.as_ref().map(Graph::rev), Ok(Rev(2)));
    assert_eq!(loaded.as_ref().map(Graph::state), Ok(expected.state()));
    assert_eq!(loaded.map(|g| g.kinds), Ok(expected.kinds));
}

/// A file saved while `webcam` had no settings loads with the `device` default, so it
/// matches the fixture saved since.
#[test]
fn a_webcam_saved_without_settings_loads_with_its_default() {
    let older = edit_file(|file| {
        for source in file["sources"].as_array_mut().into_iter().flatten() {
            if source["kind"]["kind"] == "webcam" {
                source.as_object_mut().map(|s| s.remove("settings"));
            }
        }
    });
    // The webcam's `device` setting is gone; the bus monitor's `device` is not a setting.
    assert!(!older.contains("\"device\":\"\""));
    let loaded = load(&older);
    assert_eq!(
        loaded.as_ref().map(Graph::state),
        Ok(fixture_graph().state())
    );
}

/// A file saved while `display` had no `show_cursor` (before protocol 0.36) loads with
/// its default, on, so it matches the fixture saved since.
#[test]
fn a_display_saved_without_show_cursor_loads_with_its_default() {
    let older = edit_file(|file| {
        for source in file["sources"].as_array_mut().into_iter().flatten() {
            if source["kind"]["kind"] == "display" {
                source["settings"]
                    .as_object_mut()
                    .map(|s| s.remove("show_cursor"));
            }
        }
    });
    assert!(!older.contains("\"show_cursor\""));
    let loaded = load(&older);
    assert_eq!(
        loaded.as_ref().map(Graph::state),
        Ok(fixture_graph().state())
    );
}

#[test]
fn an_empty_collection_round_trips() {
    let graph = Graph::new().with_kinds(kinds());
    let json = graph.to_collection_json();
    assert_eq!(
        serde_json::from_str::<Value>(&json).ok(),
        Some(json!({
            "schema_version": SCHEMA_VERSION,
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

/// Hotkey bindings save ordered by chord, with their actions as `hotkey.set` takes them,
/// and load back the same. A file without them (every v1 file written before they
/// existed, such as the fixture) loads with none.
#[test]
fn hotkeys_round_trip() {
    let mut graph = fixture_graph();
    assert_eq!(graph.state().hotkeys().len(), 0);
    let applied = graph.apply(
        &[
            Edit::HotkeySet {
                chord: Chord::new("Shift+F2"),
                action: Action::OutputStop {
                    output_id: OutputId::new("yt"),
                },
            },
            Edit::HotkeySet {
                chord: Chord::new("F1"),
                action: Action::SceneSwitch {
                    scene_id: SceneId::new("a"),
                    preset_id: None,
                },
            },
        ],
        None,
    );
    assert!(applied.is_ok(), "{applied:?}");
    let json = graph.to_collection_json();
    let saved: Value = serde_json::from_str(&json).unwrap_or_default();
    assert_eq!(
        saved["hotkeys"],
        json!([
            {"chord": "F1", "action": {"type": "scene.switch", "scene_id": "a"}},
            {"chord": "Shift+F2", "action": {"type": "output.stop", "output_id": "yt"}}
        ])
    );
    assert_eq!(load(&json), Ok(graph));
}

/// A binding is checked on load as `hotkey.set` checks it: here its scene is missing.
#[test]
fn a_hotkey_naming_a_missing_scene_is_refused() {
    let file = edit_file(|v| {
        v["hotkeys"] =
            json!([{"chord": "F1", "action": {"type": "scene.switch", "scene_id": "gone"}}]);
    });
    assert_eq!(
        load(&file),
        Err(LoadError::Invalid {
            object: "hotkey `F1`".into(),
            error: Box::new(ProtocolError::NotFound {
                entity: jstream_protocol::EntityKind::Scene,
                id: "gone".into(),
            }),
        })
    );
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

/// A file written before the master stage and outputs had chains (no `master`, no
/// `outputs[].filters`) loads with those chains empty and the sources' filters intact.
#[test]
fn a_file_without_master_or_output_filters_loads() {
    use jstream_protocol::FilterChain;
    let file = edit_file(|v| {
        v.as_object_mut().map(|o| o.remove("master"));
        for output in v["outputs"].as_array_mut().into_iter().flatten() {
            output.as_object_mut().map(|o| o.remove("filters"));
        }
    });
    let Ok(graph) = load(&file) else {
        panic!("the file did not load");
    };
    let state = graph.state();
    assert!(state.master().chain(FilterChain::Audio).is_empty());
    assert!(
        state
            .outputs()
            .all(|o| o.filters().chain(FilterChain::Audio).is_empty())
    );
    let Ok(full) = load(&edit_file(|_| {})) else {
        panic!("the fixture did not load");
    };
    for source in full.state().sources() {
        let Some(loaded) = state.source(source.id()) else {
            panic!("a source was lost");
        };
        assert_eq!(loaded.filters(), source.filters());
    }
}

/// A file written before sends existed (no `sends`, `buses` or `audio`): each audio
/// source keeps the master send its creation gives it, at 0 dB, rather than losing it.
#[test]
fn a_file_from_before_audio_keeps_the_master_sends() {
    let file = edit_file(|v| {
        if let Some(o) = v.as_object_mut() {
            o.remove("sends");
            o.remove("buses");
        }
        for source in v["sources"].as_array_mut().into_iter().flatten() {
            source.as_object_mut().map(|o| o.remove("audio"));
        }
    });
    let Ok(graph) = load(&file) else {
        panic!("the file did not load");
    };
    let sends: Vec<_> = graph
        .state()
        .sends()
        .map(|(source, bus, gain)| (source.as_str().to_owned(), bus.clone(), gain.get()))
        .collect();
    assert_eq!(
        sends,
        [
            ("aux".to_owned(), BusId::master(), 0.0),
            ("cam".to_owned(), BusId::master(), 0.0),
            ("cam_copy".to_owned(), BusId::master(), 0.0),
            ("desk".to_owned(), BusId::master(), 0.0),
            ("mic".to_owned(), BusId::master(), 0.0),
            ("mic_copy".to_owned(), BusId::master(), 0.0),
        ]
    );
}

/// #716: a file saved before webcams had audio gives a `webcam` no audio settings and
/// no send. It loads with the master send a new webcam gets, and its audio off, so
/// opening it never starts recording the webcam's microphone. A webcam the file gives
/// audio settings (every current file does) loads as saved.
#[test]
fn a_webcam_from_before_webcam_audio_loads_with_its_audio_off() {
    let older = edit_file(|file| {
        if let Some(sends) = file["sends"].as_array_mut() {
            sends.retain(|send| !matches!(send["source_id"].as_str(), Some("cam" | "aux")));
        }
        for source in file["sources"].as_array_mut().into_iter().flatten() {
            if matches!(source["id"].as_str(), Some("cam" | "aux")) {
                source.as_object_mut().map(|s| s.remove("audio"));
            }
        }
    });
    let Ok(graph) = load(&older) else {
        panic!("the older file did not load");
    };
    let state = graph.state();
    for id in ["cam", "aux"] {
        let source = state.source(&SourceId::new(id));
        assert_eq!(
            source.and_then(|s| s.audio()).map(|a| a.enabled),
            Some(false),
            "{id}"
        );
        assert!(
            state
                .sends()
                .any(|(source, bus, _)| source.as_str() == id && bus.is_master()),
            "{id}"
        );
    }
    // `cam_copy` kept its send in the file: as saved, its audio on.
    let copy = state.source(&SourceId::new("cam_copy"));
    assert_eq!(copy.and_then(|s| s.audio()).map(|a| a.enabled), Some(true));
    // Saved again, the webcams' audio off is written down.
    let saved = graph.to_collection_json();
    let Ok(reloaded) = load(&saved) else {
        panic!("the saved file did not load");
    };
    assert_eq!(reloaded.state(), graph.state());
}

/// A source whose master send was removed saves an empty `sends`, which tells the load
/// to remove it again.
#[test]
fn a_removed_master_send_round_trips() {
    let mut graph = Graph::new().with_kinds(kinds());
    let applied = graph.apply(
        &[
            Edit::SourceCreate {
                source_id: SourceId::new("mic"),
                name: "mic".into(),
                kind: SourceKind::Builtin { kind: "mic".into() },
                settings: jstream_protocol::Settings::default(),
                duplicated_from: None,
            },
            Edit::SendRemove {
                source_id: SourceId::new("mic"),
                bus_id: BusId::master(),
            },
        ],
        None,
    );
    assert!(applied.is_ok(), "{applied:?}");
    let json = graph.to_collection_json();
    let value: Value = serde_json::from_str(&json).unwrap_or_default();
    assert_eq!(value["sends"], json!([]));
    let loaded = load(&json);
    assert_eq!(loaded.as_ref().map(|g| g.state().sends().count()), Ok(0));
    assert_eq!(loaded.as_ref(), Ok(&graph));
}

/// #716: a webcam's audio settings are written even at their defaults, so a current
/// webcam whose master send was removed doesn't read back as one saved before webcams
/// had audio: it keeps its audio on and gets no send back.
#[test]
fn a_webcam_with_its_master_send_removed_round_trips() {
    let mut graph = Graph::new().with_kinds(kinds());
    let applied = graph.apply(
        &[
            Edit::SourceCreate {
                source_id: SourceId::new("cam"),
                name: "cam".into(),
                kind: SourceKind::Builtin {
                    kind: "webcam".into(),
                },
                settings: jstream_protocol::Settings::default(),
                duplicated_from: None,
            },
            Edit::SendRemove {
                source_id: SourceId::new("cam"),
                bus_id: BusId::master(),
            },
        ],
        None,
    );
    assert!(applied.is_ok(), "{applied:?}");
    let json = graph.to_collection_json();
    let value: Value = serde_json::from_str(&json).unwrap_or_default();
    assert_eq!(value["sends"], json!([]));
    assert_eq!(value["sources"][0]["audio"]["enabled"], json!(true));
    let loaded = load(&json);
    assert_eq!(loaded.as_ref().map(|g| g.state().sends().count()), Ok(0));
    assert_eq!(
        loaded
            .as_ref()
            .ok()
            .and_then(|g| g.state().source(&SourceId::new("cam")))
            .and_then(|s| s.audio())
            .map(|a| a.enabled),
        Some(true)
    );
    assert_eq!(loaded.as_ref(), Ok(&graph));
}

/// A file saved before video mixes existed has neither `video_mixes` nor any output's
/// `video_mix`. It loads with no mixes, every output showing none.
#[test]
fn a_file_without_video_mixes_loads() {
    let file = edit_file(|v| {
        v.as_object_mut().map(|o| o.remove("video_mixes"));
        for output in v["outputs"].as_array_mut().into_iter().flatten() {
            output.as_object_mut().map(|o| o.remove("video_mix"));
        }
    });
    let Ok(graph) = load(&file) else {
        panic!("the file did not load");
    };
    assert_eq!(graph.state().video_mixes().len(), 0);
    assert!(graph.state().outputs().all(|o| o.video_mix().is_none()));
}

/// A mix is rebuilt with `video_mix.create`, so one showing a scene the file lacks is
/// refused, and so is an output naming a mix the file lacks.
#[test]
fn mixes_and_outputs_naming_missing_objects_are_refused() {
    let file = edit_file(|v| v["video_mixes"][1]["mix"]["input"]["scene_id"] = json!("gone"));
    assert!(
        matches!(load(&file), Err(LoadError::Invalid { ref object, .. }) if object == "video mix `side`"),
        "{:?}",
        load(&file)
    );
    let file = edit_file(|v| v["outputs"][0]["video_mix"] = json!("gone"));
    assert!(
        matches!(load(&file), Err(LoadError::Invalid { ref object, .. }) if object == "output `rec`"),
        "{:?}",
        load(&file)
    );
}

/// #362: a source whose settings draw a bus loads: the buses are made before the
/// sources, so the reference finds its bus.
#[test]
fn a_source_drawing_a_bus_round_trips() {
    let mut graph = Graph::new().with_kinds(kinds());
    let applied = graph.apply(
        &[
            Edit::BusCreate {
                bus_id: BusId::new("vod"),
                name: "VOD".into(),
            },
            Edit::SourceCreate {
                source_id: SourceId::new("wave"),
                name: "wave".into(),
                kind: SourceKind::Builtin {
                    kind: "scope".into(),
                },
                settings: [("audio", json!({"type": "bus", "bus_id": "vod"}))]
                    .into_iter()
                    .collect(),
                duplicated_from: None,
            },
        ],
        None,
    );
    assert!(applied.is_ok(), "{applied:?}");
    let loaded = load(&graph.to_collection_json());
    assert_eq!(loaded.as_ref(), Ok(&graph));
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

/// Scene filters and item effects load through `filter.add`'s checks: a video chain of
/// `video_filter` kinds whose references exist.
#[test]
fn scene_filters_and_item_effects_are_checked() {
    let edit_v2 = |edit: &dyn Fn(&mut Value)| {
        let mut value: Value = serde_json::from_str(V2).unwrap_or_default();
        edit(&mut value);
        value.to_string()
    };
    assert_eq!(load(&edit_v2(&|_| {})).map(|g| g.rev()), Ok(Rev(3)));
    // Scene `a` is second in the file and holds `grade`; item `i4` in scene `b` holds
    // `edge`.
    let audio = edit_v2(&|v| {
        let filters = v["scenes"][1]["filters"]
            .as_object_mut()
            .and_then(|f| f.remove("video"));
        v["scenes"][1]["filters"]["audio"] = filters.unwrap_or_default();
    });
    let missing = edit_v2(&|v| {
        v["scenes"][0]["items"][0]["effects"][0]["settings"]["tint"]["source_id"] = json!("gone");
    });
    let not_a_filter =
        edit_v2(&|v| v["scenes"][0]["items"][0]["effects"][0]["kind"] = json!("gain"));
    for (file, of) in [
        (audio, "filter `grade` of scene `a`"),
        (missing, "filter `edge` of item `i4`"),
        (not_a_filter, "filter `edge` of item `i4`"),
    ] {
        assert!(
            matches!(load(&file), Err(LoadError::Invalid { ref object, .. }) if object == of),
            "{of}: {:?}",
            load(&file)
        );
    }
}

/// A file saved before bus monitors (protocol 0.42) loads with none, and a monitor whose
/// tap this version doesn't know is refused as its edit would be.
#[test]
fn a_file_without_monitors_loads_and_an_unknown_tap_is_refused() {
    let older = edit_file(|v| {
        for bus in v["buses"].as_array_mut().into_iter().flatten() {
            bus.as_object_mut().map(|b| b.remove("monitor"));
        }
    });
    assert!(!older.contains("\"monitor\""));
    let loaded = load(&older);
    assert!(loaded.is_ok(), "{loaded:?}");
    if let Ok(graph) = loaded {
        assert!(graph.state().buses().all(|bus| bus.monitor().is_none()));
    }
    let later = edit_file(|v| v["buses"][1]["monitor"]["tap"] = json!("pre_master"));
    assert!(
        matches!(load(&later), Err(LoadError::Invalid { ref object, .. }) if object == "bus `vod`"),
        "{:?}",
        load(&later)
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

/// `i4`'s stored geometry in a loaded collection.
fn i4_geometry(graph: &Graph) -> jstream_protocol::ItemGeometry {
    match graph
        .state()
        .scenes
        .first()
        .and_then(|scene| scene.items.first())
        .map(|item| item.placement)
    {
        Some(crate::Placement::Geometry(geometry)) => geometry,
        other => panic!("`i4` has a geometry, not {other:?}"),
    }
}

/// A geometry's flips are stored only when set, and a file without them loads unflipped.
#[test]
fn a_geometry_flip_round_trips_and_defaults_to_false() {
    let graph = load(V1).expect("the fixture loads");
    let geometry = i4_geometry(&graph);
    assert!(geometry.flip_h && !geometry.flip_v);

    let unflipped = edit_file(|v| {
        v["scenes"][0]["items"][0]["geometry"]
            .as_object_mut()
            .map(|o| o.remove("flip_h"));
    });
    let graph = load(&unflipped).expect("loads without the flip");
    let geometry = i4_geometry(&graph);
    assert!(!geometry.flip_h && !geometry.flip_v);
    assert!(!graph.to_collection_json().contains("flip_"));
}

/// An item's opacity and blend load through `item.add`'s checks. A version 1 file's
/// `effects` loads only empty, and holds no scene `filters`: its migration refuses both.
#[test]
fn opacity_blend_and_reserved_effects_are_checked() {
    let empty = edit_file(|v| v["scenes"][0]["items"][0]["effects"] = json!([]));
    assert_eq!(load(&empty).map(|g| g.rev()), Ok(Rev(2)));
    let effect = edit_file(|v| v["scenes"][0]["items"][0]["effects"] = json!([{"kind": "blur"}]));
    assert_eq!(
        load(&effect),
        Err(LoadError::Migration {
            from: 1,
            reason: "a version 1 item's `effects` must be empty".into()
        })
    );
    let filters = edit_file(|v| v["scenes"][0]["filters"] = json!({}));
    assert_eq!(
        load(&filters),
        Err(LoadError::Migration {
            from: 1,
            reason: "a version 1 scene has no `filters`".into()
        })
    );
    for (field, value) in [("opacity", json!(1.5)), ("blend", json!("multiply"))] {
        let file = edit_file(|v| v["scenes"][0]["items"][0][field] = value.clone());
        assert!(
            matches!(load(&file), Err(LoadError::Invalid { ref object, .. }) if object == "item `i4`"),
            "{field}: {:?}",
            load(&file)
        );
    }
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

/// Presets, the default, scene overrides and item settings survive a save and load,
/// including a collection that deleted the initial `cut` preset.
#[test]
fn transitions_round_trip() {
    use jstream_protocol::{Easing, Glide, ItemId, ItemTransition, TransitionPresetId};
    let mut graph = fixture_graph();
    let applied = graph.apply(
        &[
            Edit::TransitionCreate {
                preset_id: TransitionPresetId::new("smooth"),
                name: "Smooth move".into(),
                kind: "move".into(),
                duration_ms: 600,
                easing: Easing::EaseInOut,
                settings: [("unmatched", json!("slide"))].into_iter().collect(),
            },
            Edit::TransitionSetDefault {
                preset_id: TransitionPresetId::new("smooth"),
            },
            Edit::TransitionDelete {
                preset_id: TransitionPresetId::new("cut"),
            },
            Edit::SceneSetTransitions {
                scene_id: SceneId::new("a"),
                transition_in: None,
                transition_out: Some(TransitionPresetId::new("smooth")),
            },
            Edit::ItemSetTransition {
                item_id: ItemId::new("i1"),
                transition: ItemTransition {
                    delay_ms: 40,
                    glide: Glide::Never,
                    ..ItemTransition::default()
                },
            },
        ],
        None,
    );
    assert!(applied.is_ok(), "{applied:?}");
    let json = graph.to_collection_json();
    let value: Value = serde_json::from_str(&json).unwrap_or_default();
    assert_eq!(
        value["transitions"],
        json!({
            "presets": [{
                "id": "smooth",
                "name": "Smooth move",
                "kind": "move",
                "kind_version": 1,
                "duration_ms": 600,
                "easing": "ease_in_out",
                "settings": {"stagger_ms": 0, "unmatched": "slide"},
            }],
            "default": "smooth",
        })
    );
    assert_eq!(load(&json), Ok(graph.clone()));

    // The initial preset kept, but changed, loads changed.
    let applied = graph.apply(
        &[
            Edit::TransitionCreate {
                preset_id: TransitionPresetId::new("cut"),
                name: "Quick fade".into(),
                kind: "fade".into(),
                duration_ms: 250,
                easing: Easing::Linear,
                settings: Default::default(),
            },
            Edit::TransitionSetDefault {
                preset_id: TransitionPresetId::new("cut"),
            },
        ],
        None,
    );
    assert!(applied.is_ok(), "{applied:?}");
    assert_eq!(load(&graph.to_collection_json()), Ok(graph));
}

#[test]
fn a_preset_saved_by_another_kind_version_is_refused() {
    let mut graph = fixture_graph();
    let applied = graph.apply(
        &[Edit::TransitionCreate {
            preset_id: jstream_protocol::TransitionPresetId::new("f"),
            name: "F".into(),
            kind: "fade".into(),
            duration_ms: 300,
            easing: jstream_protocol::Easing::EaseOut,
            settings: Default::default(),
        }],
        None,
    );
    assert!(applied.is_ok(), "{applied:?}");
    let mut value: Value = serde_json::from_str(&graph.to_collection_json()).unwrap_or_default();
    value["transitions"]["presets"][1]["kind_version"] = json!(7);
    assert_eq!(
        load(&value.to_string()).err(),
        Some(LoadError::KindVersion {
            object: "transition `f`".into(),
            kind: "fade".into(),
            saved: 7,
            current: 1,
        })
    );
}

/// Folders, labels and organisation metadata survive a save and load (ADR-0024 §3 and
/// §4), also with a folder ahead of its parent in folder order, and a collection without
/// them saves none of their fields.
#[test]
fn folders_labels_and_metadata_round_trip() {
    let mut graph = fixture_graph();
    let plain = graph.to_collection_json();
    for field in [
        "folders",
        "labels",
        "folder",
        "favourite",
        "show_in_multiview",
    ] {
        assert!(
            !plain.contains(&format!("\"{field}\"")),
            "{field} in {plain}"
        );
    }
    let applied = graph.apply(
        &[
            Edit::FolderCreate {
                folder_id: FolderId::new("shows"),
                name: "Shows".into(),
                parent: None,
            },
            Edit::FolderCreate {
                folder_id: FolderId::new("intros"),
                name: "Intros".into(),
                parent: Some(FolderId::new("shows")),
            },
            Edit::FolderReorder {
                folder_id: FolderId::new("intros"),
                index: 0,
            },
            Edit::SceneSetFolder {
                scene_id: SceneId::new("a"),
                folder_id: Some(FolderId::new("intros")),
            },
            Edit::LabelCreate {
                label_id: LabelId::new("sponsor"),
                name: "Sponsor".into(),
                color: "#ff8800".into(),
                icon: Some("megaphone".into()),
            },
            Edit::SceneSetMeta {
                scene_id: SceneId::new("a"),
                meta: SceneMeta {
                    labels: vec![LabelId::new("sponsor")],
                    icon: Some("star".into()),
                    color: Some("#1e1e2e".into()),
                    favourite: true,
                    show_in_multiview: false,
                },
            },
            Edit::SourceSetMeta {
                source_id: SourceId::new("cam"),
                meta: SourceMeta {
                    labels: vec![LabelId::new("sponsor")],
                    favourite: true,
                },
            },
        ],
        None,
    );
    assert!(applied.is_ok(), "{applied:?}");
    let json = graph.to_collection_json();
    let saved: Value = serde_json::from_str(&json).unwrap_or_default();
    assert_eq!(
        saved["folders"],
        json!([
            {"id": "intros", "name": "Intros", "parent": "shows"},
            {"id": "shows", "name": "Shows"}
        ])
    );
    assert_eq!(
        saved["labels"],
        json!([{"id": "sponsor", "name": "Sponsor", "color": "#ff8800", "icon": "megaphone"}])
    );
    let scene = saved["scenes"]
        .as_array()
        .and_then(|scenes| scenes.iter().find(|s| s["id"] == "a"));
    assert_eq!(scene.map(|s| &s["labels"]), Some(&json!(["sponsor"])));
    assert_eq!(load(&json), Ok(graph));
}

/// A scene filed in a folder the file doesn't have is refused, naming the scene.
#[test]
fn a_scene_in_a_missing_folder_is_refused() {
    let json = edit_file(|file| file["scenes"][0]["folder"] = json!("nope"));
    match load(&json) {
        Err(LoadError::Invalid { object, error }) => {
            assert!(object.starts_with("scene `"), "{object}");
            assert!(
                matches!(*error, ProtocolError::NotFound { .. }),
                "{error:?}"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}
