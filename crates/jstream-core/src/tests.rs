//! The engine's protocol behaviour through `InProcessControl`: the handshake, atomic
//! batches, `expected_rev`, idempotency keys, queries (held to the shared parity
//! fixture), events, the placeholder outputs, and a differential run against
//! `jstream-control`'s mock engine.

mod activity;
mod audio;
mod audio_routing;
#[cfg(feature = "wasm-plugins")]
mod automation;
mod collection;
mod gpu_capture;
mod history;
mod keyframes;
mod live_values;
mod preview;
mod source_sizes;
mod sources;
mod transitions;
#[cfg(all(windows, feature = "hw-tests"))]
mod windows_gpu;
mod youtube;

use crate::{DropSnapshot, Engine, FnDropSource, InProcessControl};
use jstream_control::{Control, ControlError, EventStream, MockEngine};
use jstream_protocol::{
    Action, Batch, Call, CallResult, ChainCost, ClientId, DropCounts, Edit, EntityKind, Event,
    GestureId, Hello, IdempotencyKey, ItemId, KindCategory, OutputId, OutputState,
    PROTOCOL_VERSION, PixelSize, ProtocolError, ProtocolVersion, Query, QueryResult, RequestId,
    Rev, SceneId, SceneRole, SourceId, SourceKind, TransitionPresetId, TransitionStarted, parity,
};
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Wake, Waker};

/// Runs a future to completion, parking this thread until it is woken. The engine's
/// futures are ready at once, except `video.snapshot`'s, which the snapshot thread wakes.
fn block_on<F: Future>(future: F) -> F::Output {
    struct Unpark(std::thread::Thread);

    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut Context::from_waker(&waker)) {
            return value;
        }
        std::thread::park();
    }
}

fn poll_event(events: &mut EventStream, waker: &Waker) -> Poll<Option<Event>> {
    events.as_mut().poll_next(&mut Context::from_waker(waker))
}

/// Every event already buffered.
fn drain(events: &mut EventStream) -> Vec<Event> {
    let mut out = Vec::new();
    while let Poll::Ready(Some(event)) = poll_event(events, Waker::noop()) {
        out.push(event);
    }
    out
}

/// `events` with each `stats.tick`'s load left out, since its clock reading differs
/// from run to run.
fn unloaded(events: Vec<Event>) -> Vec<Event> {
    events
        .into_iter()
        .map(|event| match event {
            Event::StatsTick {
                seq,
                drops,
                chains,
                outputs,
                ..
            } => Event::StatsTick {
                seq,
                drops,
                load: None,
                chains,
                outputs,
            },
            other => other,
        })
        .collect()
}

fn hello() -> Hello {
    Hello {
        protocol: PROTOCOL_VERSION,
        client_name: "test".into(),
    }
}

fn connected(engine: &Engine) -> InProcessControl {
    let control = engine.connect();
    block_on(control.hello(hello())).expect("handshake");
    control
}

fn create(scene: &str) -> Edit {
    Edit::SceneCreate {
        scene_id: SceneId::new(scene),
        name: scene.to_uppercase(),
        role: None,
    }
}

/// A `webcam` source with complete settings: the mock engine stores settings as sent,
/// while the engine fills in defaults, so a parity script sends them all.
fn webcam() -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new("webcam"),
        name: "Webcam".into(),
        kind: SourceKind::Builtin {
            kind: "webcam".into(),
        },
        settings: [("device".to_owned(), serde_json::Value::from(""))]
            .into_iter()
            .collect(),
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
        opacity: None,
        blend: None,
    }
}

fn batch(control: &dyn Control, batch: Batch) -> Result<Rev, ControlError> {
    match block_on(control.call(Call::Batch(batch)))?.result {
        CallResult::Batch { rev, .. } => Ok(rev),
        other => panic!("not a batch result: {other:?}"),
    }
}

fn query(control: &dyn Control, query: Query) -> Result<QueryResult, ControlError> {
    match block_on(control.call(Call::Query { query }))?.result {
        CallResult::Query { result } => Ok(result),
        other => panic!("not a query result: {other:?}"),
    }
}

fn output_call(output: &str, start: bool) -> Call {
    let output_id = OutputId::new(output);
    Call::Action {
        action: if start {
            Action::OutputStart { output_id }
        } else {
            Action::OutputStop { output_id }
        },
    }
}

fn switch_call(scene: &str) -> Call {
    Call::Action {
        action: Action::SceneSwitch {
            scene_id: SceneId::new(scene),
            preset_id: None,
        },
    }
}

fn move_call(item: &str, scene: &str) -> Call {
    Call::Batch(Batch::single(Edit::ItemMove {
        item_id: ItemId::new(item),
        scene_id: SceneId::new(scene),
        index: 0,
    }))
}

fn role_call(scene: &str, role: SceneRole) -> Call {
    Call::Batch(Batch::single(Edit::SceneSetRole {
        scene_id: SceneId::new(scene),
        role,
    }))
}

fn code(error: &ControlError) -> Option<&'static str> {
    error.protocol().map(ProtocolError::code)
}

fn scene_ids(control: &dyn Control) -> (Rev, Vec<String>) {
    match query(control, Query::SceneList).expect("scene.list") {
        QueryResult::SceneList { rev, scenes } => (
            rev,
            scenes.into_iter().map(|s| s.scene_id.to_string()).collect(),
        ),
        other => panic!("not a scene list: {other:?}"),
    }
}

/// An engine that describes every kind of the parity fixture: the built-in ones, and
/// `mic`, the fixture's audio kind, which is not a built-in kind.
fn parity_engine() -> Engine {
    Engine::new().with_extra_kinds(parity::kinds())
}

pub(crate) fn check_stale_expected_rev_is_rejected() {
    let engine = Engine::new();
    let agent = connected(&engine);
    let user = connected(&engine);
    let mut events = agent.events();
    assert_eq!(batch(&user, Batch::single(create("a"))), Ok(Rev(1)));
    // `state.changed`, and `studio.changed` for the first scene going on program.
    assert_eq!(drain(&mut events).len(), 2);

    let stale = Batch::single(create("b")).expecting(Rev(0));
    let error = batch(&agent, stale).expect_err("conflict");
    assert_eq!(
        error.protocol(),
        Some(&ProtocolError::RevConflict {
            expected: Rev(0),
            current: Rev(1)
        })
    );
    assert_eq!(engine.rev(), Rev(1));
    assert_eq!(scene_ids(&agent), (Rev(1), vec!["a".into()]));
    assert_eq!(drain(&mut events), vec![], "a rejected batch emits nothing");

    let fresh = Batch::single(create("b")).expecting(Rev(1));
    assert_eq!(batch(&agent, fresh), Ok(Rev(2)));
    // An `expected_rev` ahead of the state conflicts too.
    let ahead = Batch::single(create("c")).expecting(Rev(9));
    let error = batch(&agent, ahead).expect_err("ahead");
    assert_eq!(code(&error), Some("rev_conflict"));
    assert_eq!(engine.rev(), Rev(2));
}

#[test]
fn sessions_are_thread_safe_and_dyn_compatible() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Engine>();
    send_sync::<InProcessControl>();
    let engine = Engine::new();
    let shared: Arc<dyn Control> = Arc::new(engine.connect());
    let before = Arc::clone(&shared);
    let refused = std::thread::spawn(move || {
        block_on(before.call(Call::Query {
            query: Query::SceneList,
        }))
    })
    .join()
    .expect("thread");
    assert_eq!(
        refused.as_ref().map_err(code),
        Err(Some("handshake_required"))
    );
    block_on(shared.hello(hello())).expect("handshake");
    let after = Arc::clone(&shared);
    let applied = std::thread::spawn(move || batch(&*after, Batch::single(create("a"))))
        .join()
        .expect("thread");
    assert_eq!(applied, Ok(Rev(1)));
}

#[test]
fn handshake_assigns_unique_client_ids_and_checks_the_version() {
    let engine = Engine::new();
    let a = engine.connect();
    let b = engine.connect();
    assert_eq!(a.client_id(), None);
    let wa = block_on(a.hello(hello())).expect("a");
    let wb = block_on(b.hello(hello())).expect("b");
    assert_ne!(wa.client_id, wb.client_id);
    assert_eq!(a.client_id(), Some(wa.client_id.clone()));
    assert_eq!(wa.rev, Rev(0));
    assert_eq!(wa.protocol, PROTOCOL_VERSION);

    // A second hello keeps the identity and reports the current rev.
    assert_eq!(batch(&b, Batch::single(create("a"))), Ok(Rev(1)));
    let again = block_on(a.hello(hello())).expect("again");
    assert_eq!(again.client_id, wa.client_id);
    assert_eq!(again.rev, Rev(1));

    let newer = Hello {
        protocol: ProtocolVersion {
            major: PROTOCOL_VERSION.major,
            minor: PROTOCOL_VERSION.minor + 1,
        },
        client_name: "future".into(),
    };
    let refusing = engine.connect();
    let refused = block_on(refusing.hello(newer)).expect_err("refused");
    assert_eq!(code(&refused), Some("unsupported_version"));
    // A refused hello opens no session.
    let still = block_on(refusing.call(Call::Query {
        query: Query::SceneList,
    }))
    .expect_err("no session");
    assert_eq!(code(&still), Some("handshake_required"));
}

#[test]
fn every_call_needs_the_handshake_and_request_ids_count_per_session() {
    let engine = Engine::new().with_output(OutputId::new("rec"));
    let control = engine.connect();
    for call in [
        Call::Batch(Batch::single(create("a"))),
        output_call("rec", true),
        Call::Query {
            query: Query::StateGet,
        },
    ] {
        let error = block_on(control.call(call)).expect_err("before hello");
        assert_eq!(code(&error), Some("handshake_required"));
    }
    assert_eq!(engine.rev(), Rev(0));
    block_on(control.hello(hello())).expect("handshake");
    let response = block_on(control.call(Call::Query {
        query: Query::SceneList,
    }))
    .expect("query");
    // Three refused calls used `req_1` to `req_3`.
    assert_eq!(response.id, RequestId::new("req_4"));
    let other = connected(&engine);
    let first = block_on(other.call(Call::Query {
        query: Query::SceneList,
    }))
    .expect("query");
    assert_eq!(first.id, RequestId::new("req_1"));
}

#[test]
fn a_batch_applies_atomically_and_bumps_rev_by_one() {
    let engine = Engine::new();
    let control = connected(&engine);
    let mut events = control.events();
    let edits = Batch {
        edits: vec![
            create("intro"),
            create("main"),
            webcam(),
            add("main", "cam", "webcam"),
            Edit::SceneRename {
                scene_id: SceneId::new("intro"),
                name: "Starting soon".into(),
            },
        ],
        expected_rev: Some(Rev(0)),
        idempotency_key: None,
        ends_gesture: None,
    };
    assert_eq!(batch(&control, edits), Ok(Rev(1)));
    assert_eq!(engine.rev(), Rev(1));
    // One `state.changed` per batch; this one also put the first scene on program.
    let first = drain(&mut events);
    assert_eq!(first.len(), 2, "one event per batch, then studio.changed");
    assert!(matches!(first[1], Event::StudioChanged { .. }));

    // The last edit fails, so none of them applies.
    let failing = Batch {
        edits: vec![
            create("extra"),
            Edit::ItemRemove {
                item_id: ItemId::new("ghost"),
            },
        ],
        expected_rev: None,
        idempotency_key: None,
        ends_gesture: None,
    };
    let error = batch(&control, failing).expect_err("fails");
    assert_eq!(
        error.protocol(),
        Some(&ProtocolError::NotFound {
            entity: EntityKind::Item,
            id: "ghost".into()
        })
    );
    assert_eq!(
        scene_ids(&control),
        (Rev(1), vec!["intro".into(), "main".into()])
    );
    let empty = Batch {
        edits: vec![],
        expected_rev: None,
        idempotency_key: None,
        ends_gesture: None,
    };
    assert_eq!(
        code(&batch(&control, empty).expect_err("empty")),
        Some("empty_batch")
    );
    let duplicate = batch(&control, Batch::single(create("main"))).expect_err("dup");
    assert_eq!(code(&duplicate), Some("already_exists"));
    assert_eq!(engine.rev(), Rev(1));
    assert_eq!(drain(&mut events), vec![], "failures emit nothing");

    // `rev_conflict` is checked before `empty_batch`, as `Graph::apply` does.
    let empty_and_stale = Batch {
        edits: vec![],
        expected_rev: Some(Rev(5)),
        idempotency_key: None,
        ends_gesture: None,
    };
    assert_eq!(
        code(&batch(&control, empty_and_stale).expect_err("stale")),
        Some("rev_conflict")
    );

    assert_eq!(
        batch(
            &control,
            Batch::single(Edit::SceneDelete {
                scene_id: SceneId::new("main"),
            })
        ),
        Ok(Rev(2))
    );
}

/// A CLAP scan adds each plugin as an audio-filter kind, which `kind.list` and
/// `kind.describe` report and which can't be created as a source (ADR-0035).
#[test]
fn scanned_clap_plugins_are_audio_filter_kinds() {
    use jstream_platform::plugin_host::fake::{FakePluginHosts, fake_param};
    use jstream_platform::{HostRequest, PlatformError, PluginHost, PluginHosts};
    use jstream_plugin_proto::control::PluginDesc;
    use std::path::{Path, PathBuf};

    struct OneDir(PathBuf, FakePluginHosts);
    impl PluginHosts for OneDir {
        fn search_paths(&self) -> Vec<PathBuf> {
            vec![self.0.clone()]
        }
        fn scan(&self, file: &Path) -> Result<Vec<PluginDesc>, PlatformError> {
            self.1.scan(file)
        }
        fn start(&self, request: &HostRequest) -> Result<Box<dyn PluginHost>, PlatformError> {
            self.1.start(request)
        }
    }

    let dir = std::env::temp_dir().join(format!("jstream-engine-clap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("gain.clap");
    std::fs::write(&file, b"").unwrap();
    let fake = FakePluginHosts::new();
    fake.add_file(
        &file,
        vec![PluginDesc {
            id: "org.example.gain".into(),
            name: "Gain".into(),
            vendor: String::new(),
            version: String::new(),
            description: String::new(),
            params: vec![fake_param()],
        }],
    );
    let engine = Engine::new();
    let scan = engine.scan_clap_plugins(std::sync::Arc::new(OneDir(dir.clone(), fake)));
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(scan.kinds.len(), 1);
    assert_eq!(scan.kinds[0].file, file);

    let control = connected(&engine);
    let Ok(QueryResult::KindList { kinds }) = query(&control, Query::KindList) else {
        panic!("kind.list");
    };
    let clap = kinds
        .iter()
        .find(|k| k.kind_id == "clap/org.example.gain")
        .expect("the CLAP kind is listed");
    assert_eq!(clap.category, KindCategory::AudioFilter);
    assert!(kinds.iter().any(|k| k.kind_id == "color"), "built-ins stay");
    let Ok(QueryResult::KindDescribe { kind, .. }) = query(
        &control,
        Query::KindDescribe {
            kind_id: "clap/org.example.gain".into(),
        },
    ) else {
        panic!("kind.describe");
    };
    assert_eq!(kind.name, "Gain");
    assert!(kind.param("1").is_some());
    let error = batch(
        &control,
        Batch::single(Edit::SourceCreate {
            source_id: SourceId::new("x"),
            name: "x".into(),
            kind: SourceKind::Builtin {
                kind: "clap/org.example.gain".into(),
            },
            settings: Default::default(),
            duplicated_from: None,
        }),
    )
    .expect_err("not a source kind");
    assert!(
        matches!(error.protocol(), Some(ProtocolError::InvalidArgument { field, .. }) if field == "edits.0.kind.kind"),
        "{error:?}"
    );
    // A CLAP filter is added like any filter, its settings completed from the plugin's
    // parameters.
    let added = batch(
        &control,
        Batch {
            edits: vec![
                Edit::SourceCreate {
                    source_id: SourceId::new("mic"),
                    name: "mic".into(),
                    kind: SourceKind::Builtin {
                        kind: "desktop_audio".into(),
                    },
                    settings: Default::default(),
                    duplicated_from: None,
                },
                Edit::FilterAdd {
                    owner: jstream_protocol::FilterOwner::Source {
                        source_id: SourceId::new("mic"),
                    },
                    chain: jstream_protocol::FilterChain::Audio,
                    filter_id: jstream_protocol::FilterId::new("plugin"),
                    kind: "clap/org.example.gain".into(),
                    settings: Default::default(),
                    enabled: None,
                    index: None,
                },
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        },
    )
    .expect("a CLAP filter is added");
    assert_eq!(added, Rev(1));
    let Ok(QueryResult::SourceList { sources, .. }) = query(&control, Query::SourceList) else {
        panic!("source.list");
    };
    let filter = &sources[0].filters.audio[0];
    assert_eq!(filter.kind, "clap/org.example.gain");
    assert_eq!(filter.settings.get("1"), Some(&serde_json::json!(1.0)));
}

/// The engine takes scene filters and item effects (ADR-0065), which run on the scene's
/// composite and the item's box, reports them, and refuses a missing owner as the graph
/// does.
#[test]
fn scene_filters_and_item_effects_are_taken() {
    use jstream_protocol::{FilterChain, FilterId, FilterOwner, ItemId, SceneId};
    let engine = Engine::new();
    let control = connected(&engine);
    let setup = vec![
        Edit::SceneCreate {
            scene_id: SceneId::new("s"),
            name: "S".into(),
            role: None,
        },
        Edit::SourceCreate {
            source_id: SourceId::new("bg"),
            name: "bg".into(),
            kind: SourceKind::Builtin {
                kind: "color".into(),
            },
            settings: Default::default(),
            duplicated_from: None,
        },
        Edit::ItemAdd {
            scene_id: SceneId::new("s"),
            item_id: ItemId::new("i"),
            source_id: SourceId::new("bg"),
            transform: None,
            geometry: Some(
                jstream_protocol::ItemGeometry::try_new(
                    jstream_protocol::Vec2::new(0.0, 0.0).expect("finite"),
                    jstream_protocol::Size::try_new(320.0, 180.0).expect("positive"),
                )
                .expect("valid"),
            ),
            opacity: None,
            blend: None,
        },
    ];
    let rev = batch(
        &control,
        Batch {
            edits: setup,
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        },
    )
    .expect("set up");
    engine
        .set_source_frame(&SourceId::new("bg"), 1, 1, &[0, 0, 0, 255])
        .expect("a frame for bg");
    let sharpen = |owner: FilterOwner| Edit::FilterAdd {
        owner,
        chain: FilterChain::Video,
        filter_id: FilterId::new("f"),
        kind: "sharpen".into(),
        settings: Default::default(),
        enabled: None,
        index: None,
    };
    let item = FilterOwner::Item {
        item_id: ItemId::new("i"),
    };
    let effect = batch(&control, Batch::single(sharpen(item))).expect("an item effect");
    assert!(effect > rev);
    let scene = FilterOwner::Scene {
        scene_id: SceneId::new("s"),
    };
    let filter = batch(&control, Batch::single(sharpen(scene))).expect("a scene filter");
    assert!(filter > effect);
    let Ok(QueryResult::StateGet { scenes, .. }) = query(&control, Query::StateGet) else {
        panic!("state.get");
    };
    let s = scenes
        .iter()
        .find(|scene| scene.scene_id == SceneId::new("s"))
        .expect("s");
    assert_eq!(s.filters.video.len(), 1);
    assert_eq!(s.items[0].effects.len(), 1);
    // `stats.tick` counts what they add to each frame (ADR-0065 §7).
    let mut events = control.events();
    engine.emit_stats_tick();
    let chains = drain(&mut events)
        .into_iter()
        .find_map(|event| match event {
            Event::StatsTick { chains, .. } => chains,
            _ => None,
        });
    let chains = chains.expect("a tick with its chain cost");
    assert_eq!(
        (
            chains.scene_chains,
            chains.scene_layers,
            chains.scene_passes
        ),
        (1, 2, 2),
        "the scene's composite and its filter"
    );
    assert_eq!(
        (chains.item_effects, chains.item_layers, chains.item_passes),
        (1, 2, 2),
        "the item's box and its effect"
    );
    let canvas = 1920 * 1080 * 8;
    assert_eq!(
        chains.layer_bytes,
        2 * canvas + 2 * 320 * 180 * 8,
        "two canvas-sized layers and two of the box"
    );
    let ghost = FilterOwner::Item {
        item_id: ItemId::new("ghost"),
    };
    let error = batch(&control, Batch::single(sharpen(ghost))).expect_err("not found");
    assert!(
        matches!(
            error.protocol(),
            Some(ProtocolError::NotFound {
                entity: EntityKind::Item,
                ..
            })
        ),
        "{error:?}"
    );
}

/// The effect packs in the folder the engine watches become `video_filter` kinds, which a
/// video chain takes; a pack removed from the folder takes its kinds out of `kind.list`.
#[test]
fn effect_packs_become_video_filter_kinds() {
    use crate::effect_packs::test_pack::{self, KIND, Temp};

    let temp = Temp::new("engine");
    test_pack::write(&temp.pack(), test_pack::MUL);
    let engine = Engine::new();
    engine.watch_effect_packs(&temp.0);
    let control = connected(&engine);
    let listed = || {
        let Ok(QueryResult::KindList { kinds }) = query(&control, Query::KindList) else {
            panic!("kind.list");
        };
        kinds
            .into_iter()
            .find(|k| k.kind_id == KIND)
            .map(|k| k.category)
    };
    assert_eq!(
        listed(),
        Some(KindCategory::VideoFilter),
        "loaded before it returns"
    );
    batch(
        &control,
        Batch {
            edits: vec![
                Edit::SourceCreate {
                    source_id: SourceId::new("bg"),
                    name: "bg".into(),
                    kind: SourceKind::Builtin {
                        kind: "color".into(),
                    },
                    settings: Default::default(),
                    duplicated_from: None,
                },
                Edit::FilterAdd {
                    owner: jstream_protocol::FilterOwner::Source {
                        source_id: SourceId::new("bg"),
                    },
                    chain: jstream_protocol::FilterChain::Video,
                    filter_id: jstream_protocol::FilterId::new("tint"),
                    kind: KIND.into(),
                    settings: [("tint", serde_json::json!("#ff0000"))]
                        .into_iter()
                        .collect(),
                    enabled: None,
                    index: None,
                },
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        },
    )
    .expect("a pack filter on a video chain");

    std::fs::remove_dir_all(temp.pack()).expect("remove the pack");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while listed().is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "the removed pack's kind stays listed"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The engine describes its built-in kinds, and validates and completes settings against
/// them (ADR-0025).
#[test]
fn kinds_are_described_and_settings_checked() {
    let engine = Engine::new();
    let control = connected(&engine);
    let Ok(QueryResult::KindList { kinds }) = query(&control, Query::KindList) else {
        panic!("kind.list");
    };
    let ids: Vec<&str> = kinds.iter().map(|k| k.kind_id.as_str()).collect();
    assert!(ids.contains(&"color") && ids.contains(&"webcam"), "{ids:?}");
    let Ok(QueryResult::KindDescribe { kind, .. }) = query(
        &control,
        Query::KindDescribe {
            kind_id: "color".into(),
        },
    ) else {
        panic!("kind.describe");
    };
    assert!(kind.param("color").is_some());

    let color = |settings: serde_json::Value| {
        Batch::single(Edit::SourceCreate {
            source_id: SourceId::new("bg"),
            name: "Background".into(),
            kind: SourceKind::Builtin {
                kind: "color".into(),
            },
            settings: serde_json::from_value(settings).expect("a settings map"),
            duplicated_from: None,
        })
    };
    let error =
        batch(&control, color(serde_json::json!({"color": "red"}))).expect_err("bad colour");
    assert!(
        matches!(error.protocol(), Some(ProtocolError::InvalidArgument { field, .. }) if field == "edits.0.settings.color"),
        "{error:?}"
    );
    assert_eq!(
        batch(&control, color(serde_json::json!({"color": "#ff0000"}))),
        Ok(Rev(1))
    );
    let Ok(QueryResult::SourceList { sources, .. }) = query(&control, Query::SourceList) else {
        panic!("source.list");
    };
    let settings = &sources[0].settings;
    assert_eq!(settings.get("color"), Some(&serde_json::json!("#ff0000")));
    assert_eq!(settings.get("width"), Some(&serde_json::json!(1920)));
}

/// The engine answers every read of the shared parity collection exactly as the fixture
/// expects, the answers `jstream-graph` and the mock are held to.
#[test]
fn queries_match_the_parity_fixture() {
    let engine = parity_engine().with_output(OutputId::new("rec"));
    let control = connected(&engine);
    assert_eq!(
        batch(&control, parity::collection()),
        Ok(parity::COLLECTION_REV)
    );
    for (call, expected) in parity::reads() {
        let got = query(&control, call.clone()).map_err(|error| error.protocol().cloned());
        assert_eq!(got, expected.map_err(Some), "{call:?}");
    }
    for (rejected, expected) in parity::rejected() {
        let error = batch(&control, rejected.clone()).expect_err("rejected");
        assert_eq!(error.protocol(), Some(&expected), "{rejected:?}");
        assert_eq!(engine.rev(), parity::COLLECTION_REV, "{rejected:?}");
    }
    // `output.status` reads the outputs, not the graph.
    assert_eq!(
        query(
            &control,
            Query::OutputStatus {
                output_id: OutputId::new("rec")
            }
        ),
        Ok(QueryResult::OutputStatus {
            output_id: OutputId::new("rec"),
            state: OutputState::Stopped,
            key_set: false,
            encoder: None,
            encoder_sessions: 0,
            hardware_sessions: 0,
            cleartext: false,
        })
    );
    // `cleartext` follows the output's server URL (#562): Twitch's is plain `rtmp://`,
    // YouTube's `rtmps://`, and the recording and the virtual camera send no key.
    for (id, want) in [("rec", false), ("tw", true), ("vcam", false), ("yt", false)] {
        let status = query(
            &control,
            Query::OutputStatus {
                output_id: OutputId::new(id),
            },
        );
        let Ok(QueryResult::OutputStatus { cleartext, .. }) = status else {
            panic!("{status:?}");
        };
        assert_eq!(cleartext, want, "{id}");
    }
    let missing = query(
        &control,
        Query::OutputStatus {
            output_id: OutputId::new("nope"),
        },
    )
    .expect_err("missing");
    assert_eq!(
        missing.protocol(),
        Some(&ProtocolError::NotFound {
            entity: EntityKind::Output,
            id: "nope".into()
        })
    );
}

#[test]
fn an_idempotent_retry_gets_the_original_result() {
    let engine = Engine::new();
    let control = connected(&engine);
    let mut events = control.events();
    let keyed = Batch::single(create("a")).with_idempotency_key(IdempotencyKey::new("k1"));
    let first = block_on(control.call(Call::Batch(keyed.clone()))).expect("first");
    assert_eq!(first.result, CallResult::batch(Rev(1)));
    assert_eq!(batch(&control, Batch::single(create("b"))), Ok(Rev(2)));
    // The retry answers before the revision check, so a now-stale `expected_rev` is fine.
    let retry =
        block_on(control.call(Call::Batch(keyed.clone().expecting(Rev(0))))).expect("retry");
    assert_eq!(retry.result, first.result);
    assert_eq!(engine.rev(), Rev(2));
    // Two `state.changed`, and `studio.changed` for the first scene.
    assert_eq!(drain(&mut events).len(), 3, "a retry emits nothing");

    // Keys are per client: another client's identical key is a new batch.
    let other = connected(&engine);
    let error = batch(&other, keyed).expect_err("scene a exists");
    assert_eq!(code(&error), Some("already_exists"));

    // A batch that failed is not remembered, so its retry is applied.
    let failing =
        Batch::single(add("ghost_scene", "i", "s")).with_idempotency_key(IdempotencyKey::new("k2"));
    assert_eq!(
        code(&batch(&control, failing).expect_err("fails")),
        Some("not_found")
    );
    let fixed = Batch::single(create("c")).with_idempotency_key(IdempotencyKey::new("k2"));
    assert_eq!(batch(&control, fixed.clone()), Ok(Rev(3)));
    assert_eq!(batch(&control, fixed), Ok(Rev(3)));
    assert_eq!(engine.rev(), Rev(3));
}

#[test]
fn events_reach_every_subscriber_in_order() {
    let engine = Engine::new().with_output(OutputId::new("yt"));
    let user = connected(&engine);
    let agent = connected(&engine);
    let mut user_events = user.events();
    let mut agent_events = agent.events();
    let mut second = agent.events();

    let by_agent = block_on(agent.call(Call::Batch(Batch::single(create("a"))))).expect("batch");
    let by_user = block_on(user.call(Call::Batch(Batch::single(create("b"))))).expect("batch");
    let started = block_on(agent.call(output_call("yt", true))).expect("start");
    assert_eq!(started.result, CallResult::action());

    let changed = |state| Event::OutputStateChanged {
        output_id: OutputId::new("yt"),
        state,
    };
    let expected = vec![
        Event::StateChanged {
            rev: Rev(1),
            client_id: agent.client_id().expect("agent"),
            request_id: Some(by_agent.id.clone()),
            cause: None,
        },
        Event::StudioChanged {
            program_scene: Some(SceneId::new("a")),
            preview_scene: Some(SceneId::new("a")),
            transition: None,
            client_id: agent.client_id().expect("agent"),
            request_id: Some(by_agent.id),
        },
        Event::StateChanged {
            rev: Rev(2),
            client_id: user.client_id().expect("user"),
            request_id: Some(by_user.id),
            cause: None,
        },
        changed(OutputState::Starting),
        changed(OutputState::Active),
    ];
    assert_eq!(drain(&mut user_events), expected);
    assert_eq!(drain(&mut agent_events), expected);
    assert_eq!(
        drain(&mut second),
        expected,
        "each subscription is independent"
    );
    // Queries emit nothing.
    query(&user, Query::StateGet).expect("state.get");
    assert_eq!(drain(&mut user_events), vec![]);
}

/// `scene.switch` puts a scene on program without an edit: no `rev`, `studio.changed`
/// to every client, `studio.get` reads it back. The first scene goes on program by
/// itself, a deleted program scene gives way to the first one left, and reordering
/// scenes doesn't move it (ADR-0030).
#[test]
fn scene_switch_changes_program_without_an_edit() {
    let engine = Engine::new();
    let user = connected(&engine);
    let agent = connected(&engine);
    let mut events = user.events();
    let program = |control: &dyn Control| match query(control, Query::StudioGet) {
        Ok(QueryResult::StudioGet { program_scene, .. }) => program_scene,
        other => panic!("not a studio result: {other:?}"),
    };
    // A switch reports the preset it ran: the initial `cut`, which takes no time.
    let switched =
        |scene: &str, client: &InProcessControl, request: RequestId| Event::StudioChanged {
            program_scene: Some(SceneId::new(scene)),
            preview_scene: Some(SceneId::new("a")),
            transition: Some(TransitionStarted {
                preset_id: TransitionPresetId::new("cut"),
                duration_ms: 0,
            }),
            client_id: client.client_id().expect("connected"),
            request_id: Some(request),
        };
    // Batches move program and preview by themselves, with no transition.
    let moved = |scene: &str, client: &InProcessControl, request: RequestId| Event::StudioChanged {
        program_scene: Some(SceneId::new(scene)),
        preview_scene: Some(SceneId::new(scene)),
        transition: None,
        client_id: client.client_id().expect("connected"),
        request_id: Some(request),
    };
    assert_eq!(program(&user), None);
    assert_eq!(
        code(&block_on(agent.call(switch_call("a"))).expect_err("no scene yet")),
        Some("not_found")
    );

    let built = block_on(user.call(Call::Batch(Batch {
        edits: vec![create("a"), create("b")],
        expected_rev: None,
        idempotency_key: None,
        ends_gesture: None,
    })))
    .expect("scenes");
    assert_eq!(
        drain(&mut events),
        vec![
            Event::StateChanged {
                rev: Rev(1),
                client_id: user.client_id().expect("connected"),
                request_id: Some(built.id.clone()),
                cause: None,
            },
            moved("a", &user, built.id),
        ]
    );
    assert_eq!(program(&agent), Some(SceneId::new("a")));

    let switch = block_on(agent.call(switch_call("b"))).expect("switch");
    assert_eq!(switch.result, CallResult::action());
    assert_eq!(engine.rev(), Rev(1), "a switch is not an edit");
    assert_eq!(drain(&mut events), vec![switched("b", &agent, switch.id)]);
    assert_eq!(program(&user), Some(SceneId::new("b")));
    // Switching to what is already on program succeeds and says nothing.
    block_on(user.call(switch_call("b"))).expect("same scene");
    assert_eq!(drain(&mut events), vec![]);
    let error = block_on(user.call(switch_call("ghost"))).expect_err("unknown scene");
    assert_eq!(
        error.protocol(),
        Some(&ProtocolError::NotFound {
            entity: EntityKind::Scene,
            id: "ghost".into(),
        })
    );
    assert_eq!(drain(&mut events), vec![]);

    // Reordering scenes leaves program where it is.
    batch(
        &user,
        Batch::single(Edit::SceneReorder {
            scene_id: SceneId::new("a"),
            index: 1,
        }),
    )
    .expect("reorder");
    assert!(matches!(
        drain(&mut events).as_slice(),
        [Event::StateChanged { rev: Rev(2), .. }]
    ));
    assert_eq!(program(&user), Some(SceneId::new("b")));

    // Deleting the program scene puts the first scene left on program, after the
    // batch's own `state.changed`; deleting the last one leaves none.
    let deleted = block_on(agent.call(Call::Batch(Batch::single(Edit::SceneDelete {
        scene_id: SceneId::new("b"),
    }))))
    .expect("delete");
    let seen = drain(&mut events);
    assert!(matches!(seen[0], Event::StateChanged { rev: Rev(3), .. }));
    assert_eq!(seen[1..], [moved("a", &agent, deleted.id)]);
    batch(
        &agent,
        Batch::single(Edit::SceneDelete {
            scene_id: SceneId::new("a"),
        }),
    )
    .expect("delete the last");
    assert!(matches!(
        drain(&mut events).as_slice(),
        [
            Event::StateChanged { rev: Rev(4), .. },
            Event::StudioChanged {
                program_scene: None,
                ..
            }
        ]
    ));
    assert_eq!(program(&user), None);
}

fn privacy_call(on: bool) -> Call {
    Call::Action {
        action: Action::PrivacySet { on },
    }
}

/// JStream has no privacy screen (ADR-0080 §4): the deprecated `privacy.set` is refused
/// as unsupported and changes nothing, `privacy.get` always answers off, and no client
/// hears `privacy.changed`.
#[test]
fn the_deprecated_privacy_calls_cover_nothing() {
    let engine = Engine::new();
    let user = connected(&engine);
    let mut events = user.events();
    for on in [true, false] {
        let error = block_on(user.call(privacy_call(on))).expect_err("refused");
        assert_eq!(
            error.protocol(),
            Some(&ProtocolError::Unsupported {
                call: "privacy.set".into()
            })
        );
    }
    assert_eq!(engine.rev(), Rev(0));
    assert!(matches!(
        query(&user, Query::PrivacyGet),
        Ok(QueryResult::PrivacyGet { on: false })
    ));
    assert_eq!(drain(&mut events), vec![]);
    assert_eq!(drain(&mut connected(&engine).events()), vec![]);
}

#[test]
fn outputs_move_through_the_placeholder_state_machine() {
    let engine = Engine::new()
        .with_output(OutputId::new("yt"))
        .with_output(OutputId::new("rec"))
        .with_output(OutputId::new("yt"));
    let control = connected(&engine);
    let mut events = control.events();
    let status = |id: &str| match query(
        &control,
        Query::OutputStatus {
            output_id: OutputId::new(id),
        },
    ) {
        Ok(QueryResult::OutputStatus { state, .. }) => state,
        other => panic!("not an output status: {other:?}"),
    };
    let act = |id: &str, start: bool| block_on(control.call(output_call(id, start)));
    let changed = |id: &str, state| Event::OutputStateChanged {
        output_id: OutputId::new(id),
        state,
    };

    assert_eq!(status("yt"), OutputState::Stopped);
    act("yt", true).expect("start");
    assert_eq!(status("yt"), OutputState::Active);
    assert_eq!(
        status("rec"),
        OutputState::Stopped,
        "outputs are independent"
    );
    assert_eq!(
        drain(&mut events),
        vec![
            changed("yt", OutputState::Starting),
            changed("yt", OutputState::Active)
        ]
    );

    let again = act("yt", true).expect_err("already running");
    assert_eq!(code(&again), Some("action_failed"));
    let idle = act("rec", false).expect_err("not running");
    assert_eq!(code(&idle), Some("action_failed"));
    assert_eq!(drain(&mut events), vec![], "a refused action emits nothing");

    act("yt", false).expect("stop");
    assert_eq!(status("yt"), OutputState::Stopped);
    assert_eq!(
        drain(&mut events),
        vec![
            changed("yt", OutputState::Stopping),
            changed("yt", OutputState::Stopped)
        ]
    );
    // It can start again.
    act("yt", true).expect("restart");
    assert_eq!(status("yt"), OutputState::Active);

    let missing = act("nope", true).expect_err("unknown output");
    assert_eq!(
        missing.protocol(),
        Some(&ProtocolError::NotFound {
            entity: EntityKind::Output,
            id: "nope".into()
        })
    );
    assert_eq!(engine.rev(), Rev(0), "actions never change rev");
}

#[test]
fn streams_end_with_their_session_or_the_engine() {
    struct Flag(AtomicBool);
    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let flag = Arc::new(Flag(AtomicBool::new(false)));
    let waker = Waker::from(Arc::clone(&flag));

    let engine = Engine::new();
    let leaving = connected(&engine);
    let staying = connected(&engine);
    let mut leaving_events = leaving.events();
    let mut staying_events = staying.events();
    assert_eq!(poll_event(&mut staying_events, &waker), Poll::Pending);

    batch(&staying, Batch::single(create("a"))).expect("batch");
    assert!(flag.0.load(Ordering::SeqCst), "the pending poll was woken");
    drop(leaving);
    // Buffered events are still delivered, then the stream ends and stays ended.
    assert!(matches!(
        poll_event(&mut leaving_events, Waker::noop()),
        Poll::Ready(Some(Event::StateChanged { rev: Rev(1), .. }))
    ));
    assert!(matches!(
        poll_event(&mut leaving_events, Waker::noop()),
        Poll::Ready(Some(Event::StudioChanged { .. }))
    ));
    assert_eq!(
        poll_event(&mut leaving_events, Waker::noop()),
        Poll::Ready(None)
    );
    batch(&staying, Batch::single(create("b"))).expect("batch");
    assert_eq!(
        poll_event(&mut leaving_events, Waker::noop()),
        Poll::Ready(None)
    );
    // Two `state.changed`, and `studio.changed` for the first scene.
    assert_eq!(drain(&mut staying_events).len(), 3);

    // Subscriptions opened later see only later events.
    let mut late = staying.events();
    assert_eq!(poll_event(&mut late, Waker::noop()), Poll::Pending);

    // The engine lives while a handle or a session does; then every stream ends.
    flag.0.store(false, Ordering::SeqCst);
    assert_eq!(poll_event(&mut staying_events, &waker), Poll::Pending);
    let other_handle = engine.clone();
    drop(engine);
    batch(&staying, Batch::single(create("c"))).expect("still running");
    assert_eq!(other_handle.rev(), Rev(3));
    drop(late);
    drop(staying);
    assert!(flag.0.load(Ordering::SeqCst));
    assert!(matches!(
        poll_event(&mut staying_events, &waker),
        Poll::Ready(Some(Event::StateChanged { rev: Rev(3), .. }))
    ));
    assert_eq!(poll_event(&mut staying_events, &waker), Poll::Ready(None));

    // A subscription of a session whose engine handle is gone ends when the session goes.
    let orphan = connected(&other_handle);
    let mut orphan_events = orphan.events();
    drop(other_handle);
    drop(orphan);
    assert_eq!(poll_event(&mut orphan_events, &waker), Poll::Ready(None));
}

/// Sessions on many threads at once: every batch applies exactly once, `rev` counts
/// them all, and every subscriber sees every `state.changed`, in `rev` order, with no
/// gap.
#[test]
fn concurrent_sessions_see_every_event_in_rev_order() {
    const THREADS: usize = 8;
    const BATCHES: usize = 25;
    let engine = Engine::new();
    let watcher = connected(&engine);
    let mut events = watcher.events();
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let engine = engine.clone();
            std::thread::spawn(move || {
                let control = connected(&engine);
                for b in 0..BATCHES {
                    batch(&control, Batch::single(create(&format!("s{t}_{b}")))).expect("batch");
                }
                control.client_id()
            })
        })
        .collect();
    let mut client_ids: Vec<ClientId> = handles
        .into_iter()
        .map(|h| h.join().expect("thread").expect("client id"))
        .collect();
    client_ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    client_ids.dedup();
    assert_eq!(client_ids.len(), THREADS, "client ids are unique");

    let total = u64::try_from(THREADS * BATCHES).expect("small");
    assert_eq!(engine.rev(), Rev(total));
    // The first batch also puts its scene on program; no later one moves it.
    let mut program_changes = 0;
    let revs: Vec<u64> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            Event::StateChanged { rev, .. } => Some(rev.0),
            Event::StudioChanged { .. } => {
                program_changes += 1;
                None
            }
            other => panic!("unexpected event {other:?}"),
        })
        .collect();
    assert_eq!(program_changes, 1);
    assert_eq!(revs, (1..=total).collect::<Vec<_>>());
}

/// The same script of calls against the engine and against `jstream-control`'s mock
/// gets the same answers (results, error codes and fields) and the same events, up to
/// client IDs. The mock is the reference implementation clients were developed against.
fn moved(item: &str, x: f32) -> Edit {
    Edit::ItemSetTransform {
        item_id: ItemId::new(item),
        scene_id: None,
        transform: jstream_protocol::ItemTransform {
            position: jstream_protocol::Vec2::new(x, 0.0).expect("finite"),
            ..jstream_protocol::ItemTransform::IDENTITY
        },
    }
}

fn live(gesture_id: &str, values: Vec<Edit>) -> Call {
    Call::Action {
        action: Action::LiveSet {
            gesture_id: GestureId::new(gesture_id),
            values,
        },
    }
}

/// The idempotency key of the organise script's scene-and-source delete, which the test
/// checks applied: a refused one would compare equal on both sides and test nothing.
const ORGANISED_DELETE: &str = "organised-delete";

/// The differential script's folder, label and metadata calls (ADR-0024 §3 and §4, #387),
/// run after the rest on scene `z` and sources `cam` and `tagged`: each check that
/// refuses one, what the reads report, the in-use deletes, undo and redo, and a filed
/// scene's and a labelled source's delete, applied and then undone with their metadata.
fn organise_script() -> Vec<Call> {
    use jstream_protocol::{FolderId, LabelId, SceneMeta, SourceMeta};
    fn one(edit: Edit) -> Call {
        Call::Batch(Batch::single(edit))
    }
    fn folder(id: &str, parent: Option<&str>) -> Edit {
        Edit::FolderCreate {
            folder_id: FolderId::new(id),
            name: format!("Folder {id}"),
            parent: parent.map(FolderId::new),
        }
    }
    fn label(id: &str, color: &str, icon: Option<&str>) -> Edit {
        Edit::LabelCreate {
            label_id: LabelId::new(id),
            name: format!("Label {id}"),
            color: color.into(),
            icon: icon.map(str::to_owned),
        }
    }
    fn labelled(labels: &[&str]) -> Edit {
        Edit::SourceSetMeta {
            source_id: SourceId::new("cam"),
            meta: SourceMeta {
                labels: labels.iter().map(|l| LabelId::new(*l)).collect(),
                favourite: true,
            },
        }
    }
    fn scene_meta(icon: Option<&str>, color: Option<&str>) -> Edit {
        Edit::SceneSetMeta {
            scene_id: SceneId::new("z"),
            meta: SceneMeta {
                labels: vec![LabelId::new("camera")],
                icon: icon.map(str::to_owned),
                color: color.map(str::to_owned),
                favourite: true,
                show_in_multiview: false,
            },
        }
    }
    let read = |query: Query| Call::Query { query };
    let undo = || {
        Call::History(jstream_protocol::HistoryCall::new(
            jstream_protocol::HistoryOp::Undo,
        ))
    };
    let redo = || {
        Call::History(jstream_protocol::HistoryCall::new(
            jstream_protocol::HistoryOp::Redo,
        ))
    };
    vec![
        Call::Batch(edits(vec![
            folder("outer", None),
            folder("inner", Some("outer")),
            label("sponsor", "#ff8800", None),
            label("camera", "#00FF00", Some("mic")),
        ])),
        // Refused: a missing parent, a taken ID, an empty ID, a blank name, a folder
        // inside itself, a colour, an icon, a missing label and one listed twice.
        one(folder("x", Some("ghost"))),
        one(folder("outer", None)),
        one(folder("", None)),
        one(Edit::FolderRename {
            folder_id: FolderId::new("outer"),
            name: " ".into(),
        }),
        one(Edit::FolderSetParent {
            folder_id: FolderId::new("outer"),
            parent: Some(FolderId::new("inner")),
        }),
        one(Edit::FolderSetParent {
            folder_id: FolderId::new("outer"),
            parent: Some(FolderId::new("outer")),
        }),
        one(label("x", "orange", None)),
        one(label("sponsor", "#000000", None)),
        one(Edit::LabelUpdate {
            label_id: LabelId::new("sponsor"),
            name: "Sponsor".into(),
            color: "#ff8800".into(),
            icon: Some("x".repeat(65)),
        }),
        one(labelled(&["ghost"])),
        one(labelled(&["sponsor", "sponsor"])),
        one(scene_meta(Some(""), None)),
        one(Edit::SceneSetMeta {
            scene_id: SceneId::new("z"),
            meta: SceneMeta {
                labels: vec![LabelId::new("ghost")],
                ..SceneMeta::default()
            },
        }),
        one(scene_meta(None, Some("#12345"))),
        one(Edit::SceneSetFolder {
            scene_id: SceneId::new("z"),
            folder_id: Some(FolderId::new("ghost")),
        }),
        one(Edit::LabelReorder {
            label_id: LabelId::new("camera"),
            index: 2,
        }),
        // Applied, then read back, undone and redone.
        Call::Batch(edits(vec![
            Edit::SceneSetFolder {
                scene_id: SceneId::new("z"),
                folder_id: Some(FolderId::new("inner")),
            },
            scene_meta(Some("star"), Some("#123456")),
            labelled(&["sponsor", "camera"]),
            Edit::FolderReorder {
                folder_id: FolderId::new("inner"),
                index: 0,
            },
            Edit::LabelReorder {
                label_id: LabelId::new("camera"),
                index: 0,
            },
            Edit::LabelUpdate {
                label_id: LabelId::new("sponsor"),
                name: "Sponsor".into(),
                color: "#FF8800".into(),
                icon: Some("star".into()),
            },
            Edit::FolderRename {
                folder_id: FolderId::new("outer"),
                name: "Shows".into(),
            },
        ])),
        read(Query::SceneList),
        read(Query::SourceList),
        read(Query::StateGet),
        undo(),
        read(Query::StateGet),
        redo(),
        // In use: a folder with a scene, a folder with a folder, a label a source carries.
        one(Edit::FolderDelete {
            folder_id: FolderId::new("inner"),
        }),
        one(Edit::FolderDelete {
            folder_id: FolderId::new("outer"),
        }),
        one(Edit::LabelDelete {
            label_id: LabelId::new("sponsor"),
        }),
        // A copy carries the source's metadata.
        one(Edit::SourceDuplicate {
            source_id: SourceId::new("cam"),
            new_source_id: SourceId::new("cam_2"),
            name: "Cam 2".into(),
        }),
        read(Query::SourceList),
        undo(),
        // A source only `z` places, carrying a label: deleting `z` and then it applies
        // (the test checks that it does), and undoing that restores the scene's folder
        // and metadata and the source's labels. Then the source goes again.
        Call::Batch(edits(vec![
            Edit::SourceCreate {
                source_id: SourceId::new("tagged"),
                name: "Tagged".into(),
                kind: SourceKind::Builtin {
                    kind: "webcam".into(),
                },
                // Complete settings, as `webcam()` sends: the mock stores them as sent.
                settings: [("device".to_owned(), serde_json::Value::from(""))]
                    .into_iter()
                    .collect(),
                duplicated_from: None,
            },
            add("z", "iz2", "tagged"),
            Edit::SourceSetMeta {
                source_id: SourceId::new("tagged"),
                meta: SourceMeta {
                    labels: vec![LabelId::new("sponsor")],
                    favourite: true,
                },
            },
        ])),
        Call::Batch(
            edits(vec![
                Edit::SceneDelete {
                    scene_id: SceneId::new("z"),
                },
                Edit::SourceDelete {
                    source_id: SourceId::new("tagged"),
                },
            ])
            .with_idempotency_key(IdempotencyKey::new(ORGANISED_DELETE)),
        ),
        read(Query::StateGet),
        undo(),
        read(Query::StateGet),
        undo(),
        // Emptying `inner` still leaves `outer` holding it, so this batch is refused
        // whole; emptied in order, the folders and labels go, and undo brings them back
        // in order.
        Call::Batch(edits(vec![
            Edit::SceneSetFolder {
                scene_id: SceneId::new("z"),
                folder_id: None,
            },
            labelled(&[]),
            Edit::FolderDelete {
                folder_id: FolderId::new("outer"),
            },
        ])),
        Call::Batch(edits(vec![
            Edit::SceneSetFolder {
                scene_id: SceneId::new("z"),
                folder_id: None,
            },
            labelled(&[]),
            Edit::FolderDelete {
                folder_id: FolderId::new("inner"),
            },
            Edit::FolderDelete {
                folder_id: FolderId::new("outer"),
            },
            Edit::LabelDelete {
                label_id: LabelId::new("camera"),
            },
            Edit::LabelDelete {
                label_id: LabelId::new("sponsor"),
            },
        ])),
        read(Query::StateGet),
        undo(),
        read(Query::StateGet),
    ]
}

/// The differential script's keyframe calls (ADR-0024 §2, ADR-0049), run last on scene
/// `kp` and its keyframe `kk`: overrides written by placement edits and cleared, what a
/// keyframe refuses, `live.set` on one, the refused parent delete, and a keyframe with a
/// role deleted and undone. An `item.remove` that drops an override is left out: the
/// mock leaves the batch result's `dropped_overrides` empty.
fn keyframe_script() -> Vec<Call> {
    use jstream_protocol::{Crop, OverrideField};
    fn one(edit: Edit) -> Call {
        Call::Batch(Batch::single(edit))
    }
    fn variant(id: &str, of: &str) -> Edit {
        Edit::SceneCreateVariant {
            scene_id: SceneId::new(id),
            variant_of: SceneId::new(of),
            name: id.to_uppercase(),
        }
    }
    fn crop(scene: &str, left: u32) -> Edit {
        Edit::ItemSetCrop {
            item_id: ItemId::new("ikp"),
            scene_id: Some(SceneId::new(scene)),
            crop: Crop {
                left,
                ..Crop::default()
            },
        }
    }
    fn clear(field: OverrideField) -> Edit {
        Edit::ItemClearOverride {
            scene_id: SceneId::new("kk"),
            item_id: ItemId::new("ikp"),
            field,
        }
    }
    let read = |query: Query| Call::Query { query };
    let undo = || {
        Call::History(jstream_protocol::HistoryCall::new(
            jstream_protocol::HistoryOp::Undo,
        ))
    };
    let delete = |scene: &str| {
        one(Edit::SceneDelete {
            scene_id: SceneId::new(scene),
        })
    };
    vec![
        Call::Batch(edits(vec![
            create("kp"),
            add("kp", "ikp", "cam"),
            variant("kk", "kp"),
            // A group placed in the parent, whose child the keyframe can override.
            Edit::SceneCreate {
                scene_id: SceneId::new("kg"),
                name: "KG".into(),
                role: Some(SceneRole::Group),
            },
            Edit::SourceCreate {
                source_id: SourceId::new("skg"),
                name: "KG".into(),
                kind: SourceKind::Scene {
                    scene_id: SceneId::new("kg"),
                },
                settings: Default::default(),
                duplicated_from: None,
            },
            add("kg", "ikg", "cam"),
            add("kp", "ipg", "skg"),
        ])),
        one(Edit::ItemSetVisible {
            item_id: ItemId::new("ikg"),
            scene_id: Some(SceneId::new("kk")),
            visible: false,
        }),
        one(Edit::ItemSetVisible {
            item_id: ItemId::new("ikp"),
            scene_id: Some(SceneId::new("kk")),
            visible: false,
        }),
        one(crop("kk", 8)),
        read(Query::StateGet),
        // Refused: a keyframe of a keyframe, an item added to one, a placement on an item
        // its parent doesn't place, and the parent's delete while it has a keyframe.
        one(variant("kx", "kk")),
        one(add("kk", "ikx", "cam")),
        one(Edit::ItemSetVisible {
            item_id: ItemId::new("ghost"),
            scene_id: Some(SceneId::new("kk")),
            visible: false,
        }),
        delete("kp"),
        // A live value on a keyframe is the keyframe's own parameter (#846).
        live("gk", vec![crop("kk", 4)]),
        one(clear(OverrideField::Visible)),
        read(Query::StateGet),
        undo(),
        Call::Batch(edits(vec![
            clear(OverrideField::Visible),
            clear(OverrideField::Crop),
        ])),
        undo(),
        read(Query::StateGet),
        // `scene.create_variant` takes no role, so undoing the delete sets it again.
        one(Edit::SceneSetRole {
            scene_id: SceneId::new("kk"),
            role: SceneRole::Group,
        }),
        delete("kk"),
        read(Query::StateGet),
        undo(),
        read(Query::StateGet),
    ]
}

#[test]
fn the_engine_answers_like_the_mock() {
    fn normalize(event: Event) -> Event {
        match event {
            Event::StateChanged {
                rev, request_id, ..
            } => Event::StateChanged {
                rev,
                client_id: ClientId::new("client"),
                request_id,
                cause: None,
            },
            Event::LiveChanged {
                gesture_id, values, ..
            } => Event::LiveChanged {
                gesture_id,
                client_id: ClientId::new("client"),
                values,
            },
            Event::LiveEnded {
                gesture_id,
                outcome,
                rev,
                ..
            } => Event::LiveEnded {
                gesture_id,
                client_id: ClientId::new("client"),
                outcome,
                rev,
            },
            Event::StudioChanged {
                program_scene,
                preview_scene,
                transition,
                request_id,
                ..
            } => Event::StudioChanged {
                program_scene,
                preview_scene,
                transition,
                client_id: ClientId::new("client"),
                request_id,
            },
            other => other,
        }
    }
    /// Error messages may differ between implementations; codes and fields may not.
    /// Client IDs are named differently too.
    fn comparable(result: Result<CallResult, ControlError>) -> Result<CallResult, ProtocolError> {
        let result = result.map(|result| match result {
            CallResult::Query {
                result: QueryResult::LiveList { mut gestures },
            } => {
                for gesture in &mut gestures {
                    gesture.client_id = ClientId::new("client");
                }
                CallResult::Query {
                    result: QueryResult::LiveList { gestures },
                }
            }
            other => other,
        });
        result.map_err(|error| match error.protocol() {
            Some(ProtocolError::InvalidArgument { field, .. }) => {
                ProtocolError::invalid_argument(field.clone(), String::new())
            }
            Some(ProtocolError::ActionFailed { .. }) => ProtocolError::ActionFailed {
                reason: String::new(),
            },
            Some(other) => other.clone(),
            None => panic!("in-memory engines never disconnect: {error:?}"),
        })
    }
    let script = vec![
        Call::Batch(parity::collection()),
        Call::Batch(Batch::single(webcam()).with_idempotency_key(IdempotencyKey::new("k"))),
        Call::Batch(Batch::single(webcam()).with_idempotency_key(IdempotencyKey::new("k"))),
        Call::Batch(Batch::single(webcam())),
        Call::Batch(Batch::single(create("a")).expecting(Rev(1))),
        Call::Batch(Batch::single(add("a", "i9", "webcam")).expecting(Rev(2))),
        // Groups: `b` is a group placed in `a` through `i2`. Moving `i1` into it is
        // undoable and redoable, moving `i2` into it would nest `b` in itself, a role is
        // only a listing hint so `b` turns into a plain scene and back, and a missing
        // item or scene is refused.
        move_call("i1", "b"),
        Call::History(jstream_protocol::HistoryCall::new(
            jstream_protocol::HistoryOp::Undo,
        )),
        Call::History(jstream_protocol::HistoryCall::new(
            jstream_protocol::HistoryOp::Redo,
        )),
        // After the redo, `i1` is in `b`.
        Call::Query {
            query: Query::ItemList {
                scene_id: SceneId::new("b"),
            },
        },
        move_call("i2", "b"),
        move_call("ghost", "b"),
        move_call("i3", "ghost"),
        role_call("b", SceneRole::Scene),
        role_call("b", SceneRole::Group),
        role_call("ghost", SceneRole::Group),
        // The parity collection reorders `b` first, so `b` goes on program: switching to
        // `a` moves it, a second switch is a no-op, and deleting `a` below (`b` can't go
        // while `a` nests it) moves it back to `b`.
        switch_call("a"),
        switch_call("a"),
        switch_call("ghost"),
        Call::Query {
            query: Query::StudioGet,
        },
        Call::Batch(Batch::single(Edit::SourceDelete {
            source_id: SourceId::new("webcam"),
        })),
        Call::Batch(Batch::single(Edit::ItemReorder {
            item_id: ItemId::new("i9"),
            index: 99,
        })),
        Call::Batch(Batch::single(Edit::SceneDelete {
            scene_id: SceneId::new("b"),
        })),
        Call::Batch(Batch::single(Edit::ItemRemove {
            item_id: ItemId::new("i2"),
        })),
        Call::Batch(Batch::single(Edit::SceneDelete {
            scene_id: SceneId::new("a"),
        })),
        Call::Query {
            query: Query::StudioGet,
        },
        // Undoing the delete brings `a` back, and program stays on `b`.
        Call::History(jstream_protocol::HistoryCall::new(
            jstream_protocol::HistoryOp::Undo,
        )),
        Call::Query {
            query: Query::StudioGet,
        },
        output_call("yt", true),
        output_call("yt", true),
        output_call("yt", false),
        output_call("yt", false),
        output_call("nope", true),
        Call::Query {
            query: Query::StateGet,
        },
        Call::Query {
            query: Query::ItemList {
                scene_id: SceneId::new("a"),
            },
        },
        Call::Query {
            query: Query::OutputStatus {
                output_id: OutputId::new("yt"),
            },
        },
        // Live values (ADR-0032): one `live.set` per gesture, so the engine's coalescing
        // sends each at once, as the mock does.
        Call::Batch(edits(vec![create("z"), add("z", "iz", "cam")])),
        live("g1", vec![moved("iz", 5.0)]),
        live("g1", vec![create("x")]),
        live("g2", vec![moved("ghost", 1.0)]),
        live("g2", vec![]),
        Call::Query {
            query: Query::LiveList,
        },
        // `ends_gesture` is checked before `expected_rev`.
        Call::Batch(
            Batch::single(moved("iz", 6.0))
                .expecting(Rev(0))
                .ending_gesture(GestureId::new("nope")),
        ),
        Call::Batch(Batch::single(moved("iz", 6.0)).ending_gesture(GestureId::new("g1"))),
        Call::Batch(Batch::single(moved("iz", 7.0)).ending_gesture(GestureId::new("g1"))),
        live("g3", vec![moved("iz", 1.0)]),
        Call::Batch(Batch::single(Edit::ItemRemove {
            item_id: ItemId::new("iz"),
        })),
        Call::Query {
            query: Query::LiveList,
        },
        Call::Action {
            action: Action::LiveCancel {
                gesture_id: GestureId::new("g3"),
            },
        },
        Call::Action {
            action: Action::LiveCancel {
                gesture_id: GestureId::new("g9"),
            },
        },
        // No plugin is installed (ADR-0069): both answer `not_found`. `plugin.install`
        // isn't sent: the engine loads the package, which the mock can't
        // (`tests/automation.rs`).
        Call::Action {
            action: Action::PluginEnable {
                plugin_id: jstream_protocol::PluginId::new("plg_1"),
            },
        },
        Call::Action {
            action: Action::PluginGrant {
                plugin_id: jstream_protocol::PluginId::new("plg_1"),
                grant: jstream_protocol::PluginGrant::default(),
            },
        },
    ];
    let script: Vec<Call> = script
        .into_iter()
        .chain(organise_script())
        .chain(keyframe_script())
        .collect();

    let engine = parity_engine().with_output(OutputId::new("yt"));
    let real = connected(&engine);
    let mut real_events = real.events();
    let mock_engine = MockEngine::new()
        .with_kinds(parity::kinds())
        .with_output(OutputId::new("yt"));
    let mock = mock_engine.connect();
    block_on(mock.hello(hello())).expect("mock handshake");
    let mut mock_events = mock.events();

    for call in script {
        let got = comparable(block_on(real.call(call.clone())).map(|r| r.result));
        let want = comparable(block_on(mock.call(call.clone())).map(|r| r.result));
        assert_eq!(got, want, "{call:?}");
        if matches!(&call, Call::Batch(b) if b.idempotency_key == Some(IdempotencyKey::new(ORGANISED_DELETE)))
        {
            assert!(
                got.is_ok(),
                "the organised scene and source delete applies: {got:?}"
            );
        }
    }
    assert_eq!(engine.rev(), mock_engine.rev());
    let real_events: Vec<Event> = drain(&mut real_events).into_iter().map(normalize).collect();
    let mock_events: Vec<Event> = drain(&mut mock_events).into_iter().map(normalize).collect();
    assert_eq!(real_events, mock_events);
    assert!(real_events.len() >= 5);
    let programs: Vec<_> = real_events
        .iter()
        .filter_map(|event| match event {
            Event::StudioChanged { program_scene, .. } => {
                Some(program_scene.as_ref().map(SceneId::as_str))
            }
            _ => None,
        })
        .collect();
    assert_eq!(programs, [Some("b"), Some("a"), Some("b")]);
}

/// Backends without a publish target do not start a pipeline and leave the output
/// stopped. The placeholder path is the engine with no backends at all.
#[test]
fn output_start_without_a_publish_target_fails() {
    use jstream_media::Timebase;
    use jstream_platform::fake::{self, FakeClock};

    let clock = FakeClock::new(Timebase::MICROS);
    let backends = fake::backends(&clock).expect("fake backends");
    let engine = Engine::new()
        .with_output(OutputId::new("yt"))
        .with_backends(backends);
    let control = connected(&engine);
    let err = block_on(control.call(output_call("yt", true))).expect_err("start");
    assert_eq!(code(&err), Some("action_failed"));
    match query(
        &control,
        Query::OutputStatus {
            output_id: OutputId::new("yt"),
        },
    )
    .expect("status")
    {
        QueryResult::OutputStatus { state, .. } => assert_eq!(state, OutputState::Stopped),
        other => panic!("not an output status: {other:?}"),
    }
}

/// `output.start` runs capture, one composite, both encodes and the FLV publish
/// against the platform fakes. A missing adapter (software in CI) skips the test, and fails
/// it when `CI` is set.
#[test]
fn output_start_publishes_fake_audio_and_video() {
    let _gpu = one_gpu_test_at_a_time();
    use std::io::Write;
    use std::thread;
    use std::time::Duration;

    use jstream_media::{ChannelLayout, FrameSize, Timebase};
    use jstream_mux::flv::TagType;
    use jstream_platform::Backends;
    use jstream_platform::fake::{
        FakeAudioIo, FakeCapture, FakeClock, FakeCodecs, FakeGpuInterop, FakeMediaDecoders,
        FakeVideoDevices,
    };
    use std::sync::Arc;

    use crate::RecordingPublish;

    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let size = FrameSize::new(320, 180).expect("size");
    let backends = Backends {
        clock: Arc::new(clock.clone()),
        video_devices: Arc::new(FakeVideoDevices::new(clock.clone())),
        capture: Arc::new(FakeCapture::new(clock.clone(), size)),
        audio: Arc::new(audio.clone()),
        codecs: Arc::new(FakeCodecs),
        gpu: Arc::new(FakeGpuInterop),
        media: Arc::new(FakeMediaDecoders::new()),
        vcam: None,
    };
    let publish = RecordingPublish::new();
    let engine = Engine::new()
        .with_output(OutputId::new("yt"))
        .with_backends(backends)
        .with_open_timeout(GPU_OPEN_TIMEOUT)
        .with_publish(Box::new(publish.clone()));
    let control = connected(&engine);
    let mut events = control.events();
    let started = block_on(control.call(output_call("yt", true)));
    if let Err(err) = &started
        && err.protocol().is_some_and(|error| {
            matches!(error, ProtocolError::ActionFailed { reason } if reason.contains("no wgpu adapter"))
        })
    {
        if std::env::var_os("CI").is_some() {
            panic!("CI has no wgpu adapter: {err}");
        }
        let _ = writeln!(
            std::io::stderr(),
            "jstream-core: skipping pipeline test, no adapter: {err}"
        );
        return;
    }
    started.expect("output.start");
    let steps = output_steps(&mut events);
    assert_eq!(steps, vec![OutputState::Starting, OutputState::Active]);

    let mut saw_media = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        audio.deliver(2048, 0.2);
        clock.advance(50_000);
        thread::sleep(Duration::from_millis(20));
        let tags = publish.tags();
        let script = tags.iter().any(|tag| tag.tag_type == TagType::Script);
        let video = tags
            .iter()
            .any(|tag| tag.tag_type == TagType::Video && tag.body.get(1) == Some(&1));
        let audio_raw = tags
            .iter()
            .any(|tag| tag.tag_type == TagType::Audio && tag.body.get(1) == Some(&1));
        if script && video && audio_raw {
            saw_media = true;
            break;
        }
    }
    assert!(saw_media, "expected script, a video NAL and raw AAC");
    let stats = engine.output_stats().expect("stats");
    assert!(
        stats.video.received >= 1,
        "video frames accepted: {stats:?}"
    );
    assert!(
        stats.audio.received >= 1,
        "audio blocks accepted: {stats:?}"
    );
    assert!(stats.readbacks >= 1, "the CPU path read a frame back");

    block_on(control.call(output_call("yt", false))).expect("output.stop");
    let steps = output_steps(&mut events);
    assert_eq!(steps, vec![OutputState::Stopping, OutputState::Stopped]);
    assert!(engine.output_stats().is_none());
}

/// `output.stop` from a second session during `starting` cancels that start.
/// The start does not settle `active`, and nothing is left running.
#[test]
fn output_stop_during_start_cancels_the_pipeline() {
    let _gpu = one_gpu_test_at_a_time();
    use std::sync::{Condvar, Mutex};
    use std::thread;
    use std::time::Duration;

    use jstream_media::{ChannelLayout, FrameSize, Timebase};
    use jstream_platform::fake::{
        FakeAudioIo, FakeCapture, FakeClock, FakeCodecs, FakeGpuInterop, FakeMediaDecoders,
        FakeVideoDevices,
    };
    use jstream_platform::{Backends, GpuInterop, PlatformError};

    struct Hold {
        ready: Mutex<bool>,
        cv: Condvar,
    }

    impl Hold {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                ready: Mutex::new(false),
                cv: Condvar::new(),
            })
        }

        fn signal(&self) {
            *self.ready.lock().expect("hold") = true;
            self.cv.notify_all();
        }

        fn wait(&self) {
            let mut ready = self.ready.lock().expect("hold");
            while !*ready {
                ready = self.cv.wait(ready).expect("hold");
            }
        }
    }

    struct HoldGpu {
        entered: Arc<Hold>,
        release: Arc<Hold>,
    }

    /// Holds the start in the output's first platform call, opening the GPU device.
    impl GpuInterop for HoldGpu {
        fn open_device(&self) -> Result<jstream_platform::GpuContext, PlatformError> {
            self.entered.signal();
            self.release.wait();
            FakeGpuInterop.open_device()
        }

        fn encode_target(
            &self,
            context: &jstream_platform::GpuContext,
            size: FrameSize,
        ) -> Result<jstream_platform::GpuFrame, PlatformError> {
            FakeGpuInterop.encode_target(context, size)
        }
    }

    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let size = FrameSize::new(320, 180).expect("size");
    let entered = Hold::new();
    let release = Hold::new();
    let backends = Backends {
        clock: Arc::new(clock.clone()),
        video_devices: Arc::new(FakeVideoDevices::new(clock.clone())),
        capture: Arc::new(FakeCapture::new(clock, size)),
        audio: Arc::new(audio),
        codecs: Arc::new(FakeCodecs),
        gpu: Arc::new(HoldGpu {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        }),
        media: Arc::new(FakeMediaDecoders::new()),
        vcam: None,
    };
    let engine = Engine::new()
        .with_output(OutputId::new("yt"))
        .with_backends(backends)
        .with_open_timeout(GPU_OPEN_TIMEOUT)
        .with_publish(Box::new(crate::RecordingPublish::new()));
    let status = connected(&engine);
    let starter = connected(&engine);
    let stopper = connected(&engine);
    let mut events = status.events();

    let start_thread = thread::spawn(move || block_on(starter.call(output_call("yt", true))));
    entered.wait();
    let stop_thread = thread::spawn(move || block_on(stopper.call(output_call("yt", false))));
    let mut saw_stopping = false;
    for _ in 0..200 {
        if output_state(&status, "yt") == OutputState::Stopping {
            saw_stopping = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(saw_stopping, "stop did not observe the in-flight start");
    release.signal();

    let stopped = stop_thread.join().expect("stop thread");
    let started = start_thread.join().expect("start thread");
    stopped.expect("output.stop");
    let start_error = started.expect_err("start lost to stop");
    assert_eq!(code(&start_error), Some("action_failed"));
    assert_eq!(output_state(&status, "yt"), OutputState::Stopped);
    assert!(engine.output_stats().is_none());
    let steps = output_steps(&mut events);
    assert_eq!(
        steps,
        vec![
            OutputState::Starting,
            OutputState::Stopping,
            OutputState::Stopped
        ]
    );
}

/// A publish failure after `active` settles `failed` and drops the pipeline.
#[test]
fn output_publish_failure_settles_failed() {
    let _gpu = one_gpu_test_at_a_time();
    use std::io::Write;
    use std::sync::atomic::AtomicBool;
    use std::thread;
    use std::time::Duration;

    use jstream_media::{ChannelLayout, FrameSize, Timebase};
    use jstream_mux::flv::FlvTag;
    use jstream_platform::Backends;
    use jstream_platform::fake::{
        FakeAudioIo, FakeCapture, FakeClock, FakeCodecs, FakeGpuInterop, FakeMediaDecoders,
        FakeVideoDevices,
    };

    use crate::{LivePublish, PublishError};

    struct FailPublish {
        fail: Arc<AtomicBool>,
    }

    impl LivePublish for FailPublish {
        fn send_tag(&mut self, _tag: &FlvTag) -> Result<(), PublishError> {
            if self.fail.load(Ordering::Relaxed) {
                Err(PublishError::new("publish broken"))
            } else {
                Ok(())
            }
        }

        fn poll(&mut self) -> Result<(), PublishError> {
            if self.fail.load(Ordering::Relaxed) {
                Err(PublishError::new("publish broken"))
            } else {
                Ok(())
            }
        }

        fn finish(&mut self) -> Result<(), PublishError> {
            Ok(())
        }
    }

    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let size = FrameSize::new(320, 180).expect("size");
    let fail = Arc::new(AtomicBool::new(false));
    let backends = Backends {
        clock: Arc::new(clock.clone()),
        video_devices: Arc::new(FakeVideoDevices::new(clock.clone())),
        capture: Arc::new(FakeCapture::new(clock, size)),
        audio: Arc::new(audio),
        codecs: Arc::new(FakeCodecs),
        gpu: Arc::new(FakeGpuInterop),
        media: Arc::new(FakeMediaDecoders::new()),
        vcam: None,
    };
    let engine = Engine::new()
        .with_output(OutputId::new("yt"))
        .with_backends(backends)
        .with_open_timeout(GPU_OPEN_TIMEOUT)
        .with_publish(Box::new(FailPublish {
            fail: Arc::clone(&fail),
        }));
    let control = connected(&engine);
    let mut events = control.events();
    let started = block_on(control.call(output_call("yt", true)));
    if let Err(err) = &started
        && err.protocol().is_some_and(|error| {
            matches!(error, ProtocolError::ActionFailed { reason } if reason.contains("no wgpu adapter"))
        })
    {
        if std::env::var_os("CI").is_some() {
            panic!("CI has no wgpu adapter: {err}");
        }
        let _ = writeln!(
            std::io::stderr(),
            "jstream-core: skipping publish-failure test, no adapter: {err}"
        );
        return;
    }
    started.expect("output.start");
    assert_eq!(
        output_steps(&mut events),
        vec![OutputState::Starting, OutputState::Active]
    );

    fail.store(true, Ordering::Relaxed);
    let mut failed = false;
    for _ in 0..100 {
        if output_state(&control, "yt") == OutputState::Failed && engine.output_stats().is_none() {
            failed = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(failed, "publish failure left the output running");
    assert_eq!(output_steps(&mut events), vec![OutputState::Failed]);
}

/// A publish failure whose teardown panics still lets the next `output.start` return.
///
/// The panic is an audio stream's drop on the publish watcher. Encoder `finish`
/// is caught inside the encode thread and comes back as an error, so it does not
/// unwind this join. Without the drain guard the second start waits forever; the
/// timeout fails that case instead of hanging the suite.
#[test]
fn output_start_returns_after_a_panicking_teardown() {
    let _gpu = one_gpu_test_at_a_time();
    use std::io::Write;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use jstream_media::{ChannelLayout, FrameSize, Timebase};
    use jstream_mux::flv::FlvTag;
    use jstream_platform::fake::{
        FakeAudioIo, FakeCapture, FakeClock, FakeCodecs, FakeGpuInterop, FakeMediaDecoders,
        FakeVideoDevices,
    };
    use jstream_platform::{
        AudioInputHandler, AudioInputTarget, AudioIo, AudioOutputHandler, AudioOutputTarget,
        AudioStream, Backends, PlatformError,
    };

    use crate::{LivePublish, PublishError, RecordingPublish};

    struct FailPublish {
        fail: Arc<AtomicBool>,
    }

    impl LivePublish for FailPublish {
        fn send_tag(&mut self, _tag: &FlvTag) -> Result<(), PublishError> {
            if self.fail.load(Ordering::Relaxed) {
                Err(PublishError::new("publish broken"))
            } else {
                Ok(())
            }
        }

        fn poll(&mut self) -> Result<(), PublishError> {
            if self.fail.load(Ordering::Relaxed) {
                Err(PublishError::new("publish broken"))
            } else {
                Ok(())
            }
        }

        fn finish(&mut self) -> Result<(), PublishError> {
            Ok(())
        }
    }

    /// One drop panics, and only when this thread is not already unwinding.
    struct PanicStream {
        inner: Box<dyn AudioStream>,
        armed: Arc<AtomicBool>,
    }

    impl AudioStream for PanicStream {
        fn format(&self) -> jstream_media::AudioFormat {
            self.inner.format()
        }
    }

    impl Drop for PanicStream {
        fn drop(&mut self) {
            if !thread::panicking() && self.armed.swap(false, Ordering::AcqRel) {
                panic!("audio stream teardown");
            }
        }
    }

    struct PanicAudio {
        inner: FakeAudioIo,
        armed: Arc<AtomicBool>,
    }

    impl AudioIo for PanicAudio {
        fn devices(&self) -> Result<Vec<jstream_platform::AudioDeviceInfo>, PlatformError> {
            self.inner.devices()
        }

        fn processes(&self) -> Result<Vec<jstream_platform::AudioProcess>, PlatformError> {
            self.inner.processes()
        }

        fn input_format(
            &self,
            target: &AudioInputTarget,
        ) -> Result<jstream_media::AudioFormat, PlatformError> {
            self.inner.input_format(target)
        }

        fn open_input(
            &self,
            target: &AudioInputTarget,
            handler: Box<dyn AudioInputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            let inner = self.inner.open_input(target, handler)?;
            Ok(Box::new(PanicStream {
                inner,
                armed: Arc::clone(&self.armed),
            }))
        }

        fn open_output(
            &self,
            target: &AudioOutputTarget,
            handler: Box<dyn AudioOutputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            self.inner.open_output(target, handler)
        }
    }

    let clock = FakeClock::new(Timebase::MICROS);
    // Armed only once the first start has succeeded: without an adapter that start
    // fails, drops the streams, and the test must skip, not panic there.
    let armed = Arc::new(AtomicBool::new(false));
    let audio = PanicAudio {
        inner: FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio"),
        armed: Arc::clone(&armed),
    };
    let size = FrameSize::new(320, 180).expect("size");
    let fail = Arc::new(AtomicBool::new(false));
    let backends = Backends {
        clock: Arc::new(clock.clone()),
        video_devices: Arc::new(FakeVideoDevices::new(clock.clone())),
        capture: Arc::new(FakeCapture::new(clock, size)),
        audio: Arc::new(audio),
        codecs: Arc::new(FakeCodecs),
        gpu: Arc::new(FakeGpuInterop),
        media: Arc::new(FakeMediaDecoders::new()),
        vcam: None,
    };
    let engine = Engine::new()
        .with_output(OutputId::new("yt"))
        .with_backends(backends)
        .with_open_timeout(GPU_OPEN_TIMEOUT)
        .with_publish(Box::new(FailPublish {
            fail: Arc::clone(&fail),
        }));
    let control = connected(&engine);
    let started = block_on(control.call(output_call("yt", true)));
    if let Err(err) = &started
        && err.protocol().is_some_and(|error| {
            matches!(error, ProtocolError::ActionFailed { reason } if reason.contains("no wgpu adapter"))
        })
    {
        if std::env::var_os("CI").is_some() {
            panic!("CI has no wgpu adapter: {err}");
        }
        let _ = writeln!(
            std::io::stderr(),
            "jstream-core: skipping panicking-teardown test, no adapter: {err}"
        );
        return;
    }
    started.expect("output.start");
    armed.store(true, Ordering::Release);

    fail.store(true, Ordering::Relaxed);
    let mut failed = false;
    for _ in 0..100 {
        if output_state(&control, "yt") == OutputState::Failed && engine.output_stats().is_none() {
            failed = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(failed, "publish failure left the output running");
    // `failed` settles before the watcher drops the pipeline, and a start that comes
    // while the program stage is still alive shares it instead of waiting, so the
    // restart would never see the panicking teardown (the first macOS run, #520).
    // Wait for the stream's drop to fire; the start below then races the unwinding
    // teardown's `draining`, which is what this test is about.
    let mut dropped = false;
    for _ in 0..500 {
        if !armed.load(Ordering::Acquire) {
            dropped = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(dropped, "the first pipeline never dropped its audio stream");

    let engine = engine.with_publish(Box::new(RecordingPublish::new()));
    let restart = connected(&engine);
    let (tx, rx) = mpsc::channel();
    let restart_thread = thread::spawn(move || {
        let result = block_on(restart.call(output_call("yt", true)));
        let _ = tx.send(());
        result
    });
    rx.recv_timeout(GPU_OPEN_TIMEOUT + Duration::from_secs(10))
        .expect("second output.start blocked after a panicking teardown");
    let restarted = restart_thread.join().expect("restart thread");
    restarted.expect("second output.start");
    assert_eq!(output_state(&control, "yt"), OutputState::Active);
}

/// Dropping the last engine handle stops a running pipeline. The publish watcher
/// must not keep that handle alive.
#[test]
fn dropping_the_engine_stops_the_pipeline() {
    let _gpu = one_gpu_test_at_a_time();
    use std::io::Write;
    use std::sync::atomic::AtomicBool;

    use jstream_media::{ChannelLayout, FrameSize, Timebase};
    use jstream_mux::flv::FlvTag;
    use jstream_platform::Backends;
    use jstream_platform::fake::{
        FakeAudioIo, FakeCapture, FakeClock, FakeCodecs, FakeGpuInterop, FakeMediaDecoders,
        FakeVideoDevices,
    };

    use crate::{LivePublish, PublishError};

    struct FinishFlag {
        finished: Arc<AtomicBool>,
    }

    impl LivePublish for FinishFlag {
        fn send_tag(&mut self, _tag: &FlvTag) -> Result<(), PublishError> {
            Ok(())
        }

        fn poll(&mut self) -> Result<(), PublishError> {
            Ok(())
        }

        fn finish(&mut self) -> Result<(), PublishError> {
            self.finished.store(true, Ordering::Release);
            Ok(())
        }
    }

    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let size = FrameSize::new(320, 180).expect("size");
    let finished = Arc::new(AtomicBool::new(false));
    let backends = Backends {
        clock: Arc::new(clock.clone()),
        video_devices: Arc::new(FakeVideoDevices::new(clock.clone())),
        capture: Arc::new(FakeCapture::new(clock, size)),
        audio: Arc::new(audio),
        codecs: Arc::new(FakeCodecs),
        gpu: Arc::new(FakeGpuInterop),
        media: Arc::new(FakeMediaDecoders::new()),
        vcam: None,
    };
    let engine = Engine::new()
        .with_output(OutputId::new("yt"))
        .with_backends(backends)
        .with_open_timeout(GPU_OPEN_TIMEOUT)
        .with_publish(Box::new(FinishFlag {
            finished: Arc::clone(&finished),
        }));
    let control = connected(&engine);
    let started = block_on(control.call(output_call("yt", true)));
    if let Err(err) = &started
        && err.protocol().is_some_and(|error| {
            matches!(error, ProtocolError::ActionFailed { reason } if reason.contains("no wgpu adapter"))
        })
    {
        if std::env::var_os("CI").is_some() {
            panic!("CI has no wgpu adapter: {err}");
        }
        let _ = writeln!(
            std::io::stderr(),
            "jstream-core: skipping engine-drop test, no adapter: {err}"
        );
        return;
    }
    started.expect("output.start");
    assert!(engine.output_stats().is_some());
    drop(control);
    drop(engine);
    assert!(
        finished.load(Ordering::Acquire),
        "dropping the engine left the pipeline running"
    );
}

fn output_state(control: &dyn Control, output: &str) -> OutputState {
    match query(
        control,
        Query::OutputStatus {
            output_id: OutputId::new(output),
        },
    )
    .expect("status")
    {
        QueryResult::OutputStatus { state, .. } => state,
        other => panic!("not an output status: {other:?}"),
    }
}

/// Edits and stored frames republish the render snapshot only when the program mix
/// changes. A `display` source shows what the pipeline captures for it until a frame is
/// stored for it, and a frame for a source nothing on program shows publishes nothing.
#[test]
fn the_render_snapshot_follows_the_program_mix() {
    let engine = Engine::new();
    let hub = engine.render_hub();
    let control = connected(&engine);
    let generation = || hub.load().generation();
    assert_eq!(generation(), 1);
    let display = Edit::SourceCreate {
        source_id: SourceId::new("screen"),
        name: "Screen".into(),
        kind: SourceKind::Builtin {
            kind: "display".into(),
        },
        settings: Default::default(),
        duplicated_from: None,
    };
    let edits = Batch {
        edits: vec![create("main"), display, add("main", "shown", "screen")],
        expected_rev: None,
        idempotency_key: None,
        ends_gesture: None,
    };
    batch(&control, edits).expect("batch");
    assert!(engine.settle_sources(), "the display source was built");
    // The scene, then the display source's build.
    assert_eq!(generation(), 3);
    let primary = std::collections::BTreeSet::from([crate::snapshot::CaptureKey::for_source(
        jstream_sources::CaptureSpec::Display(None),
        SourceId::new("screen"),
    )]);
    assert_eq!(
        hub.load().shot().captures(),
        &primary,
        "no stored frame: the primary display's capture"
    );

    engine
        .set_source_frame(&SourceId::new("elsewhere"), 1, 1, &[0, 0, 0, 255])
        .expect("frame");
    assert_eq!(generation(), 3, "nothing on program shows that source");

    engine
        .set_source_frame(&SourceId::new("screen"), 1, 1, &[0, 0, 0, 255])
        .expect("frame");
    assert_eq!(generation(), 4);
    assert!(
        hub.load().shot().captures().is_empty(),
        "a stored frame wins"
    );
    engine
        .set_source_frame(&SourceId::new("screen"), 1, 1, &[9, 9, 9, 255])
        .expect("frame");
    assert_eq!(generation(), 5, "a new frame is a new version");
}

/// The render thread's program mix follows program: a switch republishes it with the
/// new scene, a switch to the scene already on program publishes nothing, and deleting
/// the program scene moves the mix to the first scene left (ADR-0030).
#[test]
fn the_render_snapshot_follows_scene_switch() {
    let engine = Engine::new();
    let hub = engine.render_hub();
    let control = connected(&engine);
    let program = || hub.load().shot().program().map(|id| id.as_str().to_owned());
    batch(&control, edits(vec![create("a"), create("b")])).expect("scenes");
    assert_eq!(program().as_deref(), Some("a"));
    let generation = hub.load().generation();

    block_on(control.call(switch_call("b"))).expect("switch");
    assert_eq!(program().as_deref(), Some("b"));
    assert_eq!(hub.load().generation(), generation + 1);
    block_on(control.call(switch_call("b"))).expect("same scene");
    assert_eq!(
        hub.load().generation(),
        generation + 1,
        "nothing new to draw"
    );

    batch(
        &control,
        Batch::single(Edit::SceneDelete {
            scene_id: SceneId::new("b"),
        }),
    )
    .expect("delete the program scene");
    assert_eq!(program().as_deref(), Some("a"));
}

/// How long a GPU test's `output.start` waits for lavapipe or WARP to open. The
/// engine's 10 s default only guards against a stuck adapter; on a loaded CI runner a
/// software adapter can take longer, and that is not what these tests check. A real
/// failure to open still fails the test, just later.
const GPU_OPEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// The warnings logged while `run` runs on this thread, and on the pipeline threads it
/// starts, each as its message and fields.
pub(crate) fn warnings<T>(run: impl FnOnce() -> T) -> (T, Vec<String>) {
    warnings_with(|_| run())
}

/// [`warnings`], with `run` given what has been logged so far, to wait for a warning a
/// pipeline thread logs.
pub(crate) fn warnings_with<T>(
    run: impl FnOnce(&dyn Fn() -> Vec<String>) -> T,
) -> (T, Vec<String>) {
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Level, Metadata, Subscriber};

    #[derive(Clone, Default)]
    struct Warnings(Arc<std::sync::Mutex<Vec<String>>>);

    struct Text<'a>(&'a mut String);

    impl Visit for Text<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.push_str(&format!("{}={value:?} ", field.name()));
        }
    }

    impl Subscriber for Warnings {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            *metadata.level() == Level::WARN
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut text = String::new();
            event.record(&mut Text(&mut text));
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(text);
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    let logged = Warnings::default();
    let read = || {
        logged
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    };
    let out = tracing::subscriber::with_default(logged.clone(), || run(&read));
    (out, read())
}

/// One GPU test at a time, on whatever adapter opens (software in CI). Parallel compositors starve the pipeline
/// test's media wait and crash WARP. The lock schedules tests; it is not engine state.
pub(crate) fn one_gpu_test_at_a_time() -> MutexGuard<'static, ()> {
    static SLOT: Mutex<()> = Mutex::new(());
    SLOT.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The output states among the buffered events. `stats.tick`, `meters.tick` and
/// `source.size_changed` are skipped: the pipeline emits the ticks while it runs, and a
/// final `stats.tick` when it stops, and a capture source's size as its capture's
/// frames come and go, at any point of a start or stop.
fn output_steps(events: &mut EventStream) -> Vec<OutputState> {
    drain(events)
        .into_iter()
        .filter_map(|event| match event {
            Event::OutputStateChanged { state, .. } => Some(state),
            Event::StatsTick { .. } | Event::MetersTick { .. } => None,
            Event::SourceSizeChanged { .. } => None,
            other => panic!("unexpected event {other:?}"),
        })
        .collect()
}

/// `video.snapshot` composites the first scene on lavapipe (or WARP) and returns a PNG
/// whose longest side is 1280. With no adapter the test returns; in CI that is a failure.
#[test]
fn video_snapshot_reads_the_program_mix_back_as_png() {
    let _gpu = one_gpu_test_at_a_time();
    use crate::snapshot::{decode_base64, decode_png, is_no_adapter};
    use std::io::Write;

    fn solid(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity((width as usize) * (height as usize) * 4);
        for _ in 0..width * height {
            bytes.extend_from_slice(&pixel);
        }
        bytes
    }

    fn skip(result: Result<QueryResult, ControlError>) -> Option<QueryResult> {
        match result {
            Ok(result) => Some(result),
            Err(error) if error.protocol().is_some_and(is_no_adapter) => {
                if std::env::var_os("CI").is_some() {
                    panic!("CI has no wgpu adapter: {error}");
                }
                let _ = writeln!(
                    std::io::stderr(),
                    "jstream-core: skipping video.snapshot GPU test, no adapter: {error}"
                );
                None
            }
            Err(error) => panic!("video.snapshot failed: {error}"),
        }
    }

    fn image(result: &QueryResult) -> (Rev, u32, Vec<u8>) {
        let QueryResult::VideoSnapshot {
            rev,
            width,
            height,
            png,
        } = result
        else {
            panic!("not a snapshot: {result:?}");
        };
        let bytes = decode_base64(png).expect("png is base64");
        let (w, h, rgba) = decode_png(&bytes).expect("png decodes");
        assert_eq!((*width, *height), (w, h));
        assert_eq!(
            (w, h),
            (1280, 720),
            "the canvas is capped at 1280 on the long side"
        );
        (*rev, *width, rgba)
    }

    fn at(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * width as usize + x as usize) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    let engine = Engine::new();
    let control = connected(&engine);
    let mut events = control.events();
    let Some(empty) = skip(query(&control, Query::VideoSnapshot)) else {
        return;
    };
    let (rev, width, rgba) = image(&empty);
    assert_eq!(rev, Rev(0));
    assert_eq!(engine.rev(), Rev(0));
    assert!(drain(&mut events).is_empty());
    // A clear has no quad edge, so the corners are the background too.
    assert_eq!(at(&rgba, width, 0, 0), [0, 0, 0, 255]);
    assert_eq!(at(&rgba, width, 1279, 719), [0, 0, 0, 255]);

    let built = batch(
        &control,
        Batch {
            edits: vec![create("main"), webcam(), add("main", "layer", "webcam")],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        },
    )
    .expect("scene");
    assert_eq!(built, Rev(1));
    assert!(matches!(
        drain(&mut events).as_slice(),
        [
            Event::StateChanged { rev: Rev(1), .. },
            Event::StudioChanged { .. }
        ]
    ));
    engine
        .set_source_frame(
            &SourceId::new("webcam"),
            1920,
            1080,
            &solid(1920, 1080, [255, 0, 0, 255]),
        )
        .expect("frame");
    let shot = skip(query(&control, Query::VideoSnapshot)).expect("adapter already opened");
    let (rev, width, rgba) = image(&shot);
    assert_eq!(rev, Rev(1));
    assert_eq!(engine.rev(), Rev(1));
    // Inset samples stay off the quad edge, where a linear filter can mix in the clear.
    assert_eq!(at(&rgba, width, 100, 100), [255, 0, 0, 255]);
    assert_eq!(at(&rgba, width, 640, 360), [255, 0, 0, 255]);
    // The stored frame gave the webcam its size; nothing else was sent.
    assert_eq!(
        drain(&mut events),
        vec![Event::SourceSizeChanged {
            source_id: SourceId::new("webcam"),
            size: Some(PixelSize {
                width: 1920,
                height: 1080,
            }),
        }]
    );

    batch(
        &control,
        Batch::single(Edit::ItemSetVisible {
            item_id: ItemId::new("layer"),
            scene_id: None,
            visible: false,
        }),
    )
    .expect("hide");
    assert!(matches!(
        drain(&mut events).as_slice(),
        [Event::StateChanged { rev: Rev(2), .. }]
    ));
    let hidden = skip(query(&control, Query::VideoSnapshot)).expect("adapter already opened");
    let (rev, width, rgba) = image(&hidden);
    assert_eq!(rev, Rev(2));
    assert_eq!(at(&rgba, width, 640, 360), [0, 0, 0, 255]);

    // The snapshot follows program: a second scene shows only once it is switched to.
    batch(
        &control,
        Batch {
            edits: vec![
                create("other"),
                Edit::SourceCreate {
                    source_id: SourceId::new("green"),
                    name: "Green".into(),
                    kind: SourceKind::Builtin {
                        kind: "webcam".into(),
                    },
                    settings: Default::default(),
                    duplicated_from: None,
                },
                add("other", "fill", "green"),
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        },
    )
    .expect("second scene");
    engine
        .set_source_frame(
            &SourceId::new("green"),
            1920,
            1080,
            &solid(1920, 1080, [0, 255, 0, 255]),
        )
        .expect("frame");
    let before = skip(query(&control, Query::VideoSnapshot)).expect("adapter already opened");
    let (_, width, rgba) = image(&before);
    assert_eq!(
        at(&rgba, width, 640, 360),
        [0, 0, 0, 255],
        "main is still on"
    );
    block_on(control.call(switch_call("other"))).expect("switch");
    let after = skip(query(&control, Query::VideoSnapshot)).expect("adapter already opened");
    let (rev, width, rgba) = image(&after);
    assert_eq!(rev, Rev(3), "a switch is not an edit");
    assert_eq!(at(&rgba, width, 640, 360), [0, 255, 0, 255]);
    drain(&mut events);

    // A scene source is the inner scene's composite, placed in the program scene.
    let nested = Engine::new();
    let control = connected(&nested);
    batch(
        &control,
        Batch {
            edits: vec![
                create("program"),
                create("inner"),
                webcam(),
                add("inner", "fill", "webcam"),
                Edit::SourceCreate {
                    source_id: SourceId::new("inner_src"),
                    name: "Inner".into(),
                    kind: SourceKind::Scene {
                        scene_id: SceneId::new("inner"),
                    },
                    settings: Default::default(),
                    duplicated_from: None,
                },
                add("program", "nest", "inner_src"),
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        },
    )
    .expect("nested scenes");
    nested
        .set_source_frame(
            &SourceId::new("webcam"),
            1920,
            1080,
            &solid(1920, 1080, [0, 255, 0, 255]),
        )
        .expect("frame");
    let shot = skip(query(&control, Query::VideoSnapshot)).expect("adapter already opened");
    let (_, width, rgba) = image(&shot);
    assert_eq!(at(&rgba, width, 100, 100), [0, 255, 0, 255]);
    assert_eq!(at(&rgba, width, 1100, 600), [0, 255, 0, 255]);
}

/// `video.snapshot` draws at the collection's canvas size and places a geometry item by
/// the shared formula: a 16:9 red frame fitted into a 320×360 box on a 640×360 canvas is
/// letterboxed, and nothing is drawn outside the box.
#[test]
fn video_snapshot_draws_a_geometry_on_the_collection_canvas() {
    let _gpu = one_gpu_test_at_a_time();
    use crate::snapshot::{decode_base64, decode_png, is_no_adapter};
    use jstream_protocol::{FitMode, ItemGeometry, Size, Vec2};

    let engine = Engine::new();
    let control = connected(&engine);
    let geometry = ItemGeometry {
        fit_mode: FitMode::Fit,
        ..ItemGeometry::try_new(
            Vec2::new(160.0, 180.0).expect("finite"),
            Size::try_new(320.0, 360.0).expect("positive"),
        )
        .expect("positive")
    };
    batch(
        &control,
        Batch {
            edits: vec![
                Edit::CanvasSetSize {
                    width: 640,
                    height: 360,
                },
                create("main"),
                webcam(),
                Edit::ItemAdd {
                    scene_id: SceneId::new("main"),
                    item_id: ItemId::new("boxed"),
                    source_id: SourceId::new("webcam"),
                    transform: None,
                    geometry: Some(geometry),
                    opacity: None,
                    blend: None,
                },
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        },
    )
    .expect("scene");
    let red: Vec<u8> = std::iter::repeat_n([255, 0, 0, 255], 1920 * 1080)
        .flatten()
        .collect();
    engine
        .set_source_frame(&SourceId::new("webcam"), 1920, 1080, &red)
        .expect("frame");
    let shot = match query(&control, Query::VideoSnapshot) {
        Ok(shot) => shot,
        Err(error) if error.protocol().is_some_and(is_no_adapter) => {
            assert!(
                std::env::var_os("CI").is_none(),
                "CI has no wgpu adapter: {error}"
            );
            return;
        }
        Err(error) => panic!("video.snapshot failed: {error}"),
    };
    let QueryResult::VideoSnapshot {
        width, height, png, ..
    } = shot
    else {
        panic!("not a snapshot: {shot:?}");
    };
    assert_eq!((width, height), (640, 360), "the canvas, not 1920×1080");
    let (_, _, rgba) = decode_png(&decode_base64(&png).expect("base64")).expect("png");
    let at = |x: usize, y: usize| {
        let i = (y * 640 + x) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    };
    // The fitted frame is 320×180, centred in the box: rows 90 to 270.
    assert_eq!(at(160, 180), [255, 0, 0, 255]);
    assert_eq!(at(10, 100), [255, 0, 0, 255]);
    // Letterbox above and below it, and the rest of the canvas, stay clear.
    assert_eq!(at(160, 40), [0, 0, 0, 255]);
    assert_eq!(at(160, 320), [0, 0, 0, 255]);
    assert_eq!(at(480, 180), [0, 0, 0, 255]);
}

/// A geometry's `flip_h` reaches the compositor through the snapshot: a red|green frame
/// stretched over the whole 64×32 canvas and flipped draws green|red.
#[test]
fn video_snapshot_draws_a_flipped_geometry() {
    let _gpu = one_gpu_test_at_a_time();
    use crate::snapshot::{decode_base64, decode_png, is_no_adapter};
    use jstream_protocol::{FitMode, ItemGeometry};

    let engine = Engine::new();
    let control = connected(&engine);
    let geometry = ItemGeometry {
        fit_mode: FitMode::Stretch,
        flip_h: true,
        ..ItemGeometry::covering_canvas(64, 32).expect("positive")
    };
    batch(
        &control,
        Batch {
            edits: vec![
                Edit::CanvasSetSize {
                    width: 64,
                    height: 32,
                },
                create("main"),
                webcam(),
                Edit::ItemAdd {
                    scene_id: SceneId::new("main"),
                    item_id: ItemId::new("mirrored"),
                    source_id: SourceId::new("webcam"),
                    transform: None,
                    geometry: Some(geometry),
                    opacity: None,
                    blend: None,
                },
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        },
    )
    .expect("scene");
    let frame: Vec<u8> = (0..16 * 8)
        .flat_map(|i| {
            if i % 16 < 8 {
                [255, 0, 0, 255]
            } else {
                [0, 255, 0, 255]
            }
        })
        .collect();
    engine
        .set_source_frame(&SourceId::new("webcam"), 16, 8, &frame)
        .expect("frame");
    let shot = match query(&control, Query::VideoSnapshot) {
        Ok(shot) => shot,
        Err(error) if error.protocol().is_some_and(is_no_adapter) => {
            assert!(
                std::env::var_os("CI").is_none(),
                "CI has no wgpu adapter: {error}"
            );
            return;
        }
        Err(error) => panic!("video.snapshot failed: {error}"),
    };
    let QueryResult::VideoSnapshot {
        width, height, png, ..
    } = shot
    else {
        panic!("not a snapshot: {shot:?}");
    };
    assert_eq!((width, height), (64, 32));
    let (_, _, rgba) = decode_png(&decode_base64(&png).expect("base64")).expect("png");
    let at = |x: usize, y: usize| {
        let i = (y * 64 + x) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    };
    assert_eq!(at(8, 16), [0, 255, 0, 255]);
    assert_eq!(at(56, 16), [255, 0, 0, 255]);
}

#[test]
fn a_removed_drop_source_keeps_its_total() {
    let engine = Engine::new();
    let control = connected(&engine);
    let mut events = control.events();
    let frames = Arc::new(AtomicU64::new(3));
    let id = engine.add_drop_source(FnDropSource::new({
        let frames = Arc::clone(&frames);
        move || DropCounts {
            capture_frames: frames.load(Ordering::Relaxed),
            ..DropCounts::default()
        }
    }));
    let cell = DropSnapshot::new();
    cell.publish(DropCounts {
        render_ticks: 2,
        ..DropCounts::default()
    });
    engine.add_drop_source(cell.clone());

    engine.emit_stats_tick();
    assert_eq!(
        unloaded(drain(&mut events)),
        vec![Event::StatsTick {
            seq: 1,
            drops: DropCounts {
                capture_frames: 3,
                render_ticks: 2,
                ..DropCounts::default()
            },
            load: None,
            chains: Some(ChainCost::default()),
            outputs: Vec::new(),
        }]
    );

    frames.store(5, Ordering::Relaxed);
    engine.remove_drop_source(id);
    frames.store(9, Ordering::Relaxed);
    cell.publish(DropCounts {
        render_ticks: 4,
        ..DropCounts::default()
    });
    engine.emit_stats_tick();
    assert_eq!(
        unloaded(drain(&mut events)),
        vec![Event::StatsTick {
            seq: 2,
            drops: DropCounts {
                capture_frames: 5,
                render_ticks: 4,
                ..DropCounts::default()
            },
            load: None,
            chains: Some(ChainCost::default()),
            outputs: Vec::new(),
        }],
        "the removed source stays at the total it had when it was removed"
    );

    engine.remove_drop_source(id);
    engine.emit_stats_tick();
    assert_eq!(
        unloaded(drain(&mut events)),
        vec![Event::StatsTick {
            seq: 3,
            drops: DropCounts {
                capture_frames: 5,
                render_ticks: 4,
                ..DropCounts::default()
            },
            load: None,
            chains: Some(ChainCost::default()),
            outputs: Vec::new(),
        }],
        "removing an unknown source changes nothing"
    );
}

/// Each `stats.tick` carries the engine's load: the host's CPU reading in whole
/// milliseconds, no program frames while nothing runs, and a clock that never goes back.
#[test]
fn a_stats_tick_carries_the_engines_load() {
    let engine = Engine::new();
    let control = connected(&engine);
    let mut events = control.events();
    let load = |events: Vec<Event>| match events.as_slice() {
        [Event::StatsTick { load, .. }] => load.expect("a load"),
        other => panic!("not one stats.tick: {other:?}"),
    };

    engine.emit_stats_tick();
    let first = load(drain(&mut events));
    assert_eq!(first.cpu_ms, None, "no CPU reader yet");
    assert_eq!(first.program_frames, 0);
    assert!(first.cpus >= 1);

    let cpu = jstream_platform::fake::FakeCpu::new();
    engine.set_process_cpu(Arc::new(cpu.clone()));
    cpu.set(Some(std::time::Duration::from_micros(250_700)));
    engine.emit_stats_tick();
    let second = load(drain(&mut events));
    assert_eq!(second.cpu_ms, Some(250));
    assert!(second.elapsed_ms >= first.elapsed_ms);
}

/// The heartbeat a host starts emits `stats.tick` while no output runs, and a second
/// start adds no second heartbeat.
#[test]
fn the_heartbeat_ticks_while_the_engine_is_idle() {
    let engine = Engine::new();
    let control = connected(&engine);
    let mut events = control.events();
    engine.start_stats_heartbeat();
    engine.start_stats_heartbeat();
    let mut seqs = Vec::new();
    // About a second apart, so two arrive well within the bound.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while seqs.len() < 2 && std::time::Instant::now() < deadline {
        seqs.extend(
            drain(&mut events)
                .into_iter()
                .filter_map(|event| match event {
                    Event::StatsTick { seq, load, .. } => load.map(|_| seq),
                    _ => None,
                }),
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(seqs.len() >= 2, "no heartbeat ticks: {seqs:?}");
    assert_eq!(
        seqs[..2],
        [1, 2],
        "one heartbeat, numbering its ticks in order"
    );
}

/// Deleting a source drops the frame stored for it, so `Engine::frames` does not grow
/// with every source a collection ever had. A frame for a source not created yet stays.
#[test]
fn deleting_a_source_drops_its_frame() {
    let engine = Engine::new();
    let control = connected(&engine);
    let webcam_id = SourceId::new("webcam");
    let later = SourceId::new("later");
    engine
        .set_source_frame(&webcam_id, 1, 1, &[1, 2, 3, 4])
        .expect("frame");
    engine
        .set_source_frame(&later, 1, 1, &[1, 2, 3, 4])
        .expect("frame");
    batch(&control, Batch::single(webcam())).expect("create");
    assert!(engine.has_source_frame(&webcam_id));
    batch(
        &control,
        Batch::single(Edit::SourceDelete {
            source_id: webcam_id.clone(),
        }),
    )
    .expect("delete");
    assert!(
        !engine.has_source_frame(&webcam_id),
        "the frame outlived its source"
    );
    assert!(
        engine.has_source_frame(&later),
        "a frame may arrive before its source"
    );
}

// ---------------------------------------------------------------------------------------
// The pipeline on a CPU stand-in for the compositor. These run without a wgpu adapter:
// the render thread draws with `cpu_draw` (or a variant) instead of wgpu, and every
// other stage is the platform fakes, so they cover the threads' failure handling on
// any machine. The GPU path itself is covered by the tests above.

mod cpu_pipeline {
    mod captures;
    mod hardware_sessions;

    use super::{
        batch, block_on, code, connected, drain, edits, output_call, output_state, output_steps,
        query,
    };
    use crate::InProcessControl;
    use crate::runtime::{DrawFn, OpenDraw, cpu_draw};
    use crate::{Engine, LinkState, LivePublish, PublishError, RecordingPublish};
    use jstream_control::{Control, EventStream};
    use jstream_media::{ChannelLayout, FrameSize, MediaTime, Packet, PixelFormat, Timebase};
    use jstream_mux::flv::{FlvTag, TagType};
    use jstream_platform::fake::{
        FakeAudioIo, FakeCapture, FakeClock, FakeCodecs, FakeGpuInterop, FakeMediaDecoders,
        FakeVideoDevices,
    };
    use jstream_platform::{
        AudioDeviceInfo, AudioEncoder, AudioEncoderConfig, AudioInputHandler, AudioInputTarget,
        AudioIo, AudioOutputHandler, AudioOutputTarget, AudioStream, Backends, Capture,
        CaptureStats, CaptureStream, CaptureTarget, CaptureTargetInfo, CodecInfo, Codecs,
        MediaClock, PlatformError, VcamFormat, VcamSink, VideoDecoder, VideoDecoderConfig,
        VideoEncoder, VideoEncoderConfig, VideoSurface,
    };
    use jstream_protocol::{
        Edit, EncoderProfile, EncoderProfileId, EncoderSessionInfo, Event, OutputId, OutputKind,
        OutputState, Query, QueryResult, RateControl, RecordContainer,
    };
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Condvar, Mutex, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    /// One base-rate period of the pipeline on a microsecond clock (1/30 s).
    const PERIOD_US: u64 = 33_333;

    struct Fakes {
        clock: FakeClock,
        audio: FakeAudioIo,
        backends: Backends,
    }

    fn fakes() -> Fakes {
        let clock = FakeClock::new(Timebase::MICROS);
        let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
        let size = FrameSize::new(64, 36).expect("size");
        let backends = Backends {
            clock: Arc::new(clock.clone()),
            video_devices: Arc::new(FakeVideoDevices::new(clock.clone())),
            capture: Arc::new(FakeCapture::new(clock.clone(), size)),
            audio: Arc::new(audio.clone()),
            codecs: Arc::new(FakeCodecs),
            gpu: Arc::new(FakeGpuInterop),
            media: Arc::new(FakeMediaDecoders::new()),
            vcam: None,
        };
        Fakes {
            clock,
            audio,
            backends,
        }
    }

    /// Another handle on the same backends (`Backends` is not `Clone`: its vcam is boxed).
    fn share(backends: &Backends) -> Backends {
        Backends {
            clock: Arc::clone(&backends.clock),
            video_devices: Arc::clone(&backends.video_devices),
            capture: Arc::clone(&backends.capture),
            audio: Arc::clone(&backends.audio),
            codecs: Arc::clone(&backends.codecs),
            gpu: Arc::clone(&backends.gpu),
            media: Arc::clone(&backends.media),
            vcam: None,
        }
    }

    fn engine(backends: Backends, draw: OpenDraw, publish: Box<dyn LivePublish>) -> Engine {
        Engine::new()
            .with_output(OutputId::new("yt"))
            .with_backends(backends)
            .with_draw(draw)
            .with_publish(publish)
    }

    /// The render thread draws the render snapshot the control side published, without
    /// the collection lock: a batch that changes the program mix reaches the next ticks.
    /// The thread stamps the generation it adopted, and a stopped output holds no
    /// gesture up. A batch that changes nothing the mix draws publishes nothing new.
    #[test]
    fn the_render_thread_adopts_published_snapshots() {
        use super::{batch, create};
        use jstream_protocol::{Batch, SceneId};

        let fakes = fakes();
        let seen: Arc<Mutex<Vec<Option<SceneId>>>> = Arc::default();
        let draw: OpenDraw = {
            let seen = Arc::clone(&seen);
            Arc::new(move |context: Option<&jstream_platform::GpuContext>| {
                let seen = Arc::clone(&seen);
                let mut inner = (cpu_draw())(context)?;
                let draw: DrawFn = Box::new(move |shot, live, captures, frames, at| {
                    seen.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(shot.program().cloned());
                    inner(shot, live, captures, frames, at)
                });
                Ok(draw)
            })
        };
        let engine = engine(
            share(&fakes.backends),
            draw,
            Box::new(RecordingPublish::new()),
        );
        let hub = engine.render_hub();
        let last = || {
            seen.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .last()
                .cloned()
        };
        let control = connected(&engine);
        let mut events = control.events();
        start(&control, &mut events);
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || last() == Some(None)),
            "the empty collection was drawn"
        );
        assert!(hub.caught_up(1) && !hub.caught_up(2));

        let one = |edit| Batch {
            edits: vec![edit],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        };
        batch(&control, one(create("main"))).expect("scene.create");
        assert_eq!(hub.load().generation(), 2);
        let main = Some(SceneId::new("main"));
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || last() == Some(main.clone())),
            "the next ticks draw the new program scene"
        );
        assert!(hub.caught_up(2) && !hub.caught_up(3));

        // A second scene is not on program and nothing nests it.
        batch(&control, one(create("spare"))).expect("scene.create");
        assert_eq!(hub.load().generation(), 2, "nothing the mix draws changed");

        block_on(control.call(output_call("yt", false))).expect("output.stop");
        assert!(
            hub.caught_up(u64::MAX - 1),
            "a stopped render thread holds nothing up"
        );
    }

    /// Feeds both inputs and moves the clock on by 50 ms.
    fn pump(clock: &FakeClock, audio: &FakeAudioIo) {
        audio.deliver(2400, 0.2);
        clock.advance(50_000);
        thread::sleep(Duration::from_millis(10));
    }

    /// How long `pump_until` and `wait_until` keep trying. It only bounds how long a
    /// failing test hangs, it is not a timing assertion: these tests pass in well under
    /// a second, but a loaded CI or workspace run can starve the pipeline threads for
    /// seconds at a time (#392).
    const HANG_BOUND: Duration = Duration::from_secs(30);

    /// Pumps until `done`, for at most `HANG_BOUND`.
    fn pump_until(clock: &FakeClock, audio: &FakeAudioIo, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + HANG_BOUND;
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            pump(clock, audio);
        }
        done()
    }

    /// Waits until `done`, for at most `HANG_BOUND`.
    fn wait_until(mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + HANG_BOUND;
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        done()
    }

    fn readbacks(engine: &Engine) -> u64 {
        engine.output_stats().map_or(0, |stats| stats.readbacks)
    }

    fn start(control: &dyn Control, events: &mut EventStream) {
        block_on(control.call(output_call("yt", true))).expect("output.start");
        assert_eq!(
            output_steps(events),
            vec![OutputState::Starting, OutputState::Active]
        );
    }

    /// Pumps until the output has settled `failed` and its pipeline is gone.
    fn fails(engine: &Engine, control: &dyn Control, fakes: &Fakes) -> bool {
        pump_until(&fakes.clock, &fakes.audio, || {
            output_state(control, "yt") == OutputState::Failed && engine.output_stats().is_none()
        })
    }

    /// A gate one thread waits on until another opens it.
    #[derive(Default)]
    struct Hold {
        open: Mutex<bool>,
        cv: Condvar,
    }

    impl Hold {
        fn signal(&self) {
            *self.open.lock().expect("hold") = true;
            self.cv.notify_all();
        }

        fn wait(&self) {
            let mut open = self.open.lock().expect("hold");
            while !*open {
                open = self.cv.wait(open).expect("hold");
            }
        }
    }

    // --- Item: a publish opener connects on every start, as a live host needs.

    /// The engine as a live host wires it: no one-shot target, an opener that
    /// connects RTMP to a fake server on loopback. Each `output.start` opens a new
    /// session and publishes media into it, and `output.stop` ends it, so the output
    /// can start again after a stop.
    #[test]
    fn a_publish_opener_connects_rtmp_on_every_start() {
        use crate::runtime::serve_rtmp;
        use crate::{OpenPublish, connect_rtmp};
        use std::net::TcpListener;

        const STARTS: usize = 2;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let url = format!(
            "rtmp://127.0.0.1:{}/live",
            listener.local_addr().expect("addr").port()
        );
        // A baseline session that publishes no media, then one session per start, in
        // turn; each drains until the client hangs up.
        let server = thread::spawn(move || {
            (0..=STARTS)
                .map(|_| {
                    let (io, _) = listener.accept().expect("accept");
                    serve_rtmp(io, Duration::ZERO).join().expect("drain")
                })
                .collect::<Vec<u64>>()
        });
        let opens = Arc::new(AtomicU64::new(0));
        let open: OpenPublish = {
            let opens = Arc::clone(&opens);
            let url = url.clone();
            Arc::new(move |output_id: &OutputId| {
                assert_eq!(output_id, &OutputId::new("yt"));
                opens.fetch_add(1, Ordering::SeqCst);
                let publish: Box<dyn LivePublish> = Box::new(connect_rtmp(&url, "test-key")?);
                Ok(publish)
            })
        };

        let mut baseline = connect_rtmp(&url, "test-key").expect("baseline session");
        baseline.finish().expect("finish");
        drop(baseline);

        let fakes = fakes();
        let engine = Engine::new()
            .with_output(OutputId::new("yt"))
            .with_backends(share(&fakes.backends))
            .with_draw(cpu_draw())
            .with_publish_opener(open);
        let control = connected(&engine);
        let mut events = control.events();
        for run in 1..=STARTS {
            start(&control, &mut events);
            assert_eq!(
                opens.load(Ordering::SeqCst),
                run as u64,
                "one open per start"
            );
            assert!(
                pump_until(&fakes.clock, &fakes.audio, || {
                    engine.output_stats().is_some_and(|stats| {
                        stats.video_subscriber.delivered >= 3
                            && stats.audio_subscriber.delivered >= 3
                    })
                }),
                "start {run}: media never reached the publish target"
            );
            block_on(control.call(output_call("yt", false))).expect("output.stop");
            assert_eq!(
                output_steps(&mut events),
                vec![OutputState::Stopping, OutputState::Stopped]
            );
        }
        let drained = server.join().expect("server");
        let (baseline, runs) = drained.split_first().expect("sessions");
        assert_eq!(runs.len(), STARTS, "one session per start");
        for (run, bytes) in runs.iter().enumerate() {
            assert!(
                bytes > baseline,
                "session {} carried {bytes} bytes, no more than the {baseline} without media",
                run + 1
            );
        }
    }

    /// An opener that cannot connect fails the start with `action_failed` and settles
    /// `failed`; the reason says what failed without the stream key (R-SEC-001), and
    /// the opener is kept for the next start.
    #[test]
    fn a_failed_publish_opener_fails_the_start() {
        use crate::{OpenPublish, connect_rtmp};
        use std::net::TcpListener;

        // A port nothing listens on: bound, then released.
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("a free port")
            .port();
        let url = format!("rtmp://127.0.0.1:{port}/live");
        let opens = Arc::new(AtomicU64::new(0));
        let open: OpenPublish = {
            let opens = Arc::clone(&opens);
            Arc::new(move |_: &OutputId| {
                opens.fetch_add(1, Ordering::SeqCst);
                let publish: Box<dyn LivePublish> =
                    Box::new(connect_rtmp(&url, "secret-stream-key")?);
                Ok(publish)
            })
        };
        let fakes = fakes();
        let engine = Engine::new()
            .with_output(OutputId::new("yt"))
            .with_backends(share(&fakes.backends))
            .with_draw(cpu_draw())
            .with_publish_opener(open);
        let control = connected(&engine);
        let mut events = control.events();
        for run in 1..=2 {
            let err = block_on(control.call(output_call("yt", true))).expect_err("refused");
            assert_eq!(code(&err), Some("action_failed"));
            let reason = err.to_string();
            assert!(reason.contains("opening the publish target"), "{reason}");
            assert!(!reason.contains("secret-stream-key"), "{reason}");
            assert_eq!(
                output_steps(&mut events),
                vec![OutputState::Starting, OutputState::Failed]
            );
            assert_eq!(opens.load(Ordering::SeqCst), run, "the opener is kept");
            assert!(engine.output_stats().is_none());
        }
    }

    /// When the credential store refuses to remove the key of an output that moved to
    /// another server (#343), the output can't start, so the opener never sends the old
    /// key to the new server; once `output.clear_key` succeeds, it can.
    #[test]
    fn an_output_whose_old_key_is_not_removed_does_not_start() {
        use super::{clear_key, rtmp_output, set_key};
        use crate::OpenPublish;
        use jstream_platform::fake::MemoryKeyStore;
        use jstream_protocol::RtmpTarget;

        let opens = Arc::new(AtomicU64::new(0));
        let open: OpenPublish = {
            let opens = Arc::clone(&opens);
            Arc::new(move |_: &OutputId| {
                opens.fetch_add(1, Ordering::SeqCst);
                Err(crate::PublishError::new("no server in this test"))
            })
        };
        let store = MemoryKeyStore::new();
        let fakes = fakes();
        let engine = Engine::new()
            .with_key_store(Arc::new(store.clone()))
            .with_backends(share(&fakes.backends))
            .with_draw(cpu_draw())
            .with_publish_opener(open);
        let control = connected(&engine);
        batch(
            &control,
            edits(vec![Edit::OutputCreate {
                output_id: OutputId::new("yt"),
                kind: rtmp_output("rtmp://a.rtmp.youtube.com/live2"),
            }]),
        )
        .expect("output.create");
        block_on(control.call(set_key("yt", "yt-key"))).expect("set_key");
        store.set_failing(true);
        batch(
            &control,
            edits(vec![Edit::OutputSetTarget {
                output_id: OutputId::new("yt"),
                target: RtmpTarget::Generic {
                    url: "rtmp://attacker.example/live2".into(),
                },
            }]),
        )
        .expect("the batch applies even when the store refuses");
        store.set_failing(false);
        assert_eq!(store.len(), 1, "the store kept the key");

        let refused = block_on(control.call(output_call("yt", true))).expect_err("refused");
        assert_eq!(code(&refused), Some("action_failed"));
        assert!(
            refused.to_string().contains("output.clear_key"),
            "{refused}"
        );
        assert!(!refused.to_string().contains("yt-key"), "{refused}");
        assert_eq!(opens.load(Ordering::SeqCst), 0, "the opener never ran");

        block_on(control.call(clear_key("yt"))).expect("clear_key");
        assert!(store.is_empty());
        let failed = block_on(control.call(output_call("yt", true))).expect_err("no server");
        assert!(
            failed.to_string().contains("opening the publish target"),
            "{failed}"
        );
        assert_eq!(
            opens.load(Ordering::SeqCst),
            1,
            "the start reached the opener"
        );
    }

    /// A retarget that lands after `output.start` checked the key but before the host's
    /// opener reads the target (#343) doesn't pair the new server with the old key:
    /// while the start is opening, `Engine::output_kind` answers the kind the start
    /// checked, so the opener sees the server the key was set for. The store refuses
    /// the removal here, so the old key is still there when the opener reads it. An
    /// `output.set_key` sent while the start is opening is refused, so the new server's
    /// key can't be read with the old server either; once the start settles it is taken.
    #[test]
    fn a_retarget_during_the_opener_does_not_send_the_old_key_to_the_new_server() {
        use super::{rtmp_output, set_key};
        use crate::OpenPublish;
        use jstream_platform::StreamKeyStore;
        use jstream_platform::fake::MemoryKeyStore;
        use jstream_protocol::{OutputKind, RtmpTarget};

        const OLD: &str = "rtmp://a.rtmp.youtube.com/live2";
        const NEW: &str = "rtmp://attacker.example/live2";
        let store = MemoryKeyStore::new();
        // The engine and a client for the opener to retarget with, set once both exist.
        type Slot = Mutex<Option<(Engine, InProcessControl)>>;
        // The kind the opener read, and the key the store held then.
        type Seen = Mutex<Option<(Option<OutputKind>, Option<String>)>>;
        let slot: Arc<Slot> = Arc::new(Mutex::new(None));
        let seen: Arc<Seen> = Arc::new(Mutex::new(None));
        let open: OpenPublish = {
            let (slot, seen, store) = (Arc::clone(&slot), Arc::clone(&seen), store.clone());
            Arc::new(move |output_id: &OutputId| {
                let guard = slot.lock().expect("slot");
                let (engine, control) = guard.as_ref().expect("engine set");
                store.set_failing(true);
                batch(
                    control,
                    edits(vec![Edit::OutputSetTarget {
                        output_id: output_id.clone(),
                        target: RtmpTarget::Generic { url: NEW.into() },
                    }]),
                )
                .expect("the retarget applies while the output starts");
                store.set_failing(false);
                // A key set for the new server now would be read with the old server.
                let refused = block_on(control.call(set_key(output_id.as_str(), "new-key")))
                    .expect_err("set_key is refused while the output starts");
                assert_eq!(code(&refused), Some("action_failed"), "{refused}");
                assert!(!refused.to_string().contains("new-key"), "{refused}");
                let kind = engine.output_kind(output_id);
                let key = store
                    .get(output_id.as_str())
                    .expect("get")
                    .map(|key| key.expose().to_owned());
                *seen.lock().expect("seen") = Some((kind, key));
                Err(crate::PublishError::new("no server in this test"))
            })
        };
        let fakes = fakes();
        let engine = Engine::new()
            .with_key_store(Arc::new(store.clone()))
            .with_backends(share(&fakes.backends))
            .with_draw(cpu_draw())
            .with_publish_opener(open);
        let control = connected(&engine);
        batch(
            &control,
            edits(vec![Edit::OutputCreate {
                output_id: OutputId::new("yt"),
                kind: rtmp_output(OLD),
            }]),
        )
        .expect("output.create");
        block_on(control.call(set_key("yt", "yt-key"))).expect("set_key");
        *slot.lock().expect("slot") = Some((engine.clone(), connected(&engine)));

        let failed = block_on(control.call(output_call("yt", true))).expect_err("no server");
        assert!(!failed.to_string().contains("yt-key"), "{failed}");
        let (kind, key) = seen.lock().expect("seen").take().expect("the opener ran");
        assert_eq!(
            key.as_deref(),
            Some("yt-key"),
            "the refused removal left the old key, and the refused set_key stored nothing"
        );
        assert_eq!(
            kind,
            Some(rtmp_output(OLD)),
            "the opener saw the server the key was set for"
        );
        assert_eq!(
            engine.output_kind(&OutputId::new("yt")),
            Some(rtmp_output(NEW)),
            "once the start settled, the retarget shows"
        );
        block_on(control.call(set_key("yt", "new-key"))).expect("set_key once settled");
        // The opener holds the engine; drop that so the engine isn't kept alive.
        slot.lock().expect("slot").take();
    }

    // --- Item: an encoder that fails mid-stream fails the output.

    struct FailingVideo {
        inner: Box<dyn VideoEncoder>,
        fail: Arc<AtomicBool>,
    }

    impl VideoEncoder for FailingVideo {
        fn info(&self) -> &CodecInfo {
            self.inner.info()
        }

        fn can_accept(&self) -> bool {
            self.inner.can_accept()
        }

        fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
            if self.fail.load(Ordering::Acquire) {
                return Err(PlatformError::InvalidState {
                    what: "encode",
                    state: "the test broke the encoder",
                });
            }
            self.inner.encode(frame)
        }

        fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            self.inner.poll(out)
        }

        fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            self.inner.finish(out)
        }
    }

    struct FailingCodecs {
        fail: Arc<AtomicBool>,
    }

    impl Codecs for FailingCodecs {
        fn list(&self) -> Result<Vec<CodecInfo>, PlatformError> {
            FakeCodecs.list()
        }

        fn video_encoder(
            &self,
            config: &VideoEncoderConfig,
        ) -> Result<Box<dyn VideoEncoder>, PlatformError> {
            Ok(Box::new(FailingVideo {
                inner: FakeCodecs.video_encoder(config)?,
                fail: Arc::clone(&self.fail),
            }))
        }

        fn audio_encoder(
            &self,
            config: &AudioEncoderConfig,
        ) -> Result<Box<dyn AudioEncoder>, PlatformError> {
            FakeCodecs.audio_encoder(config)
        }

        fn video_decoder(
            &self,
            config: &VideoDecoderConfig,
        ) -> Result<Box<dyn VideoDecoder>, PlatformError> {
            FakeCodecs.video_decoder(config)
        }
    }

    /// A video encoder that fails while the output is `active` ends its packet stream.
    /// The output thread reports that, so the output settles `failed` instead of staying
    /// `active` with audio alone.
    #[test]
    fn an_encoder_failure_mid_stream_fails_the_output() {
        let mut fakes = fakes();
        let fail = Arc::new(AtomicBool::new(false));
        fakes.backends.codecs = Arc::new(FailingCodecs {
            fail: Arc::clone(&fail),
        });
        let engine = engine(
            share(&fakes.backends),
            cpu_draw(),
            Box::new(RecordingPublish::new()),
        );
        let control = connected(&engine);
        let mut events = control.events();
        start(&control, &mut events);
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || readbacks(&engine) >= 2),
            "the pipeline never drew"
        );

        fail.store(true, Ordering::Release);
        assert!(
            fails(&engine, &control, &fakes),
            "a failed encoder left the output {:?}",
            output_state(&control, "yt")
        );
        assert_eq!(output_steps(&mut events), vec![OutputState::Failed]);
    }

    // --- Item: render and mixer failures are reported, and capture errors are ridden out.

    struct FlakyCapture {
        inner: FakeCapture,
        broken: Arc<AtomicBool>,
    }

    struct FlakyStream {
        inner: Box<dyn CaptureStream>,
        broken: Arc<AtomicBool>,
    }

    impl Capture for FlakyCapture {
        fn targets(&self) -> Result<Vec<CaptureTargetInfo>, PlatformError> {
            self.inner.targets()
        }

        fn start(&self, target: &CaptureTarget) -> Result<Box<dyn CaptureStream>, PlatformError> {
            Ok(Box::new(FlakyStream {
                inner: self.inner.start(target)?,
                broken: Arc::clone(&self.broken),
            }))
        }
    }

    impl CaptureStream for FlakyStream {
        fn latest(&mut self) -> Result<Option<VideoSurface>, PlatformError> {
            if self.broken.load(Ordering::Acquire) {
                return Err(PlatformError::InvalidState {
                    what: "latest",
                    state: "the test broke the capture",
                });
            }
            self.inner.latest()
        }

        fn stats(&self) -> CaptureStats {
            self.inner.stats()
        }
    }

    /// A capture stream that starts failing is a missing source frame: the render
    /// thread keeps drawing the last one on every tick, and the output stays `active`.
    #[test]
    fn a_failing_capture_repeats_the_last_frame() {
        let mut fakes = fakes();
        let broken = Arc::new(AtomicBool::new(false));
        fakes.backends.capture = Arc::new(FlakyCapture {
            inner: FakeCapture::new(fakes.clock.clone(), FrameSize::new(64, 36).expect("size")),
            broken: Arc::clone(&broken),
        });
        let engine = engine(
            share(&fakes.backends),
            cpu_draw(),
            Box::new(RecordingPublish::new()),
        );
        let control = connected(&engine);
        let mut events = control.events();
        start(&control, &mut events);
        assert!(pump_until(&fakes.clock, &fakes.audio, || readbacks(
            &engine
        ) >= 2));

        broken.store(true, Ordering::Release);
        let before = readbacks(&engine);
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || readbacks(&engine)
                >= before + 5),
            "the render thread stopped drawing when capture failed"
        );
        assert_eq!(output_state(&control, "yt"), OutputState::Active);
        assert_eq!(output_steps(&mut events), vec![]);
        block_on(control.call(output_call("yt", false))).expect("output.stop");
    }

    /// A panic inside a render tick is caught at the thread boundary and reported: the
    /// output settles `failed` (R-RT-006), instead of staying `active` with no video.
    #[test]
    fn a_render_panic_fails_the_output() {
        let fakes = fakes();
        let armed = Arc::new(AtomicBool::new(false));
        let draw: OpenDraw = {
            let armed = Arc::clone(&armed);
            Arc::new(move |context: Option<&jstream_platform::GpuContext>| {
                let armed = Arc::clone(&armed);
                let mut inner = (cpu_draw())(context)?;
                let draw: DrawFn = Box::new(move |shot, live, captures, frames, at| {
                    assert!(!armed.load(Ordering::Acquire), "compositor blew up");
                    inner(shot, live, captures, frames, at)
                });
                Ok(draw)
            })
        };
        let engine = engine(
            share(&fakes.backends),
            draw,
            Box::new(RecordingPublish::new()),
        );
        let control = connected(&engine);
        let mut events = control.events();
        start(&control, &mut events);
        assert!(pump_until(&fakes.clock, &fakes.audio, || readbacks(
            &engine
        ) >= 2));

        armed.store(true, Ordering::Release);
        assert!(
            fails(&engine, &control, &fakes),
            "a render panic left the output {:?}",
            output_state(&control, "yt")
        );
        assert_eq!(output_steps(&mut events), vec![OutputState::Failed]);
    }

    /// A clock that panics when the mixer thread reads it, once armed.
    struct MixerPanicClock {
        inner: FakeClock,
        armed: Arc<AtomicBool>,
    }

    impl MediaClock for MixerPanicClock {
        fn timebase(&self) -> Timebase {
            self.inner.timebase()
        }

        fn now(&self) -> MediaTime {
            if thread::current().name() == Some("jstream-mixer")
                && self.armed.swap(false, Ordering::AcqRel)
            {
                panic!("mixer blew up");
            }
            self.inner.now()
        }
    }

    /// A panic on the mixer thread is caught at its boundary and fails the output.
    #[test]
    fn a_mixer_panic_fails_the_output() {
        let mut fakes = fakes();
        let armed = Arc::new(AtomicBool::new(false));
        fakes.backends.clock = Arc::new(MixerPanicClock {
            inner: fakes.clock.clone(),
            armed: Arc::clone(&armed),
        });
        let engine = engine(
            share(&fakes.backends),
            cpu_draw(),
            Box::new(RecordingPublish::new()),
        );
        let control = connected(&engine);
        let mut events = control.events();
        start(&control, &mut events);
        armed.store(true, Ordering::Release);
        assert!(
            fails(&engine, &control, &fakes),
            "a mixer panic left the output {:?}",
            output_state(&control, "yt")
        );
        assert!(!armed.load(Ordering::Acquire), "the mixer never panicked");
        assert_eq!(output_steps(&mut events), vec![OutputState::Failed]);
    }

    // --- Item: a late tick skips to the clock and is counted.

    /// The clock jumps ten periods past the last drawn tick. The render thread draws
    /// the current tick, stamped with the clock, and counts the nine it skipped in
    /// `render_ticks`. Before, it drew the stale tick and video fell behind audio.
    #[test]
    fn a_late_render_tick_skips_to_the_clock() {
        let fakes = fakes();
        let publish = RecordingPublish::new();
        let engine = engine(
            share(&fakes.backends),
            cpu_draw(),
            Box::new(publish.clone()),
        );
        let control = connected(&engine);
        let mut events = control.events();
        start(&control, &mut events);
        let video_tags = || {
            publish
                .tags()
                .iter()
                .filter(|tag| tag.tag_type == TagType::Video && tag.body.get(1) == Some(&1))
                .map(|tag| tag.timestamp)
                .collect::<Vec<_>>()
        };
        // The frame drawn after the jump is the second one published.
        let late_video = || video_tags().get(1).copied().unwrap_or(0);
        // The first tick is drawn at the clock's zero, before it moves. Wait until it
        // is published, so the encoder is idle and takes the next frame.
        assert!(wait_until(|| readbacks(&engine) >= 1), "no first frame");
        assert!(
            wait_until(|| !video_tags().is_empty()),
            "first frame not published: {:?}",
            engine.output_stats()
        );

        fakes.clock.advance(10 * PERIOD_US);
        assert!(wait_until(|| readbacks(&engine) >= 2), "no second frame");
        assert!(
            wait_until(|| late_video() >= 300),
            "the second frame was stamped {} ms, not at the clock",
            late_video()
        );

        engine.emit_stats_tick();
        let tick = drain(&mut events)
            .into_iter()
            .filter_map(|event| match event {
                Event::StatsTick { drops, load, .. } => Some((drops.render_ticks, load)),
                _ => None,
            })
            .next_back();
        let (render_ticks, load) = tick.expect("a stats.tick");
        assert_eq!(render_ticks, 9);
        // Two ticks drawn, and the nine skipped ones not counted as frames.
        let frames = load.expect("the engine's load").program_frames;
        assert_eq!(frames, 2, "the nine skipped ticks are not frames");
        block_on(control.call(output_call("yt", false))).expect("output.stop");
    }

    // --- Item: each running output's live stats (protocol 0.57).

    /// While an output runs, each `stats.tick` lists it with what it has sent and its
    /// uptime; once it stops, the tick leaves it out.
    #[test]
    fn a_running_output_is_in_each_stats_tick() {
        let fakes = fakes();
        let publish = RecordingPublish::new();
        let engine = engine(
            share(&fakes.backends),
            cpu_draw(),
            Box::new(publish.clone()),
        );
        let control = connected(&engine);
        let mut events = control.events();
        start(&control, &mut events);
        let video_frames = || {
            publish
                .tags()
                .iter()
                .filter(|tag| tag.tag_type == TagType::Video && tag.body.get(1) == Some(&1))
                .count()
        };
        assert!(wait_until(|| video_frames() >= 1), "no frame published");
        let outputs = |events: &mut _| {
            engine.emit_stats_tick();
            drain(events)
                .into_iter()
                .filter_map(|event| match event {
                    Event::StatsTick { outputs, .. } => Some(outputs),
                    _ => None,
                })
                .next_back()
                .expect("a stats.tick")
        };
        let running = outputs(&mut events);
        assert_eq!(running.len(), 1, "{running:?}");
        let yt = &running[0];
        assert_eq!(yt.output_id, OutputId::new("yt"));
        assert!(yt.frames >= 1, "{yt:?}");
        assert!(yt.bytes > 0, "{yt:?}");
        assert_eq!(yt.dropped_frames, 0, "{yt:?}");
        assert!(
            matches!(
                yt.health,
                jstream_protocol::OutputHealth::Good | jstream_protocol::OutputHealth::Fair
            ),
            "{yt:?}"
        );

        block_on(control.call(output_call("yt", false))).expect("output.stop");
        assert_eq!(
            outputs(&mut events),
            Vec::new(),
            "a stopped output is left out"
        );
    }

    // --- Item: a failed start's teardown does not overlap the next start.

    /// Fails its first tag, the FLV header the output thread sends before the start
    /// returns, so the start sees the failure.
    struct FailFirstTag;

    impl LivePublish for FailFirstTag {
        fn send_tag(&mut self, _tag: &FlvTag) -> Result<(), PublishError> {
            Err(PublishError::new("publish refused"))
        }

        fn poll(&mut self) -> Result<(), PublishError> {
            Ok(())
        }

        fn finish(&mut self) -> Result<(), PublishError> {
            Ok(())
        }
    }

    /// Audio whose first stream drop blocks until `release`, flagging `tearing_down`.
    struct SlowTeardownAudio {
        inner: FakeAudioIo,
        armed: Arc<AtomicBool>,
        tearing_down: Arc<AtomicBool>,
        release: Arc<Hold>,
    }

    struct SlowStream {
        inner: Box<dyn AudioStream>,
        armed: Arc<AtomicBool>,
        tearing_down: Arc<AtomicBool>,
        release: Arc<Hold>,
    }

    impl AudioStream for SlowStream {
        fn format(&self) -> jstream_media::AudioFormat {
            self.inner.format()
        }
    }

    impl Drop for SlowStream {
        fn drop(&mut self) {
            if self.armed.swap(false, Ordering::AcqRel) {
                self.tearing_down.store(true, Ordering::Release);
                self.release.wait();
                self.tearing_down.store(false, Ordering::Release);
            }
        }
    }

    impl AudioIo for SlowTeardownAudio {
        fn devices(&self) -> Result<Vec<AudioDeviceInfo>, PlatformError> {
            self.inner.devices()
        }

        fn processes(&self) -> Result<Vec<jstream_platform::AudioProcess>, PlatformError> {
            self.inner.processes()
        }

        fn input_format(
            &self,
            target: &AudioInputTarget,
        ) -> Result<jstream_media::AudioFormat, PlatformError> {
            self.inner.input_format(target)
        }

        fn open_input(
            &self,
            target: &AudioInputTarget,
            handler: Box<dyn AudioInputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            Ok(Box::new(SlowStream {
                inner: self.inner.open_input(target, handler)?,
                armed: Arc::clone(&self.armed),
                tearing_down: Arc::clone(&self.tearing_down),
                release: Arc::clone(&self.release),
            }))
        }

        fn open_output(
            &self,
            target: &AudioOutputTarget,
            handler: Box<dyn AudioOutputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            self.inner.open_output(target, handler)
        }
    }

    /// Records whether a capture was opened while a teardown was in progress.
    struct WatchCapture {
        inner: FakeCapture,
        tearing_down: Arc<AtomicBool>,
        overlapped: Arc<AtomicBool>,
    }

    impl Capture for WatchCapture {
        fn targets(&self) -> Result<Vec<CaptureTargetInfo>, PlatformError> {
            self.inner.targets()
        }

        fn start(&self, target: &CaptureTarget) -> Result<Box<dyn CaptureStream>, PlatformError> {
            if self.tearing_down.load(Ordering::Acquire) {
                self.overlapped.store(true, Ordering::Release);
            }
            self.inner.start(target)
        }
    }

    /// A start that fails after `launch` returned a pipeline emits `failed` and then
    /// joins that pipeline. A second start in that window waits for the join instead
    /// of opening devices over the teardown.
    #[test]
    fn a_failed_start_holds_off_the_next_start_until_its_teardown_ends() {
        let mut fakes = fakes();
        let tearing_down = Arc::new(AtomicBool::new(false));
        let overlapped = Arc::new(AtomicBool::new(false));
        let release = Arc::new(Hold::default());
        fakes.backends.audio = Arc::new(SlowTeardownAudio {
            inner: fakes.audio.clone(),
            armed: Arc::new(AtomicBool::new(true)),
            tearing_down: Arc::clone(&tearing_down),
            release: Arc::clone(&release),
        });
        fakes.backends.capture = Arc::new(WatchCapture {
            inner: FakeCapture::new(fakes.clock.clone(), FrameSize::new(64, 36).expect("size")),
            tearing_down: Arc::clone(&tearing_down),
            overlapped: Arc::clone(&overlapped),
        });
        let engine = engine(share(&fakes.backends), cpu_draw(), Box::new(FailFirstTag));
        let status = connected(&engine);
        let mut events = status.events();
        let first = connected(&engine);
        let first = thread::spawn(move || block_on(first.call(output_call("yt", true))));
        assert!(
            wait_until(|| output_state(&status, "yt") == OutputState::Failed),
            "the first start never failed"
        );
        assert!(
            wait_until(|| tearing_down.load(Ordering::Acquire)),
            "the teardown never started"
        );

        let engine = engine.with_publish(Box::new(RecordingPublish::new()));
        let second = connected(&engine);
        let (done_tx, done_rx) = mpsc::channel();
        let second = thread::spawn(move || {
            let result = block_on(second.call(output_call("yt", true)));
            let _ = done_tx.send(());
            result
        });
        assert!(
            done_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "the second start finished while the first pipeline was still joining"
        );
        release.signal();
        let first = first.join().expect("first start");
        assert_eq!(
            code(&first.expect_err("publish refused")),
            Some("action_failed")
        );
        second
            .join()
            .expect("second start")
            .expect("second output.start");
        assert!(
            !overlapped.load(Ordering::Acquire),
            "the second start opened capture during the first teardown"
        );
        assert_eq!(
            output_steps(&mut events),
            vec![
                OutputState::Starting,
                OutputState::Failed,
                OutputState::Starting,
                OutputState::Active
            ]
        );
    }

    // --- Item: a panicking teardown during output.stop settles the output.

    struct PanicOnDropAudio {
        inner: FakeAudioIo,
        armed: Arc<AtomicBool>,
    }

    struct PanicOnDrop {
        inner: Box<dyn AudioStream>,
        armed: Arc<AtomicBool>,
    }

    impl AudioStream for PanicOnDrop {
        fn format(&self) -> jstream_media::AudioFormat {
            self.inner.format()
        }
    }

    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            if !thread::panicking() && self.armed.swap(false, Ordering::AcqRel) {
                panic!("audio stream teardown");
            }
        }
    }

    impl AudioIo for PanicOnDropAudio {
        fn devices(&self) -> Result<Vec<AudioDeviceInfo>, PlatformError> {
            self.inner.devices()
        }

        fn processes(&self) -> Result<Vec<jstream_platform::AudioProcess>, PlatformError> {
            self.inner.processes()
        }

        fn input_format(
            &self,
            target: &AudioInputTarget,
        ) -> Result<jstream_media::AudioFormat, PlatformError> {
            self.inner.input_format(target)
        }

        fn open_input(
            &self,
            target: &AudioInputTarget,
            handler: Box<dyn AudioInputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            Ok(Box::new(PanicOnDrop {
                inner: self.inner.open_input(target, handler)?,
                armed: Arc::clone(&self.armed),
            }))
        }

        fn open_output(
            &self,
            target: &AudioOutputTarget,
            handler: Box<dyn AudioOutputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            self.inner.open_output(target, handler)
        }
    }

    /// `output.stop` whose teardown panics does not leave the output `stopping`: it
    /// settles `failed`, and a later start is not held off.
    #[test]
    fn a_panicking_stop_teardown_settles_the_output() {
        let mut fakes = fakes();
        let armed = Arc::new(AtomicBool::new(false));
        fakes.backends.audio = Arc::new(PanicOnDropAudio {
            inner: fakes.audio.clone(),
            armed: Arc::clone(&armed),
        });
        let engine = engine(
            share(&fakes.backends),
            cpu_draw(),
            Box::new(RecordingPublish::new()),
        );
        let control = connected(&engine);
        let mut events = control.events();
        start(&control, &mut events);
        armed.store(true, Ordering::Release);

        let stopped = panic::catch_unwind(AssertUnwindSafe(|| {
            block_on(control.call(output_call("yt", false)))
        }));
        assert!(stopped.is_err(), "the teardown did not panic");
        assert!(!armed.load(Ordering::Acquire));
        assert_eq!(output_state(&control, "yt"), OutputState::Failed);
        assert_eq!(
            output_steps(&mut events),
            vec![OutputState::Stopping, OutputState::Failed]
        );

        let engine = engine.with_publish(Box::new(RecordingPublish::new()));
        let restart = connected(&engine);
        let (tx, rx) = mpsc::channel();
        let restart = thread::spawn(move || {
            let result = block_on(restart.call(output_call("yt", true)));
            let _ = tx.send(());
            result
        });
        rx.recv_timeout(Duration::from_secs(10))
            .expect("a start after the panicking stop blocked");
        restart
            .join()
            .expect("restart")
            .expect("restart output.start");
    }

    // --- Item: stopping emits a final stats.tick.

    /// The run's last drops reach clients when it stops: one `stats.tick` comes after
    /// the threads stop and before `stopped`.
    #[test]
    fn stopping_emits_a_final_stats_tick() {
        let fakes = fakes();
        let engine = engine(
            share(&fakes.backends),
            cpu_draw(),
            Box::new(RecordingPublish::new()),
        );
        let control = connected(&engine);
        let mut events = control.events();
        start(&control, &mut events);
        assert!(wait_until(|| readbacks(&engine) >= 1));
        // Ten skipped ticks, so the final tick has something to report.
        fakes.clock.advance(11 * PERIOD_US);
        assert!(wait_until(|| readbacks(&engine) >= 2));
        drain(&mut events);

        block_on(control.call(output_call("yt", false))).expect("output.stop");
        let events = drain(&mut events);
        let stopped = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    Event::OutputStateChanged {
                        state: OutputState::Stopped,
                        ..
                    }
                )
            })
            .expect("stopped");
        let stopping = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    Event::OutputStateChanged {
                        state: OutputState::Stopping,
                        ..
                    }
                )
            })
            .expect("stopping");
        let last_tick = events[stopping..stopped]
            .iter()
            .rev()
            .find_map(|event| match event {
                Event::StatsTick { drops, .. } => Some(*drops),
                _ => None,
            });
        let drops = last_tick.expect("no stats.tick between stopping and stopped");
        assert_eq!(drops.render_ticks, 10, "{drops:?}");
    }

    // --- Item: a lost publish link reports `reconnecting`, and its drops are counted.

    /// The link a [`FlakyLink`] reports, set by the test.
    #[derive(Default)]
    struct Link {
        down: AtomicBool,
        gave_up: AtomicBool,
        sent: AtomicU64,
        dropped: AtomicU64,
    }

    /// A publish target whose link the test takes down and brings back, as
    /// `RtmpPublish` reports its reconnecting publisher's: tags sent while the link is
    /// down are dropped and counted, and once it has given up every send fails.
    struct FlakyLink(Arc<Link>);

    impl LivePublish for FlakyLink {
        fn send_tag(&mut self, _tag: &FlvTag) -> Result<(), PublishError> {
            if self.0.gave_up.load(Ordering::Acquire) {
                return Err(PublishError::new("gave up reconnecting"));
            }
            if self.0.down.load(Ordering::Acquire) {
                self.0.dropped.fetch_add(1, Ordering::AcqRel);
            } else {
                self.0.sent.fetch_add(1, Ordering::AcqRel);
            }
            Ok(())
        }

        fn poll(&mut self) -> Result<(), PublishError> {
            Ok(())
        }

        fn finish(&mut self) -> Result<(), PublishError> {
            Ok(())
        }

        fn link_state(&self) -> LinkState {
            if self.0.gave_up.load(Ordering::Acquire) {
                LinkState::Failed
            } else if self.0.down.load(Ordering::Acquire) {
                LinkState::Reconnecting
            } else {
                LinkState::Active
            }
        }

        fn dropped(&self) -> u64 {
            self.0.dropped.load(Ordering::Acquire)
        }
    }

    /// An engine publishing to a [`FlakyLink`], started and sending.
    fn flaky_start(
        fakes: &Fakes,
        link: &Arc<Link>,
    ) -> (Engine, crate::InProcessControl, EventStream) {
        let engine = engine(
            share(&fakes.backends),
            cpu_draw(),
            Box::new(FlakyLink(Arc::clone(link))),
        );
        let control = connected(&engine);
        let mut events = control.events();
        start(&control, &mut events);
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || link
                .sent
                .load(Ordering::Acquire)
                >= 2),
            "nothing was published"
        );
        (engine, control, events)
    }

    /// Pumps until the output is in `state`.
    fn reaches(control: &dyn Control, fakes: &Fakes, state: OutputState) -> bool {
        pump_until(&fakes.clock, &fakes.audio, || {
            output_state(control, "yt") == state
        })
    }

    /// A publish link that drops is not a failure: the output goes `reconnecting`
    /// while it is down and `active` once it is back, one `output.state_changed`
    /// each, and keeps running.
    #[test]
    fn a_lost_link_reports_reconnecting_then_active() {
        let fakes = fakes();
        let link = Arc::new(Link::default());
        let (engine, control, mut events) = flaky_start(&fakes, &link);

        link.down.store(true, Ordering::Release);
        assert!(
            reaches(&control, &fakes, OutputState::Reconnecting),
            "a lost link left the output {:?}",
            output_state(&control, "yt")
        );
        assert_eq!(output_steps(&mut events), vec![OutputState::Reconnecting]);

        link.down.store(false, Ordering::Release);
        assert!(
            reaches(&control, &fakes, OutputState::Active),
            "a restored link left the output {:?}",
            output_state(&control, "yt")
        );
        assert_eq!(output_steps(&mut events), vec![OutputState::Active]);
        assert!(engine.output_stats().is_some(), "the pipeline stopped");

        block_on(control.call(output_call("yt", false))).expect("output.stop");
        assert_eq!(
            output_steps(&mut events),
            vec![OutputState::Stopping, OutputState::Stopped]
        );
    }

    /// While its link is down an output's stats say `poor`, count what the target
    /// dropped, and stop counting bytes as sent (protocol 0.57).
    #[test]
    fn a_lost_link_makes_the_output_poor_in_its_stats() {
        let fakes = fakes();
        let link = Arc::new(Link::default());
        let (engine, control, mut events) = flaky_start(&fakes, &link);
        let yt = |events: &mut EventStream| {
            engine.emit_stats_tick();
            drain(events)
                .into_iter()
                .filter_map(|event| match event {
                    Event::StatsTick { mut outputs, .. } => outputs.pop(),
                    _ => None,
                })
                .next_back()
                .expect("a stats.tick for yt")
        };
        let mut up = yt(&mut events);
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || {
                up = yt(&mut events);
                up.frames > 0
            }),
            "no frame counted as sent: {up:?}"
        );
        assert!(up.bytes > 0, "{up:?}");

        link.down.store(true, Ordering::Release);
        assert!(reaches(&control, &fakes, OutputState::Reconnecting));
        let before = yt(&mut events).bytes;
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || link
                .dropped
                .load(Ordering::Acquire)
                >= 2),
            "nothing was dropped while the link was down"
        );
        let down = yt(&mut events);
        assert_eq!(
            down.health,
            jstream_protocol::OutputHealth::Poor,
            "{down:?}"
        );
        assert!(down.dropped_packets >= 2, "{down:?}");
        assert_eq!(down.bytes, before, "bytes counted while the link was down");

        link.down.store(false, Ordering::Release);
        block_on(control.call(output_call("yt", false))).expect("output.stop");
    }

    /// Giving up reconnecting fails the output from `reconnecting`, as a publish
    /// error fails it from `active`.
    #[test]
    fn giving_up_while_reconnecting_fails_the_output() {
        let fakes = fakes();
        let link = Arc::new(Link::default());
        let (engine, control, mut events) = flaky_start(&fakes, &link);

        link.down.store(true, Ordering::Release);
        assert!(reaches(&control, &fakes, OutputState::Reconnecting));
        assert_eq!(output_steps(&mut events), vec![OutputState::Reconnecting]);

        link.gave_up.store(true, Ordering::Release);
        assert!(
            fails(&engine, &control, &fakes),
            "giving up left the output {:?}",
            output_state(&control, "yt")
        );
        assert_eq!(output_steps(&mut events), vec![OutputState::Failed]);
    }

    /// Tags the publish target dropped while its link was down count in
    /// `stats.tick`'s `output_packets`, up to the final tick when the output stops.
    #[test]
    fn drops_while_reconnecting_count_in_stats_tick() {
        let fakes = fakes();
        let link = Arc::new(Link::default());
        let (_engine, control, mut events) = flaky_start(&fakes, &link);

        link.down.store(true, Ordering::Release);
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || link
                .dropped
                .load(Ordering::Acquire)
                >= 20),
            "too few tags were sent while the link was down"
        );
        link.down.store(false, Ordering::Release);
        assert!(reaches(&control, &fakes, OutputState::Active));

        block_on(control.call(output_call("yt", false))).expect("output.stop");
        let dropped = link.dropped.load(Ordering::Acquire);
        let last_tick = drain(&mut events)
            .into_iter()
            .filter_map(|event| match event {
                Event::StatsTick { drops, .. } => Some(drops),
                _ => None,
            })
            .next_back()
            .expect("no stats.tick");
        assert!(
            last_tick.output_packets >= dropped,
            "{dropped} tags dropped while reconnecting, but the last tick says {last_tick:?}"
        );
    }

    // --- Item: a stalled publish target cannot hold `output.stop` past the drain bound.

    /// A publish target whose peer has stopped reading: once `stalled`, every send
    /// blocks for 50 ms, and it ignores [`LivePublish::stop_by`] as a destination that
    /// cannot cut a write short would. Records the deadline it was given. A send is
    /// short next to `OUTPUT_GRACE`, so the output thread, which may still be in one
    /// when the deadline passes, ends inside the grace even on a slow runner and the
    /// stop sees what it abandoned; a send that outlasts the grace is
    /// `a_send_that_ignores_the_stop_deadline_cannot_hold_output_stop`'s case.
    struct StalledPeer {
        stalled: Arc<AtomicBool>,
        sent: Arc<AtomicU64>,
        deadline: Arc<Mutex<Option<Instant>>>,
    }

    impl LivePublish for StalledPeer {
        fn send_tag(&mut self, _tag: &FlvTag) -> Result<(), PublishError> {
            if self.stalled.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(50));
            }
            self.sent.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }

        fn poll(&mut self) -> Result<(), PublishError> {
            Ok(())
        }

        fn finish(&mut self) -> Result<(), PublishError> {
            Ok(())
        }

        fn stop_by(&mut self, deadline: Instant) {
            *self.deadline.lock().expect("deadline") = Some(deadline);
        }
    }

    /// The peer stalls while well over a hundred packets queue up behind it. Draining
    /// them all at 50 ms a tag would hold `output.stop` for several seconds; the stop
    /// gives up after `STOP_DRAIN`, drops the rest, counts it in the final
    /// `stats.tick`'s `output_packets`, and passes its deadline to the target.
    #[test]
    fn a_stalled_peer_cannot_hold_output_stop() {
        use crate::runtime::STOP_DRAIN;

        let fakes = fakes();
        let stalled = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(AtomicU64::new(0));
        let deadline = Arc::new(Mutex::new(None));
        let engine = engine(
            share(&fakes.backends),
            cpu_draw(),
            Box::new(StalledPeer {
                stalled: Arc::clone(&stalled),
                sent: Arc::clone(&sent),
                deadline: Arc::clone(&deadline),
            }),
        );
        let control = connected(&engine);
        let mut events = control.events();
        start(&control, &mut events);
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || sent.load(Ordering::Acquire)
                >= 5),
            "nothing was published"
        );

        stalled.store(true, Ordering::Release);
        // Two simulated seconds of media: about 60 video and 90 audio packets.
        for _ in 0..40 {
            pump(&fakes.clock, &fakes.audio);
        }
        drain(&mut events);

        let started = Instant::now();
        block_on(control.call(output_call("yt", false))).expect("output.stop");
        let took = started.elapsed();
        assert!(
            took < STOP_DRAIN + Duration::from_secs(2),
            "output.stop took {took:?} behind a stalled peer"
        );
        let given = deadline.lock().expect("deadline").take();
        assert!(given.is_some(), "the target was not told the stop deadline");
        let last_tick = drain(&mut events)
            .into_iter()
            .filter_map(|event| match event {
                Event::StatsTick { drops, .. } => Some(drops),
                _ => None,
            })
            .next_back()
            .expect("no stats.tick");
        assert!(
            last_tick.output_packets > 0,
            "the abandoned packets were not counted: {last_tick:?}"
        );
        assert_eq!(output_state(&control, "yt"), OutputState::Stopped);
    }

    /// A publish target whose send, once `stalled`, blocks until `release` fires (or
    /// 30 s pass) and ignores [`LivePublish::stop_by`]: a destination that neither
    /// looks at the deadline nor can be cut short. Records when it returns.
    struct BlockedPeer {
        stalled: Arc<AtomicBool>,
        sent: Arc<AtomicU64>,
        release: mpsc::Receiver<()>,
        returned: Arc<AtomicBool>,
    }

    impl LivePublish for BlockedPeer {
        fn send_tag(&mut self, _tag: &FlvTag) -> Result<(), PublishError> {
            if self.stalled.load(Ordering::Acquire) {
                let _ = self.release.recv_timeout(Duration::from_secs(30));
                self.returned.store(true, Ordering::Release);
            }
            self.sent.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }

        fn poll(&mut self) -> Result<(), PublishError> {
            Ok(())
        }

        fn finish(&mut self) -> Result<(), PublishError> {
            Ok(())
        }
    }

    /// #138: a send that blocks for good, ignoring the stop deadline, cannot hold
    /// `output.stop` either. The stop returns about `STOP_DRAIN` after it began and
    /// settles `stopped`, before the send has returned; the output thread ends once it
    /// does. Before, the stop waited out the blocked send (30 s here).
    #[test]
    fn a_send_that_ignores_the_stop_deadline_cannot_hold_output_stop() {
        use crate::runtime::STOP_DRAIN;

        let fakes = fakes();
        let stalled = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(AtomicU64::new(0));
        let returned = Arc::new(AtomicBool::new(false));
        let (release, release_rx) = mpsc::channel();
        let engine = engine(
            share(&fakes.backends),
            cpu_draw(),
            Box::new(BlockedPeer {
                stalled: Arc::clone(&stalled),
                sent: Arc::clone(&sent),
                release: release_rx,
                returned: Arc::clone(&returned),
            }),
        );
        let control = connected(&engine);
        let mut events = control.events();
        start(&control, &mut events);
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || sent.load(Ordering::Acquire)
                >= 5),
            "nothing was published"
        );

        stalled.store(true, Ordering::Release);
        for _ in 0..10 {
            pump(&fakes.clock, &fakes.audio);
        }
        drain(&mut events);

        let started = Instant::now();
        block_on(control.call(output_call("yt", false))).expect("output.stop");
        let took = started.elapsed();
        let still_blocked = !returned.load(Ordering::Acquire);
        // Lets the left-behind output thread end.
        let _ = release.send(());
        assert!(
            took < STOP_DRAIN + Duration::from_secs(3),
            "output.stop took {took:?} behind a send that ignores the deadline"
        );
        assert!(
            still_blocked,
            "the send returned before the stop did, so nothing was left behind"
        );
        assert_eq!(output_state(&control, "yt"), OutputState::Stopped);
    }

    /// A publish target whose next send, once `stalled`, blocks until `release` fires
    /// (or 30 s pass), ignoring [`LivePublish::stop_by`] like [`BlockedPeer`]. It says when a
    /// send has blocked (`blocked`) and when the output thread has finished it
    /// (`finished`), so the test waits on those instead of on the clock.
    struct StuckPeer {
        stalled: Arc<AtomicBool>,
        sent: Arc<AtomicU64>,
        blocked: Arc<AtomicBool>,
        release: mpsc::Receiver<()>,
        finished: mpsc::Sender<()>,
    }

    impl LivePublish for StuckPeer {
        fn send_tag(&mut self, _tag: &FlvTag) -> Result<(), PublishError> {
            // One send stalls; once released, the rest go through. A packet can make
            // more than one tag (the first H.264 keyframe brings its sequence header),
            // and a second stall would outlast the test's wait for the thread to end.
            if self.stalled.swap(false, Ordering::AcqRel) {
                self.blocked.store(true, Ordering::Release);
                let _ = self.release.recv_timeout(Duration::from_secs(30));
            }
            self.sent.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }

        fn poll(&mut self) -> Result<(), PublishError> {
            Ok(())
        }

        fn finish(&mut self) -> Result<(), PublishError> {
            let _ = self.finished.send(());
            Ok(())
        }
    }

    /// #385: a send that outlasts `OUTPUT_GRACE` leaves the output thread behind with
    /// packets still queued. The stop counts them itself, so the final `stats.tick`'s
    /// `output_packets` has them; before, the thread counted them only once the send
    /// returned, after the latch, and the final tick said 0. The released thread must
    /// still end.
    #[test]
    fn packets_left_behind_a_send_past_the_grace_count_in_the_final_tick() {
        let fakes = fakes();
        let stalled = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(AtomicU64::new(0));
        let blocked = Arc::new(AtomicBool::new(false));
        let (release, release_rx) = mpsc::channel();
        let (finished_tx, finished) = mpsc::channel();
        let engine = engine(
            share(&fakes.backends),
            cpu_draw(),
            Box::new(StuckPeer {
                stalled: Arc::clone(&stalled),
                sent: Arc::clone(&sent),
                blocked: Arc::clone(&blocked),
                release: release_rx,
                finished: finished_tx,
            }),
        );
        let control = connected(&engine);
        let mut events = control.events();
        start(&control, &mut events);
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || sent.load(Ordering::Acquire)
                >= 5),
            "nothing was published"
        );

        stalled.store(true, Ordering::Release);
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || blocked
                .load(Ordering::Acquire)),
            "no send blocked"
        );
        // Encoded behind the blocked send; the stop's encoder finish flushes them into
        // the output's queues before it waits for the thread.
        for _ in 0..10 {
            pump(&fakes.clock, &fakes.audio);
        }
        drain(&mut events);

        block_on(control.call(output_call("yt", false))).expect("output.stop");
        let left_behind = finished.try_recv().is_err();
        let final_tick = last_stats_tick(&mut events);
        // Lets the left-behind output thread end, and waits until it has.
        let _ = release.send(());
        let ended = finished.recv_timeout(HANG_BOUND).is_ok();

        assert!(
            left_behind,
            "the output thread finished before the stop returned, so nothing was left behind"
        );
        assert!(
            final_tick.output_packets > 0,
            "the packets left behind the stuck send were not counted: {final_tick:?}"
        );
        assert!(ended, "the left-behind output thread did not end");
        assert_eq!(output_state(&control, "yt"), OutputState::Stopped);
    }

    /// The drops of the last `stats.tick` in `events`.
    fn last_stats_tick(events: &mut EventStream) -> jstream_protocol::DropCounts {
        drain(events)
            .into_iter()
            .filter_map(|event| match event {
                Event::StatsTick { drops, .. } => Some(drops),
                _ => None,
            })
            .next_back()
            .expect("no stats.tick")
    }

    // --- Item: a recording output writes fragmented MP4 to the path it names.

    /// A directory under the system temp dir, removed on drop.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("jstream-core-record-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }

        fn join(&self, name: &str) -> std::path::PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An engine with no publish target and no opener, and the recording output `rec`
    /// created through the protocol, recording to `path`.
    fn recording_engine(
        fakes: &Fakes,
        draw: OpenDraw,
        path: &std::path::Path,
    ) -> (Engine, InProcessControl, EventStream) {
        let engine = Engine::new()
            .with_backends(share(&fakes.backends))
            .with_draw(draw);
        let control = connected(&engine);
        let events = control.events();
        batch(
            &control,
            edits(vec![Edit::OutputCreate {
                output_id: OutputId::new("rec"),
                kind: OutputKind::Record {
                    path: path.to_str().expect("a UTF-8 temp path").into(),
                    container: RecordContainer::Mp4,
                },
            }]),
        )
        .expect("output.create");
        (engine, control, events)
    }

    /// The top-level MP4 boxes of `bytes`, by type.
    fn boxes(bytes: &[u8]) -> Vec<String> {
        let mut found = Vec::new();
        let mut at = 0;
        while at + 8 <= bytes.len() {
            let size = u32::from_be_bytes(bytes[at..at + 4].try_into().expect("4 bytes")) as usize;
            found.push(String::from_utf8_lossy(&bytes[at + 4..at + 8]).into_owned());
            if size < 8 {
                break;
            }
            at += size;
        }
        found
    }

    /// `output.start` on a recording output needs no publish target: it records to the
    /// collection's path as `<path>.part` while active, and `output.stop` renames it
    /// into place, an `ftyp`, a `moov` and then fragments.
    #[test]
    fn a_recording_output_writes_mp4_to_its_path() {
        let dir = TempDir::new("writes");
        let path = dir.join("show.mp4");
        let part = dir.join("show.mp4.part");
        let fakes = fakes();
        let (engine, control, mut events) = recording_engine(&fakes, cpu_draw(), &path);
        drain(&mut events);

        block_on(control.call(output_call("rec", true))).expect("output.start");
        assert_eq!(
            output_steps(&mut events),
            vec![OutputState::Starting, OutputState::Active]
        );
        assert!(part.exists(), "nothing is written to the .part name");
        assert!(!path.exists(), "the destination exists before the stop");
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || {
                engine.output_stats().is_some_and(|stats| {
                    stats.video_subscriber.delivered >= 70 && stats.audio_subscriber.delivered >= 10
                })
            }),
            "media never reached the recording"
        );
        block_on(control.call(output_call("rec", false))).expect("output.stop");
        assert_eq!(
            output_steps(&mut events),
            vec![OutputState::Stopping, OutputState::Stopped]
        );

        assert!(!part.exists(), "the .part file was not renamed");
        let bytes = std::fs::read(&path).expect("the finished recording");
        let found = boxes(&bytes);
        assert_eq!(found.first().map(String::as_str), Some("ftyp"), "{found:?}");
        assert_eq!(found.get(1).map(String::as_str), Some("moov"), "{found:?}");
        let fragments = found.iter().filter(|name| *name == "moof").count();
        assert!(
            fragments >= 2,
            "a keyframe every two seconds over more than two seconds: {found:?}"
        );
        assert_eq!(
            found.iter().filter(|name| *name == "mdat").count(),
            fragments,
            "{found:?}"
        );
    }

    /// A recording never overwrites: a start whose path is taken fails with
    /// `action_failed`, settles `failed`, and leaves the existing file as it was.
    #[test]
    fn a_recording_start_fails_when_the_file_exists() {
        let dir = TempDir::new("exists");
        let path = dir.join("show.mp4");
        std::fs::write(&path, b"keep me").expect("existing file");
        let fakes = fakes();
        let (engine, control, mut events) = recording_engine(&fakes, cpu_draw(), &path);
        drain(&mut events);

        let err = block_on(control.call(output_call("rec", true))).expect_err("refused");
        assert_eq!(code(&err), Some("action_failed"));
        assert!(err.to_string().contains("opening the recording"), "{err}");
        assert_eq!(
            output_steps(&mut events),
            vec![OutputState::Starting, OutputState::Failed]
        );
        assert!(engine.output_stats().is_none());
        assert_eq!(std::fs::read(&path).ok().as_deref(), Some(&b"keep me"[..]));
        assert!(!dir.join("show.mp4.part").exists());
    }

    /// The collection takes `/…` and `C:\…` on every platform; a path that is not
    /// absolute on the engine's machine fails the start rather than recording to a
    /// relative name in the working directory.
    #[test]
    fn a_recording_path_not_absolute_here_fails_the_start() {
        let foreign = if cfg!(windows) {
            "/jstream-record-test/show.mp4"
        } else {
            "C:\\jstream-record-test\\show.mp4"
        };
        let fakes = fakes();
        let (_engine, control, mut events) =
            recording_engine(&fakes, cpu_draw(), std::path::Path::new(foreign));
        drain(&mut events);

        let err = block_on(control.call(output_call("rec", true))).expect_err("refused");
        assert_eq!(code(&err), Some("action_failed"));
        assert!(err.to_string().contains("not absolute"), "{err}");
        assert_eq!(
            output_steps(&mut events),
            vec![OutputState::Starting, OutputState::Failed]
        );
        assert!(!std::path::Path::new(&format!("{foreign}.part")).exists());
    }

    /// A start that fails after its recording was opened (here, the compositor) removes
    /// the `.part` file, so a later start with a working compositor records.
    #[test]
    fn a_failed_recording_start_leaves_no_part_file() {
        let dir = TempDir::new("failed-start");
        let path = dir.join("show.mp4");
        let fakes = fakes();
        let working = Arc::new(AtomicBool::new(false));
        let draw: OpenDraw = {
            let working = Arc::clone(&working);
            Arc::new(move |context: Option<&jstream_platform::GpuContext>| {
                if working.load(Ordering::Acquire) {
                    cpu_draw()(context)
                } else {
                    Err("no adapter".to_owned())
                }
            })
        };
        let (_engine, control, mut events) = recording_engine(&fakes, draw, &path);
        drain(&mut events);

        let err = block_on(control.call(output_call("rec", true))).expect_err("refused");
        assert_eq!(code(&err), Some("action_failed"));
        assert_eq!(
            output_steps(&mut events),
            vec![OutputState::Starting, OutputState::Failed]
        );
        assert!(
            !dir.join("show.mp4.part").exists(),
            "the .part file was left"
        );
        assert!(!path.exists());

        working.store(true, Ordering::Release);
        block_on(control.call(output_call("rec", true))).expect("output.start");
        block_on(control.call(output_call("rec", false))).expect("output.stop");
        assert!(path.exists(), "the second start did not record");
    }

    // --- Item: outputs that encode the same mix with the same profile share one
    // encoder session (ADR-0080), so a recording can run while streaming.

    /// The `yt` publish output and the `rec` recording output to `path`, on one engine.
    fn streaming_and_recording(
        fakes: &Fakes,
        draw: OpenDraw,
        publish: &RecordingPublish,
        path: &std::path::Path,
    ) -> (Engine, InProcessControl, EventStream) {
        let engine = engine(share(&fakes.backends), draw, Box::new(publish.clone()));
        let control = connected(&engine);
        let events = control.events();
        batch(
            &control,
            edits(vec![Edit::OutputCreate {
                output_id: OutputId::new("rec"),
                kind: OutputKind::Record {
                    path: path.to_str().expect("a UTF-8 temp path").into(),
                    container: RecordContainer::Mp4,
                },
            }]),
        )
        .expect("output.create");
        (engine, control, events)
    }

    /// `output.status` for `id`: its state, its encoder session, and the sessions open.
    fn encoder_status(
        control: &dyn Control,
        id: &str,
    ) -> (OutputState, Option<EncoderSessionInfo>, u32, u32) {
        match query(
            control,
            Query::OutputStatus {
                output_id: OutputId::new(id),
            },
        ) {
            Ok(QueryResult::OutputStatus {
                state,
                encoder,
                encoder_sessions,
                hardware_sessions,
                ..
            }) => (state, encoder, encoder_sessions, hardware_sessions),
            other => panic!("not an output status: {other:?}"),
        }
    }

    fn video_tags(publish: &RecordingPublish) -> usize {
        publish
            .tags()
            .iter()
            .filter(|tag| tag.tag_type == TagType::Video)
            .count()
    }

    /// Recording while streaming on the default profile opens one encoder session that
    /// both outputs report sharing. Stopping the recording finishes its file and leaves
    /// the stream running on the same session.
    #[test]
    fn a_recording_shares_the_streams_encoder() {
        let dir = TempDir::new("shared");
        let path = dir.join("show.mp4");
        let fakes = fakes();
        let publish = RecordingPublish::new();
        let (engine, control, mut events) =
            streaming_and_recording(&fakes, cpu_draw(), &publish, &path);
        drain(&mut events);

        block_on(control.call(output_call("yt", true))).expect("yt start");
        block_on(control.call(output_call("rec", true))).expect("rec start");
        for (id, other) in [("yt", "rec"), ("rec", "yt")] {
            let (state, encoder, sessions, hardware) = encoder_status(&control, id);
            assert_eq!(state, OutputState::Active, "{id}");
            assert_eq!(
                encoder,
                Some(EncoderSessionInfo {
                    profile_id: None,
                    shared_with: vec![OutputId::new(other)],
                    hardware: false,
                    dedicated: false,
                }),
                "{id}"
            );
            assert_eq!((sessions, hardware), (1, 0), "{id}");
        }

        assert!(
            pump_until(&fakes.clock, &fakes.audio, || {
                engine
                    .output_stats_of(&OutputId::new("rec"))
                    .is_some_and(|stats| stats.video_subscriber.delivered >= 70)
                    && video_tags(&publish) >= 70
            }),
            "media never reached both outputs"
        );
        block_on(control.call(output_call("rec", false))).expect("rec stop");

        let (state, encoder, sessions, _) = encoder_status(&control, "yt");
        assert_eq!(state, OutputState::Active);
        assert_eq!(encoder.map(|encoder| encoder.shared_with), Some(vec![]));
        assert_eq!(sessions, 1);
        let before = video_tags(&publish);
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || video_tags(&publish)
                >= before + 30),
            "the stream stopped with the recording"
        );

        let bytes = std::fs::read(&path).expect("the finished recording");
        let found = boxes(&bytes);
        assert_eq!(found.first().map(String::as_str), Some("ftyp"), "{found:?}");
        assert_eq!(found.get(1).map(String::as_str), Some("moov"), "{found:?}");
        assert!(found.iter().any(|name| name == "moof"), "{found:?}");

        block_on(control.call(output_call("yt", false))).expect("yt stop");
        assert_eq!(encoder_status(&control, "yt").2, 0);
    }

    /// A recording with an audio chain of its own (ADR-0080) gets an output tap and its
    /// own audio encode, while its video still shares the stream's session; both
    /// outputs get audio, and the recording's file finishes.
    #[test]
    fn an_output_with_an_audio_chain_encodes_its_own_audio() {
        let dir = TempDir::new("own-audio");
        let path = dir.join("show.mp4");
        let fakes = fakes();
        let publish = RecordingPublish::new();
        let (engine, control, mut events) =
            streaming_and_recording(&fakes, cpu_draw(), &publish, &path);
        batch(
            &control,
            edits(vec![Edit::FilterAdd {
                owner: jstream_protocol::FilterOwner::Output {
                    output_id: OutputId::new("rec"),
                },
                chain: jstream_protocol::FilterChain::Audio,
                filter_id: jstream_protocol::FilterId::new("trim"),
                kind: "gain".into(),
                settings: [("gain_db", serde_json::json!(-6.0))].into_iter().collect(),
                enabled: None,
                index: None,
            }]),
        )
        .expect("filter.add");
        drain(&mut events);

        block_on(control.call(output_call("yt", true))).expect("yt start");
        block_on(control.call(output_call("rec", true))).expect("rec start");
        let (state, encoder, sessions, _) = encoder_status(&control, "rec");
        assert_eq!(state, OutputState::Active);
        assert_eq!(
            encoder.map(|encoder| encoder.shared_with),
            Some(vec![OutputId::new("yt")]),
            "the video session is still shared"
        );
        assert_eq!(sessions, 1);
        let audio_tags = || {
            publish
                .tags()
                .iter()
                .filter(|tag| tag.tag_type == TagType::Audio)
                .count()
        };
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || {
                engine
                    .output_stats_of(&OutputId::new("rec"))
                    .is_some_and(|stats| {
                        stats.video_subscriber.delivered >= 70
                            && stats.audio_subscriber.delivered >= 10
                    })
                    && audio_tags() >= 10
            }),
            "audio never reached both outputs"
        );
        block_on(control.call(output_call("rec", false))).expect("rec stop");
        let before = audio_tags();
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || audio_tags() >= before + 10),
            "the stream's audio stopped with the recording"
        );

        let bytes = std::fs::read(&path).expect("the finished recording");
        let found = boxes(&bytes);
        assert_eq!(found.get(1).map(String::as_str), Some("moov"), "{found:?}");
        assert!(found.iter().any(|name| name == "moof"), "{found:?}");
        block_on(control.call(output_call("yt", false))).expect("yt stop");
    }

    /// A program-stage failure fails every output on it. A start after that waits for
    /// the failed stage to go and opens a new one.
    #[test]
    fn a_render_panic_fails_every_output() {
        let dir = TempDir::new("both-fail");
        let fakes = fakes();
        let armed = Arc::new(AtomicBool::new(false));
        let draw: OpenDraw = {
            let armed = Arc::clone(&armed);
            Arc::new(move |context| {
                let armed = Arc::clone(&armed);
                let mut inner = (cpu_draw())(context)?;
                let draw: DrawFn = Box::new(move |shot, live, captures, frames, at| {
                    assert!(!armed.load(Ordering::Acquire), "compositor blew up");
                    inner(shot, live, captures, frames, at)
                });
                Ok(draw)
            })
        };
        let publish = RecordingPublish::new();
        let (engine, control, _events) =
            streaming_and_recording(&fakes, draw, &publish, &dir.join("show.mp4"));
        block_on(control.call(output_call("yt", true))).expect("yt start");
        block_on(control.call(output_call("rec", true))).expect("rec start");
        assert!(pump_until(&fakes.clock, &fakes.audio, || readbacks(
            &engine
        ) >= 2));

        armed.store(true, Ordering::Release);
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || {
                output_state(&control, "yt") == OutputState::Failed
                    && output_state(&control, "rec") == OutputState::Failed
                    && engine.output_stats().is_none()
            }),
            "a render panic left yt {:?} and rec {:?}",
            output_state(&control, "yt"),
            output_state(&control, "rec")
        );

        armed.store(false, Ordering::Release);
        let engine = engine.with_publish(Box::new(RecordingPublish::new()));
        std::fs::remove_file(dir.join("show.mp4")).ok();
        block_on(control.call(output_call("yt", true))).expect("yt restart");
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || readbacks(&engine) >= 2),
            "the new program stage never drew"
        );
        assert_eq!(output_state(&control, "yt"), OutputState::Active);
        block_on(control.call(output_call("yt", false))).expect("yt stop");
    }

    /// Outputs on different profiles each get a session of their own.
    #[test]
    fn different_profiles_open_separate_sessions() {
        let dir = TempDir::new("separate");
        let path = dir.join("show.mp4");
        let fakes = fakes();
        let publish = RecordingPublish::new();
        let (_engine, control, mut events) =
            streaming_and_recording(&fakes, cpu_draw(), &publish, &path);
        let mut profile = EncoderProfile::DEFAULT;
        profile.video.rate_control = RateControl::Cbr { kbps: 6000 };
        batch(
            &control,
            edits(vec![
                Edit::EncoderCreate {
                    profile_id: EncoderProfileId::new("hq"),
                    name: "Recording".into(),
                    profile,
                },
                Edit::OutputSetEncoder {
                    output_id: OutputId::new("rec"),
                    profile_id: Some(EncoderProfileId::new("hq")),
                },
            ]),
        )
        .expect("encoder edits");
        drain(&mut events);

        block_on(control.call(output_call("yt", true))).expect("yt start");
        block_on(control.call(output_call("rec", true))).expect("rec start");
        let (_, yt, sessions, _) = encoder_status(&control, "yt");
        let (_, rec, _, _) = encoder_status(&control, "rec");
        assert_eq!(sessions, 2);
        assert_eq!(
            yt.map(|encoder| (encoder.profile_id, encoder.shared_with)),
            Some((None, vec![]))
        );
        assert_eq!(
            rec.map(|encoder| (encoder.profile_id, encoder.shared_with)),
            Some((Some(EncoderProfileId::new("hq")), vec![]))
        );
        block_on(control.call(output_call("rec", false))).expect("rec stop");
        block_on(control.call(output_call("yt", false))).expect("yt stop");
    }

    /// An output on a video mix gets an encoder session of its own, not shared with the
    /// program's, fed at the mix's rate: the recording on a half-rate mix gets at most
    /// one frame per two base ticks, and isn't starved while the stream on program draws.
    /// A mix an output shows can't be deleted.
    #[test]
    fn an_output_on_a_mix_encodes_it_at_its_rate() {
        use jstream_protocol::{MixInput, MixResolution, VideoMix, VideoMixId};

        let dir = TempDir::new("mix");
        let fakes = fakes();
        let publish = RecordingPublish::new();
        let (engine, control, mut events) =
            streaming_and_recording(&fakes, cpu_draw(), &publish, &dir.join("show.mp4"));
        batch(
            &control,
            edits(vec![
                Edit::VideoMixCreate {
                    mix_id: VideoMixId::new("half"),
                    name: "Half rate".into(),
                    mix: VideoMix {
                        input: MixInput::Program,
                        resolution: MixResolution {
                            width: 32,
                            height: 18,
                        },
                        fps_divisor: 2,
                    },
                },
                Edit::OutputSetVideoMix {
                    output_id: OutputId::new("rec"),
                    mix_id: Some(VideoMixId::new("half")),
                },
            ]),
        )
        .expect("mix edits");
        drain(&mut events);

        block_on(control.call(output_call("yt", true))).expect("yt start");
        block_on(control.call(output_call("rec", true))).expect("rec start");
        let (_, rec, sessions, _) = encoder_status(&control, "rec");
        assert_eq!(sessions, 2);
        assert_eq!(rec.map(|encoder| encoder.shared_with), Some(vec![]));

        let recorded = |engine: &Engine| {
            engine
                .output_stats_of(&OutputId::new("rec"))
                .map_or(0, |stats| stats.video_subscriber.delivered)
        };
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || video_tags(&publish) >= 90),
            "the stream never got its frames"
        );
        let (full, half) = (video_tags(&publish) as u64, recorded(&engine));
        // The pump moves the clock faster than a debug render thread draws, so most
        // base ticks are skipped and both mixes are drawn on nearly every tick that is:
        // the half-rate mix is held to at most one frame per two base ticks and never
        // more than the program. However many ticks are skipped it gets at least about
        // one frame for every two of the program's, so an overrun doesn't starve it. Its
        // rate on ticks that aren't skipped is checked by
        // `frames_draw_each_feed_once_on_its_ticks`.
        let micros = jstream_platform::MediaClock::now(&fakes.clock).ticks();
        let base = u64::try_from(micros / 33_333).expect("clock after zero");
        assert!(
            half + 6 >= full / 2 && half <= full + 2 && half <= base / 2 + 2,
            "{half} recorded frames against {full} streamed over {base} base ticks"
        );

        let refused = batch(
            &control,
            edits(vec![Edit::VideoMixDelete {
                mix_id: VideoMixId::new("half"),
            }]),
        );
        assert_eq!(
            refused.as_ref().err().and_then(code),
            Some("invalid_argument")
        );
        block_on(control.call(output_call("rec", false))).expect("rec stop");
        block_on(control.call(output_call("yt", false))).expect("yt stop");
    }

    // --- Item: the virtual camera publishes the program, video only.

    /// What a [`ScriptedVcam`] saw, shared with the test.
    #[derive(Default)]
    struct VcamLog {
        starts: Vec<VcamFormat>,
        stops: u64,
        /// The first and the middle pixel of every frame published, as given (BGRA).
        pixels: Vec<([u8; 4], [u8; 4])>,
        started: bool,
        /// Refuse `start` with `Unsupported`.
        refuse: bool,
        /// Fail `publish` with `Os` once this many frames went through.
        fail_after: Option<usize>,
    }

    /// A virtual-camera sink a test scripts and watches.
    struct ScriptedVcam(Arc<Mutex<VcamLog>>);

    impl VcamSink for ScriptedVcam {
        fn start(&mut self, format: VcamFormat) -> Result<(), PlatformError> {
            let mut log = self.0.lock().expect("vcam log");
            if log.refuse {
                return Err(PlatformError::Unsupported {
                    what: "this camera size".into(),
                });
            }
            assert!(!log.started, "started twice");
            log.started = true;
            log.starts.push(format);
            Ok(())
        }

        fn publish(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
            let mut log = self.0.lock().expect("vcam log");
            assert!(log.started, "published while stopped");
            if log
                .fail_after
                .is_some_and(|after| log.pixels.len() >= after)
            {
                return Err(PlatformError::Os {
                    call: "publish",
                    code: Some(5),
                    message: "the camera went away".into(),
                });
            }
            let VideoSurface::Cpu(frame) = frame else {
                panic!("not a CPU frame");
            };
            let info = frame.info();
            assert_eq!(info.format, PixelFormat::Bgra8);
            assert_eq!(Some(info.size), log.starts.last().map(|f| f.size));
            let pixel = |x: u32, y: u32| {
                let row = frame.row(0, y).expect("a row");
                let at = x as usize * 4;
                [row[at], row[at + 1], row[at + 2], row[at + 3]]
            };
            let mid = (info.size.width() / 2, info.size.height() / 2);
            log.pixels.push((pixel(0, 0), pixel(mid.0, mid.1)));
            Ok(())
        }

        fn stop(&mut self) -> Result<(), PlatformError> {
            let mut log = self.0.lock().expect("vcam log");
            if log.started {
                log.started = false;
                log.stops += 1;
            }
            Ok(())
        }

        fn format(&self) -> Option<VcamFormat> {
            let log = self.0.lock().expect("vcam log");
            log.started.then(|| *log.starts.last().expect("a start"))
        }
    }

    /// A draw that fills the program frame with one RGBA colour, so the test can tell
    /// the camera got the composite, swizzled to BGRA.
    fn solid(rgba: [u8; 4]) -> OpenDraw {
        Arc::new(move |_| {
            let draw: DrawFn = Box::new(move |_, _, _, frames, _| {
                for frame in frames.iter_mut().filter(|frame| frame.due) {
                    frame.out.clear();
                    for _ in 0..frame.size.width() as usize * frame.size.height() as usize {
                        frame.out.extend_from_slice(&rgba);
                    }
                }
                Ok(None)
            });
            Ok(draw)
        })
    }

    /// An engine whose backends have `vcam` (or none), a `canvas`, and the virtual
    /// camera output `cam` created through the protocol. The program frame is the fake
    /// capture's 64x36. No publish target or opener unless `publish` is given, for an
    /// RTMP output `yt` beside it.
    fn vcam_engine(
        fakes: &Fakes,
        draw: OpenDraw,
        vcam: Option<Box<dyn VcamSink>>,
        canvas: (u32, u32),
        publish: Option<Box<dyn LivePublish>>,
    ) -> (Engine, InProcessControl, EventStream) {
        let mut backends = share(&fakes.backends);
        backends.vcam = vcam;
        let mut engine = Engine::new().with_backends(backends).with_draw(draw);
        if let Some(publish) = publish {
            engine = engine
                .with_output(OutputId::new("yt"))
                .with_publish(publish);
        }
        let control = connected(&engine);
        let mut events = control.events();
        batch(
            &control,
            edits(vec![
                Edit::CanvasSetSize {
                    width: canvas.0,
                    height: canvas.1,
                },
                Edit::OutputCreate {
                    output_id: OutputId::new("cam"),
                    kind: OutputKind::VirtualCam,
                },
            ]),
        )
        .expect("output.create");
        drain(&mut events);
        (engine, control, events)
    }

    fn scripted(log: &Arc<Mutex<VcamLog>>) -> Option<Box<dyn VcamSink>> {
        Some(Box::new(ScriptedVcam(Arc::clone(log))))
    }

    fn start_output(control: &dyn Control, events: &mut EventStream, output: &str) {
        block_on(control.call(output_call(output, true))).expect("output.start");
        assert_eq!(
            output_steps(events),
            vec![OutputState::Starting, OutputState::Active]
        );
    }

    /// `output.start` on a virtual camera starts the platform's sink at the canvas
    /// size, as BGRA at 30 fps, and publishes every program frame to it with no
    /// publish target, encoder or audio. `output.stop` stops the camera, and the engine
    /// keeps the sink, so the output starts again.
    #[test]
    fn a_virtual_camera_publishes_the_program_until_stopped() {
        let fakes = fakes();
        let log = Arc::new(Mutex::new(VcamLog::default()));
        let (engine, control, mut events) = vcam_engine(
            &fakes,
            solid([10, 20, 30, 255]),
            scripted(&log),
            (64, 36),
            None,
        );

        for run in 1..=2u64 {
            // `readbacks` counts this start's ticks only, and the previous run may have
            // published any number of frames past its five, so this run waits for five
            // frames of its own (#415).
            let before = log.lock().expect("vcam log").pixels.len();
            start_output(&control, &mut events, "cam");
            assert!(
                pump_until(&fakes.clock, &fakes.audio, || {
                    log.lock().expect("vcam log").pixels.len() >= before + 5
                }),
                "start {run}: the camera got no frames"
            );
            let stats = engine.output_stats().expect("running");
            assert_eq!(
                stats.video.received, 0,
                "the virtual camera encodes nothing"
            );
            assert_eq!(stats.audio.received, 0, "the virtual camera has no audio");
            // Each frame published was read back first, on this start's render thread.
            assert!(
                stats.readbacks >= 5,
                "start {run}: {} readbacks for {} frames published",
                stats.readbacks,
                log.lock().expect("vcam log").pixels.len() - before
            );

            block_on(control.call(output_call("cam", false))).expect("output.stop");
            assert_eq!(
                output_steps(&mut events),
                vec![OutputState::Stopping, OutputState::Stopped]
            );
            let log = log.lock().expect("vcam log");
            assert!(!log.started, "the camera is still up after the stop");
            assert_eq!(log.stops, run);
        }
        let log = log.lock().expect("vcam log");
        assert_eq!(log.starts.len(), 2);
        for format in &log.starts {
            assert_eq!(format.size, FrameSize::new(64, 36).expect("size"));
            assert_eq!(format.pixel_format, PixelFormat::Bgra8);
            assert_eq!(format.frame_period, Timebase::new(1, 30).expect("1/30"));
        }
        assert!(
            log.pixels
                .iter()
                .all(|px| *px == ([30, 20, 10, 255], [30, 20, 10, 255])),
            "the frames are not the composite in BGRA"
        );
    }

    /// The program is drawn at the canvas, not the display (#433): a 64x64 canvas over a
    /// 64x36 display gives the camera whole 64x64 frames, with no letterbox.
    #[test]
    fn a_virtual_camera_gets_the_program_at_the_canvas_size() {
        let fakes = fakes();
        let log = Arc::new(Mutex::new(VcamLog::default()));
        let (_engine, control, mut events) = vcam_engine(
            &fakes,
            solid([10, 20, 30, 255]),
            scripted(&log),
            (64, 64),
            None,
        );
        start_output(&control, &mut events, "cam");
        assert!(pump_until(&fakes.clock, &fakes.audio, || {
            !log.lock().expect("vcam log").pixels.is_empty()
        }));
        block_on(control.call(output_call("cam", false))).expect("output.stop");
        let log = log.lock().expect("vcam log");
        assert_eq!(log.starts[0].size, FrameSize::new(64, 64).expect("size"));
        assert_eq!(log.pixels[0], ([30, 20, 10, 255], [30, 20, 10, 255]));
    }

    /// A virtual camera shares the program stage with a stream: both run at once, and
    /// stopping the camera leaves the stream running.
    #[test]
    fn a_virtual_camera_runs_beside_a_stream() {
        let fakes = fakes();
        let log = Arc::new(Mutex::new(VcamLog::default()));
        let publish = RecordingPublish::new();
        let (engine, control, mut events) = vcam_engine(
            &fakes,
            cpu_draw(),
            scripted(&log),
            (64, 36),
            Some(Box::new(publish.clone())),
        );
        start_output(&control, &mut events, "yt");
        start_output(&control, &mut events, "cam");
        assert!(pump_until(&fakes.clock, &fakes.audio, || {
            log.lock().expect("vcam log").pixels.len() >= 3
                && publish
                    .tags()
                    .iter()
                    .any(|tag| tag.tag_type == TagType::Video)
        }));
        block_on(control.call(output_call("cam", false))).expect("output.stop");
        assert_eq!(log.lock().expect("vcam log").stops, 1);
        assert_eq!(output_state(&control, "yt"), OutputState::Active);
        let before = publish.tags().len();
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || publish.tags().len() > before),
            "the stream stopped with the camera"
        );
        assert!(engine.output_stats().is_some());
        block_on(control.call(output_call("yt", false))).expect("output.stop");
    }

    /// Without a virtual camera in the backends, a start fails before `starting`.
    #[test]
    fn a_virtual_camera_needs_one_in_the_backends() {
        let fakes = fakes();
        let (_engine, control, mut events) = vcam_engine(&fakes, cpu_draw(), None, (64, 36), None);
        let err = block_on(control.call(output_call("cam", true))).expect_err("refused");
        assert_eq!(code(&err), Some("action_failed"));
        assert!(err.to_string().contains("no virtual camera"), "{err}");
        assert!(output_steps(&mut events).is_empty());
        assert_eq!(output_state(&control, "cam"), OutputState::Stopped);
    }

    /// The engine has one camera: a second virtual-camera output can't start while the
    /// first runs, and says why.
    #[test]
    fn a_second_virtual_camera_output_is_refused_while_one_runs() {
        let fakes = fakes();
        let log = Arc::new(Mutex::new(VcamLog::default()));
        let (_engine, control, mut events) =
            vcam_engine(&fakes, cpu_draw(), scripted(&log), (64, 36), None);
        batch(
            &control,
            edits(vec![Edit::OutputCreate {
                output_id: OutputId::new("cam2"),
                kind: OutputKind::VirtualCam,
            }]),
        )
        .expect("output.create");
        drain(&mut events);
        start_output(&control, &mut events, "cam");
        let err = block_on(control.call(output_call("cam2", true))).expect_err("refused");
        assert_eq!(code(&err), Some("action_failed"));
        assert!(
            err.to_string().contains("in use by another output"),
            "{err}"
        );
        block_on(control.call(output_call("cam", false))).expect("output.stop");
        drain(&mut events);
        start_output(&control, &mut events, "cam2");
        block_on(control.call(output_call("cam2", false))).expect("output.stop");
        assert_eq!(log.lock().expect("vcam log").stops, 2);
    }

    /// A camera that refuses to start fails `output.start` with its reason, and the
    /// engine keeps the sink: once the camera accepts, the next start runs.
    #[test]
    fn a_refused_virtual_camera_fails_the_start_and_keeps_the_sink() {
        let fakes = fakes();
        let log = Arc::new(Mutex::new(VcamLog {
            refuse: true,
            ..VcamLog::default()
        }));
        let (_engine, control, mut events) =
            vcam_engine(&fakes, cpu_draw(), scripted(&log), (64, 36), None);

        let err = block_on(control.call(output_call("cam", true))).expect_err("refused");
        assert_eq!(code(&err), Some("action_failed"));
        assert!(
            err.to_string().contains("starting the virtual camera"),
            "{err}"
        );
        assert_eq!(
            output_steps(&mut events),
            vec![OutputState::Starting, OutputState::Failed]
        );

        log.lock().expect("vcam log").refuse = false;
        start_output(&control, &mut events, "cam");
        block_on(control.call(output_call("cam", false))).expect("output.stop");
        assert_eq!(log.lock().expect("vcam log").stops, 1);
    }

    /// A camera that fails a publish fails the output (R-RT-006), and the camera is
    /// stopped and handed back, so the output starts again.
    #[test]
    fn a_failed_virtual_camera_publish_fails_the_output() {
        let fakes = fakes();
        let log = Arc::new(Mutex::new(VcamLog {
            fail_after: Some(3),
            ..VcamLog::default()
        }));
        let (engine, control, mut events) =
            vcam_engine(&fakes, cpu_draw(), scripted(&log), (64, 36), None);
        start_output(&control, &mut events, "cam");
        assert!(
            pump_until(&fakes.clock, &fakes.audio, || {
                output_state(&control, "cam") == OutputState::Failed
                    && engine.output_stats().is_none()
            }),
            "the output did not fail"
        );
        {
            let log = log.lock().expect("vcam log");
            assert_eq!(log.pixels.len(), 3);
            assert!(!log.started, "the camera is still up");
        }

        log.lock().expect("vcam log").fail_after = None;
        drain(&mut events);
        start_output(&control, &mut events, "cam");
        block_on(control.call(output_call("cam", false))).expect("output.stop");
        assert_eq!(log.lock().expect("vcam log").stops, 2);
    }
}

fn rtmp_output(url: &str) -> jstream_protocol::OutputKind {
    jstream_protocol::OutputKind::Rtmp {
        target: jstream_protocol::RtmpTarget::Generic { url: url.into() },
    }
}

fn edits(edits: Vec<Edit>) -> Batch {
    Batch {
        edits,
        expected_rev: None,
        idempotency_key: None,
        ends_gesture: None,
    }
}

fn set_key(output: &str, key: &str) -> Call {
    Call::Action {
        action: Action::OutputSetKey {
            output_id: OutputId::new(output),
            key: jstream_protocol::SecretString::new(key),
        },
    }
}

fn clear_key(output: &str) -> Call {
    Call::Action {
        action: Action::OutputClearKey {
            output_id: OutputId::new(output),
        },
    }
}

fn key_set(control: &dyn Control, output: &str) -> Result<bool, ControlError> {
    match query(
        control,
        Query::OutputStatus {
            output_id: OutputId::new(output),
        },
    )? {
        QueryResult::OutputStatus { key_set, .. } => Ok(key_set),
        other => panic!("not an output status: {other:?}"),
    }
}

/// `output.create`, `output.set_target` and `output.delete` change the collection like
/// any edit (rev, events, `output.list`, `state.get`), the new output can be started,
/// and a running output can't be deleted.
#[test]
fn output_edits_configure_the_collection() {
    use jstream_protocol::{OutputInfo, RtmpTarget};

    let engine = Engine::new();
    let control = connected(&engine);
    let mut events = control.events();
    let rev = batch(
        &control,
        edits(vec![Edit::OutputCreate {
            output_id: OutputId::new("out_yt"),
            kind: rtmp_output("rtmp://a.rtmp.youtube.com/live2"),
        }]),
    )
    .expect("output.create");
    assert_eq!(rev, Rev(1));
    assert!(
        drain(&mut events)
            .iter()
            .any(|e| matches!(e, Event::StateChanged { rev: Rev(1), .. })),
        "an output edit is a state change"
    );
    let listed = |control: &dyn Control| match query(control, Query::OutputList) {
        Ok(QueryResult::OutputList { outputs, .. }) => outputs,
        other => panic!("not an output list: {other:?}"),
    };
    assert_eq!(
        listed(&control),
        vec![OutputInfo {
            output_id: OutputId::new("out_yt"),
            kind: rtmp_output("rtmp://a.rtmp.youtube.com/live2"),
            encoder_profile: None,
            filters: Default::default(),
            video_mix: None,
        }]
    );
    assert_eq!(
        engine.output_kind(&OutputId::new("out_yt")),
        Some(rtmp_output("rtmp://a.rtmp.youtube.com/live2"))
    );

    batch(
        &control,
        edits(vec![Edit::OutputSetTarget {
            output_id: OutputId::new("out_yt"),
            target: RtmpTarget::Generic {
                url: "rtmps://a.rtmps.youtube.com:443/live2".into(),
            },
        }]),
    )
    .expect("output.set_target");
    match query(&control, Query::StateGet).expect("state.get") {
        QueryResult::StateGet { outputs, .. } => assert_eq!(
            outputs,
            vec![OutputInfo {
                output_id: OutputId::new("out_yt"),
                kind: rtmp_output("rtmps://a.rtmps.youtube.com:443/live2"),
                encoder_profile: None,
                filters: Default::default(),
                video_mix: None,
            }]
        ),
        other => panic!("not a state: {other:?}"),
    }

    // The new output is one the placeholder state machine runs.
    block_on(control.call(output_call("out_yt", true))).expect("start");
    let refused = batch(
        &control,
        edits(vec![Edit::OutputDelete {
            output_id: OutputId::new("out_yt"),
        }]),
    )
    .expect_err("delete while running");
    assert_eq!(code(&refused), Some("invalid_argument"));
    block_on(control.call(output_call("out_yt", false))).expect("stop");
    batch(
        &control,
        edits(vec![Edit::OutputDelete {
            output_id: OutputId::new("out_yt"),
        }]),
    )
    .expect("output.delete");
    assert_eq!(listed(&control), vec![]);
    assert_eq!(engine.output_kind(&OutputId::new("out_yt")), None);
    let gone = block_on(control.call(output_call("out_yt", true))).expect_err("deleted");
    assert_eq!(code(&gone), Some("not_found"));
}

/// A stream key is bound to the server it was set for (#343): `output.set_target` to a
/// new path on the same server keeps it, a new scheme, host or port removes it from the
/// credential store, and so does an undo or redo that moves the output to another
/// server, or deletes it. Before the fix, the key stayed and the next `output.start`
/// sent it to whatever server the output then named.
#[test]
fn a_stream_key_never_follows_its_output_to_another_server() {
    use jstream_platform::StreamKeyStore;
    use jstream_platform::fake::MemoryKeyStore;
    use jstream_protocol::{HistoryCall, HistoryOp, RtmpTarget};

    let store = MemoryKeyStore::new();
    let engine = Engine::new().with_key_store(Arc::new(store.clone()));
    let control = connected(&engine);
    let retarget = |url: &str| {
        edits(vec![Edit::OutputSetTarget {
            output_id: OutputId::new("out_yt"),
            target: RtmpTarget::Generic { url: url.into() },
        }])
    };
    let stored = || {
        store
            .get("out_yt")
            .expect("store reads")
            .map(|key| key.expose().to_owned())
    };
    let history = |op| {
        block_on(control.call(Call::History(HistoryCall::new(op)))).expect("history step");
    };
    batch(
        &control,
        edits(vec![Edit::OutputCreate {
            output_id: OutputId::new("out_yt"),
            kind: rtmp_output("rtmp://a.rtmp.youtube.com/live2"),
        }]),
    )
    .expect("output.create");
    block_on(control.call(set_key("out_yt", "yt-key"))).expect("set_key");

    // The same server, written differently and with another path: the key stays.
    batch(
        &control,
        retarget("RTMP://A.rtmp.youtube.com:1935/live2/backup"),
    )
    .expect("same server");
    assert_eq!(stored().as_deref(), Some("yt-key"));

    // Another host: the key is removed, so a start can't send it there.
    batch(&control, retarget("rtmp://attacker.example/live2")).expect("new host");
    assert_eq!(stored(), None);
    assert_eq!(key_set(&control, "out_yt").ok(), Some(false));
    // Undo moves it back, but can't bring the secret back.
    history(HistoryOp::Undo);
    assert_eq!(
        engine.output_kind(&OutputId::new("out_yt")),
        Some(rtmp_output("RTMP://A.rtmp.youtube.com:1935/live2/backup"))
    );
    assert_eq!(stored(), None);
    // A key set again is removed when a redo moves the output away once more.
    block_on(control.call(set_key("out_yt", "yt-key"))).expect("set_key");
    history(HistoryOp::Redo);
    assert_eq!(stored(), None);
    history(HistoryOp::Undo);

    // Another scheme, or another port, is another server too.
    for url in [
        "rtmps://a.rtmp.youtube.com/live2",
        "rtmps://a.rtmp.youtube.com:8443/live2",
    ] {
        block_on(control.call(set_key("out_yt", "yt-key"))).expect("set_key");
        batch(&control, retarget(url)).expect("new server");
        assert_eq!(stored(), None, "{url}");
    }

    // Deleting the output removes its key, so an output re-created with its ID starts
    // with none.
    block_on(control.call(set_key("out_yt", "yt-key"))).expect("set_key");
    batch(
        &control,
        edits(vec![Edit::OutputDelete {
            output_id: OutputId::new("out_yt"),
        }]),
    )
    .expect("output.delete");
    assert_eq!(stored(), None);
    batch(
        &control,
        edits(vec![Edit::OutputCreate {
            output_id: OutputId::new("out_yt"),
            kind: rtmp_output("rtmp://attacker.example/live2"),
        }]),
    )
    .expect("output.create");
    assert_eq!(key_set(&control, "out_yt").ok(), Some(false));
    // Another output's key is not touched.
    assert!(store.is_empty());
}

/// `output.set_key` stores the key and `output.status` reports `key_set`; every error
/// path answers with a fixed reason that never contains the key.
#[test]
fn stream_key_actions_report_without_naming_the_key() {
    use jstream_platform::fake::MemoryKeyStore;

    const KEY: &str = "live_SECRET-5f2a";
    let store = MemoryKeyStore::new();
    let engine = Engine::new().with_key_store(Arc::new(store.clone()));
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![Edit::OutputCreate {
            output_id: OutputId::new("out_yt"),
            kind: rtmp_output("rtmp://a.rtmp.youtube.com/live2"),
        }]),
    )
    .expect("output.create");
    let leaks = |error: &ControlError| format!("{error:?} {error}").contains("SECRET");

    assert_eq!(key_set(&control, "out_yt").ok(), Some(false));
    block_on(control.call(set_key("out_yt", KEY))).expect("set_key");
    assert_eq!(key_set(&control, "out_yt").ok(), Some(true));
    assert_eq!(store.len(), 1);

    // Unknown output, and keys of the wrong shape.
    let missing = block_on(control.call(set_key("nope", KEY))).expect_err("unknown output");
    assert_eq!(code(&missing), Some("not_found"));
    for (key, reason) in [
        ("", "must not be empty"),
        (
            "live SECRET",
            "must not contain whitespace or control characters",
        ),
        (
            "live\nSECRET",
            "must not contain whitespace or control characters",
        ),
        // #441: a leading byte-order mark, and any other non-ASCII character.
        ("\u{FEFF}live_SECRET", "must contain only ASCII characters"),
        ("live_S\u{e9}CRET", "must contain only ASCII characters"),
    ] {
        let error = block_on(control.call(set_key("out_yt", key))).expect_err("bad key");
        assert_eq!(
            error.protocol(),
            Some(&ProtocolError::invalid_argument("key", reason))
        );
        assert!(!leaks(&error), "{error:?}");
    }
    let long = format!("SECRET{}", "k".repeat(1024));
    let error = block_on(control.call(set_key("out_yt", &long))).expect_err("too long");
    assert_eq!(code(&error), Some("invalid_argument"));
    assert!(!leaks(&error), "{error:?}");

    // A failing store: set, clear and status all fail without naming the key.
    store.set_failing(true);
    let error = block_on(control.call(set_key("out_yt", KEY))).expect_err("store fails");
    assert_eq!(code(&error), Some("action_failed"));
    assert!(!leaks(&error), "{error:?}");
    let error = block_on(control.call(clear_key("out_yt"))).expect_err("store fails");
    assert_eq!(code(&error), Some("action_failed"));
    assert!(!leaks(&error), "{error:?}");
    let error = key_set(&control, "out_yt").expect_err("store fails");
    assert_eq!(code(&error), Some("internal"));
    assert!(!leaks(&error), "{error:?}");
    store.set_failing(false);

    block_on(control.call(clear_key("out_yt"))).expect("clear_key");
    assert_eq!(key_set(&control, "out_yt").ok(), Some(false));
    block_on(control.call(clear_key("out_yt"))).expect("clearing nothing succeeds");

    // Without a store, the key actions fail and `key_set` is false.
    let bare = Engine::new();
    let control = connected(&bare);
    batch(
        &control,
        edits(vec![Edit::OutputCreate {
            output_id: OutputId::new("out_yt"),
            kind: rtmp_output("rtmp://a.rtmp.youtube.com/live2"),
        }]),
    )
    .expect("output.create");
    let error = block_on(control.call(set_key("out_yt", KEY))).expect_err("no store");
    assert_eq!(code(&error), Some("action_failed"));
    assert!(!leaks(&error), "{error:?}");
    assert_eq!(key_set(&control, "out_yt").ok(), Some(false));
}

/// R-SEC-001: a stream key set through the protocol reaches the credential store and
/// nothing else: not the reply, an event, `state.get`, `output.list`, `output.status`,
/// the engine's log lines or its audit log.
pub(crate) fn check_secrets_are_redacted() {
    use jstream_platform::StreamKeyStore;
    use jstream_platform::fake::MemoryKeyStore;
    use std::fmt;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Metadata, Subscriber};

    /// Every span and event the engine emits, with its fields, as text.
    #[derive(Clone, Default)]
    struct Logs(Arc<Mutex<Vec<String>>>);

    impl Logs {
        fn push(&self, text: String) {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(text);
        }
    }

    #[derive(Default)]
    struct Fields(String);

    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.0.push_str(&format!("{}={value:?} ", field.name()));
        }
    }

    impl Subscriber for Logs {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, span: &Attributes<'_>) -> Id {
            let mut fields = Fields::default();
            span.record(&mut fields);
            self.push(format!("span {} {}", span.metadata().name(), fields.0));
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, values: &Record<'_>) {
            let mut fields = Fields::default();
            values.record(&mut fields);
            self.push(format!("record {}", fields.0));
        }
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut fields = Fields::default();
            event.record(&mut fields);
            self.push(format!("event {} {}", event.metadata().level(), fields.0));
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    const KEY: &str = "live_REDACT-ME-9c41";
    let logs = Logs::default();
    let store = MemoryKeyStore::new();
    let audit = Arc::new(crate::AuditMemory::new());
    let engine = Engine::new()
        .with_key_store(Arc::new(store.clone()))
        .with_audit(audit.clone());
    let control = connected(&engine);
    let mut events = control.events();

    let mut seen = Vec::new();
    tracing::subscriber::with_default(logs.clone(), || {
        batch(
            &control,
            edits(vec![Edit::OutputCreate {
                output_id: OutputId::new("out_yt"),
                kind: rtmp_output("rtmp://a.rtmp.youtube.com/live2"),
            }]),
        )
        .expect("output.create");
        let reply = block_on(control.call(set_key("out_yt", KEY))).expect("set_key");
        seen.push(format!("{reply:?}"));
        seen.push(serde_json::to_string(&reply.result).expect("serializes"));
        for query_ in [
            Query::StateGet,
            Query::OutputList,
            Query::OutputStatus {
                output_id: OutputId::new("out_yt"),
            },
            // The activity log's sentence names the action and the output, never the key
            // (ADR-0083).
            Query::ActivityList {
                after_seq: None,
                limit: None,
            },
        ] {
            let result = query(&control, query_).expect("query");
            seen.push(serde_json::to_string(&result).expect("serializes"));
        }
        for event in drain(&mut events) {
            seen.push(serde_json::to_string(&event).expect("serializes"));
        }
    });
    let logged = logs
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .join("\n");
    assert!(!logged.is_empty(), "the engine logged the calls");
    seen.push(logged);
    let audited: Vec<String> = audit.entries().iter().map(|e| e.to_json_line()).collect();
    assert!(
        audited
            .iter()
            .any(|line| line.contains("\"output.set_key\"")),
        "the key action was audited: {audited:?}"
    );
    seen.extend(audited);
    for text in &seen {
        assert!(!text.contains("REDACT-ME"), "the key leaked: {text}");
    }
    let stored = store.get("out_yt").expect("get").expect("stored");
    assert_eq!(stored.expose(), KEY, "the key reached the store");
}

/// `scene.switch` is audited like every action, naming its scene, and a switch to an
/// unknown scene with its error code (ADR-0030).
#[test]
fn scene_switch_is_audited() {
    let audit = Arc::new(crate::AuditMemory::new());
    let engine = Engine::new().with_audit(audit.clone());
    let control = connected(&engine);
    batch(&control, edits(vec![create("a"), create("b")])).expect("scenes");
    block_on(control.call(switch_call("b"))).expect("switch");
    block_on(control.call(switch_call("ghost"))).expect_err("unknown scene");

    let entries = audit.entries();
    let switches: Vec<_> = entries
        .iter()
        .skip(1)
        .map(|e| {
            (
                e.call,
                e.scene_id.as_ref().map(SceneId::as_str),
                (e.rev_before.0, e.rev_after.0),
                e.error,
            )
        })
        .collect();
    assert_eq!(
        switches,
        [
            ("scene.switch", Some("b"), (1, 1), None),
            ("scene.switch", Some("ghost"), (1, 1), Some("not_found")),
        ]
    );
    assert!(
        entries[1].to_json_line().contains(",\"scene_id\":\"b\","),
        "{:?}",
        entries[1].to_json_line()
    );
}

/// The deprecated `privacy.set` is audited like every action, with its refusal
/// (ADR-0080 §4).
#[test]
fn a_refused_privacy_set_is_audited() {
    let audit = Arc::new(crate::AuditMemory::new());
    let engine = Engine::new().with_audit(audit.clone());
    let control = connected(&engine);
    block_on(control.call(privacy_call(true))).expect_err("refused");
    let seen: Vec<_> = audit
        .entries()
        .iter()
        .map(|e| (e.call, (e.rev_before.0, e.rev_after.0), e.error))
        .collect();
    assert_eq!(seen, [("privacy.set", (0, 0), Some("unsupported"))]);
}

/// The audit log gets one entry per batch and action, with the client and its latest
/// `hello` name, the request, the
/// call, the edits by name, the `rev` before and after and the result code; queries are
/// not recorded, and an idempotent retry is marked as replayed.
#[test]
fn batches_and_actions_are_audited() {
    let audit = Arc::new(crate::AuditMemory::new());
    let engine = Engine::new()
        .with_output(OutputId::new("out_1"))
        .with_audit(audit.clone());
    let control = connected(&engine);
    let other = engine.connect();
    let named = |client_name: &str| Hello {
        protocol: PROTOCOL_VERSION,
        client_name: client_name.into(),
    };
    block_on(other.hello(named("claude-code"))).expect("handshake");
    // A second `hello` keeps the client ID and renames the client in the log.
    block_on(other.hello(named("claude-desktop"))).expect("again");

    batch(&control, Batch::single(create("a"))).expect("first");
    batch(
        &other,
        edits(vec![create("b"), webcam(), add("b", "cam", "webcam")]),
    )
    .expect("second");
    let error = batch(&control, Batch::single(create("a"))).expect_err("duplicate");
    assert_eq!(code(&error), Some("already_exists"));
    query(&control, Query::SceneList).expect("scene.list");
    let mut keyed = Batch::single(create("c"));
    keyed.idempotency_key = Some(IdempotencyKey::new("k1"));
    batch(&control, keyed.clone()).expect("keyed");
    batch(&control, keyed).expect("retry");
    block_on(control.call(output_call("out_1", true))).expect("start");
    block_on(control.call(output_call("nope", true))).expect_err("unknown output");

    let entries = audit.entries();
    let names: Vec<_> = entries.iter().map(|e| e.client_name.as_str()).collect();
    assert_eq!(
        names,
        [
            "test",
            "claude-desktop",
            "test",
            "test",
            "test",
            "test",
            "test"
        ]
    );
    let summary: Vec<_> = entries
        .iter()
        .map(|e| {
            (
                e.client_id.as_str(),
                e.request_id.as_ref().map_or("", RequestId::as_str),
                e.call,
                e.edits.clone(),
                e.output_id.as_ref().map(OutputId::as_str),
                (e.rev_before.0, e.rev_after.0),
                e.error,
                e.replayed,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                "client_1",
                "req_1",
                "batch",
                vec!["scene.create"],
                None,
                (0, 1),
                None,
                false
            ),
            (
                "client_2",
                "req_1",
                "batch",
                vec!["scene.create", "source.create", "item.add"],
                None,
                (1, 2),
                None,
                false
            ),
            (
                "client_1",
                "req_2",
                "batch",
                vec!["scene.create"],
                None,
                (2, 2),
                Some("already_exists"),
                false
            ),
            (
                "client_1",
                "req_4",
                "batch",
                vec!["scene.create"],
                None,
                (2, 3),
                None,
                false
            ),
            (
                "client_1",
                "req_5",
                "batch",
                vec!["scene.create"],
                None,
                (3, 3),
                None,
                true
            ),
            (
                "client_1",
                "req_6",
                "output.start",
                vec![],
                Some("out_1"),
                (3, 3),
                None,
                false
            ),
            (
                "client_1",
                "req_7",
                "output.start",
                vec![],
                Some("nope"),
                (3, 3),
                Some("not_found"),
                false
            ),
        ]
    );
    let line = entries[1].to_json_line();
    assert!(
        line.starts_with("{\"time\":\"")
            && line.contains(
                "\"client_id\":\"client_2\",\"client_name\":\"claude-desktop\",\
             \"request_id\":\"req_1\",\"call\":\"batch\",\
             \"edits\":[\"scene.create\",\"source.create\",\"item.add\"],\
             \"rev_before\":1,\"rev_after\":2,\"result\":\"ok\"}"
            ),
        "{line}"
    );
    // Arguments never reach the log: not the scene's name nor the source's.
    assert!(
        !line.contains("Webcam") && !line.contains("\"B\""),
        "{line}"
    );
}
