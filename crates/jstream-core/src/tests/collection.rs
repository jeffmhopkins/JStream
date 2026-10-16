//! The collection file: `Engine::with_collection_file` loads it, creates it, and saves
//! every change atomically.

use super::{
    add, batch, block_on, connected, create, edits, query, rtmp_output, scene_ids, switch_call,
    webcam,
};
use crate::{CollectionFileError, Engine};
use jstream_control::Control;
use jstream_protocol::{
    Batch, Call, Edit, HistoryCall, HistoryOp, OutputId, OutputState, Query, QueryResult, Rev,
    SceneId, Settings, SourceId, SourceKind,
};
use std::fs;
use std::path::{Path, PathBuf};

/// A new, empty directory for one test.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "jstream-core-collection-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn open(path: &Path) -> Engine {
    Engine::new()
        .with_collection_file(path)
        .expect("collection file")
}

fn saved(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).expect("read")).expect("json")
}

/// Every file in `dir`, so a test can see that no temporary file was left behind.
fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_missing_file_is_created_empty() {
    let dir = temp_dir("missing");
    let path = dir.join("scenes.json");
    let engine = open(&path);
    assert_eq!(engine.rev(), Rev(0));
    assert_eq!(saved(&path)["schema_version"], 1);
    assert_eq!(saved(&path)["rev"], 0);
    assert_eq!(files(&dir), ["scenes.json"]);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn changes_are_saved_and_load_in_a_new_engine() {
    let dir = temp_dir("reload");
    let path = dir.join("scenes.json");
    {
        let control = connected(&open(&path));
        let rev = batch(
            &control,
            edits(vec![
                create("a"),
                webcam(),
                add("a", "i1", "webcam"),
                Edit::OutputCreate {
                    output_id: OutputId::new("yt"),
                    kind: rtmp_output("rtmp://a.rtmp.youtube.com/live2"),
                },
            ]),
        );
        assert_eq!(rev.ok(), Some(Rev(1)));
        let rev = batch(&control, Batch::single(create("b")));
        assert_eq!(rev.ok(), Some(Rev(2)));
        assert_eq!(saved(&path)["rev"], 2);
    }
    let engine = open(&path);
    assert_eq!(engine.rev(), Rev(2));
    // The render thread draws the loaded collection's program scene.
    assert_eq!(
        engine
            .render_hub()
            .load()
            .shot()
            .program()
            .map(|id| id.as_str().to_owned()),
        Some("a".to_owned())
    );
    let control = connected(&engine);
    assert_eq!(
        scene_ids(&control),
        (Rev(2), vec!["a".to_owned(), "b".to_owned()])
    );
    // A loaded output is known to the outputs' runtime state.
    assert!(matches!(
        query(
            &control,
            Query::OutputStatus {
                output_id: OutputId::new("yt")
            }
        ),
        Ok(QueryResult::OutputStatus {
            state: OutputState::Stopped,
            ..
        })
    ));
    // Revisions carry on from the file's.
    assert_eq!(
        batch(&control, Batch::single(create("c"))).ok(),
        Some(Rev(3))
    );
    assert_eq!(files(&dir), ["scenes.json"]);
    let _ = fs::remove_dir_all(dir);
}

/// #459: a waveform drawing another source saves and loads with the builtin kinds,
/// the drawn source made first; and a waveform saved before `audio_source` existed
/// loads with it `null`, drawing what it drew (an added parameter needs no kind
/// version bump).
#[test]
fn a_source_drawing_a_source_round_trips() {
    let dir = temp_dir("audio-source");
    let path = dir.join("scenes.json");
    let drawn = serde_json::json!({"type": "source", "source_id": "desk"});
    let wave_settings = |engine: &Engine| {
        let control = connected(engine);
        match query(&control, Query::SourceList) {
            Ok(QueryResult::SourceList { sources, .. }) => {
                sources
                    .into_iter()
                    .find(|s| s.source_id.as_str() == "wave")
                    .expect("the waveform")
                    .settings
            }
            other => panic!("expected a source list, got {other:?}"),
        }
    };
    {
        let control = connected(&open(&path));
        batch(
            &control,
            edits(vec![
                // Created in this order, the waveform's ID sorts before the source it
                // draws, so loading must make the drawn source first.
                Edit::SourceCreate {
                    source_id: SourceId::new("desk"),
                    name: "Desktop".into(),
                    kind: SourceKind::Builtin {
                        kind: "desktop_audio".into(),
                    },
                    settings: Settings::new(),
                    duplicated_from: None,
                },
                Edit::SourceCreate {
                    source_id: SourceId::new("a_wave"),
                    name: "Waveform".into(),
                    kind: SourceKind::Builtin {
                        kind: "waveform".into(),
                    },
                    settings: [("audio_source", drawn.clone())].into_iter().collect(),
                    duplicated_from: None,
                },
                Edit::SourceCreate {
                    source_id: SourceId::new("wave"),
                    name: "Waveform".into(),
                    kind: SourceKind::Builtin {
                        kind: "waveform".into(),
                    },
                    settings: Settings::new(),
                    duplicated_from: None,
                },
            ]),
        )
        .expect("created");
    }
    let engine = open(&path);
    let control = connected(&engine);
    let Ok(QueryResult::SourceList { sources, .. }) = query(&control, Query::SourceList) else {
        panic!("a source list");
    };
    let a_wave = sources
        .iter()
        .find(|s| s.source_id.as_str() == "a_wave")
        .expect("loaded");
    assert_eq!(a_wave.settings.get("audio_source"), Some(&drawn));
    assert_eq!(
        wave_settings(&engine).get("audio_source"),
        Some(&serde_json::Value::Null)
    );
    drop((control, engine));

    // As a file from before #459: no `audio_source` at all.
    let mut file = saved(&path);
    for source in file["sources"].as_array_mut().expect("sources") {
        if let Some(settings) = source
            .get_mut("settings")
            .and_then(serde_json::Value::as_object_mut)
        {
            settings.remove("audio_source");
        }
    }
    fs::write(&path, file.to_string()).expect("write");
    let engine = open(&path);
    assert_eq!(
        wave_settings(&engine).get("audio_source"),
        Some(&serde_json::Value::Null)
    );
    let _ = fs::remove_dir_all(dir);
}

/// Program is not saved: a loaded collection's first scene is on program, whichever
/// scene was on it before the restart (ADR-0030).
#[test]
fn a_loaded_collection_puts_its_first_scene_on_program() {
    let dir = temp_dir("program");
    let path = dir.join("scenes.json");
    {
        let control = connected(&open(&path));
        batch(&control, edits(vec![create("a"), create("b")])).expect("scenes");
        block_on(control.call(switch_call("b"))).expect("switch");
    }
    let control = connected(&open(&path));
    match query(&control, Query::StudioGet).expect("studio.get") {
        QueryResult::StudioGet { program_scene } => {
            assert_eq!(program_scene, Some(SceneId::new("a")));
        }
        other => panic!("not a studio result: {other:?}"),
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn undo_and_redo_are_saved() {
    let dir = temp_dir("undo");
    let path = dir.join("scenes.json");
    let control = connected(&open(&path));
    assert!(batch(&control, Batch::single(create("a"))).is_ok());
    assert!(block_on(control.call(Call::History(HistoryCall::new(HistoryOp::Undo)))).is_ok());
    assert_eq!(saved(&path)["rev"], 2);
    assert_eq!(saved(&path)["scenes"], serde_json::json!([]));
    assert!(block_on(control.call(Call::History(HistoryCall::new(HistoryOp::Redo)))).is_ok());
    assert_eq!(saved(&path)["rev"], 3);
    assert_eq!(saved(&path)["scenes"][0]["id"], "a");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_file_that_does_not_load_is_refused_and_left_alone() {
    let dir = temp_dir("bad");
    let path = dir.join("scenes.json");
    let text = r#"{"schema_version": 1, "rev": 3, "scenes": [], "sources": [], "outputs": [], "extra": 1}"#;
    fs::write(&path, text).expect("write");
    let result = Engine::new().with_collection_file(&path);
    assert!(
        matches!(result, Err(CollectionFileError::Load { .. })),
        "{:?}",
        result.err()
    );
    assert_eq!(fs::read_to_string(&path).ok().as_deref(), Some(text));

    let newer = r#"{"schema_version": 99}"#;
    fs::write(&path, newer).expect("write");
    let message = Engine::new()
        .with_collection_file(&path)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(message.contains("schema_version 99"), "{message}");
    assert_eq!(fs::read_to_string(&path).ok().as_deref(), Some(newer));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_file_that_cannot_be_created_is_refused() {
    let dir = temp_dir("uncreatable");
    let result = Engine::new().with_collection_file(dir.join("no-such-dir").join("scenes.json"));
    assert!(
        matches!(result, Err(CollectionFileError::Create { .. })),
        "{:?}",
        result.err()
    );
    let result = Engine::new().with_collection_file(&dir);
    assert!(
        matches!(result, Err(CollectionFileError::Read { .. })),
        "{:?}",
        result.err()
    );
    let _ = fs::remove_dir_all(dir);
}

/// A save that fails leaves the change applied and the call successful; the next
/// change that can be saved writes the collection with both.
#[test]
fn a_failed_save_keeps_the_change_and_the_next_one_saves_it() {
    let dir = temp_dir("failed-save");
    let sub = dir.join("sub");
    fs::create_dir_all(&sub).expect("sub");
    let path = sub.join("scenes.json");
    let control = connected(&open(&path));
    fs::remove_dir_all(&sub).expect("remove sub");
    assert_eq!(
        batch(&control, Batch::single(create("a"))).ok(),
        Some(Rev(1))
    );
    assert!(!path.exists());
    fs::create_dir_all(&sub).expect("sub again");
    assert_eq!(
        batch(&control, Batch::single(create("b"))).ok(),
        Some(Rev(2))
    );
    assert_eq!(saved(&path)["rev"], 2);
    assert_eq!(saved(&path)["scenes"].as_array().map(Vec::len), Some(2));
    assert_eq!(files(&sub), ["scenes.json"]);
    let _ = fs::remove_dir_all(dir);
}

/// Many sessions changing the collection at once: the file ends at the last `rev`.
#[test]
fn concurrent_changes_leave_the_latest_collection() {
    let dir = temp_dir("concurrent");
    let path = dir.join("scenes.json");
    let engine = open(&path);
    std::thread::scope(|scope| {
        for t in 0..4 {
            let engine = engine.clone();
            scope.spawn(move || {
                let control = connected(&engine);
                for n in 0..10 {
                    assert!(batch(&control, Batch::single(create(&format!("s{t}_{n}")))).is_ok());
                }
            });
        }
    });
    assert_eq!(engine.rev(), Rev(40));
    assert_eq!(saved(&path)["rev"], 40);
    assert_eq!(saved(&path)["scenes"].as_array().map(Vec::len), Some(40));
    assert_eq!(files(&dir), ["scenes.json"]);
    let _ = fs::remove_dir_all(dir);
}
