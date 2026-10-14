//! The [`Engine`]: the scene graph, the outputs' runtime state and the event subscribers,
//! behind one lock, and the execution of every protocol call against them.
//!
//! Collection changes and the events they queue happen under that lock, so every
//! subscriber sees them in the order they happened, and `state.changed` events in `rev`
//! order. A pipeline start or stop holds the lock only while it changes output state.
//! Device open and thread joins happen after it is released. Subscribers are woken only
//! after the lock is released.

use crate::drops::{self, DropLedger, DropSource, DropSourceId};
use crate::events::{QueueRef, Subscribers, Subscription};
use crate::lock;
use crate::outputs::Outputs;
use crate::runtime::{
    Gate, LivePublish, MediaHost, OutputStats, Pending, Running, launch, parts_from,
};
use crate::session::InProcessControl;
use crate::snapshot::{self, Frame, SourceFrameError};
use jstream_graph::Graph;
use jstream_platform::Backends;
use jstream_protocol::{
    Action, Batch, Call, CallResult, ClientId, DropCounts, Event, IdempotencyKey, OutputId,
    OutputState, ProtocolError, Query, QueryResult, RequestId, Rev, SourceId,
};
use jstream_render::Compositor;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::task::Waker;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// The JStream engine. Every client talks to it through a session it opens with
/// [`connect`](Engine::connect).
///
/// An `Engine` is a handle: cloning it shares the same engine, so a host can hand one
/// clone to each interface (the server's connector, an MCP session). The engine lives as
/// long as any handle or session does; when the last one goes away, every event stream
/// ends after what it has buffered.
///
/// So far it holds the control plane and `video.snapshot`: the scene graph
/// (`jstream-graph`), the outputs' runtime state, the latest CPU frame of each source,
/// per-client idempotency keys, event fan-out, and the drop counters a `stats.tick`
/// reports. Without [`with_backends`](Self::with_backends), an output action only
/// steps the placeholder state machine and the call completes before its future is
/// first polled. With backends and a publish target, `output.start` blocks the call
/// until the pipeline is up or has failed, still without an async runtime. That
/// pipeline registers its drop sources and emits a tick about once a second. The
/// snapshot compositor is opened on the first `video.snapshot`.
#[derive(Clone)]
pub struct Engine {
    shared: Arc<Shared>,
    media: Option<Arc<MediaHost>>,
    /// Drop sources, on their own lock so a sample never runs under the collection
    /// lock and a source cannot deadlock by calling back into a command.
    drops: Arc<Mutex<DropLedger>>,
}

/// The collection lock and the compositor lock. They are never held together: a
/// snapshot copies its shot, drops the collection lock, then waits on the GPU.
/// The pipeline threads never take either lock.
struct Shared {
    state: Mutex<State>,
    compositor: Mutex<Option<Compositor>>,
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                compositor: Mutex::new(None),
            }),
            media: None,
            drops: Arc::new(Mutex::new(DropLedger::default())),
        }
    }
}

impl Engine {
    /// An engine with an empty collection at `rev` 0 and no outputs.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores the latest straight-alpha sRGB RGBA8 frame of `source_id`.
    ///
    /// This is runtime state: it does not change `rev` and emits no event. A snapshot
    /// uses it when an item places that source. A frame may arrive before the source
    /// exists; it is kept either way. Replaces any frame already stored for the id.
    ///
    /// # Errors
    /// [`SourceFrameError::Empty`] when a side is zero, [`SourceFrameError::TooLarge`]
    /// when the buffer would not fit, and [`SourceFrameError::Length`] when `rgba` is
    /// not `width * height * 4` bytes.
    pub fn set_source_frame(
        &self,
        source_id: &SourceId,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<(), SourceFrameError> {
        let frame = Frame::new(width, height, rgba)?;
        lock(&self.shared.state)
            .frames
            .insert(source_id.clone(), frame);
        Ok(())
    }

    /// Adds an output in the `stopped` state, so `output.start`, `output.stop` and
    /// `output.status` can name it. Outputs are runtime state: this changes no `rev` and
    /// emits no event. Adding an ID twice keeps the first.
    ///
    /// Outputs are configured this way until the protocol has edits for them.
    #[must_use]
    pub fn with_output(self, output_id: OutputId) -> Self {
        lock(&self.shared.state).outputs.add(output_id);
        self
    }

    /// Injects platform backends. `output.start` then runs the media pipeline
    /// instead of the placeholder state machine. Configure this before
    /// [`connect`](Self::connect); clones made earlier do not see it.
    ///
    /// A start still needs a publish target ([`with_publish`](Self::with_publish)).
    /// Without one it fails with `action_failed` and changes no output state.
    #[must_use]
    pub fn with_backends(mut self, backends: Backends) -> Self {
        let host = self.host();
        *lock(&host.backends) = Some(backends);
        self
    }

    /// Stores the destination `output.start` publishes into. The start consumes it.
    /// A failed launch drops it, so the host sets another before retrying. Requires
    /// [`with_backends`](Self::with_backends) first; otherwise the target is not kept.
    #[must_use]
    pub fn with_publish(self, publish: Box<dyn LivePublish>) -> Self {
        if let Some(host) = &self.media {
            *lock(&host.publish) = Some(publish);
        }
        self
    }

    /// Encoder and subscriber counters for the running pipeline, if one is up.
    pub fn output_stats(&self) -> Option<OutputStats> {
        let host = self.media.as_ref()?;
        lock(&host.gate).running.as_ref().map(Running::stats)
    }

    /// Registers `source` as a contributor to `stats.tick`.
    ///
    /// The returned id removes it. Removing latches the source's last total, so a
    /// finished stage does not make the running total fall. `source`'s
    /// [`drops`](DropSource::drops) runs on the control thread and must not call back
    /// into this engine.
    pub fn add_drop_source(&self, source: impl DropSource + 'static) -> DropSourceId {
        drops::lock_ledger(&self.drops).add(source)
    }

    /// Stops reading `id`, keeping the total it last reported. An unknown id does nothing.
    pub fn remove_drop_source(&self, id: DropSourceId) {
        drops::lock_ledger(&self.drops).remove(id);
    }

    /// Sums every drop source and emits one `stats.tick` to every subscriber.
    ///
    /// Totals are running sums since the engine started. `seq` starts at 1. The
    /// running pipeline calls this about once a second from its own sampler thread,
    /// which only takes the drop ledger and then the collection lock. A test or a
    /// host can call it too. An idle engine emits nothing on its own. Safe to call
    /// with no sources: the tick then carries zeros.
    pub fn emit_stats_tick(&self) {
        emit_sampled(&self.shared, &self.drops);
    }

    /// The ledger the pipeline's [`DropGuard`](crate::drops::DropGuard) removes from.
    pub(crate) fn drop_ledger(&self) -> Arc<Mutex<DropLedger>> {
        Arc::clone(&self.drops)
    }

    /// A thread that emits `stats.tick` about once a second until `stop` is set.
    ///
    /// It holds the shared collection and the drop ledger, not the [`MediaHost`], so
    /// dropping the last engine can still stop the pipeline and then join this thread.
    pub(crate) fn spawn_stats_ticker(&self, stop: Arc<AtomicBool>) -> Option<JoinHandle<()>> {
        let shared = Arc::clone(&self.shared);
        let ledger = Arc::clone(&self.drops);
        match thread::Builder::new()
            .name("jstream-stats".into())
            .spawn(move || {
                stats_ticker(shared, ledger, stop);
            }) {
            Ok(handle) => Some(handle),
            Err(err) => {
                tracing::error!(error = %err, "stats ticker did not start");
                None
            }
        }
    }

    /// Opens a new client session. It must send `hello` before any call.
    pub fn connect(&self) -> InProcessControl {
        InProcessControl::new(self.clone())
    }

    /// The current state revision.
    pub fn rev(&self) -> Rev {
        lock(&self.shared.state).graph.rev()
    }

    /// Assigns the next client ID and returns it with the current `rev`.
    pub(crate) fn register_client(&self) -> (ClientId, Rev) {
        let mut state = lock(&self.shared.state);
        state.clients += 1;
        let client_id = ClientId::new(format!("client_{}", state.clients));
        (client_id, state.graph.rev())
    }

    /// Opens an event subscription: the stream, and the weak handle its session keeps
    /// to end it.
    pub(crate) fn subscribe(&self) -> (Subscription, QueueRef) {
        lock(&self.shared.state).subscribers.subscribe()
    }

    /// Executes `call` for `client_id`, whose request it is, and wakes the subscribers
    /// its events reached.
    ///
    /// The call runs in a `command` span naming the command and the request. Its outcome
    /// is logged after the lock is released, so a slow log writer never holds up other
    /// sessions. Only names, IDs and error codes are logged, never a command's arguments
    /// (R-SEC-001).
    pub(crate) fn execute(
        &self,
        client_id: &ClientId,
        request_id: &RequestId,
        call: Call,
    ) -> Result<CallResult, ProtocolError> {
        let span = tracing::info_span!(
            "command",
            command = command_name(&call),
            client_id = %client_id,
            request_id = %request_id,
        );
        let _entered = span.enter();
        let output = match &call {
            Call::Action {
                action: Action::OutputStart { output_id } | Action::OutputStop { output_id },
            } => Some(output_id.clone()),
            _ => None,
        };
        let result = if self.backends_ready()
            && matches!(
                call,
                Call::Action {
                    action: Action::OutputStart { .. } | Action::OutputStop { .. }
                }
            ) {
            self.run_media_action(call)
        } else if matches!(
            call,
            Call::Query {
                query: Query::VideoSnapshot
            }
        ) {
            self.video_snapshot()
        } else {
            let mut wake = Vec::new();
            let result = lock(&self.shared.state).execute(client_id, request_id, call, &mut wake);
            for waker in wake {
                waker.wake();
            }
            result
        };
        match (&result, output) {
            (Ok(CallResult::Batch { rev }), _) => tracing::debug!(rev = %rev, "batch applied"),
            (Ok(_), Some(output_id)) => {
                tracing::info!(output_id = %output_id, "output action done");
            }
            (Ok(_), None) => tracing::debug!("call done"),
            (Err(error), Some(output_id)) => {
                tracing::info!(output_id = %output_id, code = error.code(), "output action failed");
            }
            (Err(error), None) => tracing::debug!(code = error.code(), "call failed"),
        }
        result
    }

    fn host(&mut self) -> Arc<MediaHost> {
        let host = self.media.get_or_insert_with(|| Arc::new(MediaHost::new()));
        Arc::clone(host)
    }

    fn backends_ready(&self) -> bool {
        self.media
            .as_ref()
            .is_some_and(|host| lock(&host.backends).is_some())
    }

    /// `output.start` / `output.stop` when backends are injected. Events are emitted
    /// on this thread. The pipeline threads never take the collection lock.
    fn run_media_action(&self, call: Call) -> Result<CallResult, ProtocolError> {
        let Call::Action { action } = call else {
            return Err(not_implemented("this kind of call"));
        };
        match action {
            Action::OutputStart { output_id } => self.start_pipeline(output_id),
            Action::OutputStop { output_id } => self.stop_pipeline(output_id),
            other => Err(not_implemented(other.name())),
        }
    }

    fn start_pipeline(&self, output_id: OutputId) -> Result<CallResult, ProtocolError> {
        let Some(host) = &self.media else {
            return Err(not_implemented("output.start"));
        };
        let mut wake = Vec::new();
        let (parts, publish, generation) = loop {
            // A failed pipeline may still be joining. Wait that out before opening
            // devices, and do not hold the collection lock across the wait.
            {
                let mut gate = lock(&host.gate);
                while gate.draining {
                    gate = host
                        .gate_cv
                        .wait(gate)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
            let mut state = lock(&self.shared.state);
            let mut gate = lock(&host.gate);
            if gate.draining {
                continue;
            }
            let current = state.outputs.state(&output_id)?;
            let idle = matches!(current, OutputState::Stopped | OutputState::Failed);
            if !idle {
                return Err(already_running(&output_id));
            }
            if gate.running.is_some() || gate.pending.is_some() {
                return Err(ProtocolError::ActionFailed {
                    reason: "another output is already running".into(),
                });
            }
            let Some(publish) = lock(&host.publish).take() else {
                return Err(ProtocolError::ActionFailed {
                    reason: "output.start has no publish target".into(),
                });
            };
            let parts = {
                let backends = lock(&host.backends);
                let Some(backends) = backends.as_ref() else {
                    drop(backends);
                    *lock(&host.publish) = Some(publish);
                    return Err(not_implemented("output.start"));
                };
                parts_from(backends)
            };
            let generation = host.next_generation.fetch_add(1, Ordering::Relaxed);
            gate.pending = Some(Pending {
                output_id: output_id.clone(),
                generation,
                cancel: false,
            });
            match state.outputs.begin(&output_id, true) {
                Ok(step) => emit_output(&mut state, &output_id, step, &mut wake),
                Err(err) => {
                    gate.pending = None;
                    *lock(&host.publish) = Some(publish);
                    return Err(err);
                }
            }
            break (parts, publish, generation);
        };
        wake_all(std::mem::take(&mut wake));

        // Cleared once this start settles, or on drop if the start is cancelled.
        let mut inflight = Inflight {
            host: Arc::clone(host),
            generation,
            armed: true,
        };
        let (fail_tx, fail_rx) = mpsc::channel();
        let launched =
            launch(parts, publish, output_id.clone(), generation, fail_tx).map(|mut running| {
                running.arm_stats(self);
                running
            });
        let outcome = {
            let mut state = lock(&self.shared.state);
            let mut gate = lock(&host.gate);
            let cancelled = gate
                .pending
                .as_ref()
                .is_some_and(|pending| pending.generation == generation && pending.cancel);
            if cancelled {
                StartOutcome::Cancelled(launched.ok())
            } else {
                match launched {
                    Ok(running) => match fail_rx.try_recv() {
                        Ok(reason) => {
                            abandon_start(
                                &mut state,
                                &mut gate,
                                &mut inflight,
                                &output_id,
                                &mut wake,
                            );
                            StartOutcome::Failed(Some(running), reason)
                        }
                        Err(TryRecvError::Disconnected) => {
                            abandon_start(
                                &mut state,
                                &mut gate,
                                &mut inflight,
                                &output_id,
                                &mut wake,
                            );
                            StartOutcome::Failed(
                                Some(running),
                                "the output thread ended before the pipeline was active".into(),
                            )
                        }
                        Err(TryRecvError::Empty) => {
                            let watched = output_id.clone();
                            let host_for_watch = Arc::downgrade(host);
                            let shared_for_watch = Arc::downgrade(&self.shared);
                            match thread::Builder::new()
                                .name("jstream-publish-watch".into())
                                .spawn(move || {
                                    watch_publish(
                                        shared_for_watch,
                                        host_for_watch,
                                        watched,
                                        generation,
                                        fail_rx,
                                    );
                                }) {
                                Ok(watcher) => {
                                    gate.running = Some(running);
                                    gate.watcher = Some(watcher);
                                    gate.pending = None;
                                    inflight.armed = false;
                                    host.gate_cv.notify_all();
                                    state.outputs.settle(&output_id, OutputState::Active);
                                    emit_output(
                                        &mut state,
                                        &output_id,
                                        OutputState::Active,
                                        &mut wake,
                                    );
                                    StartOutcome::Active
                                }
                                Err(err) => {
                                    abandon_start(
                                        &mut state,
                                        &mut gate,
                                        &mut inflight,
                                        &output_id,
                                        &mut wake,
                                    );
                                    StartOutcome::Failed(
                                        Some(running),
                                        format!("starting the publish watcher: {err}"),
                                    )
                                }
                            }
                        }
                    },
                    Err(reason) => {
                        abandon_start(&mut state, &mut gate, &mut inflight, &output_id, &mut wake);
                        StartOutcome::Failed(None, reason)
                    }
                }
            }
        };
        wake_all(std::mem::take(&mut wake));
        match outcome {
            StartOutcome::Active => Ok(CallResult::Action {}),
            StartOutcome::Cancelled(running) => {
                drop(running);
                drop(inflight);
                Err(ProtocolError::ActionFailed {
                    reason: format!("output `{output_id}` stopped while starting"),
                })
            }
            StartOutcome::Failed(running, reason) => {
                drop(running);
                Err(ProtocolError::ActionFailed { reason })
            }
        }
    }

    fn stop_pipeline(&self, output_id: OutputId) -> Result<CallResult, ProtocolError> {
        let Some(host) = &self.media else {
            return Err(not_implemented("output.stop"));
        };
        let mut wake = Vec::new();
        let (running, watcher, wait_for) = {
            let mut state = lock(&self.shared.state);
            let mut gate = lock(&host.gate);
            if state.outputs.state(&output_id)? == OutputState::Stopping {
                return Err(ProtocolError::ActionFailed {
                    reason: format!("output `{output_id}` is already stopping"),
                });
            }
            let step = state.outputs.begin(&output_id, false)?;
            emit_output(&mut state, &output_id, step, &mut wake);
            if let Some(pending) = gate.pending.as_mut()
                && pending.output_id == output_id
            {
                pending.cancel = true;
                (None, None, Some(pending.generation))
            } else {
                let running = match gate.running.take() {
                    Some(running) if running.output_id() == &output_id => Some(running),
                    Some(other) => {
                        gate.running = Some(other);
                        None
                    }
                    None => None,
                };
                let watcher = if running.is_some() {
                    gate.watcher.take()
                } else {
                    None
                };
                (running, watcher, None)
            }
        };
        wake_all(wake);
        if let Some(generation) = wait_for {
            // The start drops its threads, then clears `pending` and notifies.
            let mut gate = lock(&host.gate);
            while gate
                .pending
                .as_ref()
                .is_some_and(|pending| pending.generation == generation)
            {
                gate = host
                    .gate_cv
                    .wait(gate)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
        // Join outside the collection lock. `Running`'s drop stops the threads.
        drop(running);
        if let Some(watcher) = watcher
            && watcher.join().is_err()
        {
            tracing::error!("publish watcher panicked");
        }
        self.settle(&output_id, OutputState::Stopped);
        Ok(CallResult::Action {})
    }

    fn settle(&self, output_id: &OutputId, state: OutputState) {
        let mut wake = Vec::new();
        {
            let mut engine = lock(&self.shared.state);
            engine.outputs.settle(output_id, state);
            emit_output(&mut engine, output_id, state, &mut wake);
        }
        wake_all(wake);
    }

    /// `video.snapshot`. The shot is copied under the collection lock; readback runs
    /// after that lock is released.
    fn video_snapshot(&self) -> Result<CallResult, ProtocolError> {
        let shot = {
            let state = lock(&self.shared.state);
            snapshot::plan(&state.graph, &state.frames)
        };
        let result = snapshot::draw(&shot, &self.shared.compositor)?;
        Ok(CallResult::Query { result })
    }
}

enum StartOutcome {
    Active,
    /// Stop won. Any threads `launch` opened are dropped before the in-flight
    /// generation is cleared, so stop can settle `stopped`.
    Cancelled(Option<Running>),
    Failed(Option<Running>, String),
}

/// Clears `pending` on drop unless the start already settled.
struct Inflight {
    host: Arc<MediaHost>,
    generation: u64,
    armed: bool,
}

impl Drop for Inflight {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut gate = lock(&self.host.gate);
        if gate
            .pending
            .as_ref()
            .is_some_and(|pending| pending.generation == self.generation)
        {
            gate.pending = None;
            self.host.gate_cv.notify_all();
        }
    }
}

fn abandon_start(
    state: &mut State,
    gate: &mut Gate,
    inflight: &mut Inflight,
    output_id: &OutputId,
    wake: &mut Vec<Waker>,
) {
    gate.pending = None;
    inflight.armed = false;
    inflight.host.gate_cv.notify_all();
    state.outputs.settle(output_id, OutputState::Failed);
    emit_output(state, output_id, OutputState::Failed, wake);
}

/// Blocks until the publish target fails. A disconnected channel is a clean stop.
///
/// The pointers are weak, so this thread does not keep the engine alive. Dropping
/// the last [`Engine`] drops [`MediaHost`], which joins the pipeline and then this
/// thread. A real failure upgrades the pointers only long enough to settle.
fn watch_publish(
    shared: Weak<Shared>,
    host: Weak<MediaHost>,
    output_id: OutputId,
    generation: u64,
    fail_rx: Receiver<String>,
) {
    let Ok(reason) = fail_rx.recv() else {
        return;
    };
    let (Some(shared), Some(host)) = (shared.upgrade(), host.upgrade()) else {
        return;
    };
    tracing::error!(output_id = %output_id, error = %reason, "publish failed");
    let mut wake = Vec::new();
    let running = {
        let mut state = lock(&shared.state);
        let mut gate = lock(&host.gate);
        let ours = gate.running.as_ref().is_some_and(|running| {
            running.output_id() == &output_id && running.generation() == generation
        });
        let active = matches!(state.outputs.state(&output_id), Ok(OutputState::Active));
        if ours && active {
            state.outputs.settle(&output_id, OutputState::Failed);
            emit_output(&mut state, &output_id, OutputState::Failed, &mut wake);
            // The join happens below, outside the locks. A new start waits until
            // `draining` clears so it does not open devices over this teardown.
            gate.draining = true;
            // Detach this watcher. `JoinHandle`'s drop does not join, so the
            // thread can go on to drop `Running` without joining itself.
            drop(gate.watcher.take());
            gate.running.take()
        } else {
            None
        }
    };
    wake_all(wake);
    let took = running.is_some();
    drop(running);
    if took {
        let mut gate = lock(&host.gate);
        gate.draining = false;
        host.gate_cv.notify_all();
    }
}

fn already_running(output_id: &OutputId) -> ProtocolError {
    ProtocolError::ActionFailed {
        reason: format!("output `{output_id}` is already running"),
    }
}

fn emit_output(state: &mut State, output_id: &OutputId, step: OutputState, wake: &mut Vec<Waker>) {
    state.subscribers.emit(
        &Event::OutputStateChanged {
            output_id: output_id.clone(),
            state: step,
        },
        wake,
    );
}

fn emit_sampled(shared: &Shared, ledger: &Mutex<DropLedger>) {
    let sampled = drops::lock_ledger(ledger).sample();
    let mut wake = Vec::new();
    lock(&shared.state).emit_stats(sampled, &mut wake);
    for waker in wake {
        waker.wake();
    }
}

/// Wakes every 100 ms so a stop joins promptly, and emits once those waits add up to a second.
fn stats_ticker(shared: Arc<Shared>, ledger: Arc<Mutex<DropLedger>>, stop: Arc<AtomicBool>) {
    let mut waited_ms = 0u32;
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(100));
        if stop.load(Ordering::Relaxed) {
            break;
        }
        waited_ms += 100;
        if waited_ms < 1_000 {
            continue;
        }
        waited_ms = 0;
        emit_sampled(&shared, &ledger);
    }
}

fn wake_all(wake: Vec<Waker>) {
    for waker in wake {
        waker.wake();
    }
}

/// The name a `command` span carries: `batch`, or the action's or query's own name.
fn command_name(call: &Call) -> &'static str {
    match call {
        Call::Batch(_) => "batch",
        Call::Action { action } => action.name(),
        Call::Query { query } => query.name(),
        _ => "unknown",
    }
}

/// Everything the engine knows.
#[derive(Default)]
struct State {
    /// The collection and its `rev`. Only `Graph::apply` changes it.
    graph: Graph,
    outputs: Outputs,
    /// The `rev` each batch applied with an idempotency key produced, per client.
    /// Kept for the engine's lifetime: how long to remember keys is protocol open
    /// question 6.
    applied: BTreeMap<(ClientId, IdempotencyKey), Rev>,
    /// How many sessions have completed `hello`; numbers the client IDs.
    clients: u64,
    subscribers: Subscribers,
    /// The latest CPU frame of each source, keyed by id. Runtime state: not part of
    /// `rev`. A snapshot draws an item from this when the source is not itself a scene.
    frames: BTreeMap<SourceId, Frame>,
    /// How many `stats.tick` events this engine has emitted. The next one uses this plus one.
    stats_seq: u64,
}

impl State {
    fn execute(
        &mut self,
        client_id: &ClientId,
        request_id: &RequestId,
        call: Call,
        wake: &mut Vec<Waker>,
    ) -> Result<CallResult, ProtocolError> {
        match call {
            Call::Batch(batch) => self.batch(client_id, request_id, &batch, wake),
            Call::Action { action } => self.action(action, wake),
            Call::Query { query } => self.query(&query),
            _ => Err(not_implemented("this kind of call")),
        }
    }

    /// Applies `batch` atomically through `Graph::apply_batch`, which checks
    /// `expected_rev` (before rejecting an empty batch), applies every edit or none, and
    /// bumps `rev` by exactly one.
    ///
    /// A batch whose idempotency key this client already used is not applied again: it
    /// gets the `rev` it produced the first time, and emits nothing. That check comes
    /// first, so a retry succeeds even after `rev` moved on.
    fn batch(
        &mut self,
        client_id: &ClientId,
        request_id: &RequestId,
        batch: &Batch,
        wake: &mut Vec<Waker>,
    ) -> Result<CallResult, ProtocolError> {
        let keyed = batch
            .idempotency_key
            .as_ref()
            .map(|key| (client_id.clone(), key.clone()));
        if let Some(rev) = keyed.as_ref().and_then(|keyed| self.applied.get(keyed)) {
            return Ok(CallResult::Batch { rev: *rev });
        }
        // The inverse is dropped: per-client undo needs undo requests in the protocol
        // first (protocol open question 4).
        let applied = self.graph.apply_batch(batch)?;
        if let Some(keyed) = keyed {
            self.applied.insert(keyed, applied.rev);
        }
        self.subscribers.emit(
            &Event::StateChanged {
                rev: applied.rev,
                client_id: client_id.clone(),
                request_id: Some(request_id.clone()),
            },
            wake,
        );
        Ok(CallResult::Batch { rev: applied.rev })
    }

    fn action(
        &mut self,
        action: Action,
        wake: &mut Vec<Waker>,
    ) -> Result<CallResult, ProtocolError> {
        let (output_id, start) = match action {
            Action::OutputStart { output_id } => (output_id, true),
            Action::OutputStop { output_id } => (output_id, false),
            other => return Err(not_implemented(other.name())),
        };
        for state in self.outputs.transition(&output_id, start)? {
            self.subscribers.emit(
                &Event::OutputStateChanged {
                    output_id: output_id.clone(),
                    state,
                },
                wake,
            );
        }
        Ok(CallResult::Action {})
    }

    /// Collection reads go to `Graph::query`; `output.status` reads the outputs.
    fn query(&self, query: &Query) -> Result<CallResult, ProtocolError> {
        let result = match self.graph.query(query) {
            Some(result) => result?,
            None => match query {
                Query::OutputStatus { output_id } => QueryResult::OutputStatus {
                    output_id: output_id.clone(),
                    state: self.outputs.state(output_id)?,
                },
                other => return Err(not_implemented(other.name())),
            },
        };
        Ok(CallResult::Query { result })
    }

    fn emit_stats(&mut self, drops: DropCounts, wake: &mut Vec<Waker>) {
        self.stats_seq = self.stats_seq.saturating_add(1);
        self.subscribers.emit(
            &Event::StatsTick {
                seq: self.stats_seq,
                drops,
            },
            wake,
        );
    }
}

impl Drop for State {
    /// Ends every subscription when the engine goes away.
    fn drop(&mut self) {
        self.subscribers.close_all();
    }
}

/// A protocol variant added after this engine was written.
fn not_implemented(what: &str) -> ProtocolError {
    ProtocolError::Internal {
        reason: format!("the engine does not implement {what} yet"),
    }
}
