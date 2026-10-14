//! The engine's protocol behaviour through `InProcessControl`: the handshake, atomic
//! batches, `expected_rev`, idempotency keys, queries (held to the shared parity
//! fixture), events, the placeholder outputs, and a differential run against
//! `jstream-control`'s mock engine.

use crate::{Engine, InProcessControl};
use jstream_control::{Control, ControlError, EventStream, MockEngine};
use jstream_protocol::{
    Action, Batch, Call, CallResult, ClientId, Edit, EntityKind, Event, Hello, IdempotencyKey,
    ItemId, OutputId, OutputState, PROTOCOL_VERSION, ProtocolError, ProtocolVersion, Query,
    QueryResult, RequestId, Rev, SceneId, SourceId, SourceKind, parity,
};
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Wake, Waker};

/// Runs a future to completion. The engine's futures are always ready.
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("the in-process engine's futures are always ready"),
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
    }
}

fn add(scene: &str, item: &str, source: &str) -> Edit {
    Edit::ItemAdd {
        scene_id: SceneId::new(scene),
        item_id: ItemId::new(item),
        source_id: SourceId::new(source),
        transform: None,
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
            state: OutputState::Stopped
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
        },
        Event::StateChanged {
            rev: Rev(2),
            client_id: user.client_id().expect("user"),
            request_id: Some(by_user.id),
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
