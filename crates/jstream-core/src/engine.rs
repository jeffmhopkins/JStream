//! The [`Engine`]: the scene graph, the outputs' runtime state and the event subscribers,
//! behind one lock, and the execution of every protocol call against them.
//!
//! Collection changes and the events they queue happen under that lock, so every
//! subscriber sees them in the order they happened, and `state.changed` events in `rev`
//! order. A pipeline start or stop holds the lock only while it changes output state.
//! Device open and thread joins happen after it is released. Subscribers are woken only
//! after the lock is released.

use crate::audio_sources::AudioSources;
use crate::audit::{self, Audit, AuditSink};
use crate::collection::{CollectionFile, CollectionFileError};
use crate::drops::{self, DropLedger, DropSource, DropSourceId};
use crate::events::{QueueRef, Subscribers, Subscription};
use crate::history::History;
use crate::live::{Built, LiveHost, LiveSources};
use crate::lock;
use crate::outputs::Outputs;
use crate::runtime::{
    Destination, EncodeKey, Encoders, Gate, LivePublish, MediaHost, OpenPublish, OutputStats,
    Pending, PipelineEvent, RenderHub, Running, launch, launch_program, parts_from,
};
use crate::session::InProcessControl;
use crate::snapshot::{self, Frame, SnapshotReply, SnapshotWorker, SourceFrameError};
use crate::studio::Studio;
use jstream_graph::Graph;
use jstream_platform::{Backends, Secret, StreamKeyStore};
use jstream_protocol::{
    Action, Batch, Call, CallResult, ClientId, DropCounts, Edit, EncoderProfile,
    EncoderSessionInfo, EntityKind, Event, HistoryCall, HistoryDirection, IdempotencyKey, OutputId,
    OutputKind, OutputState, ProtocolError, Query, QueryResult, RecordContainer, RequestId, Rev,
    SecretString, SourceId,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
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
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            shared: Arc::new_cyclic(|shared: &Weak<Shared>| Shared {
                state: Mutex::new({
                    let mut state = State::default();
                    state.graph = Graph::new().with_kinds(crate::kinds::builtin());
                    let host: Weak<dyn LiveHost> = shared.clone();
                    state.live = LiveSources::new(host);
                    state
                }),
                snapshots: SnapshotWorker::default(),
                keys: Mutex::new(None),
                audit: Audit::default(),
                collection_file: Mutex::new(None),
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
    /// call returns, so the client whose call it was knows the file holds its `rev`. A
    /// failed save is logged at `error` and leaves the call's result as it is: the change
    /// is applied, and the next change saves again. The `rev` is saved too, so revisions
    /// keep increasing across restarts and an `expected_rev` from before one can't
    /// match a different state. Undo history, idempotency keys and the program scene are
    /// not saved: the loaded collection's first scene goes on program.
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
            // Program is not saved: the loaded collection's first scene goes on it, with
            // no event, as no client is connected yet (ADR-0030).
            state.studio = Studio::default();
            state.studio.reconcile(state.graph.state());
            // `collection` asset references are relative to the file's folder.
            state
                .live
                .set_collection_folder(opened.folder().map(Path::to_path_buf));
            state.live.sync(state.graph.state());
            state.audio.sync(state.graph.state(), state.live.audio());
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
        // Animated sources run on the platform's media clock from here on.
        lock(&self.shared.state)
            .live
            .set_clock(Arc::clone(&backends.clock));
        let host = self.host();
        *lock(&host.backends) = Some(backends);
        self
    }

    /// Gives `browser` sources the browser engine they open their pages in (ADR-0021).
    /// Without one, a `browser` source shows nothing and logs that no backend is set.
    /// The engine's own GPU device is handed to the backend by the host.
    #[must_use]
    pub fn with_browser(self, browser: Arc<dyn jstream_sources::BrowserBackend>) -> Self {
        {
            let mut state = lock(&self.shared.state);
            let state = &mut *state;
            state.live.set_browser(browser);
            state.live.sync(state.graph.state());
        }
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

    /// Encoder and subscriber counters for the running output with the lowest ID, if
    /// one is up. [`output_stats_of`](Self::output_stats_of) names the output.
    pub fn output_stats(&self) -> Option<OutputStats> {
        let host = self.media.as_ref()?;
        lock(&host.gate).running.values().next().map(Running::stats)
    }

    /// Encoder and subscriber counters for `output_id`, if it is running.
    pub fn output_stats_of(&self, output_id: &OutputId) -> Option<OutputStats> {
        let host = self.media.as_ref()?;
        lock(&host.gate).running.get(output_id).map(Running::stats)
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
        let result = if let Call::Action {
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
                Ok(CallResult::Action {})
            }
            Action::OutputClearKey { output_id } => {
                let store = self.key_store().ok_or_else(no_key_store)?;
                store
                    .clear(output_id.as_str())
                    .map_err(|err| ProtocolError::ActionFailed {
                        reason: format!("the credential store did not remove the key: {err}"),
                    })?;
                Ok(CallResult::Action {})
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
                    encoder: _,
                    encoder_sessions: _,
                    hardware_sessions: _,
                },
        }) = result
        else {
            return result;
        };
        let (encoder, encoder_sessions, hardware_sessions) = self.encoder_status(&output_id);
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
                encoder,
                encoder_sessions,
                hardware_sessions,
            },
        })
    }

    /// The encoder session `output_id` runs on, with the other running outputs that
    /// share it, and how many video sessions (and of those, hardware ones) every
    /// running output holds together. Read from the running pipelines; the placeholder
    /// path opens no encoders, so it reports none.
    fn encoder_status(&self, output_id: &OutputId) -> (Option<EncoderSessionInfo>, u32, u32) {
        let Some(host) = &self.media else {
            return (None, 0, 0);
        };
        let gate = lock(&host.gate);
        let mut sessions: Vec<&Arc<Encoders>> = Vec::new();
        for encoders in gate.running.values().filter_map(Running::encoders) {
            if !sessions.iter().any(|seen| Arc::ptr_eq(seen, encoders)) {
                sessions.push(encoders);
            }
        }
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let hardware = count(sessions.iter().filter(|e| e.hardware()).count());
        let encoder = gate
            .running
            .get(output_id)
            .and_then(Running::encoders)
            .map(|own| EncoderSessionInfo {
                profile_id: own.profile_id().cloned(),
                shared_with: gate
                    .running
                    .iter()
                    .filter(|(id, running)| {
                        *id != output_id && running.encoders().is_some_and(|e| Arc::ptr_eq(e, own))
                    })
                    .map(|(id, _)| id.clone())
                    .collect(),
                hardware: own.hardware(),
            });
        (encoder, count(sessions.len()), hardware)
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
        // One start at a time: a second waits here until the first has settled, so
        // only one ever opens the program stage. A stop never takes this lock.
        let _starting = lock(&host.starts);
        let mut wake = Vec::new();
        let (parts, publish, generation, program, key) = loop {
            // A failed pipeline may still be joining. When it held the program stage,
            // wait that out before opening devices, and do not hold the collection
            // lock across the wait.
            {
                let mut gate = lock(&host.gate);
                while gate.must_wait() {
                    gate = host
                        .gate_cv
                        .wait(gate)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
            let mut state = lock(&self.shared.state);
            let mut gate = lock(&host.gate);
            if gate.must_wait() {
                continue;
            }
            let current = state.outputs.state(&output_id)?;
            let idle = matches!(current, OutputState::Stopped | OutputState::Failed);
            if !idle {
                return Err(already_running(&output_id));
            }
            if gate.pending.is_some() {
                return Err(ProtocolError::ActionFailed {
                    reason: "another output is starting".into(),
                });
            }
            let key = encode_key(&state.graph, &output_id);
            let publish = match record_path(&state.graph, &output_id) {
                Some(path) => Target::Record(path),
                None => match lock(&host.publish).take() {
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
                    state.audio.wanted(),
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
            // A running program stage is shared; holding it here keeps it running
            // until this start has its own hold, or gives up. Taken last, so no early
            // return drops it under the locks.
            let program = gate.program();
            break (parts, publish, generation, program, key);
        };
        wake_all(std::mem::take(&mut wake));

        // Cleared once this start settles, or on drop if the start is cancelled.
        let mut inflight = Inflight {
            host: Arc::clone(host),
            generation,
            armed: true,
        };
        let (fail_tx, fail_rx) = mpsc::channel();
        let launched = publish.open(&output_id).and_then(|destination| {
            let program = match program {
                Some(program) => program,
                None => {
                    let program = launch_program(self, parts)?;
                    lock(&host.gate).set_program(&program);
                    program
                }
            };
            launch(
                self,
                program,
                key,
                destination,
                output_id.clone(),
                generation,
                fail_tx,
            )
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
                            gate.draining += 1;
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
                            gate.draining += 1;
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
                                    gate.running.insert(output_id.clone(), running);
                                    gate.watchers.insert(output_id.clone(), watcher);
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
                                    gate.draining += 1;
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
                let running = gate.running.remove(&output_id);
                let watcher = if running.is_some() {
                    // A start waits until this join ends, if it took the program stage
                    // down with it.
                    gate.draining += 1;
                    gate.watchers.remove(&output_id)
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

    /// How the program stage fares with audio source `source_id`, for tests.
    #[cfg(test)]
    pub(crate) fn audio_status(
        &self,
        source_id: &SourceId,
    ) -> Option<crate::audio_sources::AudioStatus> {
        lock(&self.shared.state).audio.status(source_id)
    }

    /// Waits until the live sources have built everything asked of them, for tests.
    #[cfg(test)]
    pub(crate) fn settle_sources(&self) -> bool {
        let worker = lock(&self.shared.state).live.worker();
        worker.settled(Duration::from_secs(10))
    }

    /// Runs the live sources on `clock`, for tests that move time by hand.
    #[cfg(test)]
    pub(crate) fn set_source_clock(&self, clock: Arc<dyn jstream_platform::MediaClock>) {
        lock(&self.shared.state).live.set_clock(clock);
    }

    /// Re-plans the program mix now, as the live sources' thread does when a frame is
    /// due, for tests that move time by hand.
    #[cfg(test)]
    pub(crate) fn replan(&self) {
        lock(&self.shared.state).publish_render();
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

/// Which encoder sessions `output_id` shares: its profile and that profile's encoding
/// now, or the engine's default for an output with none or one the host declared.
fn encode_key(graph: &Graph, output_id: &OutputId) -> EncodeKey {
    let state = graph.state();
    let profile_id = state
        .output(output_id)
        .and_then(|output| output.encoder_profile())
        .cloned();
    let profile = state
        .encoding_of(output_id)
        .unwrap_or(EncoderProfile::DEFAULT);
    (profile_id, profile)
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

/// Where a start sends its output: a one-shot publish target the host set, the opener
/// it calls once the locks are released, or a recording file.
enum Target {
    Ready(Box<dyn LivePublish>),
    Open(OpenPublish),
    Record(PathBuf),
}

impl Target {
    /// Puts a one-shot target back for the next start, when this one refused
    /// before `starting`. An opener was never taken out of the host.
    fn give_back(self, host: &MediaHost) {
        if let Target::Ready(publish) = self {
            *lock(&host.publish) = Some(publish);
        }
    }

    /// Where to send the output. Runs the opener, outside the engine's locks. A
    /// recording's file is opened by `launch`, once the frame size is known.
    fn open(self, output_id: &OutputId) -> Result<Destination, String> {
        match self {
            Target::Ready(publish) => Ok(Destination::Publish(publish)),
            Target::Open(open) => open(output_id)
                .map(Destination::Publish)
                .map_err(|err| format!("opening the publish target: {err}")),
            Target::Record(path) => Ok(Destination::Record(path)),
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

/// Whether `output_id`'s running pipeline is the one `generation` started.
fn is_ours(gate: &Gate, output_id: &OutputId, generation: u64) -> bool {
    gate.running.get(output_id).is_some_and(|running| {
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
            gate.draining += 1;
            // Detach this watcher. `JoinHandle`'s drop does not join, so the
            // thread can go on to drop `Running` without joining itself.
            drop(gate.watchers.remove(output_id));
            gate.running.remove(output_id)
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
        gate.draining = gate.draining.saturating_sub(1);
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
    /// How many frames `set_source_frame` has stored or live sources have produced: the
    /// next one's version. One count for both, so a source that switches between them
    /// is uploaded again.
    frame_versions: u64,
    /// A running `jstream-sources` source for each source whose kind produces frames
    /// (C1b). An item shows its source's stored frame if it has one, else this.
    live: LiveSources,
    /// The audio-only sources the program stage records, one mixer input each (C5).
    audio: AudioSources,
    /// The program mix the render thread and `video.snapshot` draw, republished after
    /// every change to the graph or a stored frame (it only becomes a new generation when
    /// what it draws changed).
    render: Arc<RenderHub>,
    /// How many `stats.tick` events this engine has emitted. The next one uses this plus one.
    stats_seq: u64,
    /// Which scene is on program. Runtime state, like `outputs`: not part of `rev`.
    studio: Studio,
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
            Call::Action { action } => self.action(client_id, request_id, action, wake),
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
        self.history.record_batch(
            client_id,
            applied.rev,
            applied.touched,
            applied.inverse.edits,
        );
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
        if let Some(found) = self
            .history
            .conflict(client_id, &step, Some(&applied.touched))
        {
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
            applied.touched,
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
    /// new source under its id), `state.changed` is emitted, and the program scene is
    /// reconciled (ADR-0030).
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
        self.live.sync(self.graph.state());
        self.audio.sync(self.graph.state(), self.live.audio());
        for edit in edits {
            if let Edit::SourceDelete { source_id } = edit
                && self.graph.state().source(source_id).is_none()
            {
                self.frames.remove(source_id);
            }
        }
        // Edits that deleted the program scene, or created the first scene, move
        // program before the mix is planned; that event follows their `state.changed`.
        let moved = self.studio.reconcile(self.graph.state());
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
        if moved {
            self.emit_studio(client_id, request_id, wake);
        }
    }

    fn action(
        &mut self,
        client_id: &ClientId,
        request_id: &RequestId,
        action: Action,
        wake: &mut Vec<Waker>,
    ) -> Result<CallResult, ProtocolError> {
        let (output_id, start) = match action {
            Action::OutputStart { output_id } => (output_id, true),
            Action::OutputStop { output_id } => (output_id, false),
            Action::SceneSwitch { scene_id } => {
                if self.studio.switch(self.graph.state(), &scene_id)? {
                    // The program mix shows the new scene from the next frame: a cut.
                    self.publish_render();
                    self.emit_studio(client_id, request_id, wake);
                }
                return Ok(CallResult::Action {});
            }
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

    /// Collection reads go to `Graph::query`; `output.status` reads the outputs and
    /// `studio.get` the studio state.
    fn query(&self, query: &Query) -> Result<CallResult, ProtocolError> {
        let result = match self.graph.query(query) {
            Some(result) => result?,
            None => match query {
                // `key_set` and the encoder session are filled in after the lock is
                // released (`with_key_set`).
                Query::OutputStatus { output_id } => QueryResult::OutputStatus {
                    output_id: output_id.clone(),
                    state: self.outputs.state(output_id)?,
                    key_set: false,
                    encoder: None,
                    encoder_sessions: 0,
                    hardware_sessions: 0,
                },
                Query::StudioGet => QueryResult::StudioGet {
                    program_scene: self.studio.program().cloned(),
                },
                other => return Err(not_implemented(other.name())),
            },
        };
        Ok(CallResult::Query { result })
    }

    /// `studio.changed` with the scene now on program, caused by `request_id`.
    fn emit_studio(&mut self, client_id: &ClientId, request_id: &RequestId, wake: &mut Vec<Waker>) {
        self.subscribers.emit(
            &Event::StudioChanged {
                program_scene: self.studio.program().cloned(),
                client_id: client_id.clone(),
                request_id: Some(request_id.clone()),
            },
            wake,
        );
    }

    /// Re-plans the program mix at the current time and publishes it if it draws
    /// something new. A source's stored frame wins over its live one. The live sources
    /// are told when the plan's next frame change is due, to re-plan then. The mix shows
    /// the program scene (ADR-0030).
    fn publish_render(&mut self) {
        let Self {
            graph,
            frames,
            frame_versions,
            live,
            render,
            studio,
            ..
        } = self;
        let now = live.now();
        let (shot, wake) =
            snapshot::plan_with(graph, studio.program(), &mut |id| match frames.get(id) {
                Some(frame) => Some((frame.clone(), None)),
                None => live.frame(id, now, frame_versions),
            });
        render.publish(shot);
        live.wake_at(wake);
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

impl LiveHost for Shared {
    fn install(&self, built: Built) {
        let mut state = lock(&self.state);
        let state = &mut *state;
        if state.live.install(built) {
            // A source built with audio of its own is recorded from now on.
            state.audio.sync(state.graph.state(), state.live.audio());
            state.publish_render();
        }
    }

    fn replan(&self) {
        lock(&self.state).publish_render();
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
    Err(ProtocolError::invalid_argument("key", reason))
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
