//! The engine's protocol behaviour through `InProcessControl`: the handshake, atomic
//! batches, `expected_rev`, idempotency keys, queries (held to the shared parity
//! fixture), events, the placeholder outputs, and a differential run against
//! `jstream-control`'s mock engine.

mod collection;
mod history;
mod sources;

use crate::{DropSnapshot, Engine, FnDropSource, InProcessControl};
use jstream_control::{Control, ControlError, EventStream, MockEngine};
use jstream_protocol::{
    Action, Batch, Call, CallResult, ClientId, DropCounts, Edit, EntityKind, Event, Hello,
    IdempotencyKey, ItemId, OutputId, OutputState, PROTOCOL_VERSION, ProtocolError,
    ProtocolVersion, Query, QueryResult, RequestId, Rev, SceneId, SourceId, SourceKind, parity,
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
    }
}

fn webcam() -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new("webcam"),
        name: "Webcam".into(),
        kind: SourceKind::Builtin {
            kind: "webcam".into(),
        },
        settings: Default::default(),
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

fn batch(control: &dyn Control, batch: Batch) -> Result<Rev, ControlError> {
    match block_on(control.call(Call::Batch(batch)))?.result {
        CallResult::Batch { rev } => Ok(rev),
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

pub(crate) fn check_stale_expected_rev_is_rejected() {
    let engine = Engine::new();
    let agent = connected(&engine);
    let user = connected(&engine);
    let mut events = agent.events();
    assert_eq!(batch(&user, Batch::single(create("a"))), Ok(Rev(1)));
    assert_eq!(drain(&mut events).len(), 1);

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
    };
    assert_eq!(batch(&control, edits), Ok(Rev(1)));
    assert_eq!(engine.rev(), Rev(1));
    assert_eq!(drain(&mut events).len(), 1, "one event per batch");

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
    let engine = Engine::new().with_output(OutputId::new("rec"));
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
        })
    );
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
    assert_eq!(first.result, CallResult::Batch { rev: Rev(1) });
    assert_eq!(batch(&control, Batch::single(create("b"))), Ok(Rev(2)));
    // The retry answers before the revision check, so a now-stale `expected_rev` is fine.
    let retry =
        block_on(control.call(Call::Batch(keyed.clone().expecting(Rev(0))))).expect("retry");
    assert_eq!(retry.result, first.result);
    assert_eq!(engine.rev(), Rev(2));
    assert_eq!(drain(&mut events).len(), 2, "a retry emits nothing");

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
    assert_eq!(started.result, CallResult::Action {});

    let changed = |state| Event::OutputStateChanged {
        output_id: OutputId::new("yt"),
        state,
    };
    let expected = vec![
        Event::StateChanged {
            rev: Rev(1),
            client_id: agent.client_id().expect("agent"),
            request_id: Some(by_agent.id),
            cause: None,
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
    assert_eq!(
        poll_event(&mut leaving_events, Waker::noop()),
        Poll::Ready(None)
    );
    batch(&staying, Batch::single(create("b"))).expect("batch");
    assert_eq!(
        poll_event(&mut leaving_events, Waker::noop()),
        Poll::Ready(None)
    );
    assert_eq!(drain(&mut staying_events).len(), 2);

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
    let revs: Vec<u64> = drain(&mut events)
        .into_iter()
        .map(|event| match event {
            Event::StateChanged { rev, .. } => rev.0,
            other => panic!("unexpected event {other:?}"),
        })
        .collect();
    assert_eq!(revs, (1..=total).collect::<Vec<_>>());
}

/// The same script of calls against the engine and against `jstream-control`'s mock
/// gets the same answers (results, error codes and fields) and the same events, up to
/// client IDs. The mock is the reference implementation clients were developed against.
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
            other => other,
        }
    }
    /// Error messages may differ between implementations; codes and fields may not.
    fn comparable(result: Result<CallResult, ControlError>) -> Result<CallResult, ProtocolError> {
        result.map_err(|error| match error.protocol() {
            Some(ProtocolError::InvalidArgument { field, .. }) => ProtocolError::InvalidArgument {
                field: field.clone(),
                reason: String::new(),
            },
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
    ];

    let engine = Engine::new().with_output(OutputId::new("yt"));
    let real = connected(&engine);
    let mut real_events = real.events();
    let mock_engine = MockEngine::new().with_output(OutputId::new("yt"));
    let mock = mock_engine.connect();
    block_on(mock.hello(hello())).expect("mock handshake");
    let mut mock_events = mock.events();

    for call in script {
        let got = comparable(block_on(real.call(call.clone())).map(|r| r.result));
        let want = comparable(block_on(mock.call(call.clone())).map(|r| r.result));
        assert_eq!(got, want, "{call:?}");
    }
    assert_eq!(engine.rev(), mock_engine.rev());
    let real_events: Vec<Event> = drain(&mut real_events).into_iter().map(normalize).collect();
    let mock_events: Vec<Event> = drain(&mut mock_events).into_iter().map(normalize).collect();
    assert_eq!(real_events, mock_events);
    assert!(real_events.len() >= 5);
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
/// against the platform fakes. A missing software adapter skips the test, and fails
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
        FakeAudioIo, FakeCapture, FakeClock, FakeCodecs, FakeGpuInterop, FakeVideoDevices,
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
    assert!(stats.video.received >= 1, "video frames accepted");
    assert!(stats.audio.received >= 1, "audio blocks accepted");
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
        FakeAudioIo, FakeCapture, FakeClock, FakeCodecs, FakeGpuInterop, FakeVideoDevices,
    };
    use jstream_platform::{Backends, Capture, CaptureTarget, PlatformError};

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

    struct HoldCapture {
        inner: FakeCapture,
        entered: Arc<Hold>,
        release: Arc<Hold>,
    }

    impl Capture for HoldCapture {
        fn targets(&self) -> Result<Vec<jstream_platform::CaptureTargetInfo>, PlatformError> {
            self.inner.targets()
        }

        fn start(
            &self,
            target: &CaptureTarget,
        ) -> Result<Box<dyn jstream_platform::CaptureStream>, PlatformError> {
            self.entered.signal();
            self.release.wait();
            self.inner.start(target)
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
        capture: Arc::new(HoldCapture {
            inner: FakeCapture::new(clock, size),
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        }),
        audio: Arc::new(audio),
        codecs: Arc::new(FakeCodecs),
        gpu: Arc::new(FakeGpuInterop),
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
        FakeAudioIo, FakeCapture, FakeClock, FakeCodecs, FakeGpuInterop, FakeVideoDevices,
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
        FakeAudioIo, FakeCapture, FakeClock, FakeCodecs, FakeGpuInterop, FakeVideoDevices,
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
    assert!(
        !armed.load(Ordering::Acquire),
        "the first pipeline never dropped its audio stream"
    );
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
        FakeAudioIo, FakeCapture, FakeClock, FakeCodecs, FakeGpuInterop, FakeVideoDevices,
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
/// changes. A `display` source shows the pipeline's capture until a frame is stored
/// for it, and a frame for a source nothing on program shows publishes nothing.
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
    };
    batch(&control, edits).expect("batch");
    assert_eq!(generation(), 2);
    assert!(
        hub.load().shot().uses_capture(),
        "no stored frame: the capture"
    );

    engine
        .set_source_frame(&SourceId::new("elsewhere"), 1, 1, &[0, 0, 0, 255])
        .expect("frame");
    assert_eq!(generation(), 2, "nothing on program shows that source");

    engine
        .set_source_frame(&SourceId::new("screen"), 1, 1, &[0, 0, 0, 255])
        .expect("frame");
    assert_eq!(generation(), 3);
    assert!(!hub.load().shot().uses_capture(), "a stored frame wins");
    engine
        .set_source_frame(&SourceId::new("screen"), 1, 1, &[9, 9, 9, 255])
        .expect("frame");
    assert_eq!(generation(), 4, "a new frame is a new version");
}

/// How long a GPU test's `output.start` waits for lavapipe or WARP to open. The
/// engine's 10 s default only guards against a stuck adapter; on a loaded CI runner a
/// software adapter can take longer, and that is not what these tests check. A real
/// failure to open still fails the test, just later.
const GPU_OPEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// One software-adapter test at a time. Parallel compositors starve the pipeline
/// test's media wait and crash WARP. The lock schedules tests; it is not engine state.
pub(crate) fn one_gpu_test_at_a_time() -> MutexGuard<'static, ()> {
    static SLOT: Mutex<()> = Mutex::new(());
    SLOT.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The output states among the buffered events. `stats.tick` is skipped: the
/// pipeline emits one about once a second and a final one when it stops.
fn output_steps(events: &mut EventStream) -> Vec<OutputState> {
    drain(events)
        .into_iter()
        .filter_map(|event| match event {
            Event::OutputStateChanged { state, .. } => Some(state),
            Event::StatsTick { .. } => None,
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
        },
    )
    .expect("scene");
    assert_eq!(built, Rev(1));
    assert!(matches!(
        drain(&mut events).as_slice(),
        [Event::StateChanged { rev: Rev(1), .. }]
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
    assert!(drain(&mut events).is_empty());

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
                },
            ],
            expected_rev: None,
            idempotency_key: None,
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
        drain(&mut events),
        vec![Event::StatsTick {
            seq: 1,
            drops: DropCounts {
                capture_frames: 3,
                render_ticks: 2,
                ..DropCounts::default()
            },
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
        drain(&mut events),
        vec![Event::StatsTick {
            seq: 2,
            drops: DropCounts {
                capture_frames: 5,
                render_ticks: 4,
                ..DropCounts::default()
            },
        }],
        "the removed source stays at the total it had when it was removed"
    );

    engine.remove_drop_source(id);
    engine.emit_stats_tick();
    assert_eq!(
        drain(&mut events),
        vec![Event::StatsTick {
            seq: 3,
            drops: DropCounts {
                capture_frames: 5,
                render_ticks: 4,
                ..DropCounts::default()
            },
        }],
        "removing an unknown source changes nothing"
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
    use super::{
        batch, block_on, code, connected, drain, edits, output_call, output_state, output_steps,
        query,
    };
    use crate::InProcessControl;
    use crate::runtime::{DrawFn, OpenDraw, cpu_draw};
    use crate::{Engine, LinkState, LivePublish, PublishError, RecordingPublish};
    use jstream_control::{Control, EventStream};
    use jstream_media::{ChannelLayout, FrameSize, MediaTime, Packet, Timebase};
    use jstream_mux::flv::{FlvTag, TagType};
    use jstream_platform::fake::{
        FakeAudioIo, FakeCapture, FakeClock, FakeCodecs, FakeGpuInterop, FakeVideoDevices,
    };
    use jstream_platform::{
        AudioDeviceInfo, AudioEncoder, AudioEncoderConfig, AudioInputHandler, AudioInputTarget,
        AudioIo, AudioOutputHandler, AudioOutputTarget, AudioStream, Backends, Capture,
        CaptureStats, CaptureStream, CaptureTarget, CaptureTargetInfo, CodecInfo, Codecs,
        MediaClock, PlatformError, VideoDecoder, VideoDecoderConfig, VideoEncoder,
        VideoEncoderConfig, VideoSurface,
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
            Arc::new(move || {
                let seen = Arc::clone(&seen);
                let mut inner = (cpu_draw())()?;
                let draw: DrawFn = Box::new(move |shot, size, capture, out| {
                    seen.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(shot.program().cloned());
                    inner(shot, size, capture, out)
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

    /// Pumps until `done`, for at most five seconds.
    fn pump_until(clock: &FakeClock, audio: &FakeAudioIo, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            pump(clock, audio);
        }
        done()
    }

    fn wait_until(mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
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
            Arc::new(move || {
                let armed = Arc::clone(&armed);
                let mut inner = (cpu_draw())()?;
                let draw: DrawFn = Box::new(move |shot, size, rgba, out| {
                    assert!(!armed.load(Ordering::Acquire), "compositor blew up");
                    inner(shot, size, rgba, out)
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
        let render_ticks = drain(&mut events)
            .into_iter()
            .filter_map(|event| match event {
                Event::StatsTick { drops, .. } => Some(drops.render_ticks),
                _ => None,
            })
            .next_back();
        assert_eq!(render_ticks, Some(9));
        block_on(control.call(output_call("yt", false))).expect("output.stop");
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
    /// blocks for 200 ms, and it ignores [`LivePublish::stop_by`] as a destination that
    /// cannot cut a write short would. Records the deadline it was given.
    struct StalledPeer {
        stalled: Arc<AtomicBool>,
        sent: Arc<AtomicU64>,
        deadline: Arc<Mutex<Option<Instant>>>,
    }

    impl LivePublish for StalledPeer {
        fn send_tag(&mut self, _tag: &FlvTag) -> Result<(), PublishError> {
            if self.stalled.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(200));
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
    /// them all at 200 ms a tag would hold `output.stop` for half a minute; the stop
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
            Arc::new(move || {
                if working.load(Ordering::Acquire) {
                    cpu_draw()()
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
    // encoder session (ADR-0027), so a recording can run while streaming.

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

    /// A program-stage failure fails every output on it. A start after that waits for
    /// the failed stage to go and opens a new one.
    #[test]
    fn a_render_panic_fails_every_output() {
        let dir = TempDir::new("both-fail");
        let fakes = fakes();
        let armed = Arc::new(AtomicBool::new(false));
        let draw: OpenDraw = {
            let armed = Arc::clone(&armed);
            Arc::new(move || {
                let armed = Arc::clone(&armed);
                let mut inner = (cpu_draw())()?;
                let draw: DrawFn = Box::new(move |shot, size, rgba, out| {
                    assert!(!armed.load(Ordering::Acquire), "compositor blew up");
                    inner(shot, size, rgba, out)
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
    ] {
        let error = block_on(control.call(set_key("out_yt", key))).expect_err("bad key");
        assert_eq!(
            error.protocol(),
            Some(&ProtocolError::InvalidArgument {
                field: "key".into(),
                reason: reason.into()
            })
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
                e.request_id.as_str(),
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
