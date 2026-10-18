//! An in-memory [`Control`] implementation for developing and testing clients without
//! the engine (`jstream-core`, which clients never depend on). Enabled by the `mock`
//! feature.
//!
//! [`MockEngine`] holds a tiny collection (scenes, items, sources, configured outputs,
//! encoder profiles, the audio buses and sends, and the folders, labels and
//! organisation metadata),
//! the outputs' runtime state and an in-memory stand-in for the stream-key store, and
//! follows the protocol's rules for them: the handshake, atomic batches that bump `rev`
//! by one, `expected_rev` conflicts, idempotency keys, every query, the output and
//! stream-key actions, `scene.switch`, `studio.set_preview` and the program and preview
//! scenes they set (`studio.get`),
//! `capture.targets` (one made-up primary display, [`MOCK_DISPLAY`]),
//! `state.changed`, `studio.changed` and `output.state_changed` events, per-client undo
//! and redo, and live-value gestures (`live.set`, `live.cancel`, `live.list`,
//! `ends_gesture`, takeover and expiry). Each [`MockEngine::connect`] opens one client
//! session. It does no persistence or real output work, and knows no transition preset
//! but the initial `cut` (ADR-0029): every switch cuts, and the `transition.*` edits are
//! not mocked.
//!
//! The mock has no render thread, so a gesture's values are released the moment it ends,
//! and it sends one `live.changed` per `live.set` instead of coalescing them: the engine
//! sends at most that many.
//!
//! Undo and redo build the same inverse edits as `jstream-graph`, but detect conflicts
//! more strictly than the engine: once any other client has changed the collection since
//! a step was last applied or undone, undoing or redoing it fails with `undo_conflict`
//! naming the first such change, even where the engine would find the two independent.
//! A client test that gets an undo through the mock gets it through the engine too.

use crate::control::{BoxFuture, Control, EventStream};
use crate::error::ControlError;
use futures_core::Stream;
use jstream_protocol::KindDescriptor;
use jstream_protocol::{
    Action, Batch, BlendMode, BusId, Call, CallResult, Canvas, CaptureTargetInfo,
    CaptureTargetKind, Chord, ClientId, Crop, Edit, EncoderProfile, EncoderProfileId,
    EncoderProfileInfo, EntityKind, Event, Finite32, GeometryError, GestureId, GestureInfo, Hello,
    HistoryCall, HistoryDirection, HotkeyBinding, INITIAL_PRESET, IdempotencyKey, ItemGeometry,
    ItemId, ItemInfo, ItemTransform, ItemTransition, LiveOutcome, MAX_GESTURES_PER_CLIENT,
    MAX_LIVE_VALUES, OutputId, OutputInfo, OutputKind, OutputState, PROTOCOL_VERSION, PixelSize,
    PlacementForm, ProtocolError, Query, QueryResult, RecordContainer, Reference, RequestId,
    Response, Rev, RtmpTarget, SceneId, SceneInfo, SceneRole, SceneSummary, SecretString, Settings,
    SourceAudio, SourceId, SourceInfo, SourceKind, TransitionPresetId, TransitionStarted,
    TransitionsInfo, VideoMix, VideoMixId, VideoMixInfo, Welcome, is_live_adjustable,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::{Context, Poll, Waker};

mod audio;
mod filter;
mod organise;

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

    /// Declares an output in the `stopped` state, so `output.start`, `output.stop` and
    /// `output.status` can name it, as `jstream_core::Engine::with_output` does. This
    /// changes no `rev` and emits no event. A declared output has no target, so it is not
    /// in the collection: `output.list` and `state.get` don't report it, and
    /// `output.set_key` answers `not_found` for it. `output.create` configures an output
    /// through the protocol instead.
    #[must_use]
    pub fn with_output(self, output_id: OutputId) -> Self {
        let mut state = lock(&self.shared);
        state.declared.insert(output_id.clone());
        state
            .outputs
            .entry(output_id)
            .or_insert(OutputState::Stopped);
        drop(state);
        self
    }

    /// Describes builtin source kinds by their descriptors, so their sources get audio
    /// settings when the kind produces audio, following `always` when it has no video
    /// and `when_shown` when it has (ADR-0028), as the engine does. The mock checks no
    /// settings against them. A kind it wasn't told about has no audio.
    #[must_use]
    pub fn with_kinds(self, kinds: impl IntoIterator<Item = KindDescriptor>) -> Self {
        let mut state = lock(&self.shared);
        for kind in kinds {
            if kind.audio {
                state
                    .collection
                    .audio_kinds
                    .insert(kind.kind_id, kind.video);
            } else {
                state.collection.audio_kinds.remove(&kind.kind_id);
            }
        }
        drop(state);
        self
    }

    /// Sets how many undo steps each client keeps (100 by default, as in the engine). The
    /// oldest steps are dropped first, and 0 keeps none.
    #[must_use]
    pub fn with_history_limit(self, limit: usize) -> Self {
        lock(&self.shared).history.set_limit(limit);
        self
    }

    /// The stream key `output.set_key` stored for `output_id`, if one is set. Lets client
    /// tests check what reached the engine; the protocol itself never returns a key.
    pub fn stream_key(&self, output_id: &OutputId) -> Option<SecretString> {
        lock(&self.shared).keys.get(output_id).cloned()
    }

    /// Opens a new client session. It must send `hello` before any call.
    pub fn connect(&self) -> MockControl {
        MockControl {
            shared: Arc::clone(&self.shared),
            client_id: Mutex::new(None),
            next_request: AtomicU64::new(1),
            subscriptions: Mutex::new(Vec::new()),
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

    /// How many event subscriptions are still receiving.
    ///
    /// A subscription counts while its stream exists and has not been closed. Dropping
    /// the stream, which is what `jstream-server` does when a connection starts to
    /// close, drops the count at once: the session can still be alive, finishing its
    /// last writes. A closed stream stays uncounted, including across later
    /// [`Self::emit`] calls.
    pub fn subscriber_count(&self) -> usize {
        lock(&self.shared)
            .subscribers
            .iter()
            .filter(|weak| weak.upgrade().is_some_and(|queue| !lock(&queue).closed))
            .count()
    }
}

/// One client session with a [`MockEngine`].
pub struct MockControl {
    shared: Arc<Mutex<State>>,
    client_id: Mutex<Option<ClientId>>,
    next_request: AtomicU64,
    /// This session's event subscriptions, closed when it is dropped.
    subscriptions: Mutex<Vec<Weak<Mutex<Queue>>>>,
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
        let mut mine = lock(&self.subscriptions);
        mine.retain(|weak| weak.strong_count() > 0);
        mine.push(Arc::downgrade(&queue));
        Box::pin(Subscription(queue))
    }
}

impl Drop for MockControl {
    /// Ends this session's event streams, drops its undo history and expires its open
    /// gestures: the session is gone, as when a socket closes.
    fn drop(&mut self) {
        let mine = std::mem::take(&mut *lock(&self.subscriptions));
        close_all(mine);
        if let Some(client_id) = lock(&self.client_id).take() {
            let mut state = lock(&self.shared);
            state.history.forget(&client_id);
            let open: Vec<GestureId> = state
                .gestures
                .keys()
                .filter(|(owner, _)| *owner == client_id)
                .map(|(_, gesture_id)| gesture_id.clone())
                .collect();
            for gesture_id in open {
                state.end_gesture(&client_id, &gesture_id, LiveOutcome::Expired, None);
            }
        }
    }
}

/// Everything the mock engine knows.
#[derive(Default)]
struct State {
    collection: Collection,
    /// Every output the engine knows (configured or declared), with its runtime state.
    outputs: BTreeMap<OutputId, OutputState>,
    /// The outputs [`MockEngine::with_output`] declared. They stay known whatever the
    /// collection holds.
    declared: BTreeSet<OutputId>,
    /// The in-memory stand-in for the OS credential store: the stream keys
    /// `output.set_key` stored, by output. As in the engine, a key is removed by
    /// `output.clear_key`, and by an applied batch, undo or redo that deletes its output
    /// or changes its `OutputKind::key_server` (#343).
    keys: BTreeMap<OutputId, SecretString>,
    /// The result of each batch, undo or redo applied with an idempotency key, per
    /// client: batches and history calls share a client's keys.
    applied: BTreeMap<(ClientId, IdempotencyKey), CallResult>,
    /// Every client's undo and redo stacks.
    history: History,
    /// How many sessions have completed `hello`; numbers the client IDs.
    clients: u64,
    subscribers: Vec<Weak<Mutex<Queue>>>,
    /// The scene on program: runtime state, not counted by `rev`. Follows the engine's
    /// rules (ADR-0030): the first scene created goes on program, and a deleted program
    /// scene gives way to the first scene left.
    program: Option<SceneId>,
    /// The scene on preview, which follows the same rules.
    preview: Option<SceneId>,
    /// Every client's open gestures, with the value of each parameter each holds.
    gestures: BTreeMap<(ClientId, GestureId), BTreeMap<Param, Edit>>,
    /// Which gesture holds each parameter: at most one.
    holders: BTreeMap<Param, (ClientId, GestureId)>,
}

/// A parameter a live value overrides: an item's placement (`false`) or its crop
/// (`true`). Ordered as the engine orders them, placements first, so `live.changed` and
/// `live.list` list values in the same order.
type Param = (bool, ItemId);

/// The parameter `value` sets, if it is live-adjustable.
fn param_of(value: &Edit) -> Option<Param> {
    match value {
        Edit::ItemSetTransform { item_id, .. } | Edit::ItemSetGeometry { item_id, .. } => {
            Some((false, item_id.clone()))
        }
        Edit::ItemSetCrop { item_id, .. } => Some((true, item_id.clone())),
        _ => None,
    }
}

/// The capture target ID of the one display the mock's `capture.targets` lists, a
/// 1920×1080 primary display.
pub const MOCK_DISPLAY: &str = "mock:display";

/// How many undo steps each client keeps unless [`MockEngine::with_history_limit`]
/// says otherwise, as in the engine.
const DEFAULT_HISTORY_LIMIT: usize = 100;

/// One undoable step: the edits that undo it (on the undo stack) or redo it (on the
/// redo stack).
struct Step {
    /// The `rev` the step's batch first produced. It names the step.
    step_rev: Rev,
    edits: Vec<Edit>,
    /// The `rev` the step was last applied or undone at. Another client's change after
    /// it is a conflict.
    since: Rev,
}

#[derive(Default)]
struct Stacks {
    /// Oldest first.
    undo: VecDeque<Step>,
    /// Most recently undone last.
    redo: Vec<Step>,
}

/// Every client's undo and redo stacks, and who made each change a step could conflict
/// with.
struct History {
    clients: BTreeMap<ClientId, Stacks>,
    /// The client behind each `rev` after the oldest `since` of any kept step.
    log: VecDeque<(Rev, ClientId)>,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        Self {
            clients: BTreeMap::new(),
            log: VecDeque::new(),
            limit: DEFAULT_HISTORY_LIMIT,
        }
    }
}

impl History {
    fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
        for stacks in self.clients.values_mut() {
            while stacks.undo.len() > limit {
                stacks.undo.pop_front();
            }
            let excess = stacks.redo.len().saturating_sub(limit);
            stacks.redo.drain(..excess);
        }
        self.prune();
    }

    /// A client's batch at `rev`: a new step, and nothing left to redo.
    fn record_batch(&mut self, client_id: &ClientId, rev: Rev, inverse: Vec<Edit>) {
        self.log.push_back((rev, client_id.clone()));
        let stacks = self.clients.entry(client_id.clone()).or_default();
        stacks.redo.clear();
        stacks.undo.push_back(Step {
            step_rev: rev,
            edits: inverse,
            since: rev,
        });
        while stacks.undo.len() > self.limit {
            stacks.undo.pop_front();
        }
        self.prune();
    }

    /// Takes the step an undo or redo would apply.
    fn take(&mut self, client_id: &ClientId, direction: HistoryDirection) -> Option<Step> {
        let stacks = self.clients.get_mut(client_id)?;
        match direction {
            HistoryDirection::Undo => stacks.undo.pop_back(),
            HistoryDirection::Redo => stacks.redo.pop(),
            _ => None,
        }
    }

    /// Returns a step [`Self::take`] took, unchanged.
    fn put_back(&mut self, client_id: &ClientId, direction: HistoryDirection, step: Step) {
        let stacks = self.clients.entry(client_id.clone()).or_default();
        match direction {
            HistoryDirection::Redo => stacks.redo.push(step),
            _ => stacks.undo.push_back(step),
        }
    }

    /// A step applied at `rev` in `direction`: it moves to the other stack, holding
    /// `inverse`, which undoes what was just applied.
    fn moved(
        &mut self,
        client_id: &ClientId,
        direction: HistoryDirection,
        step: Step,
        rev: Rev,
        inverse: Vec<Edit>,
    ) {
        self.log.push_back((rev, client_id.clone()));
        let step = Step {
            step_rev: step.step_rev,
            edits: inverse,
            since: rev,
        };
        let stacks = self.clients.entry(client_id.clone()).or_default();
        match direction {
            HistoryDirection::Redo => stacks.undo.push_back(step),
            _ => stacks.redo.push(step),
        }
        self.prune();
    }

    /// The first change by another client after `since`, with its `rev`.
    fn first_other(&self, client_id: &ClientId, since: Rev) -> Option<(ClientId, Rev)> {
        self.log
            .iter()
            .find(|(rev, by)| *rev > since && by != client_id)
            .map(|(rev, by)| (by.clone(), *rev))
    }

    /// How many steps `client_id` can undo and redo.
    fn depths(&self, client_id: &ClientId) -> (u32, u32) {
        let depth = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        self.clients.get(client_id).map_or((0, 0), |stacks| {
            (depth(stacks.undo.len()), depth(stacks.redo.len()))
        })
    }

    /// Drops a client's stacks: its session ended.
    fn forget(&mut self, client_id: &ClientId) {
        self.clients.remove(client_id);
        self.prune();
    }

    /// Keeps only the log entries a kept step could still conflict with.
    fn prune(&mut self) {
        let oldest = self
            .clients
            .values()
            .flat_map(|stacks| stacks.undo.iter().chain(&stacks.redo))
            .map(|step| step.since)
            .min();
        match oldest {
            Some(oldest) => self.log.retain(|(rev, _)| *rev > oldest),
            None => self.log.clear(),
        }
    }
}

/// The collection state that `rev` counts. Cloned to apply a batch atomically.
#[derive(Clone)]
struct Collection {
    rev: Rev,
    canvas: Canvas,
    /// In display order.
    scenes: Vec<Scene>,
    items: BTreeMap<ItemId, Item>,
    /// Ordered by ID, as `source.list` reports them.
    sources: BTreeMap<SourceId, Source>,
    /// The configured outputs, ordered by ID, as `output.list` reports them.
    outputs: BTreeMap<OutputId, OutputKind>,
    /// The encoder profile each output uses, for those that chose one.
    output_encoders: BTreeMap<OutputId, EncoderProfileId>,
    /// The encoder profiles, ordered by ID: name and encoding.
    encoders: BTreeMap<EncoderProfileId, (String, EncoderProfile)>,
    /// Each output's own filter chains, for those that have had a filter (ADR-0027).
    output_filters: BTreeMap<OutputId, jstream_protocol::FilterChains>,
    /// The master stage's filter chains.
    master: jstream_protocol::FilterChains,
    /// The video mix each output shows, for those that chose one.
    output_mixes: BTreeMap<OutputId, VideoMixId>,
    /// The video mixes, ordered by ID: name and mix.
    video_mixes: BTreeMap<VideoMixId, (String, VideoMix)>,
    /// The hotkey bindings, ordered by chord.
    hotkeys: BTreeMap<Chord, Action>,
    /// The scene list's folders, in folder order (ADR-0024 §3).
    folders: Vec<jstream_protocol::FolderInfo>,
    /// The labels, in label order (ADR-0024 §4).
    labels: Vec<jstream_protocol::LabelInfo>,
    /// The buses, ordered by ID, the master among them.
    buses: BTreeMap<BusId, audio::Bus>,
    /// Each send's gain in dB, keyed by bus, then by source.
    sends: BTreeMap<(BusId, SourceId), Finite32>,
    /// The builtin kinds with audio, and whether each has video too. Not collection
    /// state: [`MockEngine::with_kinds`] sets it, and no edit changes it.
    audio_kinds: BTreeMap<String, bool>,
}

impl Default for Collection {
    fn default() -> Self {
        Self {
            rev: Rev::default(),
            canvas: Canvas::default(),
            scenes: Vec::new(),
            items: BTreeMap::new(),
            sources: BTreeMap::new(),
            outputs: BTreeMap::new(),
            output_encoders: BTreeMap::new(),
            encoders: BTreeMap::new(),
            output_mixes: BTreeMap::new(),
            video_mixes: BTreeMap::new(),
            buses: BTreeMap::from([audio::Bus::master()]),
            sends: BTreeMap::new(),
            audio_kinds: BTreeMap::new(),
            output_filters: BTreeMap::new(),
            master: jstream_protocol::FilterChains::default(),
            hotkeys: BTreeMap::new(),
            folders: Vec::new(),
            labels: Vec::new(),
        }
    }
}

/// A source: its name, what it shows, and its settings. The mock describes no kinds, so
/// it stores settings as sent, without checking them or filling in defaults.
#[derive(Clone)]
struct Source {
    name: String,
    kind: SourceKind,
    settings: Settings,
    /// A plain link, which may name a deleted source; reported only while it exists.
    duplicated_from: Option<SourceId>,
    /// Its filter chains, settings as given (ADR-0026).
    filters: jstream_protocol::FilterChains,
    /// Set exactly for an audio source.
    audio: Option<SourceAudio>,
    /// Its labels, ordered by ID, and favourite flag (ADR-0024 §4).
    meta: jstream_protocol::SourceMeta,
}

#[derive(Clone)]
struct Scene {
    id: SceneId,
    name: String,
    role: SceneRole,
    /// Bottom to top.
    items: Vec<ItemId>,
    /// The folder it is filed in, if any.
    folder: Option<jstream_protocol::FolderId>,
    meta: jstream_protocol::SceneMeta,
}

/// An item: the scene that holds it, the source it places, and every property an edit
/// sets, as `item.list` and `state.get` report them.
#[derive(Clone)]
struct Item {
    scene_id: SceneId,
    source_id: SourceId,
    /// The legacy transform, or the view of `geometry` when it is set.
    transform: ItemTransform,
    geometry: Option<ItemGeometry>,
    crop: Crop,
    visible: bool,
    opacity: Finite32,
    blend: BlendMode,
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
            Call::History(call) => self.history(client_id, request_id, &call),
            Call::Action { action } => self.action(client_id, request_id, action),
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
            && let Some(result) = self.applied.get(&(client_id.clone(), key.clone()))
        {
            return Ok(result.clone());
        }
        // The engine checks `ends_gesture` before the graph's own checks.
        if let Some(gesture_id) = &batch.ends_gesture
            && !self
                .gestures
                .contains_key(&(client_id.clone(), gesture_id.clone()))
        {
            return Err(ProtocolError::invalid_argument(
                "ends_gesture",
                format!("the client has no open gesture `{gesture_id}`"),
            ));
        }
        // Same order as `jstream_graph::Graph::apply`: the revision check comes first.
        let current = self.collection.rev;
        if let Some(expected) = batch.expected_rev
            && expected != current
        {
            return Err(ProtocolError::RevConflict { expected, current });
        }
        if batch.edits.is_empty() {
            return Err(ProtocolError::EmptyBatch);
        }
        let (rev, inverse, removed_hotkeys) = self.apply(batch.edits)?;
        self.history.record_batch(client_id, rev, inverse);
        let result = CallResult::Batch {
            rev,
            removed_hotkeys,
        };
        if let Some(key) = batch.idempotency_key {
            self.applied
                .insert((client_id.clone(), key), result.clone());
        }
        self.changed(client_id, request_id, rev, None);
        if let Some(gesture_id) = batch.ends_gesture {
            self.remove_gesture(client_id, &gesture_id);
            self.emit(Event::LiveEnded {
                gesture_id,
                client_id: client_id.clone(),
                outcome: LiveOutcome::Committed,
                rev: Some(rev),
            });
        }
        Ok(result)
    }

    /// `history.undo` or `history.redo`, checked in the engine's order: the idempotency
    /// key, then `expected_rev`, then whether there is a step.
    fn history(
        &mut self,
        client_id: &ClientId,
        request_id: &RequestId,
        call: &HistoryCall,
    ) -> Result<CallResult, ProtocolError> {
        let keyed = call
            .idempotency_key
            .as_ref()
            .map(|key| (client_id.clone(), key.clone()));
        if let Some(result) = keyed.as_ref().and_then(|keyed| self.applied.get(keyed)) {
            return Ok(result.clone());
        }
        let current = self.collection.rev;
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
        if let Some((other, rev)) = self.history.first_other(client_id, step.since) {
            self.history.prune();
            return Err(ProtocolError::UndoConflict {
                step_rev: step.step_rev,
                client_id: other,
                rev,
            });
        }
        // With no other client's change since, only a running output can be in the way
        // (undoing its `output.create`): keep the step for a retry once it stops.
        let (rev, inverse, _) = match self.apply(step.edits.clone()) {
            Ok(applied) => applied,
            Err(error) => {
                self.history.put_back(client_id, direction, step);
                return Err(error);
            }
        };
        let step_rev = step.step_rev;
        self.history.moved(client_id, direction, step, rev, inverse);
        let (undo_depth, redo_depth) = self.history.depths(client_id);
        let result = CallResult::History {
            rev,
            step_rev,
            undo_depth,
            redo_depth,
        };
        if let Some(keyed) = keyed {
            self.applied.insert(keyed, result.clone());
        }
        self.changed(client_id, request_id, rev, Some(direction));
        Ok(result)
    }

    /// Applies `edits` atomically as one `rev` bump, and returns that `rev`, the edits
    /// that undo them, and the hotkey bindings their deletes removed (ADR-0040 §3).
    fn apply(
        &mut self,
        edits: Vec<Edit>,
    ) -> Result<(Rev, Vec<Edit>, Vec<HotkeyBinding>), ProtocolError> {
        let running = self.running();
        let mut next = self.collection.clone();
        let mut undo = Vec::with_capacity(edits.len());
        let mut removed = Vec::new();
        for (at, edit) in edits.into_iter().enumerate() {
            let deleted = match &edit {
                Edit::SceneDelete { scene_id } => Some((EntityKind::Scene, scene_id.to_string())),
                Edit::SourceDelete { source_id } => {
                    Some((EntityKind::Source, source_id.to_string()))
                }
                Edit::OutputDelete { output_id } => {
                    Some((EntityKind::Output, output_id.to_string()))
                }
                Edit::EncoderDelete { profile_id } => {
                    Some((EntityKind::Encoder, profile_id.to_string()))
                }
                _ => None,
            };
            let mut inverse = next.apply(edit, at, &running)?;
            if let Some((entity, id)) = deleted {
                inverse.extend(next.remove_hotkeys_naming(entity, &id, &mut removed));
            }
            undo.push(inverse);
        }
        next.rev = self
            .collection
            .rev
            .next()
            .ok_or_else(|| ProtocolError::Internal {
                reason: "rev overflowed".into(),
            })?;
        let rev = next.rev;
        // A key never follows its output to another server, or outlives it (#343).
        let stale: Vec<OutputId> = self
            .collection
            .outputs
            .iter()
            .filter(|(output_id, kind)| {
                next.outputs
                    .get(*output_id)
                    .is_none_or(|now| now.key_server() != kind.key_server())
            })
            .map(|(output_id, _)| output_id.clone())
            .collect();
        for output_id in stale {
            self.keys.remove(&output_id);
        }
        self.collection = next;
        Ok((rev, undo.into_iter().rev().flatten().collect(), removed))
    }

    /// What follows every applied batch, undo and redo: the outputs follow the
    /// collection, live values on deleted items go, `state.changed` is emitted, and the
    /// program scene is reconciled.
    fn changed(
        &mut self,
        client_id: &ClientId,
        request_id: &RequestId,
        rev: Rev,
        cause: Option<HistoryDirection>,
    ) {
        self.sync_outputs();
        let items = &self.collection.items;
        self.holders
            .retain(|(_, item_id), _| items.contains_key(item_id));
        for values in self.gestures.values_mut() {
            values.retain(|(_, item_id), _| items.contains_key(item_id));
        }
        self.emit(Event::StateChanged {
            rev,
            client_id: client_id.clone(),
            request_id: Some(request_id.clone()),
            cause,
        });
        // Edits that deleted the program or preview scene, or created the first scene,
        // move them; that event follows their `state.changed` (ADR-0030).
        let first = self.collection.scenes.first().map(|scene| scene.id.clone());
        let mut moved = false;
        for slot in [&mut self.program, &mut self.preview] {
            let kept = slot
                .as_ref()
                .is_some_and(|id| self.collection.scene_index(id).is_some());
            if !kept && *slot != first {
                *slot = first.clone();
                moved = true;
            }
        }
        if moved {
            self.emit_studio(client_id, request_id, None);
        }
    }

    fn emit_studio(
        &mut self,
        client_id: &ClientId,
        request_id: &RequestId,
        transition: Option<TransitionStarted>,
    ) {
        self.emit(Event::StudioChanged {
            program_scene: self.program.clone(),
            preview_scene: self.preview.clone(),
            transition,
            client_id: client_id.clone(),
            request_id: Some(request_id.clone()),
        });
    }

    /// The outputs that are running: every state but `stopped` and `failed`.
    /// `output.delete` refuses these, as the engine does.
    fn running(&self) -> BTreeSet<OutputId> {
        self.outputs
            .iter()
            .filter(|(_, state)| !is_idle(**state))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Follows the collection after a batch, as the engine does: an output it has gained
    /// starts `stopped`, and one it has lost is forgotten unless it was declared. Only
    /// idle outputs can have been deleted.
    fn sync_outputs(&mut self) {
        for output_id in self.collection.outputs.keys() {
            self.outputs
                .entry(output_id.clone())
                .or_insert(OutputState::Stopped);
        }
        let (declared, configured) = (&self.declared, &self.collection.outputs);
        self.outputs.retain(|id, state| {
            declared.contains(id) || configured.contains_key(id) || !is_idle(*state)
        });
    }

    fn action(
        &mut self,
        client_id: &ClientId,
        request_id: &RequestId,
        action: Action,
    ) -> Result<CallResult, ProtocolError> {
        let (output_id, start) = match action {
            Action::SceneSwitch {
                scene_id,
                preset_id,
            } => {
                self.collection.existing_scene(&scene_id)?;
                // The only preset the mock knows is the initial `cut`.
                let initial = TransitionPresetId::new(INITIAL_PRESET);
                if let Some(id) = preset_id.filter(|id| *id != initial) {
                    return Err(not_found(EntityKind::Transition, id.as_str()));
                }
                if self.program.as_ref() != Some(&scene_id) {
                    self.program = Some(scene_id);
                    let started = TransitionStarted {
                        preset_id: initial,
                        duration_ms: 0,
                    };
                    self.emit_studio(client_id, request_id, Some(started));
                }
                return Ok(CallResult::action());
            }
            Action::StudioSetPreview { scene_id } => {
                self.collection.existing_scene(&scene_id)?;
                if self.preview.as_ref() != Some(&scene_id) {
                    self.preview = Some(scene_id);
                    self.emit_studio(client_id, request_id, None);
                }
                return Ok(CallResult::action());
            }
            Action::OutputStart { output_id } => (output_id, true),
            Action::OutputStop { output_id } => (output_id, false),
            Action::LiveSet { gesture_id, values } => {
                return self.live_set(client_id, gesture_id, values);
            }
            Action::LiveCancel { gesture_id } => {
                self.end_gesture(client_id, &gesture_id, LiveOutcome::Cancelled, None);
                return Ok(CallResult::action());
            }
            Action::OutputSetKey { output_id, key } => {
                // The engine's order: the output must be configured, then the key valid.
                if !self.collection.outputs.contains_key(&output_id) {
                    return Err(not_found(EntityKind::Output, output_id.as_str()));
                }
                check_key(&key)?;
                self.keys.insert(output_id, key);
                return Ok(CallResult::action());
            }
            Action::OutputClearKey { output_id } => {
                // Succeeds with no key, and for an output no longer in the collection.
                self.keys.remove(&output_id);
                return Ok(CallResult::action());
            }
            // As the engine: the server in front answers these (ADR-0044).
            other @ (Action::PairingStart | Action::DeviceRevoke { .. }) => {
                return Err(ProtocolError::Unsupported {
                    call: other.name().into(),
                });
            }
            // The mock runs no mixer, so there is no loudness to reset.
            Action::AudioResetLoudness {} => return Ok(CallResult::action()),
            other => return Err(unimplemented_by_mock(other.name())),
        };
        let Some(&state) = self.outputs.get(&output_id) else {
            return Err(not_found(EntityKind::Output, output_id.as_str()));
        };
        let idle = is_idle(state);
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
        Ok(CallResult::action())
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
                        role: scene.role,
                        item_count: u32::try_from(scene.items.len()).unwrap_or(u32::MAX),
                        folder: scene.folder.clone(),
                        meta: scene.meta.clone(),
                    })
                    .collect(),
            },
            Query::OutputStatus { output_id } => {
                let Some(&state) = self.outputs.get(&output_id) else {
                    return Err(not_found(EntityKind::Output, output_id.as_str()));
                };
                // The mock opens no encoders, so it reports no sessions.
                QueryResult::OutputStatus {
                    key_set: self.keys.contains_key(&output_id),
                    output_id,
                    state,
                    encoder: None,
                    encoder_sessions: 0,
                    hardware_sessions: 0,
                }
            }
            Query::SourceList => QueryResult::SourceList {
                rev: self.collection.rev,
                sources: self.collection.source_infos(),
            },
            Query::ItemList { scene_id } => {
                let index = self.collection.existing_scene(&scene_id)?;
                QueryResult::ItemList {
                    rev: self.collection.rev,
                    items: self.collection.item_infos(&self.collection.scenes[index]),
                    scene_id,
                }
            }
            Query::StateGet => QueryResult::StateGet {
                rev: self.collection.rev,
                canvas: self.collection.canvas,
                sources: self.collection.source_infos(),
                scenes: self
                    .collection
                    .scenes
                    .iter()
                    .map(|scene| SceneInfo {
                        scene_id: scene.id.clone(),
                        name: scene.name.clone(),
                        role: scene.role,
                        items: self.collection.item_infos(scene),
                        transition_in: None,
                        transition_out: None,
                        folder: scene.folder.clone(),
                        meta: scene.meta.clone(),
                    })
                    .collect(),
                outputs: self.collection.output_infos(),
                encoders: self.collection.encoder_infos(),
                video_mixes: self.collection.video_mix_infos(),
                buses: self.collection.bus_infos(),
                sends: self.collection.send_infos(),
                master: Box::new(jstream_protocol::MasterInfo {
                    filters: self.collection.master.clone(),
                }),
                hotkeys: self
                    .collection
                    .hotkeys
                    .iter()
                    .map(|(chord, action)| HotkeyBinding::new(chord.clone(), action))
                    .collect(),
                transitions: Box::new(TransitionsInfo::default()),
                folders: self.collection.folders.clone(),
                labels: self.collection.labels.clone(),
            },
            Query::OutputList => QueryResult::OutputList {
                rev: self.collection.rev,
                outputs: self.collection.output_infos(),
            },
            Query::StudioGet => QueryResult::StudioGet {
                program_scene: self.program.clone(),
                preview_scene: self.preview.clone(),
            },
            Query::EncoderList => QueryResult::EncoderList {
                rev: self.collection.rev,
                encoders: self.collection.encoder_infos(),
            },
            Query::VideoMixList => QueryResult::VideoMixList {
                rev: self.collection.rev,
                video_mixes: self.collection.video_mix_infos(),
            },
            Query::CaptureTargets => QueryResult::CaptureTargets {
                targets: vec![CaptureTargetInfo {
                    kind: CaptureTargetKind::Display,
                    target: MOCK_DISPLAY.into(),
                    name: "Mock display".into(),
                    size: Some(PixelSize {
                        width: 1920,
                        height: 1080,
                    }),
                    primary: true,
                }],
            },
            Query::LiveList => QueryResult::LiveList {
                gestures: self
                    .gestures
                    .iter()
                    .map(|((client_id, gesture_id), values)| GestureInfo {
                        gesture_id: gesture_id.clone(),
                        client_id: client_id.clone(),
                        values: values.values().cloned().collect(),
                    })
                    .collect(),
            },
            Query::BusList => QueryResult::BusList {
                rev: self.collection.rev,
                buses: self.collection.bus_infos(),
                sends: self.collection.send_infos(),
            },
            // The mock describes no kinds, so it lists none.
            Query::KindList => QueryResult::KindList { kinds: Vec::new() },
            Query::DeviceList => {
                return Err(ProtocolError::Unsupported {
                    call: "device.list".into(),
                });
            }
            other => return Err(unimplemented_by_mock(other.name())),
        };
        Ok(CallResult::Query { result })
    }

    /// `live.set`, checked in the engine's order: the shape of the message, then each
    /// value against the collection as a batch's edits are, named `values.<n>`.
    fn live_set(
        &mut self,
        client_id: &ClientId,
        gesture_id: GestureId,
        values: Vec<Edit>,
    ) -> Result<CallResult, ProtocolError> {
        let invalid = |field: String, reason: &str| ProtocolError::invalid_argument(field, reason);
        if values.is_empty() {
            return Err(invalid("values".into(), "must name at least one value"));
        }
        if values.len() > MAX_LIVE_VALUES {
            return Err(invalid("values".into(), "must name at most 256 values"));
        }
        if let Some(at) = values.iter().position(|value| !is_live_adjustable(value)) {
            return Err(invalid(format!("values.{at}.type"), "not live-adjustable"));
        }
        let key = (client_id.clone(), gesture_id);
        let open = self
            .gestures
            .keys()
            .filter(|(owner, _)| owner == client_id)
            .count();
        if !self.gestures.contains_key(&key) && open >= MAX_GESTURES_PER_CLIENT {
            return Err(invalid("gesture_id".into(), "16 gestures are open"));
        }
        let running = self.running();
        let mut check = self.collection.clone();
        for (at, value) in values.iter().enumerate() {
            check
                .apply(value.clone(), at, &running)
                .map_err(|error| match error {
                    ProtocolError::InvalidArgument {
                        field,
                        reason,
                        blockers,
                    } => ProtocolError::InvalidArgument {
                        field: match field.strip_prefix("edits.") {
                            Some(rest) => format!("values.{rest}"),
                            None => field,
                        },
                        reason,
                        blockers,
                    },
                    other => other,
                })?;
        }
        let mut changed = BTreeMap::new();
        for value in values {
            let Some(param) = param_of(&value) else {
                continue;
            };
            if let Some(holder) = self.holders.insert(param.clone(), key.clone())
                && holder != key
                && let Some(other) = self.gestures.get_mut(&holder)
            {
                other.remove(&param);
            }
            changed.insert(param, value);
        }
        self.gestures
            .entry(key.clone())
            .or_default()
            .extend(changed.clone());
        self.emit(Event::LiveChanged {
            gesture_id: key.1,
            client_id: key.0,
            values: changed.into_values().collect(),
        });
        Ok(CallResult::action())
    }

    /// Ends `client_id`'s gesture without a commit, if it is open, and emits `live.ended`.
    fn end_gesture(
        &mut self,
        client_id: &ClientId,
        gesture_id: &GestureId,
        outcome: LiveOutcome,
        rev: Option<Rev>,
    ) {
        if self.remove_gesture(client_id, gesture_id) {
            self.emit(Event::LiveEnded {
                gesture_id: gesture_id.clone(),
                client_id: client_id.clone(),
                outcome,
                rev,
            });
        }
    }

    /// Closes a gesture and lets go of the parameters it held. Returns whether it was open.
    fn remove_gesture(&mut self, client_id: &ClientId, gesture_id: &GestureId) -> bool {
        let key = (client_id.clone(), gesture_id.clone());
        let Some(values) = self.gestures.remove(&key) else {
            return false;
        };
        for param in values.keys() {
            if self.holders.get(param) == Some(&key) {
                self.holders.remove(param);
            }
        }
        true
    }

    /// Queues `event` for every live subscriber, and forgets subscriptions that were
    /// dropped or closed (their session went away), so a closed stream stays ended.
    fn emit(&mut self, event: Event) {
        self.subscribers.retain(|weak| {
            let Some(queue) = weak.upgrade() else {
                return false;
            };
            let waker = {
                let mut queue = lock(&queue);
                if queue.closed {
                    return false;
                }
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
        close_all(self.subscribers.drain(..));
    }
}

/// Ends the subscriptions still alive among `queues`: each yields what it has buffered,
/// then `None`.
fn close_all(queues: impl IntoIterator<Item = Weak<Mutex<Queue>>>) {
    for queue in queues.into_iter().filter_map(|weak| weak.upgrade()) {
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

impl Collection {
    /// Applies edit number `at` of its batch to this (scratch) copy of the collection,
    /// and returns the edits that undo it, computed from the state before it as
    /// `jstream-graph` computes them.
    ///
    /// It follows the rules `jstream-graph` enforces for the same edits (existence,
    /// unique IDs, no deleting what is still referenced, index bounds, no nesting
    /// cycles from `item.add`, `item.move` or `item.set_source`, `duplicated_from` never naming the
    /// source itself, no deleting a running output, RTMP server URLs), with the same error
    /// codes and `edits.<at>.<field>` paths. It describes no kinds, so it doesn't check
    /// settings, fill in defaults, or follow settings references when it looks for
    /// cycles. `running` are the outputs that run.
    fn apply(
        &mut self,
        edit: Edit,
        at: usize,
        running: &BTreeSet<OutputId>,
    ) -> Result<Vec<Edit>, ProtocolError> {
        if let Some(applied) = self.apply_audio(&edit, at) {
            return applied;
        }
        if let Some(applied) = self.apply_organise(&edit, at) {
            return applied;
        }
        let inverse = match edit {
            Edit::SceneCreate {
                scene_id,
                name,
                role,
            } => {
                check_id(scene_id.as_str(), at, "scene_id")?;
                check_name(&name, at)?;
                if self.scene_index(&scene_id).is_some() {
                    return Err(already_exists(EntityKind::Scene, scene_id.as_str()));
                }
                self.scenes.push(Scene {
                    id: scene_id.clone(),
                    name,
                    role: role.unwrap_or_default(),
                    items: Vec::new(),
                    folder: None,
                    meta: Default::default(),
                });
                vec![Edit::SceneDelete { scene_id }]
            }
            Edit::SceneRename { scene_id, name } => {
                let index = self.existing_scene(&scene_id)?;
                check_name(&name, at)?;
                let old = std::mem::replace(&mut self.scenes[index].name, name);
                vec![Edit::SceneRename {
                    scene_id,
                    name: old,
                }]
            }
            Edit::SceneSetRole { scene_id, role } => {
                let index = self.existing_scene(&scene_id)?;
                let old = std::mem::replace(&mut self.scenes[index].role, role);
                vec![Edit::SceneSetRole {
                    scene_id,
                    role: old,
                }]
            }
            Edit::SceneDelete { scene_id } => {
                let index = self.existing_scene(&scene_id)?;
                let shown = self.sources.values().any(|source| {
                    matches!(&source.kind, SourceKind::Scene { scene_id: s } if *s == scene_id)
                });
                if shown {
                    return Err(invalid(
                        at,
                        "scene_id",
                        format!("scene `{scene_id}` is shown by a scene source; delete it first"),
                    ));
                }
                if self
                    .video_mixes
                    .values()
                    .any(|(_, mix)| mix.scene() == Some(&scene_id))
                {
                    return Err(invalid(
                        at,
                        "scene_id",
                        format!(
                            "scene `{scene_id}` is shown by a video mix; point it elsewhere first"
                        ),
                    ));
                }
                if self.is_referred_to(
                    |r| matches!(r, Reference::Scene { scene_id: s } if *s == scene_id),
                ) {
                    return Err(invalid(
                        at,
                        "scene_id",
                        format!("scene `{scene_id}` is referred to by settings; clear them first"),
                    ));
                }
                let scene = self.scenes.remove(index);
                let mut inverse = vec![Edit::SceneCreate {
                    scene_id: scene.id.clone(),
                    name: scene.name,
                    role: (!scene.role.is_scene()).then_some(scene.role),
                }];
                // Re-created scenes are appended, so only a scene that wasn't last moves back.
                if index != self.scenes.len() {
                    inverse.push(Edit::SceneReorder {
                        scene_id: scene.id.clone(),
                        index: wire_index(index)?,
                    });
                }
                if scene.folder.is_some() {
                    inverse.push(Edit::SceneSetFolder {
                        scene_id: scene.id.clone(),
                        folder_id: scene.folder.clone(),
                    });
                }
                if !scene.meta.is_default() {
                    inverse.push(Edit::SceneSetMeta {
                        scene_id: scene.id.clone(),
                        meta: scene.meta.clone(),
                    });
                }
                // Re-adding bottom to top puts each item back at its own index.
                for (position, item_id) in scene.items.iter().enumerate() {
                    if let Some(item) = self.items.remove(item_id) {
                        inverse.extend(restore_item(item_id, &item, position, position)?);
                    }
                }
                inverse
            }
            Edit::SceneReorder { scene_id, index } => {
                let from = self.existing_scene(&scene_id)?;
                let to = check_index(index, self.scenes.len(), at, "scenes")?;
                let scene = self.scenes.remove(from);
                self.scenes.insert(to, scene);
                vec![Edit::SceneReorder {
                    scene_id,
                    index: wire_index(from)?,
                }]
            }
            Edit::ItemAdd {
                scene_id,
                item_id,
                source_id,
                transform,
                geometry,
                opacity,
                blend,
            } => {
                check_id(item_id.as_str(), at, "item_id")?;
                let index = self.existing_scene(&scene_id)?;
                if !self.sources.contains_key(&source_id) {
                    return Err(not_found(EntityKind::Source, source_id.as_str()));
                }
                if self.items.contains_key(&item_id) {
                    return Err(already_exists(EntityKind::Item, item_id.as_str()));
                }
                self.check_nesting(&scene_id, &source_id, at)?;
                let transform = match (transform, geometry) {
                    (Some(_), Some(_)) => {
                        return Err(invalid(
                            at,
                            "geometry",
                            "send either `transform` or `geometry`, not both",
                        ));
                    }
                    (None, Some(geometry)) => geometry_view(&geometry, at)?,
                    (transform, None) => transform.unwrap_or(ItemTransform::IDENTITY),
                };
                let opacity = opacity.unwrap_or(Finite32::ONE);
                check_opacity(opacity, at)?;
                let blend = blend.unwrap_or_default();
                check_blend(blend, at)?;
                self.scenes[index].items.push(item_id.clone());
                self.items.insert(
                    item_id.clone(),
                    Item {
                        scene_id,
                        source_id,
                        transform,
                        geometry,
                        crop: Crop::NONE,
                        visible: true,
                        opacity,
                        blend,
                    },
                );
                vec![Edit::ItemRemove { item_id }]
            }
            Edit::ItemRemove { item_id } => {
                let Some(item) = self.items.remove(&item_id) else {
                    return Err(not_found(EntityKind::Item, item_id.as_str()));
                };
                let scene = self.existing_scene(&item.scene_id)?;
                let items = &mut self.scenes[scene].items;
                let position = items.iter().position(|id| *id == item_id).unwrap_or(0);
                items.remove(position);
                // `item.add` puts it on top, which is index `len` now that it is gone.
                restore_item(&item_id, &item, position, items.len())?
            }
            Edit::ItemReorder { item_id, index } => {
                let scene_id = self.existing_item(&item_id)?.scene_id.clone();
                let scene = self.existing_scene(&scene_id)?;
                let items = &mut self.scenes[scene].items;
                let to = check_index(index, items.len(), at, "items in the scene")?;
                let from = items.iter().position(|id| *id == item_id).unwrap_or(0);
                items.remove(from);
                items.insert(to, item_id.clone());
                vec![Edit::ItemReorder {
                    item_id,
                    index: wire_index(from)?,
                }]
            }
            Edit::ItemMove {
                item_id,
                scene_id,
                index,
            } => self.move_item(item_id, scene_id, index, at)?,
            Edit::ItemSetTransform {
                item_id,
                scene_id,
                transform,
            } => {
                let item = self.placement(&item_id, scene_id.as_ref(), at)?;
                let old = set_placement(&item_id, &scene_id, item);
                item.transform = transform;
                item.geometry = None;
                vec![old]
            }
            Edit::ItemSetGeometry {
                item_id,
                scene_id,
                geometry,
            } => {
                self.placement(&item_id, scene_id.as_ref(), at)?;
                let view = geometry_view(&geometry, at)?;
                let item = self.existing_item(&item_id)?;
                let old = set_placement(&item_id, &scene_id, item);
                item.transform = view;
                item.geometry = Some(geometry);
                vec![old]
            }
            Edit::CanvasSetSize { width, height } => {
                check_canvas_side(width, at, "width")?;
                check_canvas_side(height, at, "height")?;
                let old = std::mem::replace(&mut self.canvas, Canvas { width, height });
                vec![Edit::CanvasSetSize {
                    width: old.width,
                    height: old.height,
                }]
            }
            Edit::ItemSetCrop {
                item_id,
                scene_id,
                crop,
            } => {
                let item = self.placement(&item_id, scene_id.as_ref(), at)?;
                let old = std::mem::replace(&mut item.crop, crop);
                vec![Edit::ItemSetCrop {
                    item_id,
                    scene_id,
                    crop: old,
                }]
            }
            Edit::ItemSetSource { item_id, source_id } => {
                let scene_id = self.existing_item(&item_id)?.scene_id.clone();
                if !self.sources.contains_key(&source_id) {
                    return Err(not_found(EntityKind::Source, source_id.as_str()));
                }
                self.check_nesting(&scene_id, &source_id, at)?;
                let item = self.existing_item(&item_id)?;
                let old = std::mem::replace(&mut item.source_id, source_id);
                vec![Edit::ItemSetSource {
                    item_id,
                    source_id: old,
                }]
            }
            Edit::ItemSetVisible {
                item_id,
                scene_id,
                visible,
            } => {
                let item = self.placement(&item_id, scene_id.as_ref(), at)?;
                let old = std::mem::replace(&mut item.visible, visible);
                vec![Edit::ItemSetVisible {
                    item_id,
                    scene_id,
                    visible: old,
                }]
            }
            Edit::ItemSetOpacity {
                item_id,
                scene_id,
                opacity,
            } => {
                let item = self.placement(&item_id, scene_id.as_ref(), at)?;
                check_opacity(opacity, at)?;
                let old = std::mem::replace(&mut item.opacity, opacity);
                vec![Edit::ItemSetOpacity {
                    item_id,
                    scene_id,
                    opacity: old,
                }]
            }
            Edit::ItemSetBlend { item_id, blend } => {
                let item = self.existing_item(&item_id)?;
                check_blend(blend, at)?;
                let old = std::mem::replace(&mut item.blend, blend);
                vec![Edit::ItemSetBlend {
                    item_id,
                    blend: old,
                }]
            }
            Edit::SourceCreate {
                source_id,
                name,
                kind,
                settings,
                duplicated_from,
            } => {
                check_id(source_id.as_str(), at, "source_id")?;
                check_name(&name, at)?;
                if duplicated_from.as_ref() == Some(&source_id) {
                    return Err(invalid(
                        at,
                        "duplicated_from",
                        "must not name the source itself",
                    ));
                }
                match &kind {
                    SourceKind::Scene { scene_id } => {
                        self.existing_scene(scene_id)?;
                    }
                    SourceKind::Builtin { kind } if kind.is_empty() => {
                        return Err(invalid(at, "kind.kind", "must not be empty"));
                    }
                    SourceKind::Builtin { .. } => {}
                    _ => return Err(unimplemented_by_mock("this source kind")),
                }
                self.create_source(source_id, name, kind, settings, duplicated_from)?
            }
            Edit::SourceRename { source_id, name } => {
                check_name(&name, at)?;
                let old = std::mem::replace(&mut self.existing_source(&source_id)?.name, name);
                vec![Edit::SourceRename {
                    source_id,
                    name: old,
                }]
            }
            Edit::SourceSetSettings { source_id, patch } => {
                // No descriptor to reset from: `null` removes the entry, and the inverse
                // sets each patched entry back, or `null` for one that wasn't there.
                let source = self.existing_source(&source_id)?;
                let mut old = Settings::new();
                for (id, value) in patch.0 {
                    let before = if value.is_null() {
                        source.settings.0.remove(&id)
                    } else {
                        source.settings.0.insert(id.clone(), value)
                    };
                    old.0.insert(id, before.unwrap_or_default());
                }
                vec![Edit::SourceSetSettings {
                    source_id,
                    patch: old,
                }]
            }
            Edit::SourceDuplicate {
                source_id,
                new_source_id,
                name,
            } => {
                let Some((kind, settings, filters, meta)) = self.sources.get(&source_id).map(|s| {
                    (
                        s.kind.clone(),
                        s.settings.clone(),
                        s.filters.clone(),
                        s.meta.clone(),
                    )
                }) else {
                    return Err(not_found(EntityKind::Source, source_id.as_str()));
                };
                check_id(new_source_id.as_str(), at, "new_source_id")?;
                check_name(&name, at)?;
                let inverse = self.create_source(
                    new_source_id.clone(),
                    name,
                    kind,
                    settings,
                    Some(source_id.clone()),
                )?;
                // A duplicate keeps its original's filter IDs (ADR-0026 §2), labels and
                // favourite flag (ADR-0024 §4).
                if let Some(copy) = self.sources.get_mut(&new_source_id) {
                    copy.filters = filters;
                    copy.meta = meta;
                }
                self.audio_copied(&source_id, &new_source_id);
                inverse
            }
            Edit::SourceDelete { source_id } => {
                if !self.sources.contains_key(&source_id) {
                    return Err(not_found(EntityKind::Source, source_id.as_str()));
                }
                if self.items.values().any(|item| item.source_id == source_id) {
                    return Err(invalid(
                        at,
                        "source_id",
                        format!("source `{source_id}` is placed by items; remove them first"),
                    ));
                }
                if self.is_referred_to(
                    |r| matches!(r, Reference::Source { source_id: s } if *s == source_id),
                ) {
                    return Err(invalid(
                        at,
                        "source_id",
                        format!(
                            "source `{source_id}` is referred to by settings; clear them first"
                        ),
                    ));
                }
                let Some(source) = self.sources.remove(&source_id) else {
                    return Err(not_found(EntityKind::Source, source_id.as_str()));
                };
                let restore_audio = self.audio_deleted(&source_id, &source.kind, source.audio);
                let mut inverse = vec![Edit::SourceCreate {
                    source_id: source_id.clone(),
                    name: source.name,
                    kind: source.kind,
                    settings: source.settings,
                    duplicated_from: source.duplicated_from,
                }];
                inverse.extend(restore_audio);
                let owner = jstream_protocol::FilterOwner::Source {
                    source_id: source_id.clone(),
                };
                inverse.extend(filter::restore_all(&owner, source.filters));
                if !source.meta.is_default() {
                    inverse.push(Edit::SourceSetMeta {
                        source_id,
                        meta: source.meta,
                    });
                }
                inverse
            }
            Edit::OutputCreate { output_id, kind } => {
                check_id(output_id.as_str(), at, "output_id")?;
                if self.outputs.contains_key(&output_id) {
                    return Err(already_exists(EntityKind::Output, output_id.as_str()));
                }
                match &kind {
                    OutputKind::Rtmp { target } => check_target(target, at, "kind.target")?,
                    OutputKind::Record { path, container } => check_record(path, *container, at)?,
                    OutputKind::VirtualCam => {}
                    OutputKind::Unknown => {
                        return Err(invalid(
                            at,
                            "kind.type",
                            "is not an output kind this engine knows",
                        ));
                    }
                    _ => return Err(unimplemented_by_mock("this output kind")),
                }
                self.outputs.insert(output_id.clone(), kind);
                vec![Edit::OutputDelete { output_id }]
            }
            Edit::OutputSetTarget { output_id, target } => {
                let Some(kind) = self.outputs.get_mut(&output_id) else {
                    return Err(not_found(EntityKind::Output, output_id.as_str()));
                };
                check_target(&target, at, "target")?;
                let OutputKind::Rtmp { target: current } = kind else {
                    return Err(invalid(at, "output_id", "the output is not an RTMP output"));
                };
                let old = std::mem::replace(current, target);
                vec![Edit::OutputSetTarget {
                    output_id,
                    target: old,
                }]
            }
            Edit::OutputDelete { output_id } => {
                if running.contains(&output_id) {
                    return Err(invalid(
                        at,
                        "output_id",
                        format!("output `{output_id}` is running; stop it first"),
                    ));
                }
                let Some(kind) = self.outputs.remove(&output_id) else {
                    return Err(not_found(EntityKind::Output, output_id.as_str()));
                };
                let mut inverse = vec![Edit::OutputCreate {
                    output_id: output_id.clone(),
                    kind,
                }];
                if let Some(profile_id) = self.output_encoders.remove(&output_id) {
                    inverse.push(Edit::OutputSetEncoder {
                        output_id: output_id.clone(),
                        profile_id: Some(profile_id),
                    });
                }
                if let Some(mix_id) = self.output_mixes.remove(&output_id) {
                    inverse.push(Edit::OutputSetVideoMix {
                        output_id: output_id.clone(),
                        mix_id: Some(mix_id),
                    });
                }
                if let Some(filters) = self.output_filters.remove(&output_id) {
                    let owner = jstream_protocol::FilterOwner::Output { output_id };
                    inverse.extend(filter::restore_all(&owner, filters));
                }
                inverse
            }
            Edit::OutputSetEncoder {
                output_id,
                profile_id,
            } => {
                if !self.outputs.contains_key(&output_id) {
                    return Err(not_found(EntityKind::Output, output_id.as_str()));
                }
                let old = match profile_id {
                    Some(id) => {
                        if !self.encoders.contains_key(&id) {
                            return Err(not_found(EntityKind::Encoder, id.as_str()));
                        }
                        self.output_encoders.insert(output_id.clone(), id)
                    }
                    None => self.output_encoders.remove(&output_id),
                };
                vec![Edit::OutputSetEncoder {
                    output_id,
                    profile_id: old,
                }]
            }
            Edit::EncoderCreate {
                profile_id,
                name,
                profile,
            } => {
                check_id(profile_id.as_str(), at, "profile_id")?;
                if self.encoders.contains_key(&profile_id) {
                    return Err(already_exists(EntityKind::Encoder, profile_id.as_str()));
                }
                check_name(&name, at)?;
                check_profile(&profile, at)?;
                self.encoders.insert(profile_id.clone(), (name, profile));
                vec![Edit::EncoderDelete { profile_id }]
            }
            Edit::EncoderUpdate {
                profile_id,
                name,
                profile,
            } => {
                if !self.encoders.contains_key(&profile_id) {
                    return Err(not_found(EntityKind::Encoder, profile_id.as_str()));
                }
                check_name(&name, at)?;
                check_profile(&profile, at)?;
                let Some((old_name, old_profile)) =
                    self.encoders.insert(profile_id.clone(), (name, profile))
                else {
                    return Err(not_found(EntityKind::Encoder, profile_id.as_str()));
                };
                vec![Edit::EncoderUpdate {
                    profile_id,
                    name: old_name,
                    profile: old_profile,
                }]
            }
            Edit::EncoderDelete { profile_id } => {
                if !self.encoders.contains_key(&profile_id) {
                    return Err(not_found(EntityKind::Encoder, profile_id.as_str()));
                }
                if let Some((user, _)) = self
                    .output_encoders
                    .iter()
                    .find(|(_, id)| **id == profile_id)
                {
                    return Err(invalid(
                        at,
                        "profile_id",
                        format!(
                            "encoder profile `{profile_id}` is used by output `{user}`; point it elsewhere first"
                        ),
                    ));
                }
                let Some((name, profile)) = self.encoders.remove(&profile_id) else {
                    return Err(not_found(EntityKind::Encoder, profile_id.as_str()));
                };
                vec![Edit::EncoderCreate {
                    profile_id,
                    name,
                    profile,
                }]
            }
            Edit::VideoMixCreate { mix_id, name, mix } => {
                check_id(mix_id.as_str(), at, "mix_id")?;
                if self.video_mixes.contains_key(&mix_id) {
                    return Err(already_exists(EntityKind::VideoMix, mix_id.as_str()));
                }
                check_name(&name, at)?;
                self.check_mix(&mix, at)?;
                self.video_mixes.insert(mix_id.clone(), (name, mix));
                vec![Edit::VideoMixDelete { mix_id }]
            }
            Edit::VideoMixUpdate { mix_id, name, mix } => {
                if !self.video_mixes.contains_key(&mix_id) {
                    return Err(not_found(EntityKind::VideoMix, mix_id.as_str()));
                }
                check_name(&name, at)?;
                self.check_mix(&mix, at)?;
                let Some((old_name, old_mix)) =
                    self.video_mixes.insert(mix_id.clone(), (name, mix))
                else {
                    return Err(not_found(EntityKind::VideoMix, mix_id.as_str()));
                };
                vec![Edit::VideoMixUpdate {
                    mix_id,
                    name: old_name,
                    mix: old_mix,
                }]
            }
            Edit::VideoMixDelete { mix_id } => {
                if !self.video_mixes.contains_key(&mix_id) {
                    return Err(not_found(EntityKind::VideoMix, mix_id.as_str()));
                }
                if let Some((user, _)) = self.output_mixes.iter().find(|(_, id)| **id == mix_id) {
                    return Err(invalid(
                        at,
                        "mix_id",
                        format!(
                            "video mix `{mix_id}` is used by output `{user}`; point it elsewhere first"
                        ),
                    ));
                }
                let Some((name, mix)) = self.video_mixes.remove(&mix_id) else {
                    return Err(not_found(EntityKind::VideoMix, mix_id.as_str()));
                };
                vec![Edit::VideoMixCreate { mix_id, name, mix }]
            }
            Edit::OutputSetVideoMix { output_id, mix_id } => {
                if !self.outputs.contains_key(&output_id) {
                    return Err(not_found(EntityKind::Output, output_id.as_str()));
                }
                let old = match mix_id {
                    Some(id) => {
                        if !self.video_mixes.contains_key(&id) {
                            return Err(not_found(EntityKind::VideoMix, id.as_str()));
                        }
                        self.output_mixes.insert(output_id.clone(), id)
                    }
                    None => self.output_mixes.remove(&output_id),
                };
                vec![Edit::OutputSetVideoMix {
                    output_id,
                    mix_id: old,
                }]
            }
            edit @ (Edit::FilterAdd { .. }
            | Edit::FilterRemove { .. }
            | Edit::FilterSetEnabled { .. }
            | Edit::FilterSetSettings { .. }
            | Edit::FilterReorder { .. }) => self.apply_filter(edit, at)?,
            // The graph's checks, in its order: the chord, whether it is bound, whether
            // the action can be bound, then the objects it names.
            Edit::HotkeySet { chord, action } => {
                if let Some(reason) = chord.problem() {
                    return Err(invalid(at, "chord", reason));
                }
                if self.hotkeys.contains_key(&chord) {
                    return Err(already_exists(EntityKind::Hotkey, chord.as_str()));
                }
                if let Some((field, reason)) = action.bind_problem() {
                    return Err(invalid(at, &format!("action.{field}"), reason));
                }
                for (entity, id) in action.named_objects() {
                    let exists = match entity {
                        EntityKind::Scene => self.scenes.iter().any(|s| s.id.as_str() == id),
                        EntityKind::Source => self.sources.keys().any(|s| s.as_str() == id),
                        EntityKind::Output => self.outputs.keys().any(|o| o.as_str() == id),
                        EntityKind::Encoder => self.encoders.keys().any(|p| p.as_str() == id),
                        // The mock has only the `cut` preset every collection starts with.
                        EntityKind::Transition => id == INITIAL_PRESET,
                        _ => false,
                    };
                    if !exists {
                        return Err(not_found(entity, id));
                    }
                }
                self.hotkeys.insert(chord.clone(), action);
                vec![Edit::HotkeyClear { chord }]
            }
            Edit::HotkeyClear { chord } => {
                let Some(action) = self.hotkeys.remove(&chord) else {
                    return Err(not_found(EntityKind::Hotkey, chord.as_str()));
                };
                vec![Edit::HotkeySet { chord, action }]
            }
            other => return Err(unimplemented_by_mock(other.name())),
        };
        Ok(inverse)
    }

    /// Checks a video mix as the engine does: its own ranges, then that the scene it
    /// shows exists.
    fn check_mix(&self, mix: &VideoMix, at: usize) -> Result<(), ProtocolError> {
        if let Some((name, reason)) = mix.problem() {
            return Err(invalid(at, &format!("mix.{name}"), reason));
        }
        if let Some(scene_id) = mix.scene()
            && !self.scenes.iter().any(|scene| &scene.id == scene_id)
        {
            return Err(not_found(EntityKind::Scene, scene_id.as_str()));
        }
        Ok(())
    }

    /// Removes the hotkey bindings whose action names `entity` `id`, which a delete has
    /// just removed, records them in `removed`, and returns the `hotkey.set` edits that
    /// restore them, ordered by chord.
    fn remove_hotkeys_naming(
        &mut self,
        entity: EntityKind,
        id: &str,
        removed: &mut Vec<HotkeyBinding>,
    ) -> Vec<Edit> {
        let chords: Vec<Chord> = self
            .hotkeys
            .iter()
            .filter(|(_, action)| action.named_objects().contains(&(entity, id)))
            .map(|(chord, _)| chord.clone())
            .collect();
        chords
            .into_iter()
            .filter_map(|chord| {
                let action = self.hotkeys.remove(&chord)?;
                removed.push(HotkeyBinding::new(chord.clone(), &action));
                Some(Edit::HotkeySet { chord, action })
            })
            .collect()
    }

    /// Applies `item.move` with `jstream-graph`'s checks in its order: the item, the
    /// scene, the index (one past the top is allowed in another scene), then the cycle.
    /// Returns its inverse: `item.move` back to its previous scene and index.
    fn move_item(
        &mut self,
        item_id: ItemId,
        scene_id: SceneId,
        index: u32,
        at: usize,
    ) -> Result<Vec<Edit>, ProtocolError> {
        let (from_scene_id, source_id) = {
            let item = self.existing_item(&item_id)?;
            (item.scene_id.clone(), item.source_id.clone())
        };
        let from = self.existing_scene(&from_scene_id)?;
        let to = self.existing_scene(&scene_id)?;
        let slots = self.scenes[to].items.len() + usize::from(to != from);
        let to_index = check_index(index, slots, at, "positions in the scene")?;
        if let Some(cycle) = self.nesting_cycle(&scene_id, &source_id) {
            return Err(invalid(
                at,
                "scene_id",
                format!(
                    "moving item `{item_id}` (source `{source_id}`) into scene `{scene_id}` \
                     would create a cycle: {cycle}"
                ),
            ));
        }
        let from_index = self.scenes[from]
            .items
            .iter()
            .position(|id| *id == item_id)
            .unwrap_or_default();
        self.scenes[from].items.remove(from_index);
        self.scenes[to].items.insert(to_index, item_id.clone());
        if let Some(item) = self.items.get_mut(&item_id) {
            item.scene_id = scene_id;
        }
        Ok(vec![Edit::ItemMove {
            item_id,
            scene_id: from_scene_id,
            index: wire_index(from_index)?,
        }])
    }

    /// Creates a source and returns the edit that undoes it.
    fn create_source(
        &mut self,
        id: SourceId,
        name: String,
        kind: SourceKind,
        settings: Settings,
        duplicated_from: Option<SourceId>,
    ) -> Result<Vec<Edit>, ProtocolError> {
        if self.sources.contains_key(&id) {
            return Err(already_exists(EntityKind::Source, id.as_str()));
        }
        let audio = self.default_audio(&kind);
        self.sources.insert(
            id.clone(),
            Source {
                name,
                kind,
                settings,
                duplicated_from,
                filters: jstream_protocol::FilterChains::default(),
                audio,
                meta: Default::default(),
            },
        );
        self.audio_created(&id);
        Ok(vec![Edit::SourceDelete { source_id: id }])
    }

    fn existing_source(&mut self, source_id: &SourceId) -> Result<&mut Source, ProtocolError> {
        self.sources
            .get_mut(source_id)
            .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str()))
    }

    /// Whether any source's settings, or its filters' settings, hold a reference
    /// matching `is_target`.
    fn is_referred_to(&self, is_target: impl Fn(&Reference) -> bool) -> bool {
        self.sources.values().any(|source| {
            let filters = source.filters.video.iter().chain(&source.filters.audio);
            std::iter::once(&source.settings)
                .chain(filters.map(|f| &f.settings))
                .any(|settings| settings.references().any(|(_, r)| is_target(&r)))
        })
    }

    /// Every source, ordered by ID, as queries report them.
    fn source_infos(&self) -> Vec<SourceInfo> {
        self.sources
            .iter()
            .map(|(id, source)| SourceInfo {
                source_id: id.clone(),
                name: source.name.clone(),
                kind: source.kind.clone(),
                settings: source.settings.clone(),
                duplicated_from: source
                    .duplicated_from
                    .clone()
                    .filter(|original| self.sources.contains_key(original)),
                filters: source.filters.clone(),
                audio: source.audio,
                meta: source.meta.clone(),
            })
            .collect()
    }

    /// Every configured output, ordered by ID, as queries report them.
    fn output_infos(&self) -> Vec<OutputInfo> {
        self.outputs
            .iter()
            .map(|(id, kind)| OutputInfo {
                output_id: id.clone(),
                kind: kind.clone(),
                encoder_profile: self.output_encoders.get(id).cloned(),
                filters: self.output_filters.get(id).cloned().unwrap_or_default(),
                video_mix: self.output_mixes.get(id).cloned(),
            })
            .collect()
    }

    /// Every video mix, ordered by ID, as queries report them.
    fn video_mix_infos(&self) -> Vec<VideoMixInfo> {
        self.video_mixes
            .iter()
            .map(|(id, (name, mix))| VideoMixInfo {
                mix_id: id.clone(),
                name: name.clone(),
                mix: mix.clone(),
            })
            .collect()
    }

    /// Every encoder profile, ordered by ID, as queries report them.
    fn encoder_infos(&self) -> Vec<EncoderProfileInfo> {
        self.encoders
            .iter()
            .map(|(id, (name, profile))| EncoderProfileInfo {
                profile_id: id.clone(),
                name: name.clone(),
                profile: *profile,
            })
            .collect()
    }

    /// `scene`'s items, bottom first, as queries report them.
    fn item_infos(&self, scene: &Scene) -> Vec<ItemInfo> {
        scene
            .items
            .iter()
            .filter_map(|id| {
                self.items.get(id).map(|item| ItemInfo {
                    item_id: id.clone(),
                    source_id: item.source_id.clone(),
                    placement: if item.geometry.is_some() {
                        PlacementForm::Geometry
                    } else {
                        PlacementForm::Legacy
                    },
                    transform: item.transform,
                    geometry: item.geometry,
                    crop: item.crop,
                    visible: item.visible,
                    opacity: item.opacity,
                    blend: item.blend,
                    transition: ItemTransition::default(),
                })
            })
            .collect()
    }

    /// Rejects placing `source_id` in scene `scene_id` (`item.add`, `item.set_source`)
    /// if the scene would then contain itself, naming the cycle as `jstream-graph` does.
    fn check_nesting(
        &self,
        scene_id: &SceneId,
        source_id: &SourceId,
        at: usize,
    ) -> Result<(), ProtocolError> {
        match self.nesting_cycle(scene_id, source_id) {
            Some(cycle) => Err(invalid(
                at,
                "source_id",
                format!(
                    "placing source `{source_id}` in scene `{scene_id}` would create a cycle: {cycle}"
                ),
            )),
            None => Ok(()),
        }
    }

    /// If placing `source_id` in scene `scene_id` would make the scene contain itself,
    /// the cycle as `jstream-graph` names it (`scene `a` -> source `sb` -> scene `b` ->
    /// source `sa` -> scene `a``), else `None`. Like `jstream-graph`, it takes the first
    /// path a depth-first walk finds, items in stacking order; it follows only `scene`
    /// sources, not settings references.
    fn nesting_cycle(&self, scene_id: &SceneId, source_id: &SourceId) -> Option<String> {
        let Some(Source {
            kind: SourceKind::Scene { scene_id: nested },
            ..
        }) = self.sources.get(source_id)
        else {
            return None;
        };
        let mut path = vec![
            format!("scene `{scene_id}`"),
            format!("source `{source_id}`"),
        ];
        self.scene_path(nested, scene_id, &mut path)
            .then(|| path.join(" -> "))
    }

    /// Pushes the chain from scene `outer` to scene `inner` (both included) onto `path`
    /// if `outer` is `inner` or shows it, directly or through nested scene sources. The
    /// collection never holds a cycle, so the walk terminates.
    fn scene_path(&self, outer: &SceneId, inner: &SceneId, path: &mut Vec<String>) -> bool {
        path.push(format!("scene `{outer}`"));
        if outer == inner {
            return true;
        }
        if let Some(index) = self.scene_index(outer) {
            for item_id in &self.scenes[index].items {
                let Some(item) = self.items.get(item_id) else {
                    continue;
                };
                if let Some(Source {
                    kind: SourceKind::Scene { scene_id },
                    ..
                }) = self.sources.get(&item.source_id)
                {
                    path.push(format!("source `{}`", item.source_id));
                    if self.scene_path(scene_id, inner, path) {
                        return true;
                    }
                    path.pop();
                }
            }
        }
        path.pop();
        false
    }

    fn scene_index(&self, scene_id: &SceneId) -> Option<usize> {
        self.scenes.iter().position(|scene| scene.id == *scene_id)
    }

    fn existing_scene(&self, scene_id: &SceneId) -> Result<usize, ProtocolError> {
        self.scene_index(scene_id)
            .ok_or_else(|| not_found(EntityKind::Scene, scene_id.as_str()))
    }

    fn existing_item(&mut self, item_id: &ItemId) -> Result<&mut Item, ProtocolError> {
        self.items
            .get_mut(item_id)
            .ok_or_else(|| not_found(EntityKind::Item, item_id.as_str()))
    }

    /// The item a placement edit changes, with its optional `scene_id` checked as
    /// `jstream-graph` checks it: only the item's own scene, until keyframe scenes exist.
    fn placement(
        &mut self,
        item_id: &ItemId,
        scene_id: Option<&SceneId>,
        at: usize,
    ) -> Result<&mut Item, ProtocolError> {
        let own = self.existing_item(item_id)?.scene_id.clone();
        if let Some(scene_id) = scene_id
            && *scene_id != own
        {
            self.existing_scene(scene_id)?;
            return Err(invalid(
                at,
                "scene_id",
                format!(
                    "item `{item_id}` is in scene `{own}`, and only its own scene can be named \
                     until keyframe scenes exist"
                ),
            ));
        }
        self.existing_item(item_id)
    }
}

/// The largest canvas side the mock accepts, the same as the engine's.
const MAX_CANVAS_EDGE: u32 = 8192;

fn check_canvas_side(value: u32, at: usize, name: &str) -> Result<(), ProtocolError> {
    if value == 0 || value > MAX_CANVAS_EDGE {
        return Err(invalid(
            at,
            name,
            format!("must be from 1 to {MAX_CANVAS_EDGE}"),
        ));
    }
    Ok(())
}

/// Checks a geometry as the engine does (a positive size and perspective, neutral 3D,
/// finite corners) and returns the deprecated transform view `ItemInfo` reports.
fn geometry_view(geometry: &ItemGeometry, at: usize) -> Result<ItemTransform, ProtocolError> {
    if let Err(err) = geometry.validate() {
        let name = match err {
            GeometryError::NonPositiveSize => "geometry.size",
            GeometryError::NonPositivePerspective => "geometry.perspective_px",
            _ => "geometry",
        };
        return Err(invalid(at, name, err.to_string()));
    }
    let non_neutral = [
        ("geometry.tilt_deg", geometry.tilt_deg.get() != 0.0),
        ("geometry.turn_deg", geometry.turn_deg.get() != 0.0),
        ("geometry.depth_px", geometry.depth_px.get() != 0.0),
        ("geometry.perspective_px", geometry.perspective_px.is_some()),
    ];
    if let Some((name, _)) = non_neutral.iter().find(|(_, set)| *set) {
        return Err(invalid(
            at,
            name,
            "tilt, turn, depth and perspective are not drawn yet, so they must be neutral",
        ));
    }
    geometry
        .project()
        .and_then(|_| geometry.transform_view())
        .map_err(|err| invalid(at, "geometry", err.to_string()))
}

fn invalid(at: usize, field: &str, reason: impl Into<String>) -> ProtocolError {
    ProtocolError::invalid_argument(format!("edits.{at}.{field}"), reason)
}

/// Rejects an empty ID in a creating edit, as `jstream-graph` does.
fn check_id(id: &str, at: usize, field: &str) -> Result<(), ProtocolError> {
    if id.is_empty() {
        return Err(invalid(at, field, "must not be empty"));
    }
    Ok(())
}

fn check_name(name: &str, at: usize) -> Result<(), ProtocolError> {
    if name.trim().is_empty() {
        return Err(invalid(at, "name", "must not be empty"));
    }
    Ok(())
}

/// Converts a wire index into a position below `len`.
fn check_index(index: u32, len: usize, at: usize, what: &str) -> Result<usize, ProtocolError> {
    match usize::try_from(index) {
        Ok(i) if i < len => Ok(i),
        _ => Err(invalid(
            at,
            "index",
            format!("must be less than the number of {what} ({len})"),
        )),
    }
}

/// A position as the protocol's `u32` index.
fn wire_index(index: usize) -> Result<u32, ProtocolError> {
    u32::try_from(index).map_err(|_| ProtocolError::Internal {
        reason: format!("position {index} does not fit the protocol's u32 index"),
    })
}

/// The edit that puts `item`'s current placement back on `item_id`, in its own form and
/// addressed by the same `scene_id` as the edit it undoes, as `jstream-graph` builds it.
fn set_placement(item_id: &ItemId, scene_id: &Option<SceneId>, item: &Item) -> Edit {
    match item.geometry {
        Some(geometry) => Edit::ItemSetGeometry {
            item_id: item_id.clone(),
            scene_id: scene_id.clone(),
            geometry,
        },
        None => Edit::ItemSetTransform {
            item_id: item_id.clone(),
            scene_id: scene_id.clone(),
            transform: item.transform,
        },
    }
}

/// The edits that put a removed `item` back in its scene at stacking position `index`,
/// given that `item.add` would place it at `top`, as `jstream-graph` builds them.
fn restore_item(
    item_id: &ItemId,
    item: &Item,
    index: usize,
    top: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    // In the form it was placed in: a geometry, or the legacy transform.
    let (transform, geometry) = match item.geometry {
        Some(geometry) => (None, Some(geometry)),
        None => (Some(item.transform), None),
    };
    let mut edits = vec![Edit::ItemAdd {
        scene_id: item.scene_id.clone(),
        item_id: item_id.clone(),
        source_id: item.source_id.clone(),
        transform,
        geometry,
        opacity: (item.opacity != Finite32::ONE).then_some(item.opacity),
        blend: (!item.blend.is_normal()).then_some(item.blend),
    }];
    if index != top {
        edits.push(Edit::ItemReorder {
            item_id: item_id.clone(),
            index: wire_index(index)?,
        });
    }
    if item.crop != Crop::NONE {
        edits.push(Edit::ItemSetCrop {
            item_id: item_id.clone(),
            scene_id: None,
            crop: item.crop,
        });
    }
    if !item.visible {
        edits.push(Edit::ItemSetVisible {
            item_id: item_id.clone(),
            scene_id: None,
            visible: false,
        });
    }
    Ok(edits)
}

/// Rejects an opacity outside [0, 1], as `jstream-graph` does.
fn check_opacity(opacity: Finite32, at: usize) -> Result<(), ProtocolError> {
    if !(0.0..=1.0).contains(&opacity.get()) {
        return Err(invalid(at, "opacity", "must be from 0 to 1"));
    }
    Ok(())
}

/// Rejects every blend mode but `normal`, which is all the engine draws, as
/// `jstream-graph` does.
fn check_blend(blend: BlendMode, at: usize) -> Result<(), ProtocolError> {
    if !blend.is_normal() {
        return Err(invalid(
            at,
            "blend",
            format!(
                "`{}` is not drawn by the compositor yet; only `normal` is",
                blend.as_str()
            ),
        ));
    }
    Ok(())
}

/// Whether an output in `state` is not running: `stopped` or `failed`.
fn is_idle(state: OutputState) -> bool {
    matches!(state, OutputState::Stopped | OutputState::Failed)
}

/// The longest stream key `output.set_key` accepts, in bytes, as in the engine.
const MAX_KEY_LEN: usize = 1024;

/// Checks a stream key's shape as the engine does. The reasons never repeat the key.
fn check_key(key: &SecretString) -> Result<(), ProtocolError> {
    let key = key.expose();
    let reason = if key.is_empty() {
        "must not be empty"
    } else if key.len() > MAX_KEY_LEN {
        "must be at most 1024 bytes"
    } else if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        "must not contain whitespace or control characters"
    } else if !key.is_ascii() {
        // No streaming service issues one; a leading U+FEFF is the usual case (#441).
        "must contain only ASCII characters"
    } else {
        return Ok(());
    };
    Err(ProtocolError::invalid_argument("key", reason))
}

/// Checks an RTMP target; `name` is its field path within the edit (`kind.target`).
/// Checks an encoder profile as the engine does.
fn check_profile(profile: &EncoderProfile, at: usize) -> Result<(), ProtocolError> {
    match profile.problem() {
        Some((name, reason)) => Err(invalid(at, &format!("profile.{name}"), reason)),
        None => Ok(()),
    }
}

fn check_target(target: &RtmpTarget, at: usize, name: &str) -> Result<(), ProtocolError> {
    match target {
        RtmpTarget::Generic { url } => match rtmp_url_problem(url) {
            Some(problem) => Err(invalid(at, &format!("{name}.url"), problem)),
            None => Ok(()),
        },
        RtmpTarget::Unknown => Err(invalid(
            at,
            &format!("{name}.type"),
            "is not an RTMP target this engine knows",
        )),
        _ => Err(unimplemented_by_mock("this RTMP target")),
    }
}

/// Checks a recording output's container and path, as `jstream-graph` does.
fn check_record(path: &str, container: RecordContainer, at: usize) -> Result<(), ProtocolError> {
    match container {
        RecordContainer::Mp4 => {}
        RecordContainer::Unknown => {
            return Err(invalid(
                at,
                "kind.container",
                "is not a container this engine knows",
            ));
        }
        _ => return Err(unimplemented_by_mock("this container")),
    }
    match record_path_problem(path) {
        Some(problem) => Err(invalid(at, "kind.path", problem)),
        None => Ok(()),
    }
}

/// The longest recording path accepted, in bytes, as in `jstream-graph`.
const MAX_PATH_LEN: usize = 4096;

/// Why `path` is not an acceptable recording path, or `None` when it is: the checks and
/// fixed reasons of `jstream-graph`'s path validation, in the same order. The rule is
/// the same on every platform: `/…`, `\…`, `C:\…` or `C:/…`, not two leading
/// separators (a UNC or device path), no `:` but a drive letter's, no `?` or `*`, no
/// `..` segment, and a file name last that is not a DOS device name.
fn record_path_problem(path: &str) -> Option<&'static str> {
    if path.is_empty() {
        return Some("must not be empty");
    }
    if path.len() > MAX_PATH_LEN {
        return Some("must be at most 4096 bytes");
    }
    if path.chars().any(char::is_control) {
        return Some("must not contain control characters");
    }
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    if !(path.starts_with(['/', '\\']) || drive) {
        return Some("must be absolute: `/…`, `\\…`, `C:\\…` or `C:/…`");
    }
    let rest = if drive { &path[2..] } else { path };
    if rest[1..].starts_with(['/', '\\']) {
        return Some("must not start with two separators");
    }
    if rest.contains(':') {
        return Some("must not contain `:` but a drive letter's");
    }
    if rest.contains(['?', '*']) {
        return Some("must not contain `?` or `*`");
    }
    let mut segments = path.split(['/', '\\']);
    if segments.clone().any(|segment| segment == "..") {
        return Some("must not contain a `..` segment");
    }
    match segments.next_back() {
        Some("" | ".") | None => Some("must end in a file name"),
        Some(name) if is_dos_device_name(name) => Some("must not end in a DOS device name"),
        Some(_) => None,
    }
}

/// Whether Win32 opens the path component `name` as a DOS device, as in `jstream-graph`:
/// a stem (the name before its first `.`, without trailing spaces, in any ASCII case) of
/// `CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`, `COM¹`–`COM³`, `LPT0`–`LPT9`,
/// `LPT¹`–`LPT³`, `CONIN$` or `CONOUT$`.
fn is_dos_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => ["COM", "LPT"].iter().any(|port| {
            upper.strip_prefix(port).is_some_and(|n| {
                matches!(n.as_bytes(), [b'0'..=b'9']) || matches!(n, "\u{b9}" | "\u{b2}" | "\u{b3}")
            })
        }),
    }
}

/// The longest RTMP server URL accepted, in bytes, as in `jstream-graph`.
const MAX_URL_LEN: usize = 2048;

/// Why `url` is not an acceptable RTMP(S) server URL, or `None` when it is: the checks
/// and fixed reasons of `jstream-graph`'s target validation, in the same order. The
/// reasons never repeat the URL, which might hold a key.
fn rtmp_url_problem(url: &str) -> Option<&'static str> {
    if url.is_empty() {
        return Some("must not be empty");
    }
    if url.len() > MAX_URL_LEN {
        return Some("must be at most 2048 bytes");
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Some("must not contain whitespace or control characters");
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return Some("must start with `rtmp://` or `rtmps://`");
    };
    if !(scheme.eq_ignore_ascii_case("rtmp") || scheme.eq_ignore_ascii_case("rtmps")) {
        return Some("the scheme must be `rtmp` or `rtmps`");
    }
    if rest.contains(['?', '#']) {
        return Some("must not have a query or fragment: set the stream key with `output.set_key`");
    }
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    if authority.contains('@') {
        return Some(
            "must not carry credentials (`user:password@`): set the stream key with \
             `output.set_key`",
        );
    }
    if let Some(problem) = host_port_problem(authority) {
        return Some(problem);
    }
    if path.trim_matches('/').is_empty() {
        return Some("is missing the application name (the path after the host)");
    }
    None
}

/// Checks `host[:port]` or `[v6]:port`.
fn host_port_problem(authority: &str) -> Option<&'static str> {
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let Some((host, after)) = v6.split_once(']') else {
            return Some("has an unclosed `[` in its IPv6 host");
        };
        match after {
            "" => (host, None),
            _ => match after.strip_prefix(':') {
                Some(port) => (host, Some(port)),
                None => return Some("has unexpected text after its IPv6 host"),
            },
        }
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    if host.is_empty() {
        return Some("is missing the host");
    }
    match port.map(str::parse::<u16>) {
        None | Some(Ok(1..)) => None,
        Some(_) => Some("has a port that is not a number in 1..=65535"),
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
        Action, Batch, BlendMode, BusId, BusInfo, Call, CallResult, Canvas, ClientId, Crop, Edit,
        EncoderProfile, EncoderProfileId, EntityKind, Event, Finite32, Hello, HistoryCall,
        HistoryDirection, HistoryOp, IdempotencyKey, ItemId, ItemTransform, MixInput,
        MixResolution, OutputId, OutputInfo, OutputKind, OutputState, PROTOCOL_VERSION,
        ProtocolError, ProtocolVersion, Query, QueryResult, Rev, RtmpTarget, SceneId, SceneRole,
        SecretString, Settings, SourceId, SourceKind, TransitionsInfo, VideoMix, VideoMixId,
        parity,
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
            role: None,
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
                webcam(),
                Edit::ItemAdd {
                    scene_id: SceneId::new("main"),
                    item_id: ItemId::new("cam"),
                    source_id: SourceId::new("webcam"),
                    transform: None,
                    geometry: None,
                    opacity: None,
                    blend: None,
                },
                Edit::SceneRename {
                    scene_id: SceneId::new("intro"),
                    name: "Starting soon".into(),
                },
            ],
            expected_rev: Some(Rev(0)),
            idempotency_key: None,
            ends_gesture: None,
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
            geometry: None,
            opacity: None,
            blend: None,
        });
        assert_eq!(batch(&*control, readd), Ok(Rev(3)));
    }

    #[test]
    fn protocol_0_2_edits_follow_the_graph_rules() {
        let engine = MockEngine::new();
        let control = connected(&engine);
        let setup = Batch {
            edits: vec![
                create("a"),
                create("b"),
                webcam(),
                Edit::SourceCreate {
                    source_id: SourceId::new("nested_b"),
                    name: "B".into(),
                    kind: SourceKind::Scene {
                        scene_id: SceneId::new("b"),
                    },
                    settings: Default::default(),
                    duplicated_from: None,
                },
                add("a", "i1", "webcam"),
                add("a", "i2", "nested_b"),
                Edit::ItemSetVisible {
                    item_id: ItemId::new("i1"),
                    scene_id: None,
                    visible: false,
                },
                Edit::ItemReorder {
                    item_id: ItemId::new("i2"),
                    index: 0,
                },
                Edit::SceneReorder {
                    scene_id: SceneId::new("b"),
                    index: 0,
                },
                Edit::SourceDuplicate {
                    source_id: SourceId::new("webcam"),
                    new_source_id: SourceId::new("webcam_2"),
                    name: "Webcam 2".into(),
                },
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        };
        assert_eq!(batch(&*control, setup), Ok(Rev(1)));
        assert_eq!(
            scene_list(&*control),
            (Rev(1), vec![("b".into(), 0), ("a".into(), 2)])
        );

        let invalid_field = |edit: Edit| match batch(&*control, Batch::single(edit)) {
            Err(ControlError::Protocol(wire)) => match wire.error {
                ProtocolError::InvalidArgument { field, .. } => field,
                other => panic!("expected invalid_argument, got {other:?}"),
            },
            other => panic!("expected an error, got {other:?}"),
        };
        // Scene `a` shows `b`, so placing `a` inside `b` would be a cycle.
        let cycle = Batch {
            edits: vec![
                Edit::SourceCreate {
                    source_id: SourceId::new("nested_a"),
                    name: "A".into(),
                    kind: SourceKind::Scene {
                        scene_id: SceneId::new("a"),
                    },
                    settings: Default::default(),
                    duplicated_from: None,
                },
                add("b", "i3", "nested_a"),
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        };
        let error = batch(&*control, cycle).expect_err("cycle");
        assert!(matches!(
            error.protocol(),
            Some(ProtocolError::InvalidArgument { field, .. }) if field == "edits.1.source_id"
        ));
        let in_use = Edit::SourceDelete {
            source_id: SourceId::new("webcam"),
        };
        assert_eq!(invalid_field(in_use), "edits.0.source_id");
        let shown = Edit::SceneDelete {
            scene_id: SceneId::new("b"),
        };
        assert_eq!(invalid_field(shown), "edits.0.scene_id");
        let out_of_range = Edit::ItemReorder {
            item_id: ItemId::new("i1"),
            index: 2,
        };
        assert_eq!(invalid_field(out_of_range), "edits.0.index");
        let missing = batch(&*control, Batch::single(add("a", "i9", "ghost"))).expect_err("src");
        assert_eq!(
            missing.protocol(),
            Some(&ProtocolError::NotFound {
                entity: EntityKind::Source,
                id: "ghost".into()
            })
        );
        assert_eq!(engine.rev(), Rev(1));

        let unplaced = Batch::single(Edit::SourceDelete {
            source_id: SourceId::new("webcam_2"),
        });
        assert_eq!(batch(&*control, unplaced), Ok(Rev(2)));
    }

    #[test]
    fn capture_targets_lists_one_primary_display() {
        let engine = MockEngine::new();
        let control = connected(&engine);
        let Ok(QueryResult::CaptureTargets { targets }) = query(&*control, Query::CaptureTargets)
        else {
            panic!("a capture.targets result");
        };
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].target, super::MOCK_DISPLAY);
        assert!(targets[0].primary);
    }

    fn query(control: &dyn Control, query: Query) -> Result<QueryResult, ControlError> {
        match block_on(control.call(Call::Query { query }))?.result {
            CallResult::Query { result } => Ok(result),
            other => panic!("not a query result: {other:?}"),
        }
    }

    /// The shared parity collection (`jstream_protocol::parity`), which `jstream-graph`'s
    /// read tests build too. `jstream-graph` can't be called here: it is in the same
    /// layer (R-ARCH-002), even as a dev-dependency.
    fn parity_engine() -> (MockEngine, Box<dyn Control>) {
        let engine = MockEngine::new().with_kinds(parity::kinds());
        let control = connected(&engine);
        assert_eq!(
            batch(&*control, parity::collection()),
            Ok(parity::COLLECTION_REV)
        );
        (engine, control)
    }

    /// `kind.list` answers with no kinds, even for kinds `with_kinds` told it about: the
    /// mock describes none. It answers rather than failing, so a client can report a
    /// failed `kind.list` without the mock tripping it (#401). `kind.describe` still
    /// answers `internal`.
    #[test]
    fn kind_list_lists_no_kinds() {
        let (_engine, control) = parity_engine();
        assert_eq!(
            query(&*control, Query::KindList),
            Ok(QueryResult::KindList { kinds: Vec::new() })
        );
        let describe = Query::KindDescribe {
            kind_id: "color".into(),
        };
        let refused = query(&*control, describe).expect_err("kind.describe");
        assert!(
            matches!(refused.protocol(), Some(ProtocolError::Internal { .. })),
            "{refused:?}"
        );
    }

    /// The answer to a history call: `(rev, step_rev, undo_depth, redo_depth)`.
    fn history(
        control: &dyn Control,
        call: HistoryCall,
    ) -> Result<(Rev, Rev, u32, u32), ControlError> {
        match block_on(control.call(Call::History(call)))?.result {
            CallResult::History {
                rev,
                step_rev,
                undo_depth,
                redo_depth,
            } => Ok((rev, step_rev, undo_depth, redo_depth)),
            other => panic!("not a history result: {other:?}"),
        }
    }

    fn undo(control: &dyn Control) -> Result<(Rev, Rev, u32, u32), ControlError> {
        history(control, HistoryCall::new(HistoryOp::Undo))
    }

    fn redo(control: &dyn Control) -> Result<(Rev, Rev, u32, u32), ControlError> {
        history(control, HistoryCall::new(HistoryOp::Redo))
    }

    /// The whole state as `state.get` reports it, without its `rev`.
    fn state(control: &dyn Control) -> QueryResult {
        match query(control, Query::StateGet).expect("state.get") {
            QueryResult::StateGet {
                canvas,
                sources,
                scenes,
                outputs,
                encoders,
                master,
                video_mixes,
                buses,
                sends,
                hotkeys,
                transitions,
                folders,
                labels,
                ..
            } => QueryResult::StateGet {
                rev: Rev(0),
                canvas,
                sources,
                scenes,
                outputs,
                encoders,
                master,
                video_mixes,
                buses,
                sends,
                hotkeys,
                transitions,
                folders,
                labels,
            },
            other => panic!("not a state: {other:?}"),
        }
    }

    /// Undo and redo restore the shared parity collection exactly, through the same
    /// inverse edits `jstream-graph` builds: a removed item comes back at its index with
    /// its placement (in its own form, transform or geometry), crop and visibility, a
    /// deleted scene with its items and its position, a source with its settings and its
    /// `duplicated_from`, an item's source, an output with its encoder profile and video mix, the encoder profiles, the video mixes, the canvas size,
    /// a deleted bus with its fader and sends, a source's audio and sends as they were,
    /// the faders and sends set since, and the hotkey bindings that the deletes of scene
    /// `a` and output `yt` removed. Its
    /// placement, crop and visibility edits name the item's own scene, and their
    /// inverses do too.
    #[test]
    fn undo_and_redo_restore_the_parity_collection() {
        let (_engine, control) = parity_engine();
        let mut events = control.events();
        let built = state(&*control);
        let mut gain = Settings::new();
        gain.insert("gain", 2.into());
        let teardown = Batch {
            edits: vec![
                Edit::CanvasSetSize {
                    width: Canvas::DEFAULT.width,
                    height: Canvas::DEFAULT.height,
                },
                // `i4` is placed by a geometry: its inverse sets the geometry back. These
                // name `i4`'s own scene, and so do their inverses.
                Edit::ItemSetTransform {
                    item_id: ItemId::new("i4"),
                    scene_id: Some(SceneId::new("b")),
                    transform: ItemTransform::IDENTITY,
                },
                Edit::ItemSetVisible {
                    item_id: ItemId::new("i4"),
                    scene_id: Some(SceneId::new("b")),
                    visible: false,
                },
                Edit::ItemSetCrop {
                    item_id: ItemId::new("i4"),
                    scene_id: Some(SceneId::new("b")),
                    crop: Crop {
                        left: 1,
                        top: 2,
                        right: 3,
                        bottom: 4,
                    },
                },
                // `i1` places the clone `cam_copy`; its inverse switches it back.
                Edit::ItemSetSource {
                    item_id: ItemId::new("i1"),
                    source_id: SourceId::new("aux"),
                },
                Edit::SourceRename {
                    source_id: SourceId::new("cam"),
                    name: "Camera".into(),
                },
                Edit::SourceSetSettings {
                    source_id: SourceId::new("cam"),
                    patch: gain,
                },
                Edit::ItemRemove {
                    item_id: ItemId::new("i3"),
                },
                Edit::SceneDelete {
                    scene_id: SceneId::new("a"),
                },
                Edit::SourceDelete {
                    source_id: SourceId::new("b_src"),
                },
                // Mix `side` shows `b`, so it moves off it first; its inverse moves it back.
                Edit::VideoMixUpdate {
                    mix_id: VideoMixId::new("side"),
                    name: "Moved".into(),
                    mix: VideoMix {
                        input: MixInput::Program,
                        resolution: MixResolution {
                            width: 320,
                            height: 180,
                        },
                        fps_divisor: 1,
                    },
                },
                Edit::SceneDelete {
                    scene_id: SceneId::new("b"),
                },
                Edit::SourceDelete {
                    source_id: SourceId::new("cam"),
                },
                // Their inverses re-create the clone's link to `cam` and `aux`'s link to a
                // source that doesn't exist.
                Edit::SourceDelete {
                    source_id: SourceId::new("cam_copy"),
                },
                Edit::SourceDelete {
                    source_id: SourceId::new("aux"),
                },
                // Its inverse re-adds its filters, in order and with their switches.
                Edit::SourceDelete {
                    source_id: SourceId::new("desk"),
                },
                // The audio: a bus's name, the master's fader, a send's gain, a source's
                // audio, a new send, then a bus with its fader and send, and the audio
                // sources with their audio and sends.
                Edit::BusRename {
                    bus_id: BusId::new("vod"),
                    name: "VOD".into(),
                },
                Edit::BusSetFader {
                    bus_id: BusId::master(),
                    gain_db: Some(Finite32::ZERO),
                    muted: None,
                },
                Edit::SendSet {
                    source_id: SourceId::new("mic"),
                    bus_id: BusId::new("vod"),
                    gain_db: Finite32::ZERO,
                },
                Edit::SourceSetAudio {
                    source_id: SourceId::new("mic"),
                    enabled: Some(false),
                    follow: None,
                    gain_db: Some(Finite32::ZERO),
                    muted: None,
                },
                Edit::SendSet {
                    source_id: SourceId::new("mic_copy"),
                    bus_id: BusId::master(),
                    gain_db: Finite32::ZERO,
                },
                Edit::BusDelete {
                    bus_id: BusId::new("vod"),
                },
                Edit::SourceDelete {
                    source_id: SourceId::new("mic_copy"),
                },
                Edit::SourceDelete {
                    source_id: SourceId::new("mic"),
                },
                Edit::OutputSetEncoder {
                    output_id: OutputId::new("yt"),
                    profile_id: Some(EncoderProfileId::new("hq")),
                },
                Edit::EncoderUpdate {
                    profile_id: EncoderProfileId::new("hq"),
                    name: "Draft".into(),
                    profile: EncoderProfile::DEFAULT,
                },
                Edit::OutputSetVideoMix {
                    output_id: OutputId::new("yt"),
                    mix_id: Some(VideoMixId::new("prog")),
                },
                Edit::OutputDelete {
                    output_id: OutputId::new("tw"),
                },
                Edit::OutputDelete {
                    output_id: OutputId::new("yt"),
                },
                // Its inverse re-adds its own filter (ADR-0027).
                Edit::OutputDelete {
                    output_id: OutputId::new("rec"),
                },
                Edit::OutputDelete {
                    output_id: OutputId::new("vcam"),
                },
                Edit::FilterRemove {
                    owner: jstream_protocol::FilterOwner::Master,
                    filter_id: jstream_protocol::FilterId::new("m1"),
                },
                Edit::EncoderDelete {
                    profile_id: EncoderProfileId::new("low"),
                },
                Edit::EncoderDelete {
                    profile_id: EncoderProfileId::new("hq"),
                },
                Edit::VideoMixDelete {
                    mix_id: VideoMixId::new("side"),
                },
                Edit::VideoMixDelete {
                    mix_id: VideoMixId::new("prog"),
                },
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        };
        assert_eq!(batch(&*control, teardown), Ok(Rev(2)));
        let empty = state(&*control);
        assert_eq!(
            empty,
            QueryResult::StateGet {
                rev: Rev(0),
                canvas: Canvas::DEFAULT,
                sources: vec![],
                scenes: vec![],
                outputs: vec![],
                encoders: vec![],
                video_mixes: vec![],
                buses: vec![BusInfo {
                    bus_id: BusId::master(),
                    name: "Master".into(),
                    gain_db: Finite32::ZERO,
                    muted: false,
                }],
                sends: vec![],
                master: Default::default(),
                hotkeys: vec![],
                transitions: Box::new(TransitionsInfo::default()),
                folders: Vec::new(),
                labels: Vec::new(),
            }
        );
        drain(&mut events);

        assert_eq!(undo(&*control), Ok((Rev(3), Rev(2), 1, 1)));
        assert_eq!(state(&*control), built);
        assert_eq!(redo(&*control), Ok((Rev(4), Rev(2), 2, 0)));
        assert_eq!(state(&*control), empty);
        assert_eq!(undo(&*control), Ok((Rev(5), Rev(2), 1, 1)));
        assert_eq!(undo(&*control), Ok((Rev(6), Rev(1), 0, 2)));
        assert_eq!(state(&*control), empty);
        let error = undo(&*control).expect_err("nothing left");
        assert_eq!(
            error.protocol(),
            Some(&ProtocolError::HistoryEmpty {
                direction: HistoryDirection::Undo
            })
        );
        let mut programs = Vec::new();
        let causes: Vec<_> = drain(&mut events)
            .into_iter()
            .filter_map(|event| match event {
                Event::StateChanged { rev, cause, .. } => Some((rev, cause)),
                // Program follows undo and redo too (ADR-0030).
                Event::StudioChanged { program_scene, .. } => {
                    programs.push(program_scene);
                    None
                }
                other => panic!("unexpected event: {other:?}"),
            })
            .collect();
        assert_eq!(
            causes,
            [
                (Rev(3), Some(HistoryDirection::Undo)),
                (Rev(4), Some(HistoryDirection::Redo)),
                (Rev(5), Some(HistoryDirection::Undo)),
                (Rev(6), Some(HistoryDirection::Undo)),
            ]
        );
        let b = Some(SceneId::new("b"));
        assert_eq!(programs, [b.clone(), None, b, None]);
    }

    /// Each client undoes only its own steps, a new batch clears what it could redo, and
    /// another client's change since a step makes undoing it an `undo_conflict` that
    /// drops the step: the mock's stricter rule.
    #[test]
    fn undo_is_per_client_and_refuses_after_another_clients_change() {
        let engine = MockEngine::new();
        let (alice, bob) = (connected(&engine), connected(&engine));
        assert_eq!(batch(&*alice, Batch::single(create("a"))), Ok(Rev(1)));
        assert_eq!(batch(&*bob, Batch::single(create("b"))), Ok(Rev(2)));
        assert_eq!(batch(&*alice, Batch::single(create("c"))), Ok(Rev(3)));

        assert_eq!(undo(&*alice), Ok((Rev(4), Rev(3), 1, 1)));
        assert_eq!(batch(&*alice, Batch::single(create("d"))), Ok(Rev(5)));
        let error = redo(&*alice).expect_err("cleared");
        assert_eq!(code(&error), Some("history_empty"));

        assert_eq!(undo(&*alice), Ok((Rev(6), Rev(5), 1, 1)));
        // Bob created `b` after Alice created `a`.
        let error = undo(&*alice).expect_err("conflict");
        assert_eq!(
            error.protocol(),
            Some(&ProtocolError::UndoConflict {
                step_rev: Rev(1),
                client_id: ClientId::new("mock_2"),
                rev: Rev(2),
            })
        );
        assert_eq!(engine.rev(), Rev(6));
        assert_eq!(
            scene_list(&*alice).1,
            [("a".to_owned(), 0), ("b".to_owned(), 0)]
        );
        // The step is gone. Bob still has his own step, which Alice's later batches
        // block in turn.
        assert_eq!(
            code(&undo(&*alice).expect_err("empty")),
            Some("history_empty")
        );
        let error = undo(&*bob).expect_err("conflict");
        assert_eq!(
            error.protocol(),
            Some(&ProtocolError::UndoConflict {
                step_rev: Rev(2),
                client_id: ClientId::new("mock_1"),
                rev: Rev(3),
            })
        );
    }

    /// The engine's order of checks: a used idempotency key answers first, then
    /// `expected_rev`, then the stack. None of them drops a step.
    #[test]
    fn history_checks_the_key_then_expected_rev_then_the_stack() {
        let engine = MockEngine::new();
        let control = connected(&engine);
        assert_eq!(batch(&*control, Batch::single(create("a"))), Ok(Rev(1)));
        assert_eq!(batch(&*control, Batch::single(create("b"))), Ok(Rev(2)));

        let stale = HistoryCall::new(HistoryOp::Undo).expecting(Rev(1));
        let error = history(&*control, stale).expect_err("stale");
        assert_eq!(code(&error), Some("rev_conflict"));

        let keyed = HistoryCall::new(HistoryOp::Undo)
            .expecting(Rev(2))
            .with_idempotency_key(IdempotencyKey::new("u-1"));
        assert_eq!(
            history(&*control, keyed.clone()),
            Ok((Rev(3), Rev(2), 1, 1))
        );
        // The retry gets the first answer, though its `expected_rev` is stale now.
        assert_eq!(history(&*control, keyed), Ok((Rev(3), Rev(2), 1, 1)));
        assert_eq!(engine.rev(), Rev(3));

        let error = redo(&*control)
            .map(|_| ())
            .and_then(|()| redo(&*control).map(|_| ()));
        assert_eq!(
            code(&error.expect_err("one redo only")),
            Some("history_empty")
        );
    }

    /// A running output blocks undoing its `output.create` with the graph's error, and
    /// the step stays for a retry once it stops. A closed session takes its history
    /// with it, and a history limit keeps only the newest steps.
    #[test]
    fn a_running_output_keeps_its_step_and_history_is_bounded() {
        let engine = MockEngine::new().with_history_limit(2);
        let control = connected(&engine);
        let output = Batch::single(Edit::OutputCreate {
            output_id: OutputId::new("yt"),
            kind: rtmp("rtmp://a.example.com/live"),
        });
        assert_eq!(batch(&*control, output), Ok(Rev(1)));
        let yt = OutputId::new("yt");
        action(
            &*control,
            Action::OutputStart {
                output_id: yt.clone(),
            },
        )
        .expect("start");
        let error = undo(&*control).expect_err("running");
        assert_eq!(
            error.protocol(),
            Some(&ProtocolError::invalid_argument(
                "edits.0.output_id",
                "output `yt` is running; stop it first",
            ))
        );
        action(&*control, Action::OutputStop { output_id: yt }).expect("stop");
        assert_eq!(undo(&*control), Ok((Rev(2), Rev(1), 0, 1)));

        for scene in ["a", "b", "c"] {
            batch(&*control, Batch::single(create(scene))).expect("scene");
        }
        assert_eq!(undo(&*control), Ok((Rev(6), Rev(5), 1, 1)));
        assert_eq!(undo(&*control), Ok((Rev(7), Rev(4), 0, 2)));
        assert_eq!(
            code(&undo(&*control).expect_err("limit")),
            Some("history_empty")
        );

        // A new session starts with no history; the old one's steps went with it.
        drop(control);
        let again = connected(&engine);
        assert_eq!(
            code(&redo(&*again).expect_err("new")),
            Some("history_empty")
        );
    }

    /// Every read in the shared parity fixture gets exactly the answer the fixture
    /// expects, the same one `Graph::query` is held to.
    #[test]
    fn queries_report_what_the_edits_set() {
        let (_engine, control) = parity_engine();
        for (call, expected) in parity::reads() {
            let got = query(&*control, call.clone()).map_err(|error| error.protocol().cloned());
            assert_eq!(got, expected.map_err(Some), "{call:?}");
        }

        // A duplicate takes the new name and the original's kind; removing an item
        // leaves its source.
        let more = Batch {
            edits: vec![
                Edit::SourceDuplicate {
                    source_id: SourceId::new("cam"),
                    new_source_id: SourceId::new("cam_2"),
                    name: "Camera 2".into(),
                },
                Edit::ItemRemove {
                    item_id: ItemId::new("i1"),
                },
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        };
        assert_eq!(batch(&*control, more), Ok(Rev(2)));
        let Ok(QueryResult::SourceList { rev, sources }) = query(&*control, Query::SourceList)
        else {
            panic!("not a source list");
        };
        assert_eq!(rev, Rev(2));
        let names: Vec<(&str, &str)> = sources
            .iter()
            .map(|s| (s.source_id.as_str(), s.name.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("aux", "Aux"),
                ("b_src", "Scene B"),
                ("cam", "Camera"),
                ("cam_2", "Camera 2"),
                ("cam_copy", "Camera copy"),
                ("desk", "Desktop"),
                ("mic", "Microphone"),
                ("mic_copy", "Microphone copy"),
            ]
        );
        assert_eq!(sources[3].kind, sources[2].kind);
        assert_eq!(sources[3].duplicated_from, Some(SourceId::new("cam")));

        // Deleting the original leaves the copies, whose link then reads as absent.
        let unplace = Batch {
            edits: vec![
                Edit::ItemRemove {
                    item_id: ItemId::new("i3"),
                },
                Edit::ItemRemove {
                    item_id: ItemId::new("i4"),
                },
                Edit::SourceDelete {
                    source_id: SourceId::new("cam"),
                },
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        };
        assert_eq!(batch(&*control, unplace), Ok(Rev(3)));
        let Ok(QueryResult::SourceList { sources, .. }) = query(&*control, Query::SourceList)
        else {
            panic!("not a source list");
        };
        // `mic_copy` still names `mic`, which wasn't deleted.
        assert!(
            sources
                .iter()
                .filter(|s| s.source_id.as_str() != "mic_copy")
                .all(|s| s.duplicated_from.is_none())
        );
    }

    /// Cycle reasons name the cycle path in `jstream-graph`'s words, for `item.add` and
    /// `item.move` alike (the cases of its `three_cycles_are_rejected_even_within_one_batch`
    /// and `move_is_cycle_checked`).
    #[test]
    fn cycle_reasons_name_the_path_like_the_graph() {
        let engine = MockEngine::new();
        let control = connected(&engine);
        let scene_source = |source: &str, scene: &str| Edit::SourceCreate {
            source_id: SourceId::new(source),
            name: source.to_uppercase(),
            kind: SourceKind::Scene {
                scene_id: SceneId::new(scene),
            },
            settings: Default::default(),
            duplicated_from: None,
        };
        let edits = |edits: Vec<Edit>| Batch {
            edits,
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        };
        let setup = edits(vec![
            create("a"),
            create("b"),
            create("c"),
            scene_source("sa", "a"),
            scene_source("sb", "b"),
            scene_source("sc", "c"),
        ]);
        assert_eq!(batch(&*control, setup), Ok(Rev(1)));
        let rejected = |rejected: Batch| match batch(&*control, rejected) {
            Err(ControlError::Protocol(wire)) => match wire.error {
                ProtocolError::InvalidArgument { field, reason, .. } => (field, reason),
                other => panic!("expected invalid_argument, got {other:?}"),
            },
            other => panic!("expected an error, got {other:?}"),
        };
        assert_eq!(
            rejected(edits(vec![
                add("a", "ab", "sb"),
                add("b", "bc", "sc"),
                add("c", "ca", "sa"),
            ])),
            (
                "edits.2.source_id".to_owned(),
                "placing source `sa` in scene `c` would create a cycle: scene `c` -> source `sa` \
                 -> scene `a` -> source `sb` -> scene `b` -> source `sc` -> scene `c`"
                    .to_owned()
            )
        );
        let setup = edits(vec![add("a", "ab", "sb"), add("c", "ca", "sa")]);
        assert_eq!(batch(&*control, setup), Ok(Rev(2)));
        let mv = Edit::ItemMove {
            item_id: ItemId::new("ca"),
            scene_id: SceneId::new("b"),
            index: 0,
        };
        assert_eq!(
            rejected(edits(vec![mv])),
            (
                "edits.0.scene_id".to_owned(),
                "moving item `ca` (source `sa`) into scene `b` would create a cycle: scene `b` \
                 -> source `sa` -> scene `a` -> source `sb` -> scene `b`"
                    .to_owned()
            )
        );
    }

    /// `item.set_source` keeps the item and is cycle checked like `item.add`.
    #[test]
    fn set_source_retargets_an_item_without_a_cycle() {
        let (_engine, control) = parity_engine();
        // Scene `a` shows `b` through `i2`; a source showing `a` can't go into `b`...
        let setup = Batch {
            edits: vec![
                Edit::SourceCreate {
                    source_id: SourceId::new("a_src"),
                    name: "Scene A".into(),
                    kind: SourceKind::Scene {
                        scene_id: SceneId::new("a"),
                    },
                    settings: Default::default(),
                    duplicated_from: None,
                },
                Edit::ItemAdd {
                    scene_id: SceneId::new("b"),
                    item_id: ItemId::new("ib"),
                    source_id: SourceId::new("cam"),
                    transform: None,
                    geometry: None,
                    opacity: None,
                    blend: None,
                },
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        };
        assert_eq!(batch(&*control, setup), Ok(Rev(2)));
        let cycle = Batch::single(Edit::ItemSetSource {
            item_id: ItemId::new("ib"),
            source_id: SourceId::new("a_src"),
        });
        let error = batch(&*control, cycle).expect_err("cycle");
        assert_eq!(code(&error), Some("invalid_argument"));
        // ... until `a` stops showing `b`, in the same batch.
        let retarget = Batch {
            edits: vec![
                Edit::ItemSetSource {
                    item_id: ItemId::new("i2"),
                    source_id: SourceId::new("cam"),
                },
                Edit::ItemSetSource {
                    item_id: ItemId::new("ib"),
                    source_id: SourceId::new("a_src"),
                },
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        };
        assert_eq!(batch(&*control, retarget), Ok(Rev(3)));
        let Ok(QueryResult::ItemList { items, .. }) = query(
            &*control,
            Query::ItemList {
                scene_id: SceneId::new("b"),
            },
        ) else {
            panic!("not an item list");
        };
        let ib = items
            .iter()
            .find(|item| item.item_id.as_str() == "ib")
            .expect("ib is in b");
        assert_eq!(ib.source_id.as_str(), "a_src");
    }

    /// #362: a bus a source's settings draw can't be deleted until they are cleared, as
    /// the engine refuses it (`jstream-graph`), in the same batch or before.
    #[test]
    fn a_bus_a_source_draws_cannot_be_deleted() {
        let engine = MockEngine::new();
        let control = connected(&engine);
        let wave = |bus: serde_json::Value| Edit::SourceSetSettings {
            source_id: SourceId::new("wave"),
            patch: [("audio", bus)].into_iter().collect(),
        };
        batch(
            &*control,
            Batch {
                edits: vec![
                    Edit::BusCreate {
                        bus_id: BusId::new("vod"),
                        name: "VOD".into(),
                    },
                    Edit::SourceCreate {
                        source_id: SourceId::new("wave"),
                        name: "Waveform".into(),
                        kind: SourceKind::Builtin {
                            kind: "waveform".into(),
                        },
                        settings: Settings::new(),
                        duplicated_from: None,
                    },
                    wave(serde_json::json!({"type": "bus", "bus_id": "vod"})),
                ],
                expected_rev: None,
                idempotency_key: None,
                ends_gesture: None,
            },
        )
        .expect("created");
        let delete = Edit::BusDelete {
            bus_id: BusId::new("vod"),
        };
        let error = batch(&*control, Batch::single(delete.clone())).expect_err("refused");
        assert!(
            matches!(
                error.protocol(),
                Some(ProtocolError::InvalidArgument { field, .. }) if field == "edits.0.bus_id"
            ),
            "{error:?}"
        );
        batch(
            &*control,
            Batch {
                edits: vec![wave(serde_json::Value::Null), delete],
                expected_rev: None,
                idempotency_key: None,
                ends_gesture: None,
            },
        )
        .expect("cleared, then deleted");
    }

    /// Every batch the shared parity fixture rejects fails with exactly the error
    /// `Graph::apply` returns for it, and applies nothing.
    #[test]
    fn rejected_batches_match_the_parity_fixture() {
        let (engine, control) = parity_engine();
        for (rejected, expected) in parity::rejected() {
            let error = batch(&*control, rejected.clone()).expect_err("rejected");
            assert_eq!(error.protocol(), Some(&expected), "{rejected:?}");
            assert_eq!(engine.rev(), parity::COLLECTION_REV, "{rejected:?}");
        }
        for (call, expected) in parity::reads() {
            let got = query(&*control, call.clone()).map_err(|error| error.protocol().cloned());
            assert_eq!(got, expected.map_err(Some), "{call:?}");
        }
    }

    /// ADR-0040 §3, as `jstream-graph` does it: deleting what a binding names removes the
    /// binding, the batch result lists it, and undo brings it back.
    #[test]
    fn deletes_remove_the_hotkeys_naming_their_target() {
        let (_engine, control) = parity_engine();
        let delete = Batch::single(Edit::OutputDelete {
            output_id: OutputId::new("yt"),
        });
        let result = block_on(control.call(Call::Batch(delete)))
            .expect("applies")
            .result;
        let CallResult::Batch {
            removed_hotkeys, ..
        } = result
        else {
            panic!("not a batch result: {result:?}");
        };
        let start_yt = parity::hotkeys()
            .into_iter()
            .find(|b| b.chord.as_str() == "Ctrl+Digit1");
        assert_eq!(removed_hotkeys, start_yt.into_iter().collect::<Vec<_>>());
        let QueryResult::StateGet { hotkeys, .. } = state(&*control) else {
            panic!("not a state");
        };
        let chords: Vec<_> = hotkeys.iter().map(|b| b.chord.as_str()).collect();
        assert_eq!(chords, ["F1"]);
        assert!(undo(&*control).is_ok());
        let QueryResult::StateGet { hotkeys, .. } = state(&*control) else {
            panic!("not a state");
        };
        assert_eq!(hotkeys, parity::hotkeys());
    }

    /// `audio.reset_loudness` names nothing, so it binds and an output's delete leaves it
    /// bound.
    #[test]
    fn an_action_that_names_nothing_can_be_bound() {
        let (_engine, control) = parity_engine();
        let bind = Batch::single(Edit::HotkeySet {
            chord: jstream_protocol::Chord::new("F9"),
            action: Action::AudioResetLoudness {},
        });
        assert!(batch(&*control, bind).is_ok());
        let delete = Batch::single(Edit::OutputDelete {
            output_id: OutputId::new("yt"),
        });
        assert!(batch(&*control, delete).is_ok());
        let QueryResult::StateGet { hotkeys, .. } = state(&*control) else {
            panic!("not a state");
        };
        let chords: Vec<_> = hotkeys.iter().map(|b| b.chord.as_str()).collect();
        assert_eq!(chords, ["F1", "F9"]);
    }

    #[test]
    fn empty_ids_are_rejected_like_the_graph_does() {
        let engine = MockEngine::new();
        let control = connected(&engine);
        assert_eq!(batch(&*control, Batch::single(webcam())), Ok(Rev(1)));
        let rejected = |edit: Edit| {
            batch(&*control, Batch::single(edit))
                .expect_err("empty id")
                .protocol()
                .cloned()
        };
        // Exactly what `jstream_graph::Graph::apply` returns for this edit (checked
        // against jstream-graph 0.0.0: check_id runs before check_name). It can't be
        // called here: it is in the same layer (R-ARCH-002), even as a dev-dependency.
        assert_eq!(
            rejected(Edit::SceneCreate {
                scene_id: SceneId::new(""),
                name: "x".into(),
                role: None,
            }),
            Some(ProtocolError::invalid_argument(
                "edits.0.scene_id",
                "must not be empty"
            ))
        );
        let field = |edit: Edit| match rejected(edit) {
            Some(ProtocolError::InvalidArgument { field, .. }) => field,
            other => panic!("expected invalid_argument, got {other:?}"),
        };
        assert_eq!(field(add("missing_scene", "", "webcam")), "edits.0.item_id");
        let source = Edit::SourceCreate {
            source_id: SourceId::new(""),
            name: String::new(),
            kind: SourceKind::Builtin {
                kind: "color".into(),
            },
            settings: Default::default(),
            duplicated_from: None,
        };
        assert_eq!(field(source), "edits.0.source_id");
        let duplicate = Edit::SourceDuplicate {
            source_id: SourceId::new("webcam"),
            new_source_id: SourceId::new(""),
            name: String::new(),
        };
        assert_eq!(field(duplicate), "edits.0.new_source_id");
        assert_eq!(engine.rev(), Rev(1));
    }

    /// Protocol 0.12's groups: `scene.set_role` and `item.move`, with the checks
    /// `jstream-graph` makes in its order.
    #[test]
    fn groups_and_item_move_follow_the_graph_rules() {
        let engine = MockEngine::new();
        let control = connected(&engine);
        let group = Edit::SourceCreate {
            source_id: SourceId::new("g_src"),
            name: "Group".into(),
            kind: SourceKind::Scene {
                scene_id: SceneId::new("g"),
            },
            settings: Default::default(),
            duplicated_from: None,
        };
        let setup = Batch {
            edits: vec![
                create("a"),
                create("g"),
                webcam(),
                add("a", "i1", "webcam"),
                add("a", "i2", "webcam"),
                Edit::SceneSetRole {
                    scene_id: SceneId::new("g"),
                    role: SceneRole::Group,
                },
                group,
                add("a", "gi", "g_src"),
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        };
        assert_eq!(batch(&*control, setup), Ok(Rev(1)));
        let roles = |control: &dyn Control| match query(control, Query::SceneList) {
            Ok(QueryResult::SceneList { scenes, .. }) => scenes
                .into_iter()
                .map(|s| (s.scene_id.to_string(), s.role))
                .collect::<Vec<_>>(),
            other => panic!("not a scene list: {other:?}"),
        };
        assert_eq!(
            roles(&*control),
            [
                ("a".into(), SceneRole::Scene),
                ("g".into(), SceneRole::Group)
            ]
        );
        let items = |control: &dyn Control, scene: &str| match query(
            control,
            Query::ItemList {
                scene_id: SceneId::new(scene),
            },
        ) {
            Ok(QueryResult::ItemList { items, .. }) => items
                .into_iter()
                .map(|i| i.item_id.to_string())
                .collect::<Vec<_>>(),
            other => panic!("not an item list: {other:?}"),
        };
        let mv = |item: &str, scene: &str, index: u32| Edit::ItemMove {
            item_id: ItemId::new(item),
            scene_id: SceneId::new(scene),
            index,
        };

        // Grouping: both webcam items into the group; an empty scene takes index 0, and
        // one past the top of another scene is allowed.
        let grouped = Batch {
            edits: vec![mv("i1", "g", 0), mv("i2", "g", 1)],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        };
        assert_eq!(batch(&*control, grouped), Ok(Rev(2)));
        assert_eq!(items(&*control, "a"), ["gi"]);
        assert_eq!(items(&*control, "g"), ["i1", "i2"]);

        // Within its own scene a move is a reorder.
        assert_eq!(
            batch(&*control, Batch::single(mv("i2", "g", 0))),
            Ok(Rev(3))
        );
        assert_eq!(items(&*control, "g"), ["i2", "i1"]);

        let rejected = |edit: Edit| {
            batch(&*control, Batch::single(edit))
                .expect_err("rejected")
                .protocol()
                .cloned()
        };
        let field = |edit: Edit| match rejected(edit) {
            Some(ProtocolError::InvalidArgument { field, .. }) => field,
            other => panic!("expected invalid_argument, got {other:?}"),
        };
        // Within its own scene one past the top is out of range; in another it isn't,
        // but two past is.
        assert_eq!(field(mv("i1", "g", 2)), "edits.0.index");
        assert_eq!(field(mv("i1", "a", 2)), "edits.0.index");
        // The group's own placement moved into the group would make it contain itself.
        assert_eq!(field(mv("gi", "g", 0)), "edits.0.scene_id");
        assert_eq!(
            rejected(mv("nope", "g", 0)),
            Some(ProtocolError::NotFound {
                entity: EntityKind::Item,
                id: "nope".into(),
            })
        );
        assert_eq!(
            rejected(mv("i1", "nope", 0)),
            Some(ProtocolError::NotFound {
                entity: EntityKind::Scene,
                id: "nope".into(),
            })
        );
        assert_eq!(
            rejected(Edit::SceneSetRole {
                scene_id: SceneId::new("nope"),
                role: SceneRole::Group,
            }),
            Some(ProtocolError::NotFound {
                entity: EntityKind::Scene,
                id: "nope".into(),
            })
        );
        assert_eq!(engine.rev(), Rev(3));

        // A moved item keeps its ID and properties, and its placement edits now name
        // its new scene as its own.
        let hide = Edit::ItemSetVisible {
            item_id: ItemId::new("i1"),
            scene_id: Some(SceneId::new("g")),
            visible: false,
        };
        assert_eq!(batch(&*control, Batch::single(hide)), Ok(Rev(4)));
        let back = Batch {
            edits: vec![
                mv("i1", "a", 0),
                Edit::SceneSetRole {
                    scene_id: SceneId::new("g"),
                    role: SceneRole::Scene,
                },
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        };
        assert_eq!(batch(&*control, back), Ok(Rev(5)));
        assert_eq!(items(&*control, "a"), ["i1", "gi"]);
        match query(
            &*control,
            Query::ItemList {
                scene_id: SceneId::new("a"),
            },
        ) {
            Ok(QueryResult::ItemList { items, .. }) => assert!(!items[0].visible),
            other => panic!("not an item list: {other:?}"),
        }
        assert_eq!(
            roles(&*control),
            [
                ("a".into(), SceneRole::Scene),
                ("g".into(), SceneRole::Scene)
            ]
        );
    }

    /// Undo and redo of grouping: `item.move` returns to its scene and index,
    /// `scene.set_role` to its previous role, and deleting a group scene comes back as a
    /// group, as `jstream-graph`'s inverses do.
    #[test]
    fn undo_and_redo_restore_groups_and_moves() {
        let engine = MockEngine::new();
        let control = connected(&engine);
        let setup = Batch {
            edits: vec![
                create("a"),
                create("g"),
                webcam(),
                add("a", "i1", "webcam"),
                add("a", "i2", "webcam"),
                add("a", "i3", "webcam"),
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        };
        assert_eq!(batch(&*control, setup), Ok(Rev(1)));
        let before = state(&*control);
        let mv = |item: &str, scene: &str, index: u32| Edit::ItemMove {
            item_id: ItemId::new(item),
            scene_id: SceneId::new(scene),
            index,
        };
        let group = Batch {
            edits: vec![
                Edit::SceneSetRole {
                    scene_id: SceneId::new("g"),
                    role: SceneRole::Group,
                },
                // The middle item first, so its inverse names index 1 of three.
                mv("i2", "g", 0),
                mv("i1", "g", 0),
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        };
        assert_eq!(batch(&*control, group), Ok(Rev(2)));
        let grouped = state(&*control);
        assert_ne!(grouped, before);
        assert_eq!(undo(&*control), Ok((Rev(3), Rev(2), 1, 1)));
        assert_eq!(state(&*control), before);
        assert_eq!(redo(&*control), Ok((Rev(4), Rev(2), 2, 0)));
        assert_eq!(state(&*control), grouped);

        // Deleting the group scene (and its items) comes back as a group, with its items.
        let delete = Batch::single(Edit::SceneDelete {
            scene_id: SceneId::new("g"),
        });
        assert_eq!(batch(&*control, delete), Ok(Rev(5)));
        assert_eq!(undo(&*control), Ok((Rev(6), Rev(5), 2, 1)));
        assert_eq!(state(&*control), grouped);
    }

    /// `item.set_opacity` undoes to the old value, and removing a faded item comes back
    /// faded, as `jstream-graph`'s inverses do.
    #[test]
    fn undo_restores_opacity_and_a_removed_items_opacity() {
        let engine = MockEngine::new();
        let control = connected(&engine);
        let setup = Batch {
            edits: vec![create("a"), webcam(), add("a", "i1", "webcam")],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        };
        assert_eq!(batch(&*control, setup), Ok(Rev(1)));
        let before = state(&*control);
        let fade = Batch {
            edits: vec![
                Edit::ItemSetOpacity {
                    item_id: ItemId::new("i1"),
                    scene_id: None,
                    opacity: Finite32::new(0.25).expect("finite"),
                },
                Edit::ItemSetBlend {
                    item_id: ItemId::new("i1"),
                    blend: BlendMode::Normal,
                },
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        };
        assert_eq!(batch(&*control, fade), Ok(Rev(2)));
        let faded = state(&*control);
        assert_ne!(faded, before);
        assert_eq!(undo(&*control), Ok((Rev(3), Rev(2), 1, 1)));
        assert_eq!(state(&*control), before);
        assert_eq!(redo(&*control), Ok((Rev(4), Rev(2), 2, 0)));
        assert_eq!(state(&*control), faded);

        let remove = Batch::single(Edit::ItemRemove {
            item_id: ItemId::new("i1"),
        });
        assert_eq!(batch(&*control, remove), Ok(Rev(5)));
        assert_eq!(undo(&*control), Ok((Rev(6), Rev(5), 2, 1)));
        assert_eq!(state(&*control), faded);
    }

    #[test]
    fn rev_conflict_is_checked_before_empty_batch() {
        let engine = MockEngine::new();
        let control = connected(&engine);
        let empty_and_stale = Batch {
            edits: vec![],
            expected_rev: Some(Rev(5)),
            idempotency_key: None,
            ends_gesture: None,
        };
        // `jstream_graph::Graph::apply` answers this with `rev_conflict` too.
        let error = batch(&*control, empty_and_stale).expect_err("stale");
        assert_eq!(
            error.protocol(),
            Some(&ProtocolError::RevConflict {
                expected: Rev(5),
                current: Rev(0)
            })
        );
    }

    #[test]
    fn dropping_a_session_ends_only_its_streams() {
        let engine = MockEngine::new();
        let leaving = engine.connect();
        let staying = engine.connect();
        let mut leaving_events = leaving.events();
        let mut staying_events = staying.events();
        engine.emit(Event::Unknown);
        drop(leaving);
        // Buffered events are still delivered, then the stream ends.
        assert_eq!(
            poll_event(&mut leaving_events, Waker::noop()),
            Poll::Ready(Some(Event::Unknown))
        );
        assert_eq!(
            poll_event(&mut leaving_events, Waker::noop()),
            Poll::Ready(None)
        );
        assert_eq!(drain(&mut staying_events), vec![Event::Unknown]);
        assert_eq!(
            poll_event(&mut staying_events, Waker::noop()),
            Poll::Pending
        );
        // Events after the drop, including those caused by another session's call,
        // never reach the ended stream (review B2 on #27).
        engine.emit(Event::Unknown);
        block_on(staying.hello(hello())).expect("handshake");
        batch(&staying, Batch::single(create("a"))).expect("batch");
        assert_eq!(
            poll_event(&mut leaving_events, Waker::noop()),
            Poll::Ready(None)
        );
        // The unknown event, `state.changed`, and `studio.changed` for the first scene.
        assert_eq!(drain(&mut staying_events).len(), 3);
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
            ends_gesture: None,
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
            ends_gesture: None,
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
        // Two `state.changed`, and `studio.changed` for the first scene.
        assert_eq!(drain(&mut events).len(), 3, "a retry emits nothing");
        // Keys are per client: another client's identical key is a new batch.
        let other = connected(&engine);
        let error = batch(&*other, keyed).expect_err("scene a exists");
        assert_eq!(code(&error), Some("already_exists"));
    }

    #[test]
    fn subscriber_count_follows_live_streams() {
        let engine = MockEngine::new();
        assert_eq!(engine.subscriber_count(), 0);
        let first = engine.connect();
        let second = engine.connect();
        let first_events = first.events();
        let second_events = second.events();
        assert_eq!(engine.subscriber_count(), 2);
        // Dropping the stream unsubscribes before the session is dropped.
        drop(first_events);
        assert_eq!(engine.subscriber_count(), 1);
        drop(first);
        assert_eq!(engine.subscriber_count(), 1);
        // Closing the session ends a stream that is still held.
        drop(second);
        assert_eq!(engine.subscriber_count(), 0);
        drop(second_events);
        engine.emit(Event::OutputStateChanged {
            output_id: OutputId::new("gone"),
            state: OutputState::Stopped,
        });
        assert_eq!(engine.subscriber_count(), 0);
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
        assert_eq!(started.result, CallResult::action());

        let changed = |state| Event::OutputStateChanged {
            output_id: OutputId::new("yt"),
            state,
        };
        let expected = vec![
            Event::StateChanged {
                rev: Rev(1),
                client_id: ClientId::new("mock_2"),
                request_id: Some(response.id.clone()),
                cause: None,
            },
            Event::StudioChanged {
                program_scene: Some(SceneId::new("a")),
                preview_scene: Some(SceneId::new("a")),
                transition: None,
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
                    state: OutputState::Active,
                    key_set: false,
                    encoder: None,
                    encoder_sessions: 0,
                    hardware_sessions: 0,
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

    fn rtmp(url: &str) -> OutputKind {
        OutputKind::Rtmp {
            target: RtmpTarget::Generic { url: url.into() },
        }
    }

    fn action(control: &dyn Control, action: Action) -> Result<CallResult, ControlError> {
        block_on(control.call(Call::Action { action })).map(|response| response.result)
    }

    fn status(control: &dyn Control, output: &str) -> Result<(OutputState, bool), ControlError> {
        let output_id = OutputId::new(output);
        match query(control, Query::OutputStatus { output_id })? {
            QueryResult::OutputStatus { state, key_set, .. } => Ok((state, key_set)),
            other => panic!("not an output status: {other:?}"),
        }
    }

    /// A configured output gets a runtime state, as in the engine: it starts `stopped`,
    /// can't be deleted while it runs (`jstream_graph::Graph::apply_with_running`'s
    /// error), and is forgotten once deleted. A declared output is not in the collection.
    #[test]
    fn configured_outputs_follow_the_collection() {
        let engine = MockEngine::new().with_output(OutputId::new("declared"));
        let control = connected(&engine);
        let create = Batch::single(Edit::OutputCreate {
            output_id: OutputId::new("yt"),
            kind: rtmp("rtmp://a.rtmp.youtube.com/live2"),
        });
        assert_eq!(batch(&*control, create), Ok(Rev(1)));
        assert_eq!(status(&*control, "yt"), Ok((OutputState::Stopped, false)));
        assert_eq!(
            query(&*control, Query::OutputList),
            Ok(QueryResult::OutputList {
                rev: Rev(1),
                outputs: vec![OutputInfo {
                    output_id: OutputId::new("yt"),
                    kind: rtmp("rtmp://a.rtmp.youtube.com/live2"),
                    encoder_profile: None,
                    filters: Default::default(),
                    video_mix: None,
                }],
            })
        );

        let start = Action::OutputStart {
            output_id: OutputId::new("yt"),
        };
        assert_eq!(action(&*control, start), Ok(CallResult::action()));
        let delete = || {
            Batch::single(Edit::OutputDelete {
                output_id: OutputId::new("yt"),
            })
        };
        let running = batch(&*control, delete()).expect_err("running");
        assert_eq!(
            running.protocol(),
            Some(&ProtocolError::invalid_argument(
                "edits.0.output_id",
                "output `yt` is running; stop it first"
            ))
        );
        // A running target can still change; it applies from the next start.
        let retarget = Batch::single(Edit::OutputSetTarget {
            output_id: OutputId::new("yt"),
            target: RtmpTarget::Generic {
                url: "rtmps://a.rtmps.youtube.com/live2".into(),
            },
        });
        assert_eq!(batch(&*control, retarget), Ok(Rev(2)));
        let stop = Action::OutputStop {
            output_id: OutputId::new("yt"),
        };
        assert_eq!(action(&*control, stop), Ok(CallResult::action()));
        assert_eq!(batch(&*control, delete()), Ok(Rev(3)));
        let gone = status(&*control, "yt").expect_err("forgotten");
        assert_eq!(code(&gone), Some("not_found"));

        // The declared output stays known, but is not configured.
        assert_eq!(
            status(&*control, "declared"),
            Ok((OutputState::Stopped, false))
        );
        assert_eq!(
            query(&*control, Query::OutputList),
            Ok(QueryResult::OutputList {
                rev: Rev(3),
                outputs: vec![],
            })
        );
        let undeclare = Batch::single(Edit::OutputDelete {
            output_id: OutputId::new("declared"),
        });
        let error = batch(&*control, undeclare).expect_err("not configured");
        assert_eq!(code(&error), Some("not_found"));
    }

    /// `output.set_key` and `output.clear_key` follow the engine's rules, with the
    /// mock's in-memory store standing in for the OS credential store.
    #[test]
    fn stream_keys_are_write_only_and_follow_the_engine() {
        let engine = MockEngine::new().with_output(OutputId::new("declared"));
        let control = connected(&engine);
        let mut events = control.events();
        let create = Batch::single(Edit::OutputCreate {
            output_id: OutputId::new("yt"),
            kind: rtmp("rtmp://a.rtmp.youtube.com/live2"),
        });
        assert_eq!(batch(&*control, create), Ok(Rev(1)));
        let set = |output: &str, key: &str| {
            action(
                &*control,
                Action::OutputSetKey {
                    output_id: OutputId::new(output),
                    key: SecretString::new(key),
                },
            )
        };

        // Only a configured output takes a key: the check comes before the key's.
        for output in ["ghost", "declared"] {
            let error = set(output, "").expect_err("not configured");
            assert_eq!(
                error.protocol(),
                Some(&ProtocolError::NotFound {
                    entity: EntityKind::Output,
                    id: output.into(),
                })
            );
        }
        let long = "k".repeat(1025);
        for (key, reason) in [
            ("", "must not be empty"),
            (long.as_str(), "must be at most 1024 bytes"),
            (
                "secret key",
                "must not contain whitespace or control characters",
            ),
            (
                "secret\u{7}",
                "must not contain whitespace or control characters",
            ),
            ("\u{FEFF}secret", "must contain only ASCII characters"),
            ("s\u{e9}cret", "must contain only ASCII characters"),
        ] {
            let error = set("yt", key).expect_err("bad key");
            assert_eq!(
                error.protocol(),
                Some(&ProtocolError::invalid_argument("key", reason))
            );
        }
        assert_eq!(status(&*control, "yt"), Ok((OutputState::Stopped, false)));

        assert_eq!(set("yt", "abcd-1234"), Ok(CallResult::action()));
        assert_eq!(status(&*control, "yt"), Ok((OutputState::Stopped, true)));
        assert_eq!(
            engine
                .stream_key(&OutputId::new("yt"))
                .map(|key| key.expose().to_owned()),
            Some("abcd-1234".into())
        );
        // Not collection state: no `rev`, no event, and no read carries the key.
        assert_eq!(engine.rev(), Rev(1));
        let state = format!("{:?}", query(&*control, Query::StateGet));
        assert!(!state.contains("abcd"), "{state}");

        // As in the engine (#343): a new path on the same server keeps the key, a new
        // server removes it, and so does deleting the output.
        let retarget = |url: &str| {
            Batch::single(Edit::OutputSetTarget {
                output_id: OutputId::new("yt"),
                target: RtmpTarget::Generic { url: url.into() },
            })
        };
        let path_only = retarget("rtmp://A.rtmp.youtube.com:1935/other");
        assert_eq!(batch(&*control, path_only), Ok(Rev(2)));
        assert!(engine.stream_key(&OutputId::new("yt")).is_some());
        let elsewhere = retarget("rtmp://attacker.example/live2");
        assert_eq!(batch(&*control, elsewhere), Ok(Rev(3)));
        assert!(engine.stream_key(&OutputId::new("yt")).is_none());
        assert_eq!(set("yt", "abcd-1234"), Ok(CallResult::action()));
        let delete = Batch::single(Edit::OutputDelete {
            output_id: OutputId::new("yt"),
        });
        assert_eq!(batch(&*control, delete), Ok(Rev(4)));
        assert!(engine.stream_key(&OutputId::new("yt")).is_none());
        // Clearing an unknown output's key, or a missing key, succeeds.
        for _ in 0..2 {
            let clear = Action::OutputClearKey {
                output_id: OutputId::new("yt"),
            };
            assert_eq!(action(&*control, clear), Ok(CallResult::action()));
        }
        assert!(engine.stream_key(&OutputId::new("yt")).is_none());
        let events = drain(&mut events);
        assert_eq!(events.len(), 4, "only the four batches: {events:?}");
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

    /// The folder, label and metadata edits (ADR-0024 §3 and §4) with `jstream-graph`'s
    /// checks: missing parents and labels, a folder inside itself, colours and icons,
    /// a label listed twice, and the in-use refusals with their blockers. Reads report
    /// what they set, `source.duplicate` copies the metadata, and undo restores it
    /// through a scene's and a source's delete.
    #[test]
    fn folders_labels_and_metadata_follow_the_graph() {
        use jstream_protocol::{Blocker, FolderId, LabelId, SceneMeta, SourceMeta};
        fn field(error: ControlError) -> String {
            match error.protocol() {
                Some(ProtocolError::InvalidArgument { field, .. }) => field.clone(),
                other => panic!("not invalid_argument: {other:?}"),
            }
        }
        fn folder(id: &str, parent: Option<&str>) -> Edit {
            Edit::FolderCreate {
                folder_id: FolderId::new(id),
                name: id.to_uppercase(),
                parent: parent.map(FolderId::new),
            }
        }
        fn label(id: &str, color: &str) -> Edit {
            Edit::LabelCreate {
                label_id: LabelId::new(id),
                name: id.to_uppercase(),
                color: color.into(),
                icon: None,
            }
        }
        fn many(edits: Vec<Edit>) -> Batch {
            Batch {
                edits,
                ..Batch::single(create("unused"))
            }
        }
        fn labelled(labels: &[&str]) -> Edit {
            Edit::SourceSetMeta {
                source_id: SourceId::new("webcam"),
                meta: SourceMeta {
                    labels: labels.iter().map(|l| LabelId::new(*l)).collect(),
                    favourite: true,
                },
            }
        }
        let engine = MockEngine::new();
        let control = connected(&engine);
        let setup = many(vec![
            create("a"),
            webcam(),
            add("a", "i1", "webcam"),
            folder("outer", None),
            folder("inner", Some("outer")),
            label("sponsor", "#ff8800"),
            label("camera", "#00FF00"),
        ]);
        assert_eq!(batch(&*control, setup), Ok(Rev(1)));
        let before = state(&*control);

        let refused = |edit: Edit| batch(&*control, Batch::single(edit)).expect_err("refused");
        assert_eq!(
            code(&refused(folder("x", Some("ghost")))),
            Some("not_found")
        );
        assert_eq!(
            code(&refused(folder("outer", None))),
            Some("already_exists")
        );
        let cycle = Edit::FolderSetParent {
            folder_id: FolderId::new("outer"),
            parent: Some(FolderId::new("inner")),
        };
        assert_eq!(field(refused(cycle)), "edits.0.parent");
        assert_eq!(field(refused(label("x", "orange"))), "edits.0.color");
        let icon = Edit::LabelUpdate {
            label_id: LabelId::new("sponsor"),
            name: "Sponsor".into(),
            color: "#ff8800".into(),
            icon: Some(String::new()),
        };
        assert_eq!(field(refused(icon)), "edits.0.icon");
        assert_eq!(code(&refused(labelled(&["ghost"]))), Some("not_found"));
        assert_eq!(
            field(refused(labelled(&["sponsor", "sponsor"]))),
            "edits.0.labels"
        );
        let bad_meta = Edit::SceneSetMeta {
            scene_id: SceneId::new("a"),
            meta: SceneMeta {
                color: Some("#123".into()),
                ..SceneMeta::default()
            },
        };
        assert_eq!(field(refused(bad_meta)), "edits.0.color");

        let organise = many(vec![
            Edit::SceneSetFolder {
                scene_id: SceneId::new("a"),
                folder_id: Some(FolderId::new("inner")),
            },
            Edit::SceneSetMeta {
                scene_id: SceneId::new("a"),
                meta: SceneMeta {
                    favourite: true,
                    ..SceneMeta::default()
                },
            },
            labelled(&["sponsor", "camera"]),
            Edit::FolderReorder {
                folder_id: FolderId::new("inner"),
                index: 0,
            },
        ]);
        assert_eq!(batch(&*control, organise), Ok(Rev(2)));
        let Ok(QueryResult::SourceList { sources, .. }) = query(&*control, Query::SourceList)
        else {
            panic!("a source list");
        };
        assert_eq!(
            sources[0].meta.labels,
            [LabelId::new("camera"), LabelId::new("sponsor")]
        );
        let Ok(QueryResult::SceneList { scenes, .. }) = query(&*control, Query::SceneList) else {
            panic!("a scene list");
        };
        assert_eq!(scenes[0].folder, Some(FolderId::new("inner")));
        assert!(scenes[0].meta.favourite);
        let Ok(QueryResult::StateGet { folders, .. }) = query(&*control, Query::StateGet) else {
            panic!("a state");
        };
        assert_eq!(folders[0].folder_id, FolderId::new("inner"));

        let in_use = refused(Edit::FolderDelete {
            folder_id: FolderId::new("inner"),
        });
        let Some(ProtocolError::InvalidArgument {
            field: in_use_field,
            blockers,
            ..
        }) = in_use.protocol()
        else {
            panic!("not invalid_argument: {in_use:?}");
        };
        assert_eq!(in_use_field, "edits.0.folder_id");
        assert_eq!(
            blockers,
            &[Blocker {
                entity: EntityKind::Scene,
                id: "a".into(),
                field: "folder".into(),
            }]
        );
        let blocked = |error: ControlError| match error.protocol() {
            Some(ProtocolError::InvalidArgument {
                field, blockers, ..
            }) => (field.clone(), blockers.clone()),
            other => panic!("not invalid_argument: {other:?}"),
        };
        let blocker = |entity, id: &str, field: &str| Blocker {
            entity,
            id: id.into(),
            field: field.into(),
        };
        let label_in_use = refused(Edit::LabelDelete {
            label_id: LabelId::new("sponsor"),
        });
        assert_eq!(
            blocked(label_in_use),
            (
                "edits.0.label_id".into(),
                vec![blocker(EntityKind::Source, "webcam", "labels")]
            )
        );
        let outer = refused(Edit::FolderDelete {
            folder_id: FolderId::new("outer"),
        });
        assert_eq!(
            blocked(outer),
            (
                "edits.0.folder_id".into(),
                vec![blocker(EntityKind::Folder, "inner", "parent")]
            )
        );

        // The copy carries the labels and favourite flag.
        let copy = Edit::SourceDuplicate {
            source_id: SourceId::new("webcam"),
            new_source_id: SourceId::new("webcam_2"),
            name: "Copy".into(),
        };
        assert_eq!(batch(&*control, Batch::single(copy)), Ok(Rev(3)));
        let Ok(QueryResult::SourceList { sources, .. }) = query(&*control, Query::SourceList)
        else {
            panic!("a source list");
        };
        assert_eq!(sources[0].meta, sources[1].meta);
        assert!(undo(&*control).is_ok());

        // Deleting the scene and the source, then undoing, restores their metadata.
        let organised = state(&*control);
        let delete = many(vec![
            Edit::SceneDelete {
                scene_id: SceneId::new("a"),
            },
            Edit::SourceDelete {
                source_id: SourceId::new("webcam"),
            },
        ]);
        assert!(batch(&*control, delete).is_ok());
        assert!(undo(&*control).is_ok());
        assert_eq!(state(&*control), organised);
        assert!(undo(&*control).is_ok());
        assert_eq!(state(&*control), before);
    }
}
