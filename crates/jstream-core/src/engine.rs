//! The [`Engine`]: the scene graph, the outputs' runtime state and the event subscribers,
//! behind one lock, and the execution of every protocol call against them.
//!
//! Collection changes and the events they queue happen under that lock, so every
//! subscriber sees them in the order they happened, and `state.changed` events in `rev`
//! order. A pipeline start or stop holds the lock only while it changes output state.
//! Device open and thread joins happen after it is released. Subscribers are woken only
//! after the lock is released.

use crate::audit::{self, Audit, AuditSink};
use crate::collection::{CollectionFile, CollectionFileError};
use crate::drops::{self, DropLedger, DropSource, DropSourceId};
use crate::events::{QueueRef, Subscribers, Subscription};
use crate::history::{History, Touched};
use crate::lock;
use crate::outputs::Outputs;
use crate::runtime::{
    Destination, Gate, LivePublish, MediaHost, OpenPublish, OutputStats, Pending, PipelineEvent,
    RenderHub, Running, launch, parts_from,
};
use crate::session::InProcessControl;
use crate::snapshot::{self, Frame, SnapshotReply, SnapshotWorker, SourceFrameError};
use crate::youtube::{Youtube, YoutubeSetup};
use jstream_graph::Graph;
use jstream_platform::{Backends, Secret, StreamKeyStore};
use jstream_protocol::{
    Action, ActionResult, Batch, Call, CallResult, ClientId, DropCounts, Edit, EntityKind, Event,
    HistoryCall, HistoryDirection, IdempotencyKey, OutputId, OutputKind, OutputState,
    ProtocolError, Query, QueryResult, RecordContainer, RequestId, Rev, RtmpTarget, SecretString,
    SourceId, YoutubeAuth,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
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

/// The collection lock, the render snapshot, and the snapshot thread that owns the
/// snapshot compositor. Every change to what the program mix draws publishes a new render
/// snapshot under the lock. A `video.snapshot` reads `rev` and the published snapshot
/// under the lock, drops it, then queues the snapshot for that thread. The pipeline
/// threads never take the lock.
struct Shared {
    state: Mutex<State>,
    snapshots: SnapshotWorker,
    /// Where `output.set_key` stores stream keys (R-SEC-001). On its own lock, taken
    /// only to clone the handle: the store is never called under the collection lock.
    keys: Mutex<Option<Arc<dyn StreamKeyStore>>>,
    /// Where each executed batch, undo, redo and action is recorded, if the host gave a sink.
    audit: Audit,
    /// Where the collection is saved, if the host named a file. Taken before the
    /// collection lock, never while holding it, so the disk write happens outside it.
    collection_file: Mutex<Option<CollectionFile>>,
    /// The YouTube sign-in and Live API client, if the host gave a setup
    /// ([`Engine::with_youtube`]). On its own lock, taken only to clone the handle: no
    /// YouTube call runs under the collection lock.
    youtube: Mutex<Option<Arc<Youtube>>>,
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new({
                    let mut state = State::default();
                    state.graph = Graph::new().with_kinds(crate::kinds::builtin());
                    state
                }),
                snapshots: SnapshotWorker::default(),
                keys: Mutex::new(None),
                audit: Audit::default(),
                collection_file: Mutex::new(None),
                youtube: Mutex::new(None),
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
    /// A `source.delete` drops the deleted source's frame.
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
        // Copied before the lock is taken; only the version is assigned under it.
        let frame = Frame::new(width, height, rgba, 0)?;
        let mut state = lock(&self.shared.state);
        state.frame_versions = state.frame_versions.saturating_add(1);
        let frame = frame.with_version(state.frame_versions);
        state.frames.insert(source_id.clone(), frame);
        state.publish_render();
        Ok(())
    }

    /// Adds an output in the `stopped` state, so `output.start`, `output.stop` and
    /// `output.status` can name it. Outputs are runtime state: this changes no `rev` and
    /// emits no event. Adding an ID twice keeps the first.
    ///
    /// A declared output is a placeholder: it has no target, so it is not in the
    /// collection. `output.create` configures an output through the protocol instead.
    #[must_use]
    pub fn with_output(self, output_id: OutputId) -> Self {
        lock(&self.shared.state).outputs.add(output_id);
        self
    }

    /// Gives the engine the OS credential store that holds stream keys (R-SEC-001):
    /// `output.set_key` writes to it, `output.clear_key` removes from it, and
    /// `output.status` reports whether it holds a key. Without one, those actions fail
    /// with `action_failed` and `key_set` is `false`. Clones share the store, made
    /// before or after.
    #[must_use]
    pub fn with_key_store(self, store: Arc<dyn StreamKeyStore>) -> Self {
        *lock(&self.shared.keys) = Some(store);
        self
    }

    /// Gives the engine YouTube (ADR-0041): the app's OAuth client, the HTTPS
    /// transport and the store for the account's refresh token. The YouTube actions and
    /// queries then work, and an output with a `youtube` target can start. Without it,
    /// `youtube.status` reports `unavailable` and the other YouTube calls fail with
    /// `action_failed`. Reads the store once, here: a token kept by an earlier sign-in
    /// signs the engine in. Configure this before [`connect`](Self::connect).
    #[must_use]
    pub fn with_youtube(self, setup: YoutubeSetup) -> Self {
        let shared = Arc::downgrade(&self.shared);
        let emit = Arc::new(move |event: Event| {
            if let Some(shared) = shared.upgrade() {
                let mut wake = Vec::new();
                lock(&shared.state).subscribers.emit(&event, &mut wake);
                wake_all(wake);
            }
        });
        *lock(&self.shared.youtube) = Some(Arc::new(Youtube::new(setup, emit)));
        self
    }

    /// The YouTube side, if the host gave one. The handle is cloned so no engine lock
    /// is held while it calls YouTube.
    pub(crate) fn youtube(&self) -> Option<Arc<Youtube>> {
        lock(&self.shared.youtube).clone()
    }

    /// Records every batch, undo, redo and action this engine executes in `sink`, after
    /// the call and with no engine lock held (see [`AuditSink`]). Without a sink nothing
    /// is recorded. Every clone of the engine shares it.
    #[must_use]
    pub fn with_audit(self, sink: Arc<dyn AuditSink>) -> Self {
        self.shared.audit.set(sink);
        self
    }

    /// Loads the collection from the file at `path` and saves it there after every
    /// change. Configure this before [`connect`](Self::connect): it replaces the
    /// collection and its `rev` with the file's.
    ///
    /// The file is a versioned JSON collection (`jstream-graph`), upgraded on load if it
    /// is older, and checked as edits are, against this engine's kinds. When there is
    /// no file at `path`, it is created with the current, empty, collection, so a path
    /// that can't be written fails here rather than at the first change.
    ///
    /// Every applied batch, undo and redo then replaces the file atomically before its
    /// call returns, so a client that has its `rev` knows the file holds it. A failed
    /// save is logged at `error` and leaves the call's result as it is: the change is
    /// applied, and the next change saves again. The `rev` is saved too, so revisions
    /// keep increasing across restarts and an `expected_rev` from before one can't
    /// match a different state. Undo history and idempotency keys are not saved.
    ///
    /// # Errors
    /// [`CollectionFileError`] when the file can't be read, holds no loadable
    /// collection (it is then left as it is), or can't be created.
    pub fn with_collection_file(
        self,
        path: impl Into<PathBuf>,
    ) -> Result<Self, CollectionFileError> {
        {
            let mut file = lock(&self.shared.collection_file);
            // The file is read (or created) with only the file's lock held, from a copy
            // of the graph taken under the collection lock.
            let current = lock(&self.shared.state).graph.clone();
            let (opened, graph) = CollectionFile::open(path.into(), &current)?;
            let mut state = lock(&self.shared.state);
            let state = &mut *state;
            state.graph = graph;
            state.outputs.sync(state.graph.state());
            // The loaded collection is what the program mix draws now.
            state.publish_render();
            *file = Some(opened);
        }
        Ok(self)
    }

    /// Saves the collection to its file, if the host named one and the file doesn't
    /// already hold this `rev` or a later one. Takes the file's lock, then the
    /// collection lock only to serialize, and writes with only the file's lock held, so
    /// saves happen one at a time and a late one never overwrites a newer collection.
    fn save_collection(&self) {
        let mut file = lock(&self.shared.collection_file);
        let Some(file) = file.as_mut() else {
            return;
        };
        let (rev, json) = {
            let state = lock(&self.shared.state);
            if state.graph.rev() <= file.saved() {
                return;
            }
            (state.graph.rev(), state.graph.to_collection_json())
        };
        if let Err(err) = file.save(rev, &json) {
            tracing::error!(
                path = %file.path().display(),
                rev = %rev,
                error = %err,
                "collection not saved"
            );
        }
    }

    /// What an output in the collection is and where it sends, or `None` if the
    /// collection has no such output. A host reads its target here when it opens the
    /// publish target for `output.start`, and its key from the store it injected. A
    /// recording output (`OutputKind::Record`) needs no publish target: the engine
    /// opens its file itself.
    pub fn output_kind(&self, output_id: &OutputId) -> Option<OutputKind> {
        lock(&self.shared.state)
            .graph
            .state()
            .output(output_id)
            .map(|output| output.kind().clone())
    }

    /// Injects platform backends. `output.start` then runs the media pipeline
    /// instead of the placeholder state machine. Configure this before
    /// [`connect`](Self::connect); clones made earlier do not see it.
    ///
    /// A start still needs a publish target ([`with_publish`](Self::with_publish))
    /// or an opener ([`with_publish_opener`](Self::with_publish_opener)). Without
    /// either it fails with `action_failed` and changes no output state. A recording
    /// output needs neither: its start records to the file the collection names.
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

    /// Stores how `output.start` opens its publish target when no one-shot target
    /// ([`with_publish`](Self::with_publish)) is waiting: every start calls `open`
    /// once, after `starting` and outside the engine's locks (except for a recording
    /// output, which never calls it), so a host that is live
    /// (with [`connect_rtmp`](crate::connect_rtmp)) connects on each start and can
    /// start again after a stop or a failure. An `open` error fails that start with
    /// `action_failed`. Requires [`with_backends`](Self::with_backends) first;
    /// otherwise it is not kept.
    #[must_use]
    pub fn with_publish_opener(self, open: OpenPublish) -> Self {
        if let Some(host) = &self.media {
            *lock(&host.open_publish) = Some(open);
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

    /// Emits one `stats.tick` when called: the pipeline's final tick after it stops.
    /// Holds the collection and the ledger, not the [`MediaHost`], like the ticker.
    pub(crate) fn stats_emitter(&self) -> Box<dyn FnOnce() + Send> {
        let shared = Arc::clone(&self.shared);
        let ledger = Arc::clone(&self.drops);
        Box::new(move || emit_sampled(&shared, &ledger))
    }

    /// Whether a frame is stored for `source_id`.
    #[cfg(test)]
    pub(crate) fn has_source_frame(&self, source_id: &SourceId) -> bool {
        lock(&self.shared.state).frames.contains_key(source_id)
    }

    /// Raises how long `output.start` waits for the compositor to open. The GPU tests
    /// use it: lavapipe on a loaded CI runner can take longer than the 10 s default.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_open_timeout(self, timeout: std::time::Duration) -> Self {
        if let Some(host) = &self.media {
            *lock(&host.open_timeout) = timeout;
        }
        self
    }

    /// Replaces how the render thread composites. Tests use it to run the pipeline
    /// without a wgpu adapter. Requires [`with_backends`](Self::with_backends) first.
    #[must_use]
    pub(crate) fn with_draw(self, open: crate::runtime::OpenDraw) -> Self {
        if let Some(host) = &self.media {
            *lock(&host.draw) = open;
        }
        self
    }

    /// Test support, not a product option: the render thread composites on the CPU
    /// instead of opening the wgpu compositor, so the pipeline runs on a machine with
    /// no adapter. It copies the captured frame, or draws black, which is what this
    /// cut's one-item composite produces. Engine scenario tests
    /// (`crates/jstream-core/tests/scenarios.rs`) use it with the platform fakes.
    /// Requires [`with_backends`](Self::with_backends) first; otherwise it does nothing.
    #[doc(hidden)]
    #[must_use]
    pub fn with_cpu_compositor(self) -> Self {
        self.with_draw(crate::runtime::cpu_draw())
    }

    /// Opens a new client session. It must send `hello` before any call.
    pub fn connect(&self) -> InProcessControl {
        InProcessControl::new(self.clone())
    }

    /// The current state revision.
    pub fn rev(&self) -> Rev {
        lock(&self.shared.state).graph.rev()
    }

    /// Sets how many undo steps each client keeps (100 by default). The oldest steps
    /// are dropped first, and 0 keeps none.
    #[must_use]
    pub fn with_history_limit(self, limit: usize) -> Self {
        lock(&self.shared.state).history.set_limit(limit);
        self
    }

    /// Drops `client_id`'s undo and redo history: its session ended (ADR-0032).
    pub(crate) fn forget_client(&self, client_id: &ClientId) {
        lock(&self.shared.state).history.forget(client_id);
    }

    /// How many steps `client_id` can undo and redo.
    #[cfg(test)]
    pub(crate) fn history_depths(&self, client_id: &ClientId) -> (u32, u32) {
        lock(&self.shared.state).history.depths(client_id)
    }

    /// Assigns the next client ID, records `client_name` for it (the name the audit log
    /// shows), and returns the ID with the current `rev`.
    pub(crate) fn register_client(&self, client_name: &str) -> (ClientId, Rev) {
        let mut state = lock(&self.shared.state);
        state.clients += 1;
        let client_id = ClientId::new(format!("client_{}", state.clients));
        state
            .client_names
            .insert(client_id.clone(), audit::client_name(client_name));
        (client_id, state.graph.rev())
    }

    /// Records `client_name` as the name of `client_id`, which sent `hello` again, and
    /// returns the current `rev`.
    pub(crate) fn rename_client(&self, client_id: &ClientId, client_name: &str) -> Rev {
        let mut state = lock(&self.shared.state);
        state
            .client_names
            .insert(client_id.clone(), audit::client_name(client_name));
        state.graph.rev()
    }

    /// What an audit entry needs before its call runs: the current `rev` and the
    /// client's name from its latest `hello`.
    fn audit_context(&self, client_id: &ClientId) -> (Rev, String) {
        let state = lock(&self.shared.state);
        let name = state
            .client_names
            .get(client_id)
            .cloned()
            .unwrap_or_default();
        (state.graph.rev(), name)
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
    ///
    /// Every call but `video.snapshot` runs to completion here. A snapshot is queued
    /// for the snapshot thread and comes back [`Executed::Pending`], with its span; its
    /// outcome is logged when it completes.
    pub(crate) fn execute(
        &self,
        client_id: &ClientId,
        request_id: &RequestId,
        call: Call,
    ) -> Executed {
        let span = tracing::info_span!(
            "command",
            command = command_name(&call),
            client_id = %client_id,
            request_id = %request_id,
        );
        let _entered = span.enter();
        if matches!(
            call,
            Call::Query {
                query: Query::VideoSnapshot
            }
        ) {
            drop(_entered);
            return Executed::Pending(self.video_snapshot(), span);
        }
        let audit = self.shared.audit.sink_for(&call).map(|sink| {
            let (rev, client_name) = self.audit_context(client_id);
            (sink, audit::Pending::new(&call, rev, client_name))
        });
        let output = match &call {
            Call::Action {
                action:
                    Action::OutputStart { output_id }
                    | Action::OutputStop { output_id }
                    | Action::OutputSetKey { output_id, .. }
                    | Action::OutputClearKey { output_id },
            } => Some(output_id.clone()),
            _ => None,
        };
        let result = if let Some(result) = self.youtube_call(&call) {
            result
        } else if let Call::Action {
            action: action @ (Action::OutputSetKey { .. } | Action::OutputClearKey { .. }),
        } = call
        {
            self.key_action(action)
        } else if self.backends_ready()
            && matches!(
                call,
                Call::Action {
                    action: Action::OutputStart { .. } | Action::OutputStop { .. }
                }
            )
        {
            self.run_media_action(call)
        } else {
            let mut wake = Vec::new();
            let result = lock(&self.shared.state).execute(client_id, request_id, call, &mut wake);
            for waker in wake {
                waker.wake();
            }
            self.with_key_set(result)
        };
        if matches!(
            result,
            Ok(CallResult::Batch { .. } | CallResult::History { .. })
        ) {
            self.save_collection();
        }
        match (&result, output) {
            (Ok(CallResult::Batch { rev }), _) => tracing::debug!(rev = %rev, "batch applied"),
            (Ok(CallResult::History { rev, step_rev, .. }), _) => {
                tracing::debug!(rev = %rev, step_rev = %step_rev, "history step applied");
            }
            (Ok(_), Some(output_id)) => {
                tracing::info!(output_id = %output_id, "output action done");
            }
            (Ok(_), None) => tracing::debug!("call done"),
            (Err(error), Some(output_id)) => {
                tracing::info!(output_id = %output_id, code = error.code(), "output action failed");
            }
            (Err(error), None) => tracing::debug!(code = error.code(), "call failed"),
        }
        if let Some((sink, pending)) = audit {
            sink.record(pending.finish(client_id, request_id, &result));
        }
        Executed::Ready(result)
    }

    /// The YouTube actions and queries, or `None` for any other call. They run with no
    /// engine lock held, since most of them call the Live API.
    fn youtube_call(&self, call: &Call) -> Option<Result<CallResult, ProtocolError>> {
        let youtube = self.youtube();
        let unavailable = || ProtocolError::ActionFailed {
            reason: "this engine has no YouTube client".into(),
        };
        let action = |result: Result<ActionResult, ProtocolError>| {
            result.map(|result| CallResult::Action {
                result: Some(result),
            })
        };
        let query = |result: Result<QueryResult, ProtocolError>| {
            result.map(|result| CallResult::Query { result })
        };
        Some(match call {
            Call::Action { action: a } => match a {
                Action::YoutubeSignIn => {
                    action(youtube.ok_or_else(unavailable).and_then(|y| y.sign_in()))
                }
                Action::YoutubeSignOut => youtube
                    .ok_or_else(unavailable)
                    .and_then(|y| y.sign_out())
                    .map(|()| CallResult::Action { result: None }),
                Action::YoutubeCreateBroadcast {
                    title,
                    description,
                    scheduled_start_time,
                    privacy,
                    made_for_kids,
                } => action(youtube.ok_or_else(unavailable).and_then(|y| {
                    y.create_broadcast(
                        title.clone(),
                        description.clone(),
                        scheduled_start_time.clone(),
                        *privacy,
                        *made_for_kids,
                    )
                })),
                Action::YoutubeBindBroadcast {
                    broadcast_id,
                    stream_id,
                } => action(
                    youtube
                        .ok_or_else(unavailable)
                        .and_then(|y| y.bind_broadcast(broadcast_id, stream_id.clone())),
                ),
                Action::YoutubeTransitionBroadcast { broadcast_id, to } => action(
                    youtube
                        .ok_or_else(unavailable)
                        .and_then(|y| y.transition_broadcast(broadcast_id, *to)),
                ),
                _ => return None,
            },
            Call::Query { query: q } => {
                match q {
                    Query::YoutubeStatus => Ok(CallResult::Query {
                        result: youtube.map_or(
                            QueryResult::YoutubeStatus {
                                auth: YoutubeAuth::Unavailable,
                            },
                            |y| y.status(),
                        ),
                    }),
                    Query::YoutubeListBroadcasts {
                        status,
                        broadcast_type,
                        page_token,
                    } => query(youtube.ok_or_else(unavailable).and_then(|y| {
                        y.list_broadcasts(*status, *broadcast_type, page_token.clone())
                    })),
                    Query::YoutubeListStreams { page_token } => query(
                        youtube
                            .ok_or_else(unavailable)
                            .and_then(|y| y.list_streams(page_token.clone())),
                    ),
                    _ => return None,
                }
            }
            _ => return None,
        })
    }

    /// The key store, if the host gave one. The handle is cloned so the store is
    /// called with no engine lock held.
    fn key_store(&self) -> Option<Arc<dyn StreamKeyStore>> {
        lock(&self.shared.keys).clone()
    }

    /// `output.set_key` and `output.clear_key`. The collection lock is held only to
    /// check that the output exists; the store is called after it is released. Nothing
    /// here logs, returns or emits the key: errors carry fixed reasons, and a store's
    /// error must not contain one (R-SEC-001).
    fn key_action(&self, action: Action) -> Result<CallResult, ProtocolError> {
        match action {
            Action::OutputSetKey { output_id, key } => {
                let exists = lock(&self.shared.state)
                    .graph
                    .state()
                    .output(&output_id)
                    .is_some();
                if !exists {
                    return Err(ProtocolError::NotFound {
                        entity: EntityKind::Output,
                        id: output_id.as_str().to_owned(),
                    });
                }
                check_key(&key)?;
                let store = self.key_store().ok_or_else(no_key_store)?;
                store
                    .set(output_id.as_str(), &Secret::new(key.expose()))
                    .map_err(|err| ProtocolError::ActionFailed {
                        reason: format!("the credential store did not store the key: {err}"),
                    })?;
                Ok(CallResult::Action { result: None })
            }
            Action::OutputClearKey { output_id } => {
                let store = self.key_store().ok_or_else(no_key_store)?;
                store
                    .clear(output_id.as_str())
                    .map_err(|err| ProtocolError::ActionFailed {
                        reason: format!("the credential store did not remove the key: {err}"),
                    })?;
                Ok(CallResult::Action { result: None })
            }
            other => Err(not_implemented(other.name())),
        }
    }

    /// Fills `output.status`'s `key_set` from the key store, after the collection lock
    /// is released. Without a store it stays `false`.
    fn with_key_set(
        &self,
        result: Result<CallResult, ProtocolError>,
    ) -> Result<CallResult, ProtocolError> {
        let Ok(CallResult::Query {
            result:
                QueryResult::OutputStatus {
                    output_id,
                    state,
                    key_set: _,
                },
        }) = result
        else {
            return result;
        };
        let key_set = match self.key_store() {
            Some(store) => {
                store
                    .is_set(output_id.as_str())
                    .map_err(|err| ProtocolError::Internal {
                        reason: format!("reading the credential store: {err}"),
                    })?
            }
            None => false,
        };
        Ok(CallResult::Query {
            result: QueryResult::OutputStatus {
                output_id,
                state,
                key_set,
            },
        })
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
        // A `youtube` output needs a signed-in client. Checked before `starting`, and
        // without the collection lock, since it waits for a YouTube call in flight.
        let youtube = match youtube_broadcast(&lock(&self.shared.state).graph, &output_id) {
            Some(_) => match self.youtube() {
                Some(youtube) if youtube.signed_in() => Some(youtube),
                Some(_) => {
                    return Err(ProtocolError::ActionFailed {
                        reason: "not signed in to YouTube: send `youtube.sign_in`".into(),
                    });
                }
                None => {
                    return Err(ProtocolError::ActionFailed {
                        reason: "this engine has no YouTube client".into(),
                    });
                }
            },
            None => None,
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
            let publish = match (
                record_path(&state.graph, &output_id),
                youtube_broadcast(&state.graph, &output_id),
                &youtube,
            ) {
                (Some(path), _, _) => Target::Record(path),
                (None, Some(broadcast_id), Some(youtube)) => Target::YouTube {
                    broadcast_id,
                    youtube: Arc::clone(youtube),
                },
                // Retargeted to YouTube since the check above: sign-in wasn't checked.
                (None, Some(_), None) => {
                    return Err(ProtocolError::ActionFailed {
                        reason: format!("output `{output_id}` changed while starting"),
                    });
                }
                (None, None, _) => match lock(&host.publish).take() {
                    Some(publish) => Target::Ready(publish),
                    None => match lock(&host.open_publish).as_ref() {
                        Some(open) => Target::Open(Arc::clone(open)),
                        None => {
                            return Err(ProtocolError::ActionFailed {
                                reason: "output.start has no publish target".into(),
                            });
                        }
                    },
                },
            };
            let parts = {
                let backends = lock(&host.backends);
                let Some(backends) = backends.as_ref() else {
                    drop(backends);
                    publish.give_back(host);
                    return Err(not_implemented("output.start"));
                };
                parts_from(
                    backends,
                    Arc::clone(&lock(&host.draw)),
                    *lock(&host.open_timeout),
                    Arc::clone(&state.render),
                )
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
                    publish.give_back(host);
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
        let mut health = None;
        let launched = publish
            .open(&output_id)
            .and_then(|(destination, watch)| {
                health = watch;
                launch(parts, destination, output_id.clone(), generation, fail_tx)
            })
            .map(|mut running| {
                running.arm_stats(self);
                running
            });
        // A link change the output thread reported before `active` is replayed once
        // the start settles; the watcher takes every later one.
        let mut link_down = false;
        let first = loop {
            match fail_rx.try_recv() {
                Ok(PipelineEvent::LinkDown) => link_down = true,
                Ok(PipelineEvent::LinkUp) => link_down = false,
                other => break other,
            }
        };
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
                    Ok(running) => match first {
                        Ok(PipelineEvent::Failed(reason)) => {
                            abandon_start(
                                &mut state,
                                &mut gate,
                                &mut inflight,
                                &output_id,
                                &mut wake,
                            );
                            // Joined below, outside the locks. A new start waits.
                            gate.draining = true;
                            StartOutcome::Failed(Some(running), reason)
                        }
                        Ok(PipelineEvent::LinkDown | PipelineEvent::LinkUp)
                        | Err(TryRecvError::Disconnected) => {
                            abandon_start(
                                &mut state,
                                &mut gate,
                                &mut inflight,
                                &output_id,
                                &mut wake,
                            );
                            gate.draining = true;
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
                                    if link_down {
                                        state.outputs.settle(&output_id, OutputState::Reconnecting);
                                        emit_output(
                                            &mut state,
                                            &output_id,
                                            OutputState::Reconnecting,
                                            &mut wake,
                                        );
                                    }
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
                                    gate.draining = true;
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
            StartOutcome::Active => {
                if let Some(watch) = health {
                    self.watch_health(&output_id, watch);
                }
                Ok(CallResult::Action { result: None })
            }
            StartOutcome::Cancelled(running) => {
                drop(running);
                drop(inflight);
                Err(ProtocolError::ActionFailed {
                    reason: format!("output `{output_id}` stopped while starting"),
                })
            }
            StartOutcome::Failed(running, reason) => {
                // `failed` is already out and `pending` cleared, so `draining` (set
                // above when there is a pipeline) holds a new start off until this
                // join ends, including when it unwinds.
                let _clear_drain = ClearDrain::arm(Arc::clone(host), running.is_some());
                drop(running);
                Err(ProtocolError::ActionFailed { reason })
            }
        }
    }

    fn stop_pipeline(&self, output_id: OutputId) -> Result<CallResult, ProtocolError> {
        let Some(host) = &self.media else {
            return Err(not_implemented("output.stop"));
        };
        if let Some(youtube) = self.youtube() {
            youtube.unwatch_health(&output_id);
        }
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
                    // A start for another output waits until this join ends.
                    gate.draining = true;
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
        // Join outside the collection lock. `Running`'s drop stops the threads. If
        // that teardown unwinds, the guards still settle the output (`failed`, since
        // the teardown did not finish) and then clear `draining`, so it is not left
        // `stopping` and a later start is not left waiting.
        let clear_drain = ClearDrain::arm(Arc::clone(host), running.is_some());
        let mut settle = SettleOnDrop {
            engine: self,
            output_id: &output_id,
            state: OutputState::Failed,
        };
        drop(running);
        if let Some(watcher) = watcher
            && watcher.join().is_err()
        {
            tracing::error!("publish watcher panicked");
        }
        settle.state = OutputState::Stopped;
        drop(settle);
        drop(clear_drain);
        Ok(CallResult::Action { result: None })
    }

    /// Polls the health of the stream a `youtube` output started on, while the output
    /// is `active` or `reconnecting`.
    fn watch_health(&self, output_id: &OutputId, watch: HealthTarget) {
        let shared = Arc::downgrade(&self.shared);
        let watched = output_id.clone();
        let alive = Box::new(move || {
            shared.upgrade().is_some_and(|shared| {
                matches!(
                    lock(&shared.state).outputs.state(&watched),
                    Ok(OutputState::Active | OutputState::Reconnecting)
                )
            })
        });
        watch.youtube.watch_health(
            output_id.clone(),
            watch.broadcast_id,
            watch.stream_id,
            alive,
        );
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

    /// `video.snapshot`. The `rev` and the render snapshot published for it are read
    /// together under the collection lock; the snapshot thread renders it after that
    /// lock is released.
    fn video_snapshot(&self) -> SnapshotReply {
        let (snapshot, rev) = {
            let state = lock(&self.shared.state);
            (state.render.load(), state.graph.rev())
        };
        self.shared.snapshots.request(snapshot, rev)
    }

    /// The render hub the pipeline's render thread reads, for tests.
    #[cfg(test)]
    pub(crate) fn render_hub(&self) -> Arc<RenderHub> {
        Arc::clone(&lock(&self.shared.state).render)
    }
}

/// What [`Engine::execute`] produced: an answer, or a snapshot still being drawn.
pub(crate) enum Executed {
    Ready(Result<CallResult, ProtocolError>),
    /// `video.snapshot`, answered by the snapshot thread, and the call's `command` span.
    Pending(SnapshotReply, tracing::Span),
}

/// Settles an output in `state` when dropped, including during an unwind.
struct SettleOnDrop<'a> {
    engine: &'a Engine,
    output_id: &'a OutputId,
    state: OutputState,
}

impl Drop for SettleOnDrop<'_> {
    fn drop(&mut self) {
        self.engine.settle(self.output_id, self.state);
    }
}

/// Where a recording output in the collection records to, or `None` when the output
/// is not a recording (or not in the collection).
fn record_path(graph: &Graph, output_id: &OutputId) -> Option<PathBuf> {
    match graph.state().output(output_id)?.kind() {
        OutputKind::Record {
            path,
            container: RecordContainer::Mp4,
        } => Some(PathBuf::from(path)),
        _ => None,
    }
}

/// The broadcast a `youtube` output in the collection sends to, or `None` when the
/// output has another target (or is not in the collection).
fn youtube_broadcast(graph: &Graph, output_id: &OutputId) -> Option<String> {
    match graph.state().output(output_id)?.kind() {
        OutputKind::Rtmp {
            target: RtmpTarget::YouTube { broadcast_id },
        } => Some(broadcast_id.clone()),
        _ => None,
    }
}

/// Where a start sends its output: a one-shot publish target the host set, the opener
/// it calls once the locks are released, a recording file, or a YouTube broadcast.
enum Target {
    Ready(Box<dyn LivePublish>),
    Open(OpenPublish),
    Record(PathBuf),
    /// A `youtube` output: the ingest is read from the Live API when it opens.
    YouTube {
        broadcast_id: String,
        youtube: Arc<Youtube>,
    },
}

/// The stream a started `youtube` output sends to, whose health the engine polls.
struct HealthTarget {
    youtube: Arc<Youtube>,
    broadcast_id: String,
    stream_id: String,
}

impl Target {
    /// Puts a one-shot target back for the next start, when this one refused
    /// before `starting`. An opener was never taken out of the host.
    fn give_back(self, host: &MediaHost) {
        if let Target::Ready(publish) = self {
            *lock(&host.publish) = Some(publish);
        }
    }

    /// Where to send the output, and for a `youtube` output the stream to watch. Runs
    /// the opener, or reads the YouTube ingest and connects, outside the engine's locks.
    /// A recording's file is opened by `launch`, once the frame size is known.
    fn open(self, output_id: &OutputId) -> Result<(Destination, Option<HealthTarget>), String> {
        match self {
            Target::Ready(publish) => Ok((Destination::Publish(publish), None)),
            Target::Open(open) => open(output_id)
                .map(|publish| (Destination::Publish(publish), None))
                .map_err(|err| format!("opening the publish target: {err}")),
            Target::Record(path) => Ok((Destination::Record(path), None)),
            Target::YouTube {
                broadcast_id,
                youtube,
            } => {
                let ingest = youtube.ingest(&broadcast_id)?;
                let publish = youtube
                    .connect(&ingest)
                    .map_err(|err| format!("connecting to YouTube's ingest: {err}"))?;
                Ok((
                    Destination::Publish(publish),
                    Some(HealthTarget {
                        youtube,
                        broadcast_id,
                        stream_id: ingest.stream_id,
                    }),
                ))
            }
        }
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

/// Follows the pipeline until it fails: a lost publish link settles `reconnecting`,
/// a regained one `active`, and a fatal error `failed`, which ends the watch. A
/// disconnected channel is a clean stop.
///
/// The pointers are weak, so this thread does not keep the engine alive. Dropping
/// the last [`Engine`] drops [`MediaHost`], which joins the pipeline and then this
/// thread. Each event upgrades the pointers only long enough to settle.
fn watch_publish(
    shared: Weak<Shared>,
    host: Weak<MediaHost>,
    output_id: OutputId,
    generation: u64,
    fail_rx: Receiver<PipelineEvent>,
) {
    loop {
        let Ok(event) = fail_rx.recv() else {
            return;
        };
        let (Some(shared), Some(host)) = (shared.upgrade(), host.upgrade()) else {
            return;
        };
        match event {
            PipelineEvent::Failed(reason) => {
                publish_failed(&shared, &host, &output_id, generation, &reason);
                return;
            }
            PipelineEvent::LinkDown => link_changed(
                &shared,
                &host,
                &output_id,
                generation,
                [OutputState::Active, OutputState::Reconnecting],
            ),
            PipelineEvent::LinkUp => link_changed(
                &shared,
                &host,
                &output_id,
                generation,
                [OutputState::Reconnecting, OutputState::Active],
            ),
        }
    }
}

/// Whether the running pipeline is the one `generation` started for `output_id`.
fn is_ours(gate: &Gate, output_id: &OutputId, generation: u64) -> bool {
    gate.running.as_ref().is_some_and(|running| {
        running.output_id() == output_id && running.generation() == generation
    })
}

/// Settles `to` if the output is still this pipeline's and in `from`. A stop that
/// has taken the pipeline, or a state the output has already left, wins.
fn link_changed(
    shared: &Shared,
    host: &MediaHost,
    output_id: &OutputId,
    generation: u64,
    [from, to]: [OutputState; 2],
) {
    let mut wake = Vec::new();
    let settled = {
        let mut state = lock(&shared.state);
        let gate = lock(&host.gate);
        let settle = is_ours(&gate, output_id, generation)
            && state.outputs.state(output_id).ok() == Some(from);
        if settle {
            state.outputs.settle(output_id, to);
            emit_output(&mut state, output_id, to, &mut wake);
        }
        settle
    };
    if settled && to == OutputState::Reconnecting {
        tracing::warn!(output_id = %output_id, "publish link lost; reconnecting");
    } else if settled {
        tracing::info!(output_id = %output_id, "publish link restored");
    }
    wake_all(wake);
}

/// Settles `failed` and joins the pipeline, outside the locks.
fn publish_failed(
    shared: &Shared,
    host: &Arc<MediaHost>,
    output_id: &OutputId,
    generation: u64,
    reason: &str,
) {
    tracing::error!(output_id = %output_id, error = %reason, "publish failed");
    let mut wake = Vec::new();
    let running = {
        let mut state = lock(&shared.state);
        let mut gate = lock(&host.gate);
        let ours = is_ours(&gate, output_id, generation);
        let live = matches!(
            state.outputs.state(output_id),
            Ok(OutputState::Active | OutputState::Reconnecting)
        );
        if ours && live {
            state.outputs.settle(output_id, OutputState::Failed);
            emit_output(&mut state, output_id, OutputState::Failed, &mut wake);
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
    // `pipeline` is bound after the guard. On the normal path it is dropped
    // first, and an unwind (`wake_all`, or the join itself) drops it first too,
    // so `draining` clears only after the join.
    let _clear_drain = ClearDrain::arm(Arc::clone(host), running.is_some());
    let pipeline = running;
    wake_all(wake);
    drop(pipeline);
}

/// Clears `Gate::draining` after the failed pipeline has been joined, including
/// when that join unwinds.
struct ClearDrain {
    host: Arc<MediaHost>,
    armed: bool,
}

impl ClearDrain {
    fn arm(host: Arc<MediaHost>, armed: bool) -> Self {
        Self { host, armed }
    }
}

impl Drop for ClearDrain {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut gate = lock(&self.host.gate);
        gate.draining = false;
        self.host.gate_cv.notify_all();
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
        Call::History(history) => history.op.name(),
        _ => "unknown",
    }
}

/// Everything the engine knows.
#[derive(Default)]
struct State {
    /// The collection and its `rev`. Only `Graph::apply` changes it.
    graph: Graph,
    outputs: Outputs,
    /// The result of each batch, undo or redo applied with an idempotency key, per
    /// client: batches and history calls share a client's keys (ADR-0032). Kept for the
    /// engine's lifetime: how long to remember keys is protocol open question 6.
    applied: BTreeMap<(ClientId, IdempotencyKey), CallResult>,
    /// Every client's undo and redo stacks.
    history: History,
    /// How many sessions have completed `hello`; numbers the client IDs.
    clients: u64,
    /// Each client's name from its latest `hello`, for the audit log. Kept for the
    /// engine's lifetime, like the client IDs.
    client_names: BTreeMap<ClientId, String>,
    subscribers: Subscribers,
    /// The latest CPU frame of each source, keyed by id. Runtime state: not part of
    /// `rev`. A snapshot draws an item from this when the source is not itself a scene.
    /// An applied `source.delete` removes the deleted source's entry.
    frames: BTreeMap<SourceId, Frame>,
    /// How many frames `set_source_frame` has stored: the next one's version.
    frame_versions: u64,
    /// The program mix the render thread and `video.snapshot` draw, republished after
    /// every change to the graph or a stored frame (it only becomes a new generation when
    /// what it draws changed).
    render: Arc<RenderHub>,
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
            Call::History(history) => self.history(client_id, request_id, &history, wake),
            _ => Err(not_implemented("this kind of call")),
        }
    }

    /// Applies `batch` atomically through `Graph::apply_with_running`, which checks
    /// `expected_rev` (before rejecting an empty batch), applies every edit or none, and
    /// bumps `rev` by exactly one. The batch becomes one step of the client's undo
    /// history, holding the inverse the graph returned, and clears what it could redo.
    ///
    /// A batch whose idempotency key this client already used is not applied again: it
    /// gets the result of the first call with that key, and emits nothing. That check
    /// comes first, so a retry succeeds even after `rev` moved on.
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
        if let Some(result) = keyed.as_ref().and_then(|keyed| self.applied.get(keyed)) {
            return Ok(result.clone());
        }
        // A running output can't be deleted.
        let applied = self.graph.apply_with_running(
            &batch.edits,
            batch.expected_rev,
            &self.outputs.running(),
        )?;
        self.history
            .record_batch(client_id, applied.rev, &batch.edits, applied.inverse.edits);
        let result = CallResult::Batch { rev: applied.rev };
        if let Some(keyed) = keyed {
            self.applied.insert(keyed, result.clone());
        }
        self.changed(client_id, request_id, &batch.edits, applied.rev, None, wake);
        Ok(result)
    }

    /// Undoes or redoes one of the client's own steps (ADR-0032): applies the edits its
    /// history holds as one batch, which bumps `rev` by one, and moves the step to the
    /// other stack. No new step is made.
    ///
    /// In order: a used idempotency key gets its first result; a stale `expected_rev`
    /// fails with `rev_conflict`; an empty stack fails with `history_empty`. A step
    /// whose edits, applied now, would touch what another client's later batch touched
    /// (a scene's undone `scene.create` touches the items the scene holds now), or whose
    /// edits no longer apply after one,
    /// fails with `undo_conflict` and is dropped, so the next undo reaches the step
    /// before it. Edits that fail only because an output is running (an output the step
    /// would delete) fail with the graph's error, and the step stays: stop the output
    /// and retry.
    fn history(
        &mut self,
        client_id: &ClientId,
        request_id: &RequestId,
        call: &HistoryCall,
        wake: &mut Vec<Waker>,
    ) -> Result<CallResult, ProtocolError> {
        let keyed = call
            .idempotency_key
            .as_ref()
            .map(|key| (client_id.clone(), key.clone()));
        if let Some(result) = keyed.as_ref().and_then(|keyed| self.applied.get(keyed)) {
            return Ok(result.clone());
        }
        let current = self.graph.rev();
        if let Some(expected) = call.expected_rev
            && expected != current
        {
            return Err(ProtocolError::RevConflict { expected, current });
        }
        let direction = call.op.direction();
        let step = self
            .history
            .take(client_id, direction)
            .ok_or(ProtocolError::HistoryEmpty { direction })?;
        let step_rev = step.step_rev;
        let conflict = move |(client_id, rev)| ProtocolError::UndoConflict {
            step_rev,
            client_id,
            rev,
        };
        // Applied to a copy first: what the edits touch now decides the conflict, and
        // the state changes only once there is none.
        let running = self.outputs.running();
        let mut next = self.graph.clone();
        let applied = match next.apply_with_running(&step.edits, None, &running) {
            Ok(applied) => applied,
            Err(error) => {
                // Only a running output in the way: keep the step for a retry.
                let blocked_by_output = !running.is_empty()
                    && self
                        .graph
                        .clone()
                        .apply_with_running(&step.edits, None, &BTreeSet::new())
                        .is_ok();
                let found = self
                    .history
                    .conflict(client_id, &step, None)
                    .or_else(|| self.history.latest_other(client_id, &step));
                let error = match found {
                    Some(found) if !blocked_by_output => conflict(found),
                    _ => {
                        self.history.put_back(client_id, direction, step);
                        return Err(error);
                    }
                };
                self.history.dropped();
                return Err(error);
            }
        };
        let now = Touched::of(&step.edits, &applied.inverse.edits);
        if let Some(found) = self.history.conflict(client_id, &step, Some(&now)) {
            let error = conflict(found);
            self.history.dropped();
            return Err(error);
        }
        self.graph = next;
        let edits = step.edits.clone();
        self.history.moved(
            client_id,
            direction,
            step,
            applied.rev,
            applied.inverse.edits,
        );
        let (undo_depth, redo_depth) = self.history.depths(client_id);
        let result = CallResult::History {
            rev: applied.rev,
            step_rev,
            undo_depth,
            redo_depth,
        };
        if let Some(keyed) = keyed {
            self.applied.insert(keyed, result.clone());
        }
        self.changed(
            client_id,
            request_id,
            &edits,
            applied.rev,
            Some(direction),
            wake,
        );
        Ok(result)
    }

    /// What follows every applied batch, undo and redo: the outputs follow the
    /// collection, a deleted source's stored frame goes (unless the same edits made a
    /// new source under its id), and `state.changed` is emitted.
    fn changed(
        &mut self,
        client_id: &ClientId,
        request_id: &RequestId,
        edits: &[Edit],
        rev: Rev,
        cause: Option<HistoryDirection>,
        wake: &mut Vec<Waker>,
    ) {
        self.outputs.sync(self.graph.state());
        for edit in edits {
            if let Edit::SourceDelete { source_id } = edit
                && self.graph.state().source(source_id).is_none()
            {
                self.frames.remove(source_id);
            }
        }
        self.publish_render();
        self.subscribers.emit(
            &Event::StateChanged {
                rev,
                client_id: client_id.clone(),
                request_id: Some(request_id.clone()),
                cause,
            },
            wake,
        );
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
        Ok(CallResult::Action { result: None })
    }

    /// Collection reads go to `Graph::query`; `output.status` reads the outputs.
    fn query(&self, query: &Query) -> Result<CallResult, ProtocolError> {
        let result = match self.graph.query(query) {
            Some(result) => result?,
            None => match query {
                // `key_set` is filled in after the lock is released (`with_key_set`).
                Query::OutputStatus { output_id } => QueryResult::OutputStatus {
                    output_id: output_id.clone(),
                    state: self.outputs.state(output_id)?,
                    key_set: false,
                },
                other => return Err(not_implemented(other.name())),
            },
        };
        Ok(CallResult::Query { result })
    }

    /// Re-plans the program mix and publishes it if it draws something new.
    fn publish_render(&self) {
        self.render
            .publish(snapshot::plan(&self.graph, &self.frames));
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

/// The longest stream key `output.set_key` accepts, in bytes.
const MAX_KEY_LEN: usize = 1024;

/// Checks a stream key's shape. The reasons are fixed text: they never repeat the key.
fn check_key(key: &SecretString) -> Result<(), ProtocolError> {
    let key = key.expose();
    let reason = if key.is_empty() {
        "must not be empty"
    } else if key.len() > MAX_KEY_LEN {
        "must be at most 1024 bytes"
    } else if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        "must not contain whitespace or control characters"
    } else {
        return Ok(());
    };
    Err(ProtocolError::InvalidArgument {
        field: "key".into(),
        reason: reason.into(),
    })
}

fn no_key_store() -> ProtocolError {
    ProtocolError::ActionFailed {
        reason: "this engine has no credential store for stream keys".into(),
    }
}

/// A protocol variant added after this engine was written.
fn not_implemented(what: &str) -> ProtocolError {
    ProtocolError::Internal {
        reason: format!("the engine does not implement {what} yet"),
    }
}
