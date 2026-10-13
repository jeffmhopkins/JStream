//! An in-memory [`Control`] implementation for developing and testing clients before the
//! engine exists. Enabled by the `mock` feature.
//!
//! [`MockEngine`] holds a tiny collection (scenes and their items) plus a set of outputs,
//! and follows the protocol's rules for it: the handshake, atomic batches that bump `rev`
//! by one, `expected_rev` conflicts, idempotency keys, `state.changed` and
//! `output.state_changed` events. Each [`MockEngine::connect`] opens one client session.
//! It does no undo, persistence or real output work.

use crate::control::{BoxFuture, Control, EventStream};
use crate::error::ControlError;
use futures_core::Stream;
use jstream_protocol::{
    Action, Batch, Call, CallResult, ClientId, Edit, EntityKind, Event, Hello, IdempotencyKey,
    ItemId, OutputId, OutputState, PROTOCOL_VERSION, ProtocolError, Query, QueryResult, RequestId,
    Response, Rev, SceneId, SceneSummary, Welcome,
};
use std::collections::{BTreeMap, VecDeque};
use std::future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::{Context, Poll, Waker};

/// Locks `mutex`, ignoring poisoning: the mock's state stays consistent because every
/// change is computed first and committed without panicking.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An in-memory engine that any number of [`MockControl`] sessions connect to.
///
/// Cloning it shares the same state.
#[derive(Clone, Default)]
pub struct MockEngine {
    shared: Arc<Mutex<State>>,
}

impl MockEngine {
    /// An engine at `rev` 0 with no scenes and no outputs.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an output in the `stopped` state, so `output.start` and `output.status` can
    /// name it. Outputs are runtime state: this changes no `rev` and emits no event.
    #[must_use]
    pub fn with_output(self, output_id: OutputId) -> Self {
        lock(&self.shared)
            .outputs
            .insert(output_id, OutputState::Stopped);
        self
    }

    /// Opens a new client session. It must send `hello` before any call.
    pub fn connect(&self) -> MockControl {
        MockControl {
            shared: Arc::clone(&self.shared),
            client_id: Mutex::new(None),
            next_request: AtomicU64::new(1),
        }
    }

    /// The current state revision.
    pub fn rev(&self) -> Rev {
        lock(&self.shared).collection.rev
    }

    /// Pushes `event` to every subscriber, as if the engine had emitted it. Lets client
    /// tests script events the mock never produces itself (`stats.tick`, failures).
    pub fn emit(&self, event: Event) {
        lock(&self.shared).emit(event);
    }
}

/// One client session with a [`MockEngine`].
pub struct MockControl {
    shared: Arc<Mutex<State>>,
    client_id: Mutex<Option<ClientId>>,
    next_request: AtomicU64,
}

impl Control for MockControl {
    fn hello(&self, hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
        let result = if PROTOCOL_VERSION.can_serve(hello.protocol) {
            let mut state = lock(&self.shared);
            let mut session = lock(&self.client_id);
            let client_id = session
                .get_or_insert_with(|| {
                    state.clients += 1;
                    ClientId::new(format!("mock_{}", state.clients))
                })
                .clone();
            Ok(Welcome {
                protocol: PROTOCOL_VERSION,
                client_id,
                rev: state.collection.rev,
            })
        } else {
            Err(ProtocolError::UnsupportedVersion {
                client: hello.protocol,
                server: PROTOCOL_VERSION,
            }
            .into())
        };
        Box::pin(future::ready(result))
    }

    fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
        let id = RequestId::new(format!(
            "req_{}",
            self.next_request.fetch_add(1, Ordering::Relaxed)
        ));
        let client_id = lock(&self.client_id).clone();
        let result = match client_id {
            None => Err(ProtocolError::HandshakeRequired),
            Some(client_id) => lock(&self.shared).call(&client_id, &id, call),
        };
        Box::pin(future::ready(
            result
                .map(|result| Response { id, result })
                .map_err(ControlError::from),
        ))
    }

    fn events(&self) -> EventStream {
        let queue = Arc::new(Mutex::new(Queue::default()));
        lock(&self.shared).subscribers.push(Arc::downgrade(&queue));
        Box::pin(Subscription(queue))
    }
}

/// Everything the mock engine knows.
#[derive(Default)]
struct State {
    collection: Collection,
    outputs: BTreeMap<OutputId, OutputState>,
    /// The `rev` each applied batch with an idempotency key produced, per client.
    applied: BTreeMap<(ClientId, IdempotencyKey), Rev>,
    /// How many sessions have completed `hello`; numbers the client IDs.
    clients: u64,
    subscribers: Vec<Weak<Mutex<Queue>>>,
}

/// The collection state that `rev` counts. Cloned to apply a batch atomically.
#[derive(Clone, Default)]
struct Collection {
    rev: Rev,
    /// In display order.
    scenes: Vec<Scene>,
    items: BTreeMap<ItemId, Item>,
}

#[derive(Clone)]
struct Scene {
    id: SceneId,
    name: String,
    /// Bottom to top.
    items: Vec<ItemId>,
}

/// An item. The mock keeps only what its checks need: no query reports an item's
/// source or transform yet.
#[derive(Clone)]
struct Item {
    scene_id: SceneId,
}

impl State {
    fn call(
        &mut self,
        client_id: &ClientId,
        request_id: &RequestId,
        call: Call,
    ) -> Result<CallResult, ProtocolError> {
        match call {
            Call::Batch(batch) => self.batch(client_id, request_id, batch),
            Call::Action { action } => self.action(action),
            Call::Query { query } => self.query(query),
            _ => Err(unimplemented_by_mock("this kind of call")),
        }
    }

    fn batch(
        &mut self,
        client_id: &ClientId,
        request_id: &RequestId,
        batch: Batch,
    ) -> Result<CallResult, ProtocolError> {
        if let Some(key) = &batch.idempotency_key
            && let Some(rev) = self.applied.get(&(client_id.clone(), key.clone()))
        {
            return Ok(CallResult::Batch { rev: *rev });
        }
        if batch.edits.is_empty() {
            return Err(ProtocolError::EmptyBatch);
        }
        let current = self.collection.rev;
        if let Some(expected) = batch.expected_rev
            && expected != current
        {
            return Err(ProtocolError::RevConflict { expected, current });
        }
        let mut next = self.collection.clone();
        for edit in batch.edits {
            next.apply(edit)?;
        }
        next.rev = current.next().ok_or_else(|| ProtocolError::Internal {
            reason: "rev overflowed".into(),
        })?;
        let rev = next.rev;
        self.collection = next;
        if let Some(key) = batch.idempotency_key {
            self.applied.insert((client_id.clone(), key), rev);
        }
        self.emit(Event::StateChanged {
            rev,
            client_id: client_id.clone(),
            request_id: Some(request_id.clone()),
        });
        Ok(CallResult::Batch { rev })
    }

    fn action(&mut self, action: Action) -> Result<CallResult, ProtocolError> {
        let (output_id, start) = match action {
            Action::OutputStart { output_id } => (output_id, true),
            Action::OutputStop { output_id } => (output_id, false),
            other => return Err(unimplemented_by_mock(other.name())),
        };
        let Some(&state) = self.outputs.get(&output_id) else {
            return Err(not_found(EntityKind::Output, output_id.as_str()));
        };
        let idle = matches!(state, OutputState::Stopped | OutputState::Failed);
        let steps = match (start, idle) {
            (true, true) => [OutputState::Starting, OutputState::Active],
            (false, false) => [OutputState::Stopping, OutputState::Stopped],
            (true, false) => {
                return Err(ProtocolError::ActionFailed {
                    reason: format!("output `{output_id}` is already running"),
                });
            }
            (false, true) => {
                return Err(ProtocolError::ActionFailed {
                    reason: format!("output `{output_id}` is not running"),
                });
            }
        };
        for state in steps {
            self.outputs.insert(output_id.clone(), state);
            self.emit(Event::OutputStateChanged {
                output_id: output_id.clone(),
                state,
            });
        }
        Ok(CallResult::Action {})
    }

    fn query(&self, query: Query) -> Result<CallResult, ProtocolError> {
        let result = match query {
            Query::SceneList => QueryResult::SceneList {
                rev: self.collection.rev,
                scenes: self
                    .collection
                    .scenes
                    .iter()
                    .map(|scene| SceneSummary {
                        scene_id: scene.id.clone(),
                        name: scene.name.clone(),
                        item_count: u32::try_from(scene.items.len()).unwrap_or(u32::MAX),
                    })
                    .collect(),
            },
            Query::OutputStatus { output_id } => {
                let Some(&state) = self.outputs.get(&output_id) else {
                    return Err(not_found(EntityKind::Output, output_id.as_str()));
                };
                QueryResult::OutputStatus { output_id, state }
            }
            other => return Err(unimplemented_by_mock(other.name())),
        };
        Ok(CallResult::Query { result })
    }

    /// Queues `event` for every live subscriber and drops the closed ones.
    fn emit(&mut self, event: Event) {
        self.subscribers.retain(|weak| {
            let Some(queue) = weak.upgrade() else {
                return false;
            };
            let waker = {
                let mut queue = lock(&queue);
                queue.events.push_back(event.clone());
                queue.waker.take()
            };
            if let Some(waker) = waker {
                waker.wake();
            }
            true
        });
    }
}

impl Drop for State {
    /// Ends every subscription when the engine goes away.
    fn drop(&mut self) {
        for queue in self.subscribers.drain(..).filter_map(|weak| weak.upgrade()) {
            let waker = {
                let mut queue = lock(&queue);
                queue.closed = true;
                queue.waker.take()
            };
            if let Some(waker) = waker {
                waker.wake();
            }
        }
    }
}

impl Collection {
    /// Applies one edit to this (scratch) copy of the collection.
    fn apply(&mut self, edit: Edit) -> Result<(), ProtocolError> {
        match edit {
            Edit::SceneCreate { scene_id, name } => {
                if self.scene_index(&scene_id).is_some() {
                    return Err(already_exists(EntityKind::Scene, scene_id.as_str()));
                }
                self.scenes.push(Scene {
                    id: scene_id,
                    name,
                    items: Vec::new(),
                });
            }
            Edit::SceneRename { scene_id, name } => {
                let index = self.existing_scene(&scene_id)?;
                self.scenes[index].name = name;
            }
            Edit::SceneDelete { scene_id } => {
                let index = self.existing_scene(&scene_id)?;
                let scene = self.scenes.remove(index);
                for item_id in &scene.items {
                    self.items.remove(item_id);
                }
            }
            Edit::ItemAdd {
                scene_id, item_id, ..
            } => {
                let index = self.existing_scene(&scene_id)?;
                if self.items.contains_key(&item_id) {
                    return Err(already_exists(EntityKind::Item, item_id.as_str()));
                }
                self.scenes[index].items.push(item_id.clone());
                self.items.insert(item_id, Item { scene_id });
            }
            Edit::ItemRemove { item_id } => {
                let Some(item) = self.items.remove(&item_id) else {
                    return Err(not_found(EntityKind::Item, item_id.as_str()));
                };
                if let Some(index) = self.scene_index(&item.scene_id) {
                    self.scenes[index].items.retain(|id| *id != item_id);
                }
            }
            Edit::ItemSetTransform { item_id, .. } => {
                if !self.items.contains_key(&item_id) {
                    return Err(not_found(EntityKind::Item, item_id.as_str()));
                }
            }
            other => return Err(unimplemented_by_mock(other.name())),
        }
        Ok(())
    }

    fn scene_index(&self, scene_id: &SceneId) -> Option<usize> {
        self.scenes.iter().position(|scene| scene.id == *scene_id)
    }

    fn existing_scene(&self, scene_id: &SceneId) -> Result<usize, ProtocolError> {
        self.scene_index(scene_id)
            .ok_or_else(|| not_found(EntityKind::Scene, scene_id.as_str()))
    }
}

fn not_found(entity: EntityKind, id: &str) -> ProtocolError {
    ProtocolError::NotFound {
        entity,
        id: id.to_owned(),
    }
}

fn already_exists(entity: EntityKind, id: &str) -> ProtocolError {
    ProtocolError::AlreadyExists {
        entity,
        id: id.to_owned(),
    }
}

/// A protocol variant added after this mock was written.
fn unimplemented_by_mock(what: &str) -> ProtocolError {
    ProtocolError::Internal {
        reason: format!("the mock engine does not implement {what}"),
    }
}

/// The buffer behind one event subscription.
#[derive(Default)]
struct Queue {
    events: VecDeque<Event>,
    waker: Option<Waker>,
    closed: bool,
}

/// One event subscription. The engine holds only a weak reference, so dropping this
/// unsubscribes.
struct Subscription(Arc<Mutex<Queue>>);

impl Stream for Subscription {
    type Item = Event;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Event>> {
        let mut queue = lock(&self.0);
        if let Some(event) = queue.events.pop_front() {
            Poll::Ready(Some(event))
        } else if queue.closed {
            Poll::Ready(None)
        } else {
            queue.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MockEngine;
    use crate::{Control, ControlError, EventStream};
    use jstream_protocol::{
        Action, Batch, Call, CallResult, ClientId, Edit, EntityKind, Event, Hello, IdempotencyKey,
        ItemId, OutputId, OutputState, PROTOCOL_VERSION, ProtocolError, ProtocolVersion, Query,
        QueryResult, Rev, SceneId, SourceId,
    };
    use std::future::Future;
    use std::pin::pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll, Wake, Waker};

    /// Runs a future to completion. The mock's futures are always ready, so a no-op waker
    /// is enough.
    fn block_on<F: Future>(future: F) -> F::Output {
        let mut future = pin!(future);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return value;
            }
        }
    }

    /// Polls the stream once.
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

    fn connected(engine: &MockEngine) -> Box<dyn Control> {
        let control = Box::new(engine.connect());
        block_on(control.hello(hello())).expect("handshake");
        control
    }

    fn create(scene: &str) -> Edit {
        Edit::SceneCreate {
            scene_id: SceneId::new(scene),
            name: scene.to_uppercase(),
        }
    }

    fn batch(control: &dyn Control, batch: Batch) -> Result<Rev, ControlError> {
        match block_on(control.call(Call::Batch(batch)))?.result {
            CallResult::Batch { rev } => Ok(rev),
            other => panic!("not a batch result: {other:?}"),
        }
    }

    fn scene_list(control: &dyn Control) -> (Rev, Vec<(String, u32)>) {
        let call = Call::Query {
            query: Query::SceneList,
        };
        match block_on(control.call(call)).expect("query").result {
            CallResult::Query {
                result: QueryResult::SceneList { rev, scenes },
            } => (
                rev,
                scenes
                    .into_iter()
                    .map(|s| (s.scene_id.to_string(), s.item_count))
                    .collect(),
            ),
            other => panic!("not a scene list: {other:?}"),
        }
    }

    fn code(error: &ControlError) -> Option<&'static str> {
        error.protocol().map(ProtocolError::code)
    }

    #[test]
    fn control_is_dyn_compatible_and_thread_safe() {
        fn send_sync<T: Send + Sync + ?Sized>() {}
        send_sync::<dyn Control>();
        let engine = MockEngine::new();
        let shared: Arc<dyn Control> = Arc::new(engine.connect());
        let handle = std::thread::spawn(move || {
            block_on(shared.call(Call::Query {
                query: Query::SceneList,
            }))
            .map(|r| r.id)
        });
        let answered = handle.join().expect("thread");
        assert!(matches!(
            answered.as_ref().map_err(code),
            Err(Some("handshake_required"))
        ));
    }

    #[test]
    fn handshake_assigns_client_ids_and_checks_the_version() {
        let engine = MockEngine::new();
        let a = engine.connect();
        let b = engine.connect();
        let wa = block_on(a.hello(hello())).expect("a");
        let wb = block_on(b.hello(hello())).expect("b");
        assert_ne!(wa.client_id, wb.client_id);
        assert_eq!(wa.rev, Rev(0));
        assert_eq!(wa.protocol, PROTOCOL_VERSION);
        // A second hello keeps the identity.
        assert_eq!(block_on(a.hello(hello())).expect("again"), wa);

        let newer = Hello {
            protocol: ProtocolVersion {
                major: PROTOCOL_VERSION.major,
                minor: PROTOCOL_VERSION.minor + 1,
            },
            client_name: "future".into(),
        };
        let refused = block_on(engine.connect().hello(newer)).expect_err("refused");
        assert_eq!(code(&refused), Some("unsupported_version"));
    }

    #[test]
    fn a_batch_round_trips_and_bumps_rev_once() {
        let engine = MockEngine::new();
        let control = connected(&engine);
        let edits = Batch {
            edits: vec![
                create("intro"),
                create("main"),
                Edit::ItemAdd {
                    scene_id: SceneId::new("main"),
                    item_id: ItemId::new("cam"),
                    source_id: SourceId::new("webcam"),
                    transform: None,
                },
                Edit::SceneRename {
                    scene_id: SceneId::new("intro"),
                    name: "Starting soon".into(),
                },
            ],
            expected_rev: Some(Rev(0)),
            idempotency_key: None,
        };
        assert_eq!(batch(&*control, edits), Ok(Rev(1)));
        assert_eq!(engine.rev(), Rev(1));
        assert_eq!(
            scene_list(&*control),
            (Rev(1), vec![("intro".into(), 0), ("main".into(), 1)])
        );

        let delete = Batch::single(Edit::SceneDelete {
            scene_id: SceneId::new("main"),
        });
        assert_eq!(batch(&*control, delete), Ok(Rev(2)));
        // The item went with its scene, so its ID is free again.
        let readd = Batch::single(Edit::ItemAdd {
            scene_id: SceneId::new("intro"),
            item_id: ItemId::new("cam"),
            source_id: SourceId::new("webcam"),
            transform: None,
        });
        assert_eq!(batch(&*control, readd), Ok(Rev(3)));
    }

    #[test]
    fn a_failing_edit_applies_nothing() {
        let engine = MockEngine::new();
        let control = connected(&engine);
        let edits = Batch {
            edits: vec![
                create("a"),
                Edit::ItemRemove {
                    item_id: ItemId::new("ghost"),
                },
            ],
            expected_rev: None,
            idempotency_key: None,
        };
        let error = batch(&*control, edits).expect_err("fails");
        assert_eq!(
            error.protocol(),
            Some(&ProtocolError::NotFound {
                entity: EntityKind::Item,
                id: "ghost".into()
            })
        );
        assert_eq!(scene_list(&*control), (Rev(0), vec![]));

        assert_eq!(batch(&*control, Batch::single(create("a"))), Ok(Rev(1)));
        let duplicate = batch(&*control, Batch::single(create("a"))).expect_err("dup");
        assert_eq!(code(&duplicate), Some("already_exists"));
        let empty = Batch {
            edits: vec![],
            expected_rev: None,
            idempotency_key: None,
        };
        let empty = batch(&*control, empty).expect_err("empty");
        assert_eq!(code(&empty), Some("empty_batch"));
        assert_eq!(engine.rev(), Rev(1));
    }

    #[test]
    fn a_stale_expected_rev_conflicts() {
        let engine = MockEngine::new();
        let agent = connected(&engine);
        let user = connected(&engine);
        assert_eq!(batch(&*user, Batch::single(create("a"))), Ok(Rev(1)));
        let stale = Batch::single(create("b")).expecting(Rev(0));
        let error = batch(&*agent, stale).expect_err("conflict");
        assert_eq!(
            error.protocol(),
            Some(&ProtocolError::RevConflict {
                expected: Rev(0),
                current: Rev(1)
            })
        );
        assert_eq!(engine.rev(), Rev(1));
        let fresh = Batch::single(create("b")).expecting(Rev(1));
        assert_eq!(batch(&*agent, fresh), Ok(Rev(2)));
    }

    #[test]
    fn an_idempotent_retry_gets_the_original_result() {
        let engine = MockEngine::new();
        let control = connected(&engine);
        let mut events = control.events();
        let keyed = Batch::single(create("a")).with_idempotency_key(IdempotencyKey::new("k1"));
        assert_eq!(batch(&*control, keyed.clone()), Ok(Rev(1)));
        assert_eq!(batch(&*control, Batch::single(create("b"))), Ok(Rev(2)));
        assert_eq!(batch(&*control, keyed.clone()), Ok(Rev(1)));
        assert_eq!(engine.rev(), Rev(2));
        assert_eq!(drain(&mut events).len(), 2, "a retry emits nothing");
        // Keys are per client: another client's identical key is a new batch.
        let other = connected(&engine);
        let error = batch(&*other, keyed).expect_err("scene a exists");
        assert_eq!(code(&error), Some("already_exists"));
    }

    #[test]
    fn events_reach_every_subscriber_in_order() {
        let engine = MockEngine::new().with_output(OutputId::new("yt"));
        let user = connected(&engine);
        let agent = connected(&engine);
        let mut user_events = user.events();
        let mut agent_events = agent.events();

        let response =
            block_on(agent.call(Call::Batch(Batch::single(create("a"))))).expect("batch");
        let start = Call::Action {
            action: Action::OutputStart {
                output_id: OutputId::new("yt"),
            },
        };
        let started = block_on(agent.call(start)).expect("start");
        assert_eq!(started.result, CallResult::Action {});

        let changed = |state| Event::OutputStateChanged {
            output_id: OutputId::new("yt"),
            state,
        };
        let expected = vec![
            Event::StateChanged {
                rev: Rev(1),
                client_id: ClientId::new("mock_2"),
                request_id: Some(response.id),
            },
            changed(OutputState::Starting),
            changed(OutputState::Active),
        ];
        assert_eq!(drain(&mut user_events), expected);
        assert_eq!(drain(&mut agent_events), expected);

        let status = Call::Query {
            query: Query::OutputStatus {
                output_id: OutputId::new("yt"),
            },
        };
        let status = block_on(agent.call(status)).expect("status").result;
        assert_eq!(
            status,
            CallResult::Query {
                result: QueryResult::OutputStatus {
                    output_id: OutputId::new("yt"),
                    state: OutputState::Active
                }
            }
        );
        let again = Call::Action {
            action: Action::OutputStart {
                output_id: OutputId::new("yt"),
            },
        };
        let again = block_on(agent.call(again)).expect_err("already running");
        assert_eq!(code(&again), Some("action_failed"));
        let missing = Call::Action {
            action: Action::OutputStop {
                output_id: OutputId::new("nope"),
            },
        };
        let missing = block_on(agent.call(missing)).expect_err("no such output");
        assert_eq!(code(&missing), Some("not_found"));
    }

    #[test]
    fn a_pending_subscriber_is_woken_and_streams_end_with_the_engine() {
        struct Flag(AtomicBool);
        impl Wake for Flag {
            fn wake(self: Arc<Self>) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let flag = Arc::new(Flag(AtomicBool::new(false)));
        let waker = Waker::from(Arc::clone(&flag));

        let engine = MockEngine::new();
        let control = engine.connect();
        let mut events = control.events();
        // Subscriptions opened later see only later events.
        engine.emit(Event::Unknown);
        let mut late = control.events();
        assert_eq!(
            poll_event(&mut events, &waker),
            Poll::Ready(Some(Event::Unknown))
        );
        assert_eq!(poll_event(&mut events, &waker), Poll::Pending);
        assert_eq!(poll_event(&mut late, Waker::noop()), Poll::Pending);
        assert!(!flag.0.load(Ordering::SeqCst));

        engine.emit(Event::Unknown);
        assert!(flag.0.load(Ordering::SeqCst), "the pending poll was woken");
        assert_eq!(
            poll_event(&mut events, &waker),
            Poll::Ready(Some(Event::Unknown))
        );

        drop(late);
        drop(control);
        drop(engine);
        assert_eq!(poll_event(&mut events, &waker), Poll::Ready(None));
    }
}
