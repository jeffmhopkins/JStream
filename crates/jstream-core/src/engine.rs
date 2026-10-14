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
use crate::session::InProcessControl;
use jstream_graph::Graph;
use jstream_protocol::{
    Action, Batch, Call, CallResult, ClientId, Event, IdempotencyKey, OutputId, ProtocolError,
    Query, QueryResult, RequestId, Rev,
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
/// So far it holds the control plane only: the scene graph (`jstream-graph`), the
/// outputs' runtime state (a placeholder until the media pipeline lands), per-client
/// idempotency keys and event fan-out. It starts no threads and needs no async runtime:
/// every call completes before its future is first polled.
#[derive(Clone, Default)]
pub struct Engine {
    shared: Arc<Mutex<State>>,
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
        let mut wake = Vec::new();
        let result = lock(&self.shared).execute(client_id, request_id, call, &mut wake);
        for waker in wake {
            waker.wake();
        }
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
