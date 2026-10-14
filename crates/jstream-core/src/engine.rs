//! The [`Engine`]: the scene graph, the outputs' runtime state and the event subscribers,
//! behind one lock, and the execution of every protocol call against them.
//!
//! Everything a call changes is changed under that lock, and its events are queued under
//! it too, so every subscriber sees events in the order the changes happened, and
//! `state.changed` events in `rev` order. Subscribers are woken only after the lock is
//! released.

use crate::events::{QueueRef, Subscribers, Subscription};
use crate::lock;
use crate::outputs::Outputs;
use crate::runtime::{LivePublish, MediaHost, OutputStats, Running, launch, parts_from};
use crate::session::InProcessControl;
use jstream_graph::Graph;
use jstream_platform::Backends;
use jstream_protocol::{
    Action, Batch, Call, CallResult, ClientId, Event, IdempotencyKey, OutputId, OutputState,
    ProtocolError, Query, QueryResult, RequestId, Rev,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::task::Waker;

/// The JStream engine. Every client talks to it through a session it opens with
/// [`connect`](Engine::connect).
///
/// An `Engine` is a handle: cloning it shares the same engine, so a host can hand one
/// clone to each interface (the server's connector, an MCP session). The engine lives as
/// long as any handle or session does; when the last one goes away, every event stream
/// ends after what it has buffered.
///
/// So far the control plane is the scene graph (`jstream-graph`), the outputs'
/// runtime state, per-client idempotency keys and event fan-out. Without
/// [`with_backends`](Self::with_backends), an output action only steps the placeholder
/// state machine and the call completes before its future is first polled. With
/// backends and a publish target, `output.start` blocks the call until the pipeline
/// is up or has failed, still without an async runtime.
#[derive(Clone, Default)]
pub struct Engine {
    shared: Arc<Mutex<State>>,
    media: Option<Arc<MediaHost>>,
}

impl Engine {
    /// An engine with an empty collection at `rev` 0 and no outputs.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an output in the `stopped` state, so `output.start`, `output.stop` and
    /// `output.status` can name it. Outputs are runtime state: this changes no `rev` and
    /// emits no event. Adding an ID twice keeps the first.
    ///
    /// Outputs are configured this way until the protocol has edits for them.
    #[must_use]
    pub fn with_output(self, output_id: OutputId) -> Self {
        lock(&self.shared).outputs.add(output_id);
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
        lock(&host.running).as_ref().map(Running::stats)
    }

    /// Opens a new client session. It must send `hello` before any call.
    pub fn connect(&self) -> InProcessControl {
        InProcessControl::new(self.clone())
    }

    /// The current state revision.
    pub fn rev(&self) -> Rev {
        lock(&self.shared).graph.rev()
    }

    /// Assigns the next client ID and returns it with the current `rev`.
    pub(crate) fn register_client(&self) -> (ClientId, Rev) {
        let mut state = lock(&self.shared);
        state.clients += 1;
        let client_id = ClientId::new(format!("client_{}", state.clients));
        (client_id, state.graph.rev())
    }

    /// Opens an event subscription: the stream, and the weak handle its session keeps
    /// to end it.
    pub(crate) fn subscribe(&self) -> (Subscription, QueueRef) {
        lock(&self.shared).subscribers.subscribe()
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
        } else {
            let mut wake = Vec::new();
            let result = lock(&self.shared).execute(client_id, request_id, call, &mut wake);
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
        let (parts, publish) = {
            let mut state = lock(&self.shared);
            let current = state.outputs.state(&output_id)?;
            let idle = matches!(current, OutputState::Stopped | OutputState::Failed);
            if !idle {
                return Err(already_running(&output_id));
            }
            if lock(&host.running).is_some() {
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
            match state.outputs.begin(&output_id, true) {
                Ok(step) => emit_output(&mut state, &output_id, step, &mut wake),
                Err(err) => {
                    *lock(&host.publish) = Some(publish);
                    return Err(err);
                }
            }
            (parts, publish)
        };
        wake_all(wake);

        match launch(parts, publish, output_id.clone()) {
            Ok(running) => {
                *lock(&host.running) = Some(running);
                self.settle(&output_id, OutputState::Active);
                Ok(CallResult::Action {})
            }
            Err(reason) => {
                self.settle(&output_id, OutputState::Failed);
                Err(ProtocolError::ActionFailed { reason })
            }
        }
    }

    fn stop_pipeline(&self, output_id: OutputId) -> Result<CallResult, ProtocolError> {
        let Some(host) = &self.media else {
            return Err(not_implemented("output.stop"));
        };
        let mut wake = Vec::new();
        let running = {
            let mut state = lock(&self.shared);
            let step = state.outputs.begin(&output_id, false)?;
            emit_output(&mut state, &output_id, step, &mut wake);
            match lock(&host.running).take() {
                Some(running) if running.output_id() == &output_id => Some(running),
                Some(other) => {
                    *lock(&host.running) = Some(other);
                    None
                }
                None => None,
            }
        };
        wake_all(wake);
        // Join outside the collection lock. `Running`'s drop stops the threads.
        drop(running);
        self.settle(&output_id, OutputState::Stopped);
        Ok(CallResult::Action {})
    }

    fn settle(&self, output_id: &OutputId, state: OutputState) {
        let mut wake = Vec::new();
        {
            let mut engine = lock(&self.shared);
            engine.outputs.settle(output_id, state);
            emit_output(&mut engine, output_id, state, &mut wake);
        }
        wake_all(wake);
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
