//! The [`Engine`]: the scene graph, the outputs' runtime state and the event subscribers,
//! behind one lock, and the execution of every protocol call against them.
//!
//! Collection changes and the events they queue happen under that lock, so every
//! subscriber sees them in the order they happened, and `state.changed` events in `rev`
//! order. A pipeline start or stop holds the lock only while it changes output state.
//! Device open and thread joins happen after it is released. Subscribers are woken only
//! after the lock is released.

use crate::activity_log::{self, Activity};
use crate::audio::{AudioRouting, MixerHandles};
use crate::audio_sources::AudioSources;
use crate::audit::{self, Audit, AuditEntry, AuditSink};
use crate::collection::{CollectionFile, CollectionFileError};
use crate::drops::{self, DropLedger, DropSource, DropSourceId};
use crate::events::{QueueRef, Subscribers, Subscription};
use crate::forget::ForgetSources;
use crate::gesture::{Ended, Gestures};
use crate::history::History;
use crate::live::{Built, LiveHost, LiveSources, LoadedAsset};
use crate::load::LoadMeter;
use crate::lock;
use crate::outputs::Outputs;
use crate::plugins::kinds::{self as clap_kinds, ClapScan};
use crate::preview::{
    Current, Opening, PreviewCapture, PreviewHub, PreviewSource, SessionPreviews,
};
use crate::runtime::{
    Destination, EncodeKey, Encoders, Gate, LivePublish, MediaHost, MixKey, OpenPublish,
    OutputEncode, OutputStats, Pending, PipelineEvent, Release, RenderHub, RenderSnapshot, Running,
    SharedVideoDevices, SizeChange, StartError, VcamOut, launch, launch_program, launch_vcam,
    parts_from,
};
use crate::session::InProcessControl;
use crate::snapshot::{
    self, CaptureKey, Frame, Look, SnapshotReply, SnapshotWorker, SourceFrameError,
};
use crate::studio::{self, Studio};
use crate::transition;
use crate::youtube::{NO_YOUTUBE_CLIENT, Youtube, YoutubeSetup};
use jstream_audio::OutputTapId;
use jstream_graph::Graph;
use jstream_media::FrameSize;
use jstream_platform::{
    Backends, CaptureTarget, CaptureTargetInfo, GpuContext, PluginHosts, ProcessCpu, Secret,
    StreamKeyStore,
};
use jstream_protocol::{
    Action, ActionResult, Batch, Call, CallResult, CaptureTargetKind, ClientId, DropCounts, Edit,
    EncoderProfile, EncoderSessionInfo, EngineLoad, EntityKind, Event, FilterChain, GestureId,
    HistoryCall, HistoryDirection, IdempotencyKey, LiveOutcome, OutputId, OutputKind, OutputState,
    PixelSize, PreviewSettings, ProtocolError, Query, QueryResult, RecordContainer, RequestId, Rev,
    RtmpTarget, SceneId, SecretString, SourceId, SourceSize, TransitionPresetId, TransitionStarted,
    YoutubeAuth,
};
use jstream_sources::AudioScopes;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::task::Waker;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

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
    /// The latest audio of the program mix and of every bus a source draws: the mixer
    /// thread pushes each block, and the `waveform` and `spectrum` sources draw it.
    audio_scopes: Arc<AudioScopes>,
    /// The YouTube sign-in and Live API client, if the host gave a setup
    /// ([`Engine::with_youtube`]). On its own lock, taken only to clone the handle: no
    /// YouTube call runs under the collection lock.
    youtube: Mutex<Option<Arc<Youtube>>>,
    /// The preview thread, which draws and encodes every session's preview streams.
    previews: PreviewHub,
    /// When the engine started, the process CPU time and the idle heartbeat, for
    /// `stats.tick`'s load. Its lock is its own, never taken with the collection's.
    load: LoadMeter,
    /// The WASM automation plugins installed this run (ADR-0069). On its own lock,
    /// never taken with the collection lock held, nor while a plugin runs.
    #[cfg(feature = "wasm-plugins")]
    automation: crate::plugins::automation::Registry,
}

/// A handle to an [`Engine`] that doesn't keep it alive: what a plugin's thread holds,
/// so an engine with plugins installed still ends when its last handle and session go.
#[cfg(feature = "wasm-plugins")]
#[derive(Clone)]
pub(crate) struct WeakEngine {
    shared: Weak<Shared>,
    media: Option<Weak<MediaHost>>,
    drops: Weak<Mutex<DropLedger>>,
}

#[cfg(feature = "wasm-plugins")]
impl WeakEngine {
    /// The engine, while any handle or session keeps it alive.
    pub(crate) fn upgrade(&self) -> Option<Engine> {
        let media = match &self.media {
            Some(media) => Some(media.upgrade()?),
            None => None,
        };
        Some(Engine {
            shared: self.shared.upgrade()?,
            media,
            drops: self.drops.upgrade()?,
        })
    }
}

impl Default for Engine {
    fn default() -> Self {
        let audio_scopes = Arc::new(AudioScopes::new());
        Self {
            shared: Arc::new_cyclic(|shared: &Weak<Shared>| Shared {
                state: Mutex::new({
                    let mut state = State::default();
                    state.graph = Graph::new().with_kinds(crate::kinds::builtin());
                    let host: Weak<dyn LiveHost> = shared.clone();
                    state.live = LiveSources::new(host);
                    state.shared = shared.clone();
                    state.live.set_audio_scopes(Arc::clone(&audio_scopes));
                    state
                }),
                snapshots: SnapshotWorker::default(),
                keys: Mutex::new(None),
                audit: Audit::default(),
                collection_file: Mutex::new(None),
                audio_scopes,
                youtube: Mutex::new(None),
                previews: PreviewHub::default(),
                load: LoadMeter::default(),
                #[cfg(feature = "wasm-plugins")]
                automation: crate::plugins::automation::Registry::default(),
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
    /// This is runtime state: it does not change `rev`, and the only event it can send
    /// is a `source.size_changed` when the frame changes the source's size. A snapshot
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
        let mut wake = Vec::new();
        state.sync_sizes(&mut wake);
        drop(state);
        wake_all(wake);
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

    /// Scans the CLAP search directories with `hosts` (ADR-0035) and adds each audio
    /// effect found to `kind.list` and `kind.describe` as an audio-filter kind,
    /// `clap/<plugin id>`, with the plugin's parameters as its settings. A kind an
    /// earlier scan found stays. Each plugin file is scanned in its own host process, so
    /// this can take seconds: call it from a worker thread. The engine's locks are held
    /// only to add the kinds, after the scan.
    ///
    /// From then on a source's audio chain runs its CLAP filters, each in a host process
    /// `hosts` starts. A collection may hold CLAP filters before the scan that
    /// describes their kinds (it loads before the scan runs): they are bypassed until
    /// then.
    pub fn scan_clap_plugins(&self, hosts: Arc<dyn PluginHosts>) -> ClapScan {
        let scan = clap_kinds::scan(hosts.as_ref());
        let mut state = lock(&self.shared.state);
        state.graph = std::mem::take(&mut state.graph)
            .with_kinds(scan.kinds.iter().map(|k| k.descriptor.clone()));
        state.audio.add_clap_plugins(
            hosts,
            scan.kinds.iter().map(|k| {
                (
                    k.descriptor.kind_id.clone(),
                    (k.file.clone(), k.plugin_id.clone()),
                )
            }),
        );
        scan
    }

    /// Gives the engine YouTube (ADR-0058): a Google OAuth client, the HTTPS transport
    /// and the store for the account's refresh token. The host passes
    /// [`YoutubeSetup::built_in`] when the build has a built-in client (ADR-0058), or a
    /// developer's override built with [`YoutubeSetup::new`]. The YouTube actions
    /// and queries then work, and an output with a `youtube` target can start. Without
    /// it, YouTube's API features are off: `youtube.status` reports `unavailable`, the
    /// other YouTube calls fail with `action_failed` saying the build has no client, and
    /// clients hide them. Reads the store once, here: a token kept by an earlier sign-in
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
    /// is written to it; the in-memory activity log (`activity.list`, ADR-0083) keeps
    /// the last entries either way. Every clone of the engine shares it.
    #[must_use]
    pub fn with_audit(self, sink: Arc<dyn AuditSink>) -> Self {
        self.shared.audit.set(sink);
        self
    }

    /// Saves the installed WASM plugins and their grants in `plugins.json` in the folder
    /// `dir` from now on, after every install, grant, enable and disable, and loads the
    /// ones saved there now: each that was enabled is loaded again, as `plugin.enable`
    /// loads it (ADR-0076). The folder and file are created if missing. A file that can't
    /// be read, or that a newer version wrote, is logged at `error` and left as it is, and
    /// then no plugin is loaded or saved this run. Built without the `wasm-plugins`
    /// feature, the engine runs no plugins and this does nothing.
    ///
    /// Call it once the engine is built, after [`with_backends`](Self::with_backends)
    /// and the other `with_` methods: a plugin it loads holds a handle to the engine as
    /// it is at this call, so one opened earlier would act on an engine without its
    /// media host. A second call is logged and ignored.
    pub fn open_plugin_dir(&self, dir: impl Into<PathBuf>) {
        #[cfg(feature = "wasm-plugins")]
        crate::plugins::automation::open_dir(self, dir.into());
        #[cfg(not(feature = "wasm-plugins"))]
        {
            let _ = dir.into();
            tracing::debug!("built without WASM plugins: no plugin folder to open");
        }
    }

    /// Loads the collection from the file at `path` and saves it there after every
    /// change. Configure this before [`connect`](Self::connect): it replaces the
    /// collection and its `rev` with the file's.
    ///
    /// The file is a versioned JSON collection (`jstream-graph`), upgraded on load if it
    /// is older, and checked as edits are, against this engine's kinds. When there is
    /// no file at `path`, it is created with the current collection, given the
    /// prototype's four sample labels if it has none
    /// ([`Graph::with_sample_labels`](jstream_graph::Graph::with_sample_labels)), still
    /// at its `rev`, so a path that can't be written fails here rather than at the first
    /// change.
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
            state.route_audio();
            // The loaded collection is what the program mix draws now.
            state.publish_render();
            // No client is connected yet, so nobody is woken.
            state.sync_sizes(&mut Vec::new());
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
    /// opens its file itself. Nor does the virtual camera (`OutputKind::VirtualCam`),
    /// which publishes to the backends' [`VcamSink`](jstream_platform::VcamSink).
    ///
    /// While an `output.start` of this output is opening, it answers the kind the start
    /// saw when it checked the output's key, not the collection's current one (#343). A
    /// retarget that lands during the opener then can't pair the new server's URL with
    /// the old server's key: the opener connects to the server the start was checked
    /// for, and the next start sees the new target.
    pub fn output_kind(&self, output_id: &OutputId) -> Option<OutputKind> {
        if let Some(kind) = self.starting_kind(output_id) {
            return kind;
        }
        lock(&self.shared.state)
            .graph
            .state()
            .output(output_id)
            .map(|output| output.kind().clone())
    }

    /// The kind an `output.start` of this output saw when it checked the output's key,
    /// while that start is opening (#343); `None` when no start of it is pending. Takes
    /// the gate lock alone.
    fn starting_kind(&self, output_id: &OutputId) -> Option<Option<OutputKind>> {
        let host = self.media.as_ref()?;
        let gate = lock(&host.gate);
        gate.pending
            .as_ref()
            .filter(|pending| pending.output_id == *output_id)
            .map(|pending| pending.kind.clone())
    }

    /// Injects platform backends. `output.start` then runs the media pipeline
    /// instead of the placeholder state machine. Configure this before
    /// [`connect`](Self::connect); clones made earlier do not see it.
    ///
    /// A start still needs a publish target ([`with_publish`](Self::with_publish))
    /// or an opener ([`with_publish_opener`](Self::with_publish_opener)). Without
    /// either it fails with `action_failed` and changes no output state. A recording
    /// output needs neither: its start records to the file the collection names. Nor
    /// does a virtual-camera output: its start publishes the program, video only, to
    /// `backends.vcam` at the canvas size. The engine keeps that sink for its whole
    /// life, starting it on each such start and stopping it on the stop.
    #[must_use]
    pub fn with_backends(mut self, mut backends: Backends) -> Self {
        // Animated sources run on the platform's media clock from here on, and media
        // files play through the platform's decoders.
        {
            let mut state = lock(&self.shared.state);
            let state = &mut *state;
            state.live.set_clock(Arc::clone(&backends.clock));
            state
                .live
                .set_media(Arc::clone(&backends.media), state.graph.state());
            state.forget = ForgetSources::spawn(Arc::clone(&backends.capture));
        }
        // A webcam or capture card opens once, shared by the program stage's capture of
        // it and the preview's.
        backends.video_devices = Arc::new(SharedVideoDevices::new(backends.video_devices));
        // The preview captures what the program shows itself.
        self.shared.previews.set_capture(PreviewCapture {
            capture: Arc::clone(&backends.capture),
            devices: Arc::clone(&backends.video_devices),
            codecs: Arc::clone(&backends.codecs),
            clock: backends.clock.timebase(),
        });
        let host = self.host();
        let vcam = backends.vcam.take();
        host.has_vcam.store(vcam.is_some(), Ordering::Release);
        *lock(&host.vcam) = vcam;
        *lock(&host.backends) = Some(backends);
        self
    }

    /// Gives `browser` sources and their `youtube_chat` preset the browser engine they
    /// open their pages in (ADR-0021). Without one, such a source shows nothing and logs
    /// that no backend is set; giving one builds the ones already there again.
    /// The host also hands the backend a GPU device (`use_device`), one it opens on the
    /// media adapter itself, for paints before a program stage; while a stage runs the
    /// engine lends the backend the stage's own context instead (`use_gpu`).
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

    /// Reads the process's CPU time from `cpu` for each `stats.tick`'s load from now
    /// on (protocol 0.48). Without one a tick's `cpu_ms` is left out. The engine's
    /// stats threads call it about once a second, outside the collection lock.
    pub fn set_process_cpu(&self, cpu: Arc<dyn ProcessCpu>) {
        self.shared.load.set_cpu(cpu);
    }

    /// Starts the engine's stats heartbeat: a `jstream-stats-idle` thread that emits a
    /// `stats.tick` about once a second while no output runs, so a client sees the
    /// engine's load while it is idle too (protocol 0.48). While the program stage runs,
    /// its own sampler emits the ticks and the heartbeat stays quiet. A host calls it
    /// once; later calls do nothing. The thread holds the engine weakly and ends once
    /// the last engine handle is gone.
    pub fn start_stats_heartbeat(&self) {
        if !self.shared.load.claim_heartbeat() {
            return;
        }
        let shared = Arc::downgrade(&self.shared);
        let ledger = Arc::downgrade(&self.drops);
        if let Err(err) = thread::Builder::new()
            .name("jstream-stats-idle".into())
            .spawn(move || stats_heartbeat(&shared, &ledger))
        {
            tracing::error!(error = %err, "stats heartbeat did not start");
        }
    }

    /// The ledger the pipeline's [`DropGuard`](crate::drops::DropGuard) removes from.
    pub(crate) fn drop_ledger(&self) -> Arc<Mutex<DropLedger>> {
        Arc::clone(&self.drops)
    }

    /// The scopes the mixer thread feeds: the program mix's, and one per bus a source
    /// draws.
    pub(crate) fn audio_scopes(&self) -> Arc<AudioScopes> {
        Arc::clone(&self.shared.audio_scopes)
    }

    /// The scope the program mix's audio goes to.
    #[cfg(test)]
    pub(crate) fn audio_scope(&self) -> Arc<jstream_sources::AudioScope> {
        Arc::clone(self.shared.audio_scopes.program())
    }

    /// The body of the program stage's `jstream-capture-sizes` thread: records each
    /// capture's size as the render thread reports it, and announces the sources whose
    /// size it changes (ADR-0022 §4). When the render thread lets go of its captures,
    /// every capture's size is forgotten. It holds the shared collection, not the
    /// [`MediaHost`], like the ticker.
    pub(crate) fn capture_sizes_watch(
        &self,
        changes: Receiver<SizeChange>,
    ) -> impl FnOnce() + Send + 'static {
        let shared = Arc::clone(&self.shared);
        move || {
            for change in changes {
                shared.capture_sizes(|sizes| match change.size {
                    Some(size) => {
                        sizes.insert(change.key, size);
                    }
                    None => {
                        sizes.remove(&change.key);
                    }
                });
            }
            shared.capture_sizes(BTreeMap::clear);
        }
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

    /// Routes the collection's audio into the program's mixer, until the returned
    /// closure is called, when the program stops. The closure holds the collection, not
    /// the [`MediaHost`], like the ticker.
    pub(crate) fn attach_mixer(&self, mixer: MixerHandles) -> Box<dyn FnOnce() + Send> {
        let generation = {
            let mut state = lock(&self.shared.state);
            let state = &mut *state;
            let inputs = state.audio.inputs();
            state
                .routing
                .attach(mixer, state.graph.state(), state.studio.program(), &inputs)
        };
        let shared = Arc::clone(&self.shared);
        Box::new(move || lock(&shared.state).routing.detach(generation))
    }

    /// Adds an output tap for `output_id` to the program's mixer, running the output's
    /// audio chain, and returns its id and the closure that removes it once the output
    /// has stopped. The closure holds the collection, not the [`MediaHost`].
    ///
    /// # Errors
    /// When no program mixer runs, or the mixer refuses the tap.
    pub(crate) fn add_output_tap(
        &self,
        output_id: &OutputId,
    ) -> Result<(OutputTapId, Release), String> {
        let tap = {
            let mut state = lock(&self.shared.state);
            let state = &mut *state;
            state
                .routing
                .add_output_tap(output_id, state.graph.state())?
        };
        let id = tap.id();
        let shared = Arc::clone(&self.shared);
        let output_id = output_id.clone();
        Ok((
            id,
            Box::new(move || {
                lock(&shared.state)
                    .routing
                    .remove_output_tap(&output_id, id);
            }),
        ))
    }

    /// Adds kinds the built-in registry doesn't have: the parity fixture's `mic`, an
    /// audio kind with no parameters.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_extra_kinds(self, kinds: Vec<jstream_protocol::KindDescriptor>) -> Self {
        {
            let mut state = lock(&self.shared.state);
            let graph = std::mem::take(&mut state.graph);
            state.graph = graph.with_kinds(kinds);
        }
        self
    }

    /// Emits one `meters.tick` now, if the program mixer runs. The stats ticker calls
    /// this about ten times a second.
    #[cfg(test)]
    pub(crate) fn emit_meters_tick(&self) {
        let mut wake = Vec::new();
        lock(&self.shared.state).emit_meters(&mut wake);
        wake_all(wake);
    }

    /// Lists `input` as audio source `source_id`'s mixer input, as the program stage
    /// does when it opens one, and routes it as the collection says.
    #[cfg(test)]
    pub(crate) fn set_audio_input(&self, source_id: SourceId, input: jstream_audio::InputControl) {
        let mut state = lock(&self.shared.state);
        state.audio.insert_input(source_id, input);
        state.route_audio();
    }

    /// Lets live sources that paint on the GPU (`browser`) paint on `context`, the
    /// program stage's platform GPU context, and re-plans the program mix. The returned
    /// call takes the context away again; the stage makes it before it stops.
    pub(crate) fn use_gpu(&self, context: &GpuContext) -> Box<dyn FnOnce() + Send + Sync> {
        set_gpu(&self.shared, Some(context));
        let shared = Arc::clone(&self.shared);
        Box::new(move || set_gpu(&shared, None))
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
        self.shared.previews.set_draw(crate::preview::cpu_draw());
        self.with_draw(crate::runtime::cpu_draw())
    }

    /// Replaces how the preview thread draws, for tests that watch what it is given.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_preview_draw(self, draw: crate::preview::OpenPreviewDraw) -> Self {
        self.shared.previews.set_draw(draw);
        self
    }

    /// Opens a new client session. It must send `hello` before any call.
    pub fn connect(&self) -> InProcessControl {
        InProcessControl::new(self.clone())
    }

    /// A handle to this engine that doesn't keep it alive.
    #[cfg(feature = "wasm-plugins")]
    pub(crate) fn downgrade(&self) -> WeakEngine {
        WeakEngine {
            shared: Arc::downgrade(&self.shared),
            media: self.media.as_ref().map(Arc::downgrade),
            drops: Arc::downgrade(&self.drops),
        }
    }

    /// Sends `event` to every subscriber, outside any call.
    #[cfg(feature = "wasm-plugins")]
    pub(crate) fn emit(&self, event: &Event) {
        let mut wake = Vec::new();
        lock(&self.shared.state).subscribers.emit(event, &mut wake);
        wake_all(wake);
    }

    /// The installed WASM automation plugins.
    #[cfg(feature = "wasm-plugins")]
    pub(crate) fn automation(&self) -> &crate::plugins::automation::Registry {
        &self.shared.automation
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

    /// Drops `client_id`'s undo and redo history and expires its open gestures: its
    /// session ended (ADR-0032). Each gesture's parameters show their collection values
    /// again, `live.ended` (`expired`) goes out after any values not sent yet, and each
    /// gets an audit entry.
    pub(crate) fn forget_client(&self, client_id: &ClientId) {
        let mut wake = Vec::new();
        let (ended, rev, client_name) = {
            let mut state = lock(&self.shared.state);
            state.history.forget(client_id);
            for gesture_id in state.gestures.of_client(client_id) {
                state.end_gesture(
                    client_id,
                    &gesture_id,
                    LiveOutcome::Expired,
                    None,
                    &mut wake,
                );
            }
            let name = state
                .client_names
                .get(client_id)
                .cloned()
                .unwrap_or_default();
            (std::mem::take(&mut state.ended), state.graph.rev(), name)
        };
        wake_all(wake);
        self.audit_gestures(ended, rev, &client_name);
    }

    /// Records one audit entry for each gesture in `ended`, which ended at `rev`
    /// cancelled or expired (ADR-0032): a committed gesture's entry is its batch's.
    fn audit_gestures(&self, ended: Vec<(Option<RequestId>, Ended)>, rev: Rev, client_name: &str) {
        if ended.is_empty() {
            return;
        }
        for (request_id, ended) in ended {
            self.record(audit::gesture_entry(
                ended,
                client_name.to_owned(),
                request_id,
                rev,
            ));
        }
    }

    /// Records `entry`: it joins the activity log, then goes to the host's audit sink,
    /// if any, with no lock held.
    fn record(&self, entry: AuditEntry) {
        lock(&self.shared.state).activity.push(&entry);
        if let Some(sink) = self.shared.audit.sink() {
            sink.record(entry);
        }
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

    /// What an audit entry needs before `call` runs: the current `rev`, the client's
    /// name from its latest `hello`, and the activity log's sentence for the call.
    fn audit_context(&self, client_id: &ClientId, call: &Call) -> audit::Pending {
        let state = lock(&self.shared.state);
        let name = state
            .client_names
            .get(client_id)
            .cloned()
            .unwrap_or_default();
        let summary = activity_log::describe(call, state.graph.state());
        audit::Pending::new(call, state.graph.rev(), name, summary)
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
        previews: &SessionPreviews,
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
        let audit = Audit::records(&call).then(|| self.audit_context(client_id, &call));
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
        let result = if let Some(result) = self.plugin_call(&call) {
            result
        } else if let Some(result) = self.youtube_call(&call) {
            result
        } else if let Call::Action {
            action: action @ (Action::OutputSetKey { .. } | Action::OutputClearKey { .. }),
        } = call
        {
            self.key_action(action)
        } else if matches!(
            call,
            Call::Query {
                query: Query::CaptureTargets
            }
        ) {
            self.capture_targets()
        } else if let Call::Action {
            action: action @ (Action::PreviewSubscribe { .. } | Action::PreviewUnsubscribe { .. }),
        } = call
        {
            self.preview_action(previews, action)
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
            let (result, stale, ended, rev, client_name) = {
                let mut state = lock(&self.shared.state);
                let (result, stale) =
                    state.execute_and_unbind_keys(client_id, request_id, call, &mut wake);
                let ended = std::mem::take(&mut state.ended);
                let name = if ended.is_empty() {
                    String::new()
                } else {
                    state
                        .client_names
                        .get(client_id)
                        .cloned()
                        .unwrap_or_default()
                };
                (result, stale, ended, state.graph.rev(), name)
            };
            for waker in wake {
                waker.wake();
            }
            self.remove_stale_keys(stale);
            self.audit_gestures(ended, rev, &client_name);
            self.with_key_set(result)
        };
        // Before the save, so a client that reads `activity.list` after the call's events
        // usually finds the entry; one that reads faster than this finds it on its next
        // read.
        if let Some(pending) = audit {
            self.record(pending.finish(client_id, request_id, &result));
        }
        if matches!(
            result,
            Ok(CallResult::Batch { .. } | CallResult::History { .. })
        ) {
            self.save_collection();
        }
        match (&result, output) {
            (Ok(CallResult::Batch { rev, .. }), _) => tracing::debug!(rev = %rev, "batch applied"),
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
        Executed::Ready(Box::new(result))
    }

    /// `plugin.install`, `plugin.grant`, `plugin.enable` and `plugin.list`, or `None` for any other call
    /// or without the `wasm-plugins` feature. They run with no engine lock held: a plugin
    /// loads, and may call back into the engine, while its install waits.
    #[cfg(feature = "wasm-plugins")]
    fn plugin_call(&self, call: &Call) -> Option<Result<CallResult, ProtocolError>> {
        match call {
            Call::Action { action } => crate::plugins::automation::action(self, action),
            Call::Query {
                query: Query::PluginList,
            } => Some(Ok(CallResult::Query {
                result: QueryResult::PluginList {
                    plugins: crate::plugins::automation::list(self),
                },
            })),
            _ => None,
        }
    }

    #[cfg(not(feature = "wasm-plugins"))]
    fn plugin_call(&self, _: &Call) -> Option<Result<CallResult, ProtocolError>> {
        None
    }

    /// The YouTube actions and queries, or `None` for any other call. They run with no
    /// engine lock held, since most of them call the Live API.
    fn youtube_call(&self, call: &Call) -> Option<Result<CallResult, ProtocolError>> {
        let youtube = self.youtube();
        let unavailable = || ProtocolError::ActionFailed {
            reason: NO_YOUTUBE_CLIENT.into(),
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
                    .map(|()| CallResult::action()),
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
            Call::Query { query: q } => match q {
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
                } => {
                    query(youtube.ok_or_else(unavailable).and_then(|y| {
                        y.list_broadcasts(*status, *broadcast_type, page_token.clone())
                    }))
                }
                Query::YoutubeListStreams { page_token } => query(
                    youtube
                        .ok_or_else(unavailable)
                        .and_then(|y| y.list_streams(page_token.clone())),
                ),
                Query::YoutubeStats => query(youtube.ok_or_else(unavailable).map(|y| y.audience())),
                _ => return None,
            },
            _ => return None,
        })
    }

    /// `preview.subscribe` and `preview.unsubscribe`, on the calling session's
    /// previews. Nothing here holds the collection lock past reading `rev`.
    fn preview_action(
        &self,
        previews: &SessionPreviews,
        action: Action,
    ) -> Result<CallResult, ProtocolError> {
        match action {
            Action::PreviewSubscribe {
                channel,
                max_width,
                max_height,
                fps,
                codecs,
                scene_id,
                source,
            } => {
                let settings = PreviewSettings::resolve(max_width, max_height, fps, &codecs)?;
                if scene_id.is_some() && source.is_some() {
                    return Err(ProtocolError::invalid_argument(
                        "source",
                        "a preview stream shows a scene or a source, not both",
                    ));
                }
                let (rev, kinds, canvas) = {
                    let state = lock(&self.shared.state);
                    if let Some(scene_id) = &scene_id
                        && state.graph.state().scene(scene_id).is_none()
                    {
                        return Err(ProtocolError::NotFound {
                            entity: EntityKind::Scene,
                            id: scene_id.as_str().to_owned(),
                        });
                    }
                    // Only the kinds and the canvas: the draft is planned unlocked.
                    let kinds = source
                        .as_ref()
                        .map(|_| state.graph.kinds().cloned().collect::<Vec<_>>());
                    (state.graph.rev(), kinds, state.graph.state().canvas())
                };
                let draft = match (&source, kinds) {
                    (Some(source), Some(kinds)) => {
                        Some(crate::preview::plan_draft(kinds, canvas, source)?)
                    }
                    _ => None,
                };
                let codecs = self.media.as_ref().and_then(|host| {
                    lock(&host.backends)
                        .as_ref()
                        .map(|backends| Arc::clone(&backends.codecs))
                });
                let shared: Arc<dyn PreviewSource> = self.shared.clone();
                previews.subscribe(
                    &self.shared.previews,
                    Arc::downgrade(&shared),
                    channel,
                    settings,
                    Opening {
                        codecs,
                        rev,
                        scene: scene_id.clone(),
                        draft,
                    },
                )?;
                if scene_id.is_some() {
                    // Plans the scene, which the stream waits for.
                    self.shared.replan_and_sync();
                }
                Ok(CallResult::Action { result: None })
            }
            Action::PreviewUnsubscribe { channel } => {
                if previews.unsubscribe(channel) {
                    // Stops planning the scene, unless another stream shows it.
                    self.shared.replan_and_sync();
                }
                Ok(CallResult::Action { result: None })
            }
            other => Err(not_implemented(other.name())),
        }
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
                // The opener of a start that is opening reads the server the start
                // checked (`output_kind`), which a retarget since may have changed, so
                // a key set now could be paired with the old server (#343).
                if self.starting_kind(&output_id).is_some() {
                    return Err(ProtocolError::ActionFailed {
                        reason: format!(
                            "output `{output_id}` is starting: set its key once the start \
                             has settled"
                        ),
                    });
                }
                let store = self.key_store().ok_or_else(no_key_store)?;
                store
                    .set(output_id.as_str(), &Secret::new(key.expose()))
                    .map_err(|err| ProtocolError::ActionFailed {
                        reason: format!("the credential store did not store the key: {err}"),
                    })?;
                lock(&self.shared.state).stale_keys.remove(&output_id);
                Ok(CallResult::action())
            }
            Action::OutputClearKey { output_id } => {
                let store = self.key_store().ok_or_else(no_key_store)?;
                store
                    .clear(output_id.as_str())
                    .map_err(|err| ProtocolError::ActionFailed {
                        reason: format!("the credential store did not remove the key: {err}"),
                    })?;
                lock(&self.shared.state).stale_keys.remove(&output_id);
                Ok(CallResult::action())
            }
            other => Err(not_implemented(other.name())),
        }
    }

    /// Removes the stream keys of outputs that an applied batch, undo or redo deleted or
    /// pointed at another server (`OutputKind::key_server`, #343), after the collection
    /// lock is released. Each stays in `State::stale_keys`, which refuses its
    /// `output.start`, until its key is gone: a store that refuses keeps it there until an
    /// `output.set_key` or `output.clear_key` succeeds. The batch itself has already
    /// succeeded, so a refusal is logged, never returned. Without a store there is no
    /// key to remove.
    fn remove_stale_keys(&self, stale: Vec<OutputId>) {
        if stale.is_empty() {
            return;
        }
        let store = self.key_store();
        for output_id in stale {
            let removed = match &store {
                Some(store) => store.clear(output_id.as_str()),
                None => Ok(()),
            };
            match removed {
                Ok(()) => {
                    lock(&self.shared.state).stale_keys.remove(&output_id);
                }
                Err(err) => tracing::warn!(
                    output_id = %output_id,
                    "the credential store did not remove the stream key of an output whose \
                     server changed: {err}"
                ),
            }
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
                    cleartext,
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
                cleartext,
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
                dedicated: own.dedicated(),
            });
        (encoder, count(sessions.len()), hardware)
    }

    fn host(&mut self) -> Arc<MediaHost> {
        let host = self.media.get_or_insert_with(|| Arc::new(MediaHost::new()));
        Arc::clone(host)
    }

    /// `capture.targets`: what the platform's capture backend can capture now, listed
    /// with no engine lock held, since listing windows calls the OS.
    fn capture_targets(&self) -> Result<CallResult, ProtocolError> {
        let capture = self.media.as_ref().and_then(|host| {
            lock(&host.backends)
                .as_ref()
                .map(|backends| Arc::clone(&backends.capture))
        });
        let Some(capture) = capture else {
            return Err(ProtocolError::ActionFailed {
                reason: "the engine has no capture backend".into(),
            });
        };
        let mut targets: Vec<_> = capture
            .targets()
            .map_err(|err| ProtocolError::ActionFailed {
                reason: format!("listing capture targets: {err}"),
            })?
            .into_iter()
            .filter_map(capture_target_info)
            .collect();
        targets.sort_by_key(|info| info.kind != CaptureTargetKind::Display);
        Ok(CallResult::Query {
            result: QueryResult::CaptureTargets { targets },
        })
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
        // A `youtube` output needs a signed-in client. Checked under the start lock,
        // before the output goes `starting`, and without the collection lock, since it
        // waits for a YouTube call in flight: the guard is dropped at the end of this
        // statement, before the match below.
        let broadcast = youtube_broadcast(&lock(&self.shared.state).graph, &output_id);
        let youtube = match broadcast {
            Some(_) => match self.youtube() {
                Some(youtube) if youtube.signed_in() => Some(youtube),
                Some(_) => {
                    return Err(ProtocolError::ActionFailed {
                        reason: "not signed in to YouTube: send `youtube.sign_in`".into(),
                    });
                }
                None => {
                    return Err(ProtocolError::ActionFailed {
                        reason: NO_YOUTUBE_CLIENT.into(),
                    });
                }
            },
            None => None,
        };
        let mut wake = Vec::new();
        let (parts, publish, generation, program, key, own_audio, hardware_held) = loop {
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
            if state.stale_keys.contains(&output_id) {
                return Err(ProtocolError::ActionFailed {
                    reason: format!(
                        "output `{output_id}` changed server and its old stream key is not \
                         removed yet: send `output.clear_key`, or `output.set_key` with the \
                         new server's key"
                    ),
                });
            }
            let key = encode_key(&state.graph, &output_id);
            let own_audio = has_audio_chain(&state.graph, &output_id);
            let publish = match (
                own_target(&state.graph, &output_id),
                youtube_broadcast(&state.graph, &output_id),
                &youtube,
            ) {
                (Some(OwnTarget::Record(path)), _, _) => Target::Record(path),
                (Some(OwnTarget::Vcam(canvas)), _, _) => {
                    let sink = lock(&host.vcam).take();
                    let Some(sink) = sink else {
                        let reason = if host.has_vcam.load(Ordering::Acquire) {
                            "the virtual camera is in use by another output"
                        } else {
                            "this engine has no virtual camera"
                        };
                        return Err(ProtocolError::ActionFailed {
                            reason: reason.into(),
                        });
                    };
                    Target::Vcam(VcamOut::new(sink, Arc::clone(&host.vcam)), canvas)
                }
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
                    state.audio.wanted(),
                    canvas_size(&state.graph),
                )
            };
            let generation = host.next_generation.fetch_add(1, Ordering::Relaxed);
            gate.pending = Some(Pending {
                output_id: output_id.clone(),
                generation,
                cancel: false,
                kind: state
                    .graph
                    .state()
                    .output(&output_id)
                    .map(|output| output.kind().clone()),
            });
            match state.outputs.begin(&output_id, true) {
                Ok(step) => emit_output(&mut state, &output_id, step, &mut wake),
                Err(err) => {
                    gate.pending = None;
                    publish.give_back(host);
                    return Err(err);
                }
            }
            // The outputs holding hardware video sessions now, stopping ones included:
            // while there is one, a new session that hardware refuses is the session
            // limit (ADR-0080 §3, #834).
            let hardware_held = gate.hardware_held();
            // A running program stage is shared; holding it here keeps it running
            // until this start has its own hold, or gives up. Taken last, so no early
            // return drops it under the locks.
            let program = gate.program();
            break (
                parts,
                publish,
                generation,
                program,
                key,
                own_audio,
                hardware_held,
            );
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
        let launched =
            publish
                .open(&output_id)
                .map_err(StartError::from)
                .and_then(|(opened, watch)| {
                    health = watch;
                    let program = match program {
                        Some(program) => program,
                        None => {
                            let program = launch_program(self, parts)?;
                            lock(&host.gate).set_program(&program);
                            program
                        }
                    };
                    match opened {
                        Opened::Muxed(destination) => launch(
                            self,
                            program,
                            OutputEncode {
                                key,
                                own_audio,
                                hardware_held,
                            },
                            destination,
                            output_id.clone(),
                            generation,
                            fail_tx,
                        ),
                        Opened::Vcam(vcam, canvas) => launch_vcam(
                            program,
                            vcam,
                            canvas,
                            output_id.clone(),
                            generation,
                            fail_tx,
                        )
                        .map_err(StartError::from),
                    }
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
                            gate.drain(&output_id, &running);
                            StartOutcome::Failed(
                                Some(running),
                                ProtocolError::ActionFailed { reason },
                            )
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
                            gate.drain(&output_id, &running);
                            StartOutcome::Failed(
                                Some(running),
                                ProtocolError::ActionFailed {
                                    reason: "the output thread ended before the pipeline was \
                                             active"
                                        .into(),
                                },
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
                                    gate.drain(&output_id, &running);
                                    StartOutcome::Failed(
                                        Some(running),
                                        ProtocolError::ActionFailed {
                                            reason: format!("starting the publish watcher: {err}"),
                                        },
                                    )
                                }
                            }
                        }
                    },
                    Err(err) => {
                        abandon_start(&mut state, &mut gate, &mut inflight, &output_id, &mut wake);
                        StartOutcome::Failed(None, err.into())
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
                Ok(CallResult::action())
            }
            StartOutcome::Cancelled(running) => {
                drop(running);
                drop(inflight);
                Err(ProtocolError::ActionFailed {
                    reason: format!("output `{output_id}` stopped while starting"),
                })
            }
            StartOutcome::Failed(running, err) => {
                // `failed` is already out and `pending` cleared, so `draining` (set
                // above when there is a pipeline) holds a new start off until this
                // join ends, including when it unwinds.
                let _clear_drain = ClearDrain::arm(Arc::clone(host), running.as_ref());
                drop(running);
                Err(err)
            }
        }
    }

    fn stop_pipeline(&self, output_id: OutputId) -> Result<CallResult, ProtocolError> {
        let Some(host) = &self.media else {
            return Err(not_implemented("output.stop"));
        };
        if let Some(youtube) = self.youtube() {
            youtube.unwatch(&output_id);
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
                let running = gate.running.remove(&output_id);
                let watcher = if let Some(running) = &running {
                    // A start waits until this join ends, if it took the program stage
                    // down with it, and counts its hardware session until that closes.
                    gate.drain(&output_id, running);
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
        let clear_drain = ClearDrain::arm(Arc::clone(host), running.as_ref());
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
        Ok(CallResult::action())
    }

    /// Polls the health of the stream a `youtube` output started on, and its
    /// broadcast's audience, while the output is `active` or `reconnecting`.
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
        watch.youtube.watch(
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

    /// `video.snapshot`. The `rev`, the render snapshot published for it and the live
    /// values on the parameter channel are read together under the collection lock; the
    /// snapshot thread renders them after that lock is released, so the image shows
    /// open gestures as the output does.
    fn video_snapshot(&self) -> SnapshotReply {
        let (snapshot, live, rev, at) = {
            let state = lock(&self.shared.state);
            (
                state.render.load(),
                state.render.load_live(),
                state.graph.rev(),
                state.live.now(),
            )
        };
        self.shared.snapshots.request(snapshot, live, rev, at)
    }

    /// The render hub the pipeline's render thread reads, for tests.
    #[cfg(test)]
    pub(crate) fn render_hub(&self) -> Arc<RenderHub> {
        Arc::clone(&lock(&self.shared.state).render)
    }

    /// The scenes planned for preview streams, for tests.
    #[cfg(test)]
    pub(crate) fn previewed_scenes(&self) -> Vec<SceneId> {
        lock(&self.shared.state)
            .scene_shots
            .keys()
            .cloned()
            .collect()
    }

    /// How many CLAP filters run on a host's block in the master chain and in each
    /// output tap's chain, for tests.
    #[cfg(test)]
    pub(crate) fn routed_clap_hosted(&self) -> (usize, BTreeMap<OutputId, usize>) {
        lock(&self.shared.state).routing.hosted()
    }

    /// How the program stage fares with audio source `source_id`, for tests.
    #[cfg(test)]
    pub(crate) fn audio_status(
        &self,
        source_id: &SourceId,
    ) -> Option<crate::audio_sources::AudioStatus> {
        lock(&self.shared.state).audio.status(source_id)
    }

    /// How the program stage fares with bus `bus_id`'s monitor, for tests.
    #[cfg(test)]
    pub(crate) fn monitor_status(
        &self,
        bus_id: &jstream_protocol::BusId,
    ) -> Option<crate::audio_sources::AudioStatus> {
        lock(&self.shared.state).audio.monitor_status(bus_id)
    }

    /// The packets the mixer input of `source_id` has received, for tests.
    #[cfg(test)]
    pub(crate) fn audio_packets(&self, source_id: &SourceId) -> Option<u64> {
        lock(&self.shared.state).audio.packets(source_id)
    }

    /// Waits until the live sources have built everything asked of them, for tests.
    #[cfg(test)]
    pub(crate) fn settle_sources(&self) -> bool {
        let worker = lock(&self.shared.state).live.worker();
        worker.settled(Duration::from_secs(10))
    }

    /// Forgets every source's size without telling anyone, as if plans had taken new
    /// frames since the last `sync_sizes`, so a test sees which re-plan catches up.
    #[cfg(test)]
    pub(crate) fn forget_sizes(&self) {
        lock(&self.shared.state).sizes.clear();
    }

    /// Hands the first video filter asset back to the engine as if its load had just
    /// finished, through [`LiveHost::install_asset`]. Returns whether there was one.
    #[cfg(test)]
    pub(crate) fn reinstall_asset(&self) -> bool {
        let Some(loaded) = lock(&self.shared.state).live.reload_asset() else {
            return false;
        };
        LiveHost::install_asset(&*self.shared, loaded);
        true
    }

    /// Runs the live sources on `clock`, for tests that move time by hand.
    #[cfg(test)]
    pub(crate) fn set_source_clock(&self, clock: Arc<dyn jstream_platform::MediaClock>) {
        lock(&self.shared.state).live.set_clock(clock);
    }

    /// Plays `media` sources through `media`, for tests that give no full backends.
    #[cfg(test)]
    pub(crate) fn set_source_media(&self, media: Arc<dyn jstream_platform::MediaDecoders>) {
        let mut state = lock(&self.shared.state);
        let state = &mut *state;
        state.live.set_media(media, state.graph.state());
    }

    /// Re-plans now, so the scenes preview streams show follow streams that a session's
    /// end closed.
    pub(crate) fn replan_previews(&self) {
        self.shared.replan_and_sync();
    }

    /// Re-plans the program mix and each video mix's scene now, as the live sources'
    /// thread does when a frame is due, for tests that move time by hand.
    #[cfg(test)]
    pub(crate) fn replan(&self) {
        self.shared.replan_and_sync();
    }
}

/// What [`Engine::execute`] produced: an answer, or a snapshot still being drawn.
pub(crate) enum Executed {
    /// Boxed: a `state.get` result is large, and the snapshot case is small.
    Ready(Box<Result<CallResult, ProtocolError>>),
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

/// Which encoder sessions `output_id` shares: the video mix it shows with that mix's
/// size and rate now, its profile with that profile's encoding now, and the output
/// itself when it has its own video chain, so it shares with no one (ADR-0080 §3). An
/// output with no mix, or one the host declared, gets the program at the program
/// stage's size and base rate; one with no profile the engine's default.
fn encode_key(graph: &Graph, output_id: &OutputId) -> EncodeKey {
    let state = graph.state();
    let output = state.output(output_id);
    let profile_id = output.and_then(|output| output.encoder_profile()).cloned();
    let profile = state
        .encoding_of(output_id)
        .unwrap_or(EncoderProfile::DEFAULT);
    let mix = output
        .and_then(|output| output.video_mix())
        .and_then(|id| state.video_mix(id))
        .map_or(MixKey::PROGRAM, |mix| {
            let spec = mix.mix();
            MixKey {
                mix: Some(mix.id().clone()),
                // The graph keeps every resolution in range, so this is never `None`.
                size: jstream_media::FrameSize::new(spec.resolution.width, spec.resolution.height)
                    .ok(),
                divisor: spec.fps_divisor,
                owner: None,
            }
        });
    let owner = output
        .filter(|output| output.has_video_chain())
        .map(|_| output_id.clone());
    (MixKey { owner, ..mix }, profile_id, profile)
}

/// Whether `output_id` starts with its own audio encode: it has an audio chain
/// (ADR-0080), even one whose filters are all off, so switching one on later changes
/// nothing but the chain. A chain added to a running output takes effect at its next
/// start.
fn has_audio_chain(graph: &Graph, output_id: &OutputId) -> bool {
    graph
        .state()
        .output(output_id)
        .is_some_and(|output| !output.filters().chain(FilterChain::Audio).is_empty())
}

/// Where an output the engine sends to itself goes, rather than to a publish target
/// the host opens.
enum OwnTarget {
    /// A recording, to this file.
    Record(PathBuf),
    /// The virtual camera, at the canvas size.
    Vcam(FrameSize),
}

/// The collection's canvas, width and height.
fn canvas_size(graph: &Graph) -> (u32, u32) {
    let canvas = graph.state().canvas();
    (canvas.width, canvas.height)
}

/// Where a recording output in the collection records to, or the size a virtual camera
/// runs at. `None` when the output needs a publish target (or is not in the
/// collection).
fn own_target(graph: &Graph, output_id: &OutputId) -> Option<OwnTarget> {
    match graph.state().output(output_id)?.kind() {
        OutputKind::Record {
            path,
            container: RecordContainer::Mp4,
        } => Some(OwnTarget::Record(PathBuf::from(path))),
        OutputKind::VirtualCam => {
            let canvas = graph.state().canvas();
            // The graph keeps both sides above zero, so this always succeeds.
            FrameSize::new(canvas.width, canvas.height)
                .ok()
                .map(OwnTarget::Vcam)
        }
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
/// it calls once the locks are released, a recording file, the virtual camera, or a
/// YouTube broadcast.
enum Target {
    Ready(Box<dyn LivePublish>),
    Open(OpenPublish),
    Record(PathBuf),
    Vcam(VcamOut, FrameSize),
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

/// A start's opened target: a stream the output thread muxes, or the virtual camera.
enum Opened {
    Muxed(Destination),
    Vcam(VcamOut, FrameSize),
}

impl Target {
    /// Puts a one-shot target back for the next start, when this one refused
    /// before `starting`. An opener was never taken out of the host. The virtual
    /// camera's sink goes back to the host when its [`VcamOut`] drops.
    fn give_back(self, host: &MediaHost) {
        if let Target::Ready(publish) = self {
            *lock(&host.publish) = Some(publish);
        }
    }

    /// Where to send the output, and for a `youtube` output the stream to watch. Runs
    /// the opener, or reads the YouTube ingest and connects, outside the engine's locks.
    /// A recording's file is opened by `launch`, once the frame size is known.
    fn open(self, output_id: &OutputId) -> Result<(Opened, Option<HealthTarget>), String> {
        match self {
            Target::Ready(publish) => Ok((Opened::Muxed(Destination::Publish(publish)), None)),
            Target::Open(open) => open(output_id)
                .map(|publish| (Opened::Muxed(Destination::Publish(publish)), None))
                .map_err(|err| format!("opening the publish target: {err}")),
            Target::Record(path) => Ok((Opened::Muxed(Destination::Record(path)), None)),
            Target::Vcam(vcam, canvas) => Ok((Opened::Vcam(vcam, canvas), None)),
            Target::YouTube {
                broadcast_id,
                youtube,
            } => {
                let ingest = youtube.ingest(&broadcast_id)?;
                let publish = youtube
                    .connect(&ingest)
                    .map_err(|err| format!("connecting to YouTube's ingest: {err}"))?;
                Ok((
                    Opened::Muxed(Destination::Publish(publish)),
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
    Failed(Option<Running>, ProtocolError),
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
            let running = gate.running.remove(output_id);
            match &running {
                Some(running) => gate.drain(output_id, running),
                None => gate.draining += 1,
            }
            // Detach this watcher. `JoinHandle`'s drop does not join, so the
            // thread can go on to drop `Running` without joining itself.
            drop(gate.watchers.remove(output_id));
            running
        } else {
            None
        }
    };
    // `pipeline` is bound after the guard. On the normal path it is dropped
    // first, and an unwind (`wake_all`, or the join itself) drops it first too,
    // so `draining` clears only after the join.
    let _clear_drain = ClearDrain::arm(Arc::clone(host), running.as_ref());
    let pipeline = running;
    wake_all(wake);
    drop(pipeline);
}

/// Undoes `Gate::drain` after the pipeline has been joined, including when that join
/// unwinds: clears `draining`, and stops counting its hardware session as held.
struct ClearDrain {
    host: Arc<MediaHost>,
    armed: bool,
    session: Option<Weak<Encoders>>,
}

impl ClearDrain {
    /// Armed when there is a pipeline to join.
    fn arm(host: Arc<MediaHost>, running: Option<&Running>) -> Self {
        Self {
            host,
            armed: running.is_some(),
            session: running.and_then(Running::encoders).map(Arc::downgrade),
        }
    }
}

impl Drop for ClearDrain {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut gate = lock(&self.host.gate);
        gate.drained(self.session.as_ref());
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

/// [`Engine::use_gpu`] and its undo.
fn set_gpu(shared: &Shared, context: Option<&GpuContext>) {
    let mut wake = Vec::new();
    {
        let mut state = lock(&shared.state);
        state.live.use_gpu(context);
        state.publish_render();
        state.sync_sizes(&mut wake);
    }
    wake_all(wake);
}

fn emit_sampled(shared: &Shared, ledger: &Mutex<DropLedger>) {
    let sampled = drops::lock_ledger(ledger).sample();
    // Read before the collection lock: the CPU reader makes a system call.
    let cpu = shared.load.cpu_time();
    let mut wake = Vec::new();
    {
        let mut state = lock(&shared.state);
        let load = shared.load.load(state.render.drawn(), cpu);
        state.emit_stats(sampled, load, &mut wake);
    }
    for waker in wake {
        waker.wake();
    }
}

/// The idle heartbeat ([`Engine::start_stats_heartbeat`]): about once a second, a
/// `stats.tick` unless a program stage's sampler runs, until the engine is gone.
fn stats_heartbeat(shared: &Weak<Shared>, ledger: &Weak<Mutex<DropLedger>>) {
    loop {
        thread::sleep(Duration::from_secs(1));
        let (Some(shared), Some(ledger)) = (shared.upgrade(), ledger.upgrade()) else {
            return;
        };
        if !shared.load.ticking() {
            emit_sampled(&shared, &ledger);
        }
    }
}

/// Wakes every 100 ms so a stop joins promptly, emits `meters.tick` on each wake, and
/// `stats.tick` once those waits add up to a second.
fn stats_ticker(shared: Arc<Shared>, ledger: Arc<Mutex<DropLedger>>, stop: Arc<AtomicBool>) {
    shared.load.ticker_started();
    let mut waited_ms = 0u32;
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(100));
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let mut wake = Vec::new();
        lock(&shared.state).emit_meters(&mut wake);
        wake_all(wake);
        waited_ms += 100;
        if waited_ms < 1_000 {
            continue;
        }
        waited_ms = 0;
        emit_sampled(&shared, &ledger);
    }
    shared.load.ticker_ended();
}

/// A frame size as the protocol reports it.
fn pixel_size(size: FrameSize) -> PixelSize {
    PixelSize {
        width: size.width(),
        height: size.height(),
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
    /// What each scene with an open preview stream draws, planned with the program mix
    /// and replaced only when what it draws changed: read by the preview thread alone.
    scene_shots: Arc<BTreeMap<SceneId, Arc<RenderSnapshot>>>,
    /// How many `stats.tick` events this engine has emitted. The next one uses this plus one.
    stats_seq: u64,
    /// Every client's open live-value gestures (ADR-0032), and the committed values the
    /// render thread has not caught up with yet.
    gestures: Gestures,
    /// The last entries of the audit log, with their sentences (ADR-0083), for
    /// `activity.list`.
    activity: Activity,
    /// Gestures cancelled or expired by the call running now, with its request, for the
    /// audit log once the lock is released.
    ended: Vec<(Option<RequestId>, Ended)>,
    /// Whether the live ticker thread is running: it sends coalesced `live.changed`
    /// events and releases committed live values ([`live_ticker`]).
    ticking: bool,
    /// The engine this state belongs to, for the live ticker.
    shared: Weak<Shared>,
    /// The program mixer's controls while it runs: it follows the collection's audio.
    routing: AudioRouting,
    /// Outputs whose stream key must be removed because an applied change deleted them
    /// or moved them to another server (#343). Marked under this lock in the same
    /// critical section as the change, and unmarked once the store has removed the key,
    /// so `output.start` (which refuses a marked output) never opens the new server with
    /// the old key.
    stale_keys: BTreeSet<OutputId>,
    /// Which scene is on program. Runtime state, like `outputs`: not part of `rev`.
    studio: Studio,
    /// Forgets the capture backend's grant for each deleted source, once there are
    /// backends ([`ForgetSources`]).
    forget: Option<ForgetSources>,
    /// Each source's size as `source.sizes` reports it and `source.size_changed` last
    /// announced it (ADR-0022 §4): runtime state, not part of `rev`. A source with no
    /// size has no entry.
    sizes: BTreeMap<SourceId, FrameSize>,
    /// The size of each capture the program stage runs, as its render thread reports
    /// it: what a `display`, `window` or `webcam` source's size comes from.
    capture_sizes: BTreeMap<CaptureKey, FrameSize>,
}

impl State {
    /// Executes `call`, and when it is a batch, undo or redo that applied, marks in
    /// `stale_keys` and returns every output whose stream key it made stale: one it
    /// deleted, or whose `OutputKind::key_server` it changed (#343). The caller removes
    /// those keys once this lock is released, since the store is never called under it.
    fn execute_and_unbind_keys(
        &mut self,
        client_id: &ClientId,
        request_id: &RequestId,
        call: Call,
        wake: &mut Vec<Waker>,
    ) -> (Result<CallResult, ProtocolError>, Vec<OutputId>) {
        if !matches!(call, Call::Batch(_) | Call::History(_)) {
            return (self.execute(client_id, request_id, call, wake), Vec::new());
        }
        let before: Vec<(OutputId, Option<String>)> = self
            .graph
            .state()
            .outputs()
            .map(|output| (output.id().clone(), output.kind().key_server()))
            .collect();
        let result = self.execute(client_id, request_id, call, wake);
        if result.is_err() {
            return (result, Vec::new());
        }
        let after = self.graph.state();
        let stale: Vec<OutputId> = before
            .into_iter()
            .filter(|(output_id, server)| {
                after
                    .output(output_id)
                    .is_none_or(|output| output.kind().key_server() != *server)
            })
            .map(|(output_id, _)| output_id)
            .collect();
        self.stale_keys.extend(stale.iter().cloned());
        (result, stale)
    }

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
    /// history, holding the inverse the graph returned, and clears what it could redo; a
    /// batch that changed nothing, with an empty inverse, is no step. The result reports
    /// the keyframe overrides the batch dropped (ADR-0040 §1).
    ///
    /// A batch whose idempotency key this client already used is not applied again: it
    /// gets the result of the first call with that key, and emits nothing. That check
    /// comes first, so a retry succeeds even after `rev` moved on. An `ends_gesture` the
    /// client has no open gesture for is refused next, as the mock does.
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
        if let Some(gesture_id) = &batch.ends_gesture
            && !self.gestures.is_open(client_id, gesture_id)
        {
            return Err(ProtocolError::invalid_argument(
                "ends_gesture",
                format!("the client has no open gesture `{gesture_id}`"),
            ));
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
        let result = CallResult::Batch {
            rev: applied.rev,
            removed_hotkeys: applied.removed_hotkeys,
            dropped_overrides: applied.dropped_overrides,
        };
        if let Some(keyed) = keyed {
            self.applied.insert(keyed, result.clone());
        }
        if let Some(gesture_id) = &batch.ends_gesture
            && let Some(event) = self.gestures.flush(client_id, gesture_id)
        {
            self.subscribers.emit(&event, wake);
        }
        self.changed(client_id, request_id, &batch.edits, applied.rev, None, wake);
        if let Some(gesture_id) = &batch.ends_gesture {
            self.commit_gesture(client_id, gesture_id, applied.rev, wake);
        }
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

    /// What follows every applied batch, undo and redo: the outputs, the live sources and
    /// the program mixer follow the collection, a deleted source's stored frame goes
    /// (unless the same edits made a new source under its id), live values on a deleted
    /// item go, `state.changed` is emitted, and the program scene is reconciled
    /// (ADR-0030).
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
                // The capture backend's grant for it (the portal's restore token) goes
                // too, so a new source given its ID asks again. Its capture, if one
                // runs, stops when the render thread takes the next snapshot.
                if let Some(forget) = &self.forget {
                    forget.forget(source_id.clone());
                }
            }
        }
        // Edits that deleted the program scene, or created the first scene, move
        // program before the mix is planned; that event follows their `state.changed`.
        let moved = self.studio.reconcile(self.graph.state());
        // After program is reconciled: a scene edit can change what is shown.
        self.route_audio();
        self.publish_render();
        let state = self.graph.state();
        if self.gestures.drop_missing(|item_id, keyframe| {
            state.item(item_id).is_some() && keyframe.is_none_or(|id| is_keyframe(state, id))
        }) {
            self.publish_live();
        }
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
            self.emit_studio(client_id, request_id, None, wake);
        }
        // After `state.changed`, so a deleted source's size goes after its deletion.
        self.sync_sizes(wake);
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
            Action::LiveSet { gesture_id, values } => {
                return self.live_set(client_id, &gesture_id, values, wake);
            }
            Action::LiveCancel { gesture_id } => {
                self.end_gesture(
                    client_id,
                    &gesture_id,
                    LiveOutcome::Cancelled,
                    Some(request_id),
                    wake,
                );
                return Ok(CallResult::action());
            }
            // Restarts the master's integrated loudness, if the mixer runs; there is
            // nothing to restart otherwise, which is no failure.
            Action::AudioResetLoudness {} => {
                self.routing.reset_loudness();
                return Ok(CallResult::action());
            }
            Action::SceneSwitch {
                scene_id,
                preset_id,
            } => {
                self.switch(client_id, request_id, scene_id, preset_id.as_ref(), wake)?;
                return Ok(CallResult::action());
            }
            Action::StudioSetPreview { scene_id } => {
                if self.studio.set_preview(self.graph.state(), &scene_id)? {
                    self.emit_studio(client_id, request_id, None, wake);
                }
                return Ok(CallResult::action());
            }
            other @ (Action::PairingStart
            | Action::DeviceRevoke { .. }
            | Action::UiSettingsSet { .. }) => {
                return Err(served_by_the_server(other.name()));
            }
            // Built without the `wasm-plugins` feature, the engine can't run a plugin, so
            // none is ever installed. With it, `Engine::execute` answers these before
            // taking the collection lock.
            other @ Action::PluginInstall { .. } => {
                return Err(ProtocolError::Unsupported {
                    call: other.name().into(),
                });
            }
            Action::PluginGrant { plugin_id, .. } | Action::PluginEnable { plugin_id } => {
                return Err(ProtocolError::NotFound {
                    entity: EntityKind::Plugin,
                    id: plugin_id.to_string(),
                });
            }
            // Deprecated: JStream has no privacy screen (ADR-0080 §4).
            Action::PrivacySet { .. } => {
                return Err(ProtocolError::Unsupported {
                    call: "privacy.set".into(),
                });
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
        Ok(CallResult::action())
    }

    /// Collection reads go to `Graph::query`; `output.status` reads the outputs,
    /// `live.list` the open gestures and `studio.get` the studio state; the deprecated
    /// `privacy.get` always answers off (ADR-0080 §4).
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
                    cleartext: self
                        .graph
                        .state()
                        .output(output_id)
                        .is_some_and(|output| output.kind().sends_key_in_cleartext()),
                },
                Query::LiveList => QueryResult::LiveList {
                    gestures: self.gestures.list(),
                },
                Query::StudioGet => QueryResult::StudioGet {
                    program_scene: self.studio.program().cloned(),
                    preview_scene: self.studio.preview().cloned(),
                },
                Query::DeviceList | Query::UiSettingsGet { .. } => {
                    return Err(served_by_the_server(query.name()));
                }
                // Built without the `wasm-plugins` feature, no plugin is ever installed.
                // With it, `Engine::execute` answers it before taking the collection lock.
                Query::PluginList => QueryResult::PluginList {
                    plugins: Vec::new(),
                },
                Query::ActivityList { after_seq, limit } => QueryResult::ActivityList {
                    now_ms: activity_log::unix_ms(std::time::SystemTime::now()),
                    entries: self.activity.list(*after_seq, *limit),
                },
                Query::PrivacyGet => QueryResult::PrivacyGet { on: false },
                Query::SourceSizes => QueryResult::SourceSizes {
                    sources: self
                        .graph
                        .state()
                        .sources()
                        .map(|source| SourceSize {
                            source_id: source.id().clone(),
                            size: self.sizes.get(source.id()).copied().map(pixel_size),
                        })
                        .collect(),
                },
                other => return Err(not_implemented(other.name())),
            },
        };
        Ok(CallResult::Query { result })
    }

    /// `scene.switch`: puts `scene_id` on program with the transition its preset runs
    /// (ADR-0029). A switch while a transition runs retargets it: the new one starts from
    /// what is on screen now. Switching to the scene already on program (or already
    /// transitioning onto it) changes nothing and emits nothing.
    fn switch(
        &mut self,
        client_id: &ClientId,
        request_id: &RequestId,
        scene_id: SceneId,
        named: Option<&TransitionPresetId>,
        wake: &mut Vec<Waker>,
    ) -> Result<(), ProtocolError> {
        studio::exists(self.graph.state(), &scene_id)?;
        let from_scene = self.studio.program().cloned();
        let (preset_id, run) =
            transition::resolve(&self.graph, from_scene.as_ref(), &scene_id, named)?;
        if from_scene.as_ref() == Some(&scene_id) {
            return Ok(());
        }
        let now = self.live.now();
        let running = match (run, from_scene) {
            (Some(run), Some(from_scene)) => {
                let state = self.graph.state();
                let by_item = state.family_of(&from_scene) == state.family_of(&scene_id);
                let from = match self.studio.running() {
                    Some(running) => {
                        let Self {
                            graph,
                            frames,
                            frame_versions,
                            live,
                            ..
                        } = self;
                        let assets = live.filter_assets();
                        let visuals = snapshot::on_screen(
                            graph,
                            running,
                            now,
                            &mut |id| match frames.get(id) {
                                Some(frame) => Some(Look::Frame(frame.clone(), None)),
                                None => live.look(id, now, frame_versions),
                            },
                            &assets,
                        );
                        transition::From::OnScreen(visuals.into())
                    }
                    None => transition::From::Scene(from_scene),
                };
                Some(transition::Running {
                    from,
                    to: scene_id.clone(),
                    start: now,
                    run,
                    by_item,
                })
            }
            // Nothing on program yet: there is nothing to transition from.
            _ => None,
        };
        self.studio.switch(scene_id, running);
        let end_ms = self.publish_render();
        // The new scene's `when_shown` audio follows at the switch, ramping over one
        // mixer block; it doesn't follow the transition's timing yet (D29).
        self.route_audio();
        self.emit_studio(
            client_id,
            request_id,
            Some(transition::started(preset_id, end_ms)),
            wake,
        );
        // The scene now on program can show a source no plan reached before.
        self.sync_sizes(wake);
        Ok(())
    }

    /// `studio.changed` with the scenes now on program and preview, caused by
    /// `request_id`, and the transition a switch started.
    fn emit_studio(
        &mut self,
        client_id: &ClientId,
        request_id: &RequestId,
        transition: Option<TransitionStarted>,
        wake: &mut Vec<Waker>,
    ) {
        self.subscribers.emit(
            &Event::StudioChanged {
                program_scene: self.studio.program().cloned(),
                preview_scene: self.studio.preview().cloned(),
                transition,
                client_id: client_id.clone(),
                request_id: Some(request_id.clone()),
            },
            wake,
        );
    }

    /// Brings `sizes` in line with each source's size now and sends a
    /// `source.size_changed` for every change (ADR-0022 §4): a stored frame's size wins
    /// over the live source's, as its frame does, and a source deleted since loses its
    /// size. Runs after every plan.
    fn sync_sizes(&mut self, wake: &mut Vec<Waker>) {
        let Self {
            graph,
            frames,
            live,
            sizes,
            capture_sizes,
            subscribers,
            ..
        } = self;
        let state = graph.state();
        let mut emit = |source_id: &SourceId, size: Option<FrameSize>| {
            subscribers.emit(
                &Event::SourceSizeChanged {
                    source_id: source_id.clone(),
                    size: size.map(pixel_size),
                },
                wake,
            );
        };
        sizes.retain(|source_id, _| {
            let kept = state.source(source_id).is_some();
            if !kept {
                emit(source_id, None);
            }
            kept
        });
        for source in state.sources() {
            let source_id = source.id();
            let now = match frames.get(source_id) {
                Some(frame) => frame.extent(),
                None => live
                    .size(source_id)
                    .or_else(|| capture_sizes.get(&live.capture_key(source_id)?).copied()),
            };
            if sizes.get(source_id) == now.as_ref() {
                continue;
            }
            match now {
                Some(size) => sizes.insert(source_id.clone(), size),
                None => sizes.remove(source_id),
            };
            emit(source_id, now);
        }
    }

    /// Re-plans the program mix and the scene each video mix shows at the current time,
    /// and publishes them if they draw something new. A source's stored frame wins over
    /// its live one. The live sources are told when the plan's next frame change is due,
    /// to re-plan then. The program mix shows the program scene (ADR-0030), or the
    /// transition running toward it (ADR-0029), whose length in milliseconds it returns;
    /// a transition that has ended is let go. The shot lists the displays and windows the
    /// program and every mix capture. A source the shot shows enlarged is drawn again at
    /// the density that keeps it sharp, when its kind can be (ADR-0073).
    fn publish_render(&mut self) -> Option<f32> {
        let Self {
            graph,
            frames,
            frame_versions,
            live,
            audio,
            render,
            studio,
            scene_shots,
            shared,
            ..
        } = self;
        let now = live.now();
        live.begin_plan();
        let assets = live.filter_assets();
        let planning = snapshot::plan_with(
            graph,
            studio.program(),
            studio.running().map(|running| (running, now)),
            &mut |id| match frames.get(id) {
                Some(frame) => Some(Look::Frame(frame.clone(), None)),
                None => live.look(id, now, frame_versions),
            },
            &assets,
        );
        live.end_plan();
        live.densities(&planning.shot.scales());
        if let Some(sound) = live.take_audio() {
            audio.sync_live(sound);
        }
        if planning.finished {
            studio.finish();
        }
        render.publish(planning.shot);
        // Each previewed scene, after the program mix's plan has ended, so it reaches
        // no source: a preview never makes a source shown or heard (ADR-0028).
        let mut wake = planning.wake;
        let mut shots = BTreeMap::new();
        let previews = shared.upgrade();
        for scene_id in previews.iter().flat_map(|shared| shared.previews.scenes()) {
            if graph.state().scene(&scene_id).is_none() {
                if let Some(shared) = &previews {
                    shared
                        .previews
                        .end_scene(&scene_id, graph.rev(), "the scene was deleted");
                }
                continue;
            }
            let scene = snapshot::plan_scene(
                graph,
                &scene_id,
                &mut |id| match frames.get(id) {
                    Some(frame) => Some(Look::Frame(frame.clone(), None)),
                    None => live.look_quiet(id, now, frame_versions),
                },
                &assets,
            );
            wake = snapshot::earliest(wake, scene.wake);
            let shot = RenderSnapshot::following(scene_shots.get(&scene_id), scene.shot);
            shots.insert(scene_id, shot);
        }
        let same = shots.len() == scene_shots.len()
            && shots
                .iter()
                .zip(scene_shots.iter())
                .all(|((a, x), (b, y))| a == b && Arc::ptr_eq(x, y));
        if !same {
            *scene_shots = Arc::new(shots);
        }
        live.wake_at(wake);
        planning.end_ms
    }

    /// Opens or updates `client_id`'s gesture with `values` (ADR-0032). They are checked
    /// as a batch's edits are, on a copy of the collection, and an error names the value
    /// as `values.<n>`; nothing is applied unless all of them pass. Then they go on the
    /// render parameter channel, and a `live.changed` goes out now or, within
    /// [`CHANGED_INTERVAL`](crate::gesture::CHANGED_INTERVAL) of the gesture's last
    /// one, from the live ticker.
    fn live_set(
        &mut self,
        client_id: &ClientId,
        gesture_id: &GestureId,
        values: Vec<Edit>,
        wake: &mut Vec<Waker>,
    ) -> Result<CallResult, ProtocolError> {
        self.gestures.check_set(client_id, gesture_id, &values)?;
        self.graph
            .clone()
            .apply(&values, None)
            .map_err(values_error)?;
        // A value addressed to a keyframe scene is a parameter of that keyframe's: it
        // shows only where the keyframe draws the item (#846).
        let state = self.graph.state();
        self.gestures
            .set(client_id, gesture_id, values, |id| is_keyframe(state, id));
        self.publish_live();
        self.live_tick(Instant::now(), wake);
        Ok(CallResult::action())
    }

    /// Ends one of `client_id`'s gestures without a commit, if it is open: its values
    /// not sent yet go out, then `live.ended` with `outcome`, and its parameters show
    /// their collection values again. It is queued for the audit log with `request_id`.
    fn end_gesture(
        &mut self,
        client_id: &ClientId,
        gesture_id: &GestureId,
        outcome: LiveOutcome,
        request_id: Option<&RequestId>,
        wake: &mut Vec<Waker>,
    ) {
        if let Some(event) = self.gestures.flush(client_id, gesture_id) {
            self.subscribers.emit(&event, wake);
        }
        let Some(ended) = self.gestures.cancel(client_id, gesture_id, outcome) else {
            return;
        };
        self.publish_live();
        self.subscribers.emit(
            &Event::LiveEnded {
                gesture_id: gesture_id.clone(),
                client_id: client_id.clone(),
                outcome,
                rev: None,
            },
            wake,
        );
        self.ended.push((request_id.cloned(), ended));
    }

    /// Ends a gesture whose committing batch produced `rev`, after its `state.changed`.
    /// Its live values stay on the parameter channel until the render thread holds the
    /// snapshot with the committed ones (ARCH › Threads, "Live-value handover"): at
    /// once if it already does or isn't adopting snapshots, else from the live ticker.
    fn commit_gesture(
        &mut self,
        client_id: &ClientId,
        gesture_id: &GestureId,
        rev: Rev,
        wake: &mut Vec<Waker>,
    ) {
        let generation = self.render.generation();
        self.gestures.commit(client_id, gesture_id, generation);
        self.subscribers.emit(
            &Event::LiveEnded {
                gesture_id: gesture_id.clone(),
                client_id: client_id.clone(),
                outcome: LiveOutcome::Committed,
                rev: Some(rev),
            },
            wake,
        );
        self.live_tick(Instant::now(), wake);
    }

    /// Publishes the gestures' live values on the render parameter channel.
    fn publish_live(&self) {
        self.render.publish_live(self.gestures.params());
    }

    /// Sends the `live.changed` events due at `now` and releases the committed values
    /// the render thread has caught up with. Starts the live ticker if work remains,
    /// and returns when it should run next.
    fn live_tick(&mut self, now: Instant, wake: &mut Vec<Waker>) -> Option<Instant> {
        for event in self.gestures.due(now) {
            self.subscribers.emit(&event, wake);
        }
        let render = &self.render;
        if self
            .gestures
            .release(|generation| render.caught_up(generation))
        {
            self.publish_live();
        }
        let release = self.gestures.releasing().then(|| now + RELEASE_POLL);
        let next = match (self.gestures.next_due(), release) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if next.is_some() && !self.ticking {
            self.ticking = spawn_live_ticker(self.shared.clone());
        }
        next
    }

    /// Brings the program mixer in line with the collection's audio, the audio sources'
    /// inputs included.
    fn route_audio(&mut self) {
        let inputs = self.audio.inputs();
        self.routing
            .sync(self.graph.state(), self.studio.program(), &inputs);
    }

    /// Emits a `meters.tick` if the program mixer runs, first routing any audio source
    /// input the program stage opened since the last one.
    fn emit_meters(&mut self, wake: &mut Vec<Waker>) {
        if self.audio.take_inputs_changed() {
            self.route_audio();
        }
        let inputs = self.audio.inputs();
        let meters = self.routing.meters(self.graph.state(), &inputs);
        drop(inputs);
        if let Some((sources, buses)) = meters {
            self.subscribers
                .emit(&Event::MetersTick { sources, buses }, wake);
        }
    }

    fn emit_stats(&mut self, drops: DropCounts, load: EngineLoad, wake: &mut Vec<Waker>) {
        self.stats_seq = self.stats_seq.saturating_add(1);
        self.subscribers.emit(
            &Event::StatsTick {
                seq: self.stats_seq,
                drops,
                load: Some(load),
                chains: Some(self.render.load().shot().chain_cost()),
            },
            wake,
        );
    }
}

impl PreviewSource for Shared {
    fn current(&self) -> Current {
        let state = lock(&self.state);
        Current {
            program: state.render.load(),
            scenes: Arc::clone(&state.scene_shots),
            live: state.render.load_live(),
            rev: state.graph.rev(),
            at: state.live.now(),
        }
    }
}

impl Shared {
    /// Changes the captures' sizes and announces the sources whose size that changes,
    /// waking the subscribers once the collection lock is released.
    fn capture_sizes(&self, change: impl FnOnce(&mut BTreeMap<CaptureKey, FrameSize>)) {
        let mut wake = Vec::new();
        {
            let mut state = lock(&self.state);
            change(&mut state.capture_sizes);
            state.sync_sizes(&mut wake);
        }
        wake_all(wake);
    }

    /// Re-plans now and announces the sizes that changed, waking the subscribers once
    /// the collection lock is released.
    fn replan_and_sync(&self) {
        let mut wake = Vec::new();
        {
            let mut state = lock(&self.state);
            state.publish_render();
            state.sync_sizes(&mut wake);
        }
        wake_all(wake);
    }
}

impl LiveHost for Shared {
    fn install(&self, built: Built) {
        let mut guard = lock(&self.state);
        let state = &mut *guard;
        let mut wake = Vec::new();
        if state.live.install(built) {
            // A source built with audio of its own is recorded from now on.
            state.audio.sync(state.graph.state(), state.live.audio());
            state.publish_render();
            state.sync_sizes(&mut wake);
        }
        drop(guard);
        wake_all(wake);
    }

    fn install_asset(&self, loaded: LoadedAsset) {
        let mut wake = Vec::new();
        {
            let mut state = lock(&self.state);
            if state.live.install_asset(loaded) {
                // The plan can take a source's new frame, and so a new size.
                state.publish_render();
                state.sync_sizes(&mut wake);
            }
        }
        wake_all(wake);
    }

    fn replan(&self) {
        self.replan_and_sync();
    }
}

impl Drop for State {
    /// Ends every subscription when the engine goes away.
    fn drop(&mut self) {
        self.subscribers.close_all();
    }
}

/// How often the live ticker checks whether the render thread has caught up with a
/// committed gesture's snapshot.
const RELEASE_POLL: Duration = Duration::from_millis(4);

/// Starts `jstream-live`, which runs [`State::live_tick`] when it is next due, until
/// nothing is left to send or release or the engine is gone. Returns whether it
/// started; if it didn't, the work waits for the next live call or commit.
fn spawn_live_ticker(shared: Weak<Shared>) -> bool {
    let started = thread::Builder::new()
        .name("jstream-live".into())
        .spawn(move || live_ticker(&shared));
    if let Err(error) = &started {
        tracing::warn!(%error, "the live ticker could not start");
    }
    started.is_ok()
}

fn live_ticker(shared: &Weak<Shared>) {
    loop {
        let Some(engine) = shared.upgrade() else {
            return;
        };
        let mut wake = Vec::new();
        let next = {
            let mut state = lock(&engine.state);
            let next = state.live_tick(Instant::now(), &mut wake);
            if next.is_none() {
                state.ticking = false;
            }
            next
        };
        drop(engine);
        wake_all(wake);
        match next {
            Some(at) => thread::sleep(at.saturating_duration_since(Instant::now())),
            None => return,
        }
    }
}

/// A `live.set` value's error, named as the value: the graph names a batch's edits
/// `edits.<n>`, and `live.set` carries them as `values.<n>`.
fn values_error(error: ProtocolError) -> ProtocolError {
    match error {
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
    } else if !key.is_ascii() {
        // No streaming service issues one; a leading U+FEFF is the usual case (#441).
        "must contain only ASCII characters"
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

/// A platform capture target as `capture.targets` reports it, or `None` for a kind of
/// target no source kind captures.
fn capture_target_info(info: CaptureTargetInfo) -> Option<jstream_protocol::CaptureTargetInfo> {
    let (kind, target) = match info.target {
        CaptureTarget::Display(id) => (CaptureTargetKind::Display, id),
        CaptureTarget::Window(id) => (CaptureTargetKind::Window, id),
        _ => return None,
    };
    Some(jstream_protocol::CaptureTargetInfo {
        kind,
        target: target.as_str().to_owned(),
        name: info.name,
        size: info.size.map(|size| PixelSize {
            width: size.width(),
            height: size.height(),
        }),
        primary: info.primary,
    })
}

/// A protocol variant added after this engine was written.
/// `unsupported`, for the calls that concern the listener's credentials (`pairing.start`,
/// `device.list`, `device.revoke`) and the UI settings it keeps (`ui_settings.get`,
/// `ui_settings.set`): the server in front of the engine answers them (ADR-0044,
/// ADR-0084), so one that reaches the engine came from a client with no server between.
fn served_by_the_server(call: &str) -> ProtocolError {
    ProtocolError::Unsupported { call: call.into() }
}

fn not_implemented(what: &str) -> ProtocolError {
    ProtocolError::Internal {
        reason: format!("the engine does not implement {what} yet"),
    }
}

/// Whether `scene_id` names a keyframe scene of `state` (ADR-0024 §2).
fn is_keyframe(state: &jstream_graph::State, scene_id: &SceneId) -> bool {
    state
        .scene(scene_id)
        .is_some_and(|scene| scene.variant_of().is_some())
}
