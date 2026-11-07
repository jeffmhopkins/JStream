//! Projects (ADR-0121): `collection.open`, `collection.new`, `collection.save_as` and
//! `collection.info`, with stream keys scoped to one project file.

use super::{add, batch, block_on, connected, create, drain, edits, query, rtmp_output};
use crate::Engine;
use jstream_control::Control;
use jstream_platform::fake::MemoryKeyStore;
use jstream_platform::{Secret, StreamKeyStore};
use jstream_protocol::{
    Action, AssetRoot, Batch, Call, CallResult, CollectionCause, Edit, Event, HistoryCall,
    HistoryOp, OutputId, OutputState, ProtocolError, Query, QueryResult, Reference, Rev, Settings,
    SourceId, SourceKind,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A new, empty directory for one test.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "jstream-core-projects-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir");
    std::path::absolute(dir).expect("absolute")
}

fn saved(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).expect("read")).expect("json")
}

fn text(path: &Path) -> String {
    path.display().to_string()
}

fn act(control: &dyn Control, action: Action) -> Result<CallResult, ProtocolError> {
    block_on(control.call(Call::Action { action }))
        .map(|response| response.result)
        .map_err(|err| err.protocol().cloned().expect("a protocol error"))
}

fn open_call(path: &Path) -> Action {
    Action::CollectionOpen { path: text(path) }
}

fn undo(control: &dyn Control) -> Result<CallResult, ProtocolError> {
    block_on(control.call(Call::History(HistoryCall {
        op: HistoryOp::Undo,
        expected_rev: None,
        idempotency_key: None,
    })))
    .map(|response| response.result)
    .map_err(|err| err.protocol().cloned().expect("a protocol error"))
}

fn scenes(control: &dyn Control) -> (Rev, Vec<String>) {
    super::scene_ids(control)
}

fn info(control: &dyn Control) -> (String, Vec<String>) {
    match query(control, Query::CollectionInfo).expect("collection.info") {
        QueryResult::CollectionInfo { path, recent } => (path, recent),
        other => panic!("not collection.info: {other:?}"),
    }
}

/// A collection file at `path` holding one scene, `scene`, at `rev` 2.
fn write_project(path: &Path, scene: &str) {
    let engine = Engine::new()
        .with_collection_file(path)
        .expect("collection");
    let control = connected(&engine);
    batch(&control, Batch::single(create(scene))).expect("created");
    batch(&control, Batch::single(create(&format!("{scene}2")))).expect("created");
}

#[test]
fn opening_a_project_replaces_the_collection_and_clears_undo() {
    let dir = temp_dir("open");
    let a = dir.join("a.json");
    let b = dir.join("b.json");
    write_project(&b, "b");
    let engine = Engine::new()
        .with_collection_file(&a)
        .expect("collection")
        .with_recent_collections(dir.join("recent.json"));
    let control = connected(&engine);
    let other = connected(&engine);
    for n in 0..4 {
        batch(&control, Batch::single(create(&format!("a{n}")))).expect("created");
    }
    let mut events = other.events();
    assert_eq!(act(&control, open_call(&b)), Ok(CallResult::action()));
    // One past the old rev, since the file's (2) is older.
    let (rev, ids) = scenes(&other);
    assert_eq!(rev, Rev(5));
    assert_eq!(ids, ["b", "b2"]);
    assert_eq!(saved(&b)["rev"], 5);
    let client_id = control.client_id().expect("client");
    let seen = drain(&mut events);
    assert!(
        seen.iter()
            .any(|e| matches!(e, Event::StateChanged { rev: Rev(5), .. }))
    );
    assert!(seen.iter().any(|e| matches!(
        e,
        Event::StudioChanged { program_scene: Some(s), .. } if s.as_str() == "b"
    )));
    assert_eq!(
        seen.last(),
        Some(&Event::CollectionFileChanged {
            path: text(&b),
            cause: CollectionCause::Open,
            client_id,
            request_id: seen.iter().find_map(|e| match e {
                Event::CollectionFileChanged { request_id, .. } => request_id.clone(),
                _ => None,
            }),
        })
    );
    // The old project's history is gone.
    assert!(matches!(
        undo(&control),
        Err(ProtocolError::HistoryEmpty { .. })
    ));
    // Changes now save to the new file, and the old one keeps what it had.
    batch(&control, Batch::single(create("c"))).expect("created");
    assert_eq!(saved(&b)["rev"], 6);
    assert_eq!(saved(&a)["rev"], 4);
    assert_eq!(info(&control), (text(&b), vec![text(&b), text(&a)]));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_broken_or_missing_file_leaves_the_open_project_and_its_outputs() {
    let dir = temp_dir("broken");
    let a = dir.join("a.json");
    let broken = dir.join("broken.json");
    fs::write(&broken, r#"{"schema_version": 999}"#).expect("written");
    let engine = Engine::new().with_collection_file(&a).expect("collection");
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("a"),
            Edit::OutputCreate {
                output_id: OutputId::new("yt"),
                kind: rtmp_output("rtmp://a.example/live"),
            },
        ]),
    )
    .expect("created");
    act(
        &control,
        Action::OutputStart {
            output_id: OutputId::new("yt"),
        },
    )
    .expect("started");
    for path in [&broken, &dir.join("missing.json")] {
        assert!(matches!(
            act(&control, open_call(path)),
            Err(ProtocolError::ActionFailed { .. })
        ));
    }
    assert_eq!(scenes(&control), (Rev(1), vec!["a".to_owned()]));
    assert!(matches!(
        query(
            &control,
            Query::OutputStatus {
                output_id: OutputId::new("yt")
            }
        ),
        Ok(QueryResult::OutputStatus {
            state: OutputState::Active,
            ..
        })
    ));
    assert_eq!(
        fs::read_to_string(&broken).expect("kept"),
        r#"{"schema_version": 999}"#
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn opening_stops_every_running_output() {
    let dir = temp_dir("stops");
    let b = dir.join("b.json");
    write_project(&b, "b");
    let engine = Engine::new()
        .with_collection_file(dir.join("a.json"))
        .expect("collection");
    let control = connected(&engine);
    batch(
        &control,
        Batch::single(Edit::OutputCreate {
            output_id: OutputId::new("yt"),
            kind: rtmp_output("rtmp://a.example/live"),
        }),
    )
    .expect("created");
    act(
        &control,
        Action::OutputStart {
            output_id: OutputId::new("yt"),
        },
    )
    .expect("started");
    let mut events = control.events();
    act(&control, open_call(&b)).expect("opened");
    assert!(drain(&mut events).contains(&Event::OutputStateChanged {
        output_id: OutputId::new("yt"),
        state: OutputState::Stopped,
    }));
    // The output isn't in the new project.
    assert!(
        query(
            &control,
            Query::OutputStatus {
                output_id: OutputId::new("yt")
            }
        )
        .is_err()
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_new_project_is_empty_and_never_replaces_a_file() {
    let dir = temp_dir("new");
    let a = dir.join("a.json");
    let engine = Engine::new().with_collection_file(&a).expect("collection");
    let control = connected(&engine);
    batch(&control, Batch::single(create("a"))).expect("created");
    assert_eq!(
        act(&control, Action::CollectionNew { path: text(&a) }),
        Err(ProtocolError::AlreadyExists {
            entity: jstream_protocol::EntityKind::File,
            id: text(&a),
        })
    );
    let fresh = dir.join("fresh.json");
    act(&control, Action::CollectionNew { path: text(&fresh) }).expect("created");
    assert_eq!(scenes(&control), (Rev(2), Vec::<String>::new()));
    let file = saved(&fresh);
    assert_eq!(file["rev"], 2);
    assert_eq!(file["labels"].as_array().map(Vec::len), Some(4));
    assert!(
        file.get("key_scope").is_none(),
        "no key scope in a project file"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn paths_must_be_absolute_and_need_a_collection_file() {
    let dir = temp_dir("paths");
    let engine = Engine::new()
        .with_collection_file(dir.join("a.json"))
        .expect("collection");
    let control = connected(&engine);
    for path in ["b.json", "../b.json", "//host/share/b.json"] {
        assert!(matches!(
            act(&control, Action::CollectionOpen { path: path.into() }),
            Err(ProtocolError::InvalidArgument { field, .. }) if field == "path"
        ));
    }
    assert!(matches!(
        act(
            &control,
            Action::CollectionSaveAs {
                path: text(&dir.join("a.json"))
            }
        ),
        Err(ProtocolError::AlreadyExists { .. })
    ));
    let bare = connected(&Engine::new());
    assert!(matches!(
        act(&bare, open_call(&dir.join("a.json"))),
        Err(ProtocolError::Unsupported { .. })
    ));
    assert!(matches!(
        query(&bare, Query::CollectionInfo),
        Err(err) if err.protocol().map(ProtocolError::code) == Some("unsupported")
    ));
    let _ = fs::remove_dir_all(dir);
}

fn image(path: &str) -> Edit {
    let file = Reference::Asset {
        root: AssetRoot::Collection,
        path: path.into(),
    };
    let settings: Settings = [("file".to_owned(), file.to_value())].into_iter().collect();
    Edit::SourceCreate {
        source_id: SourceId::new("pic"),
        name: "Pic".into(),
        kind: SourceKind::Builtin {
            kind: "image".into(),
        },
        settings,
        duplicated_from: None,
    }
}

fn file_setting(control: &dyn Control) -> serde_json::Value {
    match query(control, Query::StateGet).expect("state.get") {
        QueryResult::StateGet { sources, .. } => sources
            .iter()
            .find(|s| s.source_id.as_str() == "pic")
            .and_then(|s| s.settings.get("file").cloned())
            .expect("the file setting"),
        other => panic!("not state.get: {other:?}"),
    }
}

#[test]
fn save_as_keeps_assets_on_the_same_files() {
    let dir = temp_dir("save-as");
    let show = dir.join("show");
    fs::create_dir_all(show.join("img")).expect("dir");
    let a = show.join("a.json");
    let engine = Engine::new().with_collection_file(&a).expect("collection");
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("a"),
            image("img/logo.png"),
            add("a", "i1", "pic"),
        ]),
    )
    .expect("created");
    // Beside the open file: nothing to rewrite, the history stays.
    let copy = show.join("copy.json");
    act(&control, Action::CollectionSaveAs { path: text(&copy) }).expect("saved");
    assert_eq!(scenes(&control).0, Rev(1));
    assert_eq!(saved(&copy)["rev"], 1);
    assert!(undo(&control).is_ok());
    batch(
        &control,
        edits(vec![
            create("a"),
            image("img/logo.png"),
            add("a", "i1", "pic"),
        ]),
    )
    .expect("created again");
    // A save as never replaces a file, a project or anything else.
    let other = dir.join("notes.txt");
    fs::write(&other, "mine").expect("written");
    for taken in [&a, &other] {
        assert!(matches!(
            act(&control, Action::CollectionSaveAs { path: text(taken) }),
            Err(ProtocolError::AlreadyExists { .. })
        ));
    }
    assert_eq!(fs::read_to_string(&other).expect("read"), "mine");
    // A folder above keeps it relative, through the subfolder.
    let up = dir.join("up.json");
    act(&control, Action::CollectionSaveAs { path: text(&up) }).expect("saved");
    assert_eq!(
        file_setting(&control),
        serde_json::json!({"type": "asset", "root": "collection", "path": "show/img/logo.png"})
    );
    // Elsewhere: the reference becomes absolute, which is a new rev and no undo.
    fs::create_dir_all(dir.join("other")).expect("dir");
    let away = dir.join("other").join("away.json");
    let mut events = control.events();
    act(&control, Action::CollectionSaveAs { path: text(&away) }).expect("saved");
    let logo = text(&show.join("img").join("logo.png"));
    assert_eq!(
        file_setting(&control),
        serde_json::json!({"type": "asset", "root": "absolute", "path": logo})
    );
    assert_eq!(scenes(&control).0, Rev(5));
    assert!(matches!(
        undo(&control),
        Err(ProtocolError::HistoryEmpty { .. })
    ));
    let seen = drain(&mut events);
    assert!(seen.iter().any(|e| matches!(
        e,
        Event::CollectionFileChanged {
            cause: CollectionCause::SaveAs,
            ..
        }
    )));
    // Saved back under the asset's folder: relative again.
    act(
        &control,
        Action::CollectionSaveAs {
            path: text(&show.join("b.json")),
        },
    )
    .expect("saved");
    assert_eq!(
        file_setting(&control),
        serde_json::json!({"type": "asset", "root": "collection", "path": "img/logo.png"})
    );
    let _ = fs::remove_dir_all(dir);
}

fn key(store: &MemoryKeyStore, name: &str) -> Option<String> {
    store
        .get(name)
        .expect("store")
        .map(|secret| secret.expose().to_owned())
}

#[test]
fn stream_keys_belong_to_one_project_file() {
    let dir = temp_dir("keys");
    let a = dir.join("a.json");
    {
        let engine = Engine::new().with_collection_file(&a).expect("collection");
        let control = connected(&engine);
        batch(
            &control,
            Batch::single(Edit::OutputCreate {
                output_id: OutputId::new("yt"),
                kind: rtmp_output("rtmp://a.example/live"),
            }),
        )
        .expect("created");
    }
    // A key from before scopes, under the bare output ID.
    let store = Arc::new(MemoryKeyStore::new());
    store.set("yt", &Secret::new("key-a")).expect("set");
    let engine = Engine::new()
        .with_collection_file(&a)
        .expect("collection")
        .with_key_store(store.clone());
    let yt = OutputId::new("yt");
    let scoped = engine.stream_key_name(&yt);
    assert!(
        scoped.ends_with("/yt") && scoped.len() == 32 + 3,
        "{scoped}"
    );
    assert_eq!(key(&store, &scoped).as_deref(), Some("key-a"));
    assert_eq!(key(&store, "yt"), None, "the bare entry is removed");
    assert!(
        saved(&a).get("key_scope").is_none(),
        "the file names no scope"
    );
    let control = connected(&engine);
    let key_set = |control: &dyn Control| match query(
        control,
        Query::OutputStatus {
            output_id: yt.clone(),
        },
    ) {
        Ok(QueryResult::OutputStatus { key_set, .. }) => key_set,
        other => panic!("not an output status: {other:?}"),
    };
    assert!(key_set(&control));
    // A save as that finds a file at its path copies no key there.
    let taken = dir.join("taken.json");
    fs::write(&taken, saved(&a).to_string()).expect("written");
    assert!(matches!(
        act(&control, Action::CollectionSaveAs { path: text(&taken) }),
        Err(ProtocolError::AlreadyExists { .. })
    ));
    // A save as copies the key to the new file's scope.
    let copy = dir.join("copy.json");
    act(&control, Action::CollectionSaveAs { path: text(&copy) }).expect("saved");
    let copied = engine.stream_key_name(&yt);
    assert_ne!(copied, scoped);
    assert_eq!(key(&store, &copied).as_deref(), Some("key-a"));
    assert!(key_set(&control));
    // The same project at another path, as a shared copy would be, gets none of those
    // keys: the scope follows the path on this machine, not the file's contents.
    act(&control, open_call(&taken)).expect("opened");
    assert!(!key_set(&control));
    // Back to the first project: its key is there.
    act(&control, open_call(&a)).expect("opened");
    assert!(key_set(&control));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_project_with_no_scene_still_reports_studio() {
    let dir = temp_dir("no-scene");
    let engine = Engine::new()
        .with_collection_file(dir.join("a.json"))
        .expect("collection");
    let control = connected(&engine);
    batch(&control, Batch::single(create("a"))).expect("created");
    let mut events = control.events();
    act(
        &control,
        Action::CollectionNew {
            path: text(&dir.join("b.json")),
        },
    )
    .expect("new");
    assert!(drain(&mut events).iter().any(|e| matches!(
        e,
        Event::StudioChanged {
            program_scene: None,
            ..
        }
    )));
    let _ = fs::remove_dir_all(dir);
}
