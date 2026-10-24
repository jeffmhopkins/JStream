//! Live sources: one running [`Source`] from `jstream-sources` per source in the
//! collection whose kind produces frames, and the `jstream-sources` thread that builds
//! them and wakes the program mix when an animation's frame is due.
//!
//! A source is built when it is created and built again when its settings change; a
//! deleted source's instance goes with it. Building reads and decodes files, so it never
//! runs under the collection lock: the engine records what it wants under the lock
//! ([`LiveSources::sync`]) and the thread builds it and installs the result
//! ([`LiveSources::install`]), which re-plans the program mix. Until then the source
//! keeps showing what it showed before, or nothing.
//!
//! [`snapshot::plan`](crate::snapshot) asks the live source of every item it reaches for
//! its frame at the current time on the media clock, and when that frame next changes.
//! The thread re-plans at the earliest such time, so an animated image or a video
//! advances on the clock; a still frame is planned once. A source that draws its frames
//! as they come due (a [`Painter`]: the `waveform` and `spectrum` kinds) is painted by
//! the thread just before it re-plans, holding no engine lock, so asking it for its
//! frame under the collection lock only hands out the frame painted last. A source that
//! a plan reaches after plans that didn't is told it is shown again ([`Source::show`]),
//! so a media file can restart. A source built before plans that didn't reach it (on a
//! scene that isn't the program yet, or behind a hidden item) counts as shown again when
//! one first does.
//!
//! A media file's sound is heard while the latest plan reaches it
//! ([`LiveSources::end_plan`]). The engine hands its audio to the program stage, which
//! mixes it like an audio source (`crate::audio_sources`): [`LiveSources::take_audio`]
//! says when what there is to mix changed. A source with audio of its own that it makes
//! itself (`browser`, [`Source::audio`]) hands it to the program stage's audio inputs
//! once it is built ([`LiveSources::audio`]), whether or not a plan reaches it.
//!
//! A `browser` source paints on the GPU (ADR-0021). While a program stage runs on a
//! platform GPU context, the browser engine paints on that context's device
//! ([`LiveSources::use_gpu`]), and the plan takes each source's newest texture
//! (`Source::gpu_frame`) in place of its transparent stand-in, which the render thread
//! samples where it is. Pages repaint on their own, so such a source asks for a re-plan
//! every program frame, as an animation asks at its next frame; a page that has not
//! painted yet asks too. Without a context the browser engine has no device and the
//! source shows its stand-in.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use jstream_media::{FrameSize, MediaTime, Rounding, Timebase, VideoFrame};
use jstream_platform::{ContextKey, GpuContext, GpuFrame, MediaClock, MediaDecoders};
use jstream_protocol::{AssetRoot, FilterChain, Settings, SourceId, SourceKind};
use jstream_sources::{
    AudioScopes, BrowserBackend, CaptureSpec, Context, MediaAudio, Painter, Registry, Source,
    SourceAudio, SourceError,
};

use crate::filter_assets::{self, AssetKey, FilterAsset, FilterAssets, LoadError, Loaded};
use crate::lock;
use crate::runtime::{FRAME_DEN, FRAME_NUM};
use crate::snapshot::{CaptureKey, Frame, Look};

/// What the `jstream-sources` thread calls back into: the engine, held weakly so the
/// thread never keeps it alive.
pub(crate) trait LiveHost: Send + Sync {
    /// Installs a built source and re-plans the program mix.
    fn install(&self, built: Built);
    /// Installs a video filter's loaded asset and re-plans the program mix.
    fn install_asset(&self, loaded: LoadedAsset);
    /// Re-plans the program mix, because an animation's next frame is due.
    fn replan(&self);
}

/// The live sources of one engine. Lives in the engine state, under the collection lock.
pub(crate) struct LiveSources {
    registry: Arc<Registry>,
    /// Declared before `ctx`, so the running sources drop first: a `browser` page closes
    /// while the backend in `ctx` is still there.
    entries: BTreeMap<SourceId, Entry>,
    ctx: Context,
    /// Numbers the builds, so a result installs only if nothing changed the source
    /// since it was asked for.
    tickets: u64,
    worker: Arc<Worker>,
    host: Weak<dyn LiveHost>,
    /// Whether the thread has been started.
    started: bool,
    /// Numbers the plans of the program mix, from 1, so a source can tell whether the
    /// previous plan reached it.
    plan: u64,
    /// Whether a source's sound came, went or moved on the clock since
    /// [`take_audio`](Self::take_audio) last said.
    audio_changed: bool,
    /// The program stage's GPU context, which the browser engine paints on, while one
    /// runs.
    context: Option<ContextKey>,
    /// Every asset a video filter of the collection reads, and what loading it gave.
    assets: HashMap<AssetKey, AssetEntry>,
    /// The loaded ones, as plans read them: rebuilt when one is installed or let go, so
    /// a plan takes them with no copy.
    loaded: Arc<FilterAssets>,
}

/// One asset a video filter reads.
struct AssetEntry {
    /// The load asked for last. Only its result is installed, as that version.
    ticket: u64,
    loaded: Option<Arc<FilterAsset>>,
}

/// A live source's sound, for the program stage to mix.
#[derive(Clone, Debug)]
pub(crate) struct LiveAudio {
    /// The build it came from: a new build is a new input.
    pub(crate) ticket: u64,
    /// The source's time zero on the media clock, which its audio is stamped from.
    pub(crate) start: MediaTime,
    pub(crate) audio: MediaAudio,
}

impl PartialEq for LiveAudio {
    fn eq(&self, other: &Self) -> bool {
        self.ticket == other.ticket && self.start == other.start
    }
}

impl Eq for LiveAudio {}

/// What the engine wants for one source, and what it runs now.
struct Entry {
    kind: String,
    settings: Settings,
    /// The density its frames are drawn at (ADR-0073), as asked for last: every build
    /// asks for it.
    density: f32,
    /// The build asked for last. Only its result is installed.
    ticket: u64,
    running: Option<Running>,
}

/// A built source and the frame it showed last.
struct Running {
    source: Box<dyn Source>,
    /// The build it came from.
    ticket: u64,
    /// Its sound, if it has any.
    audio: Option<MediaAudio>,
    /// When it was built, on the media clock: its time zero.
    start: MediaTime,
    shown: Option<(Arc<VideoFrame>, Frame)>,
    /// The last plan that reached it, or the plan current when it was built, so the
    /// first plan to reach it after that counts as showing it again when one between
    /// didn't.
    planned: u64,
    /// The texture it showed last, for a source that paints on the GPU.
    shown_gpu: Option<(GpuFrame, Frame)>,
    /// Its own size, which its frames are placed at, for a kind that draws them at a
    /// density ([`Source::set_density`]); `None` for the others, which are never built
    /// again for one.
    extent: Option<FrameSize>,
}

/// One source to build.
pub(crate) struct Job {
    source_id: SourceId,
    kind: String,
    settings: Settings,
    density: f32,
    ticket: u64,
    registry: Arc<Registry>,
    ctx: Context,
}

/// One video filter asset to load.
pub(crate) struct AssetJob {
    key: AssetKey,
    ticket: u64,
    ctx: Context,
}

/// A finished asset load, for [`LiveSources::install_asset`].
pub(crate) struct LoadedAsset {
    key: AssetKey,
    ticket: u64,
    result: Result<Loaded, LoadError>,
}

/// A finished build, for [`LiveSources::install`].
pub(crate) struct Built {
    source_id: SourceId,
    ticket: u64,
    result: Result<Box<dyn Source>, SourceError>,
    /// What [`Source::set_density`] returned, asked on the thread after creating it.
    extent: Option<FrameSize>,
}

impl Default for LiveSources {
    /// Live sources that no engine owns: builds are queued but never installed.
    fn default() -> Self {
        let host: Weak<dyn LiveHost> = Weak::<NoHost>::new();
        Self::new(host)
    }
}

/// The host of a [`LiveSources`] made before its engine.
struct NoHost;

impl LiveHost for NoHost {
    fn install(&self, _: Built) {}
    fn install_asset(&self, _: LoadedAsset) {}
    fn replan(&self) {}
}

impl LiveSources {
    /// The built-in kinds, with no asset folders yet and `host` to call back.
    pub(crate) fn new(host: Weak<dyn LiveHost>) -> Self {
        Self {
            registry: Arc::new(Registry::builtin()),
            ctx: Context::default(),
            entries: BTreeMap::new(),
            tickets: 0,
            worker: Arc::new(Worker::new(Arc::new(SystemClock::new()))),
            host,
            started: false,
            plan: 0,
            audio_changed: false,
            context: None,
            assets: HashMap::new(),
            loaded: Arc::new(FilterAssets::new()),
        }
    }

    /// The clock animations run on: the platform's media clock once the host injects
    /// backends, else a monotonic clock of the engine's own.
    ///
    /// Times on the old clock mean nothing on the new one, so every running source
    /// restarts its timing now, and the re-plan the thread was waiting for, timed on the
    /// old clock, is dropped: the thread re-plans at once on the new clock, which times
    /// the next frame change afresh.
    pub(crate) fn set_clock(&mut self, clock: Arc<dyn MediaClock>) {
        let now = clock.now();
        let mut running = false;
        for source in self.entries.values_mut().filter_map(|e| e.running.as_mut()) {
            source.start = now;
            running = true;
        }
        self.audio_changed = true;
        let replan = running || lock(&self.worker.queue).wake.is_some();
        if replan {
            self.start();
        }
        self.share_painters();
        let mut queue = lock(&self.worker.queue);
        queue.clock = clock;
        queue.wake = replan.then_some(now);
        self.worker.wake.notify_all();
    }

    /// The program mix's and the buses' audio, for the `waveform` and `spectrum`
    /// sources.
    pub(crate) fn set_audio_scopes(&mut self, scopes: Arc<AudioScopes>) {
        self.ctx.audio = Some(scopes);
    }

    /// The platform's media files, which `media` sources play through. Sources built
    /// before have none, so they are built again now, from `state`, the collection's:
    /// one loaded before the host injected backends would otherwise wait for its next
    /// edit (#422).
    pub(crate) fn set_media(
        &mut self,
        media: Arc<dyn MediaDecoders>,
        state: &jstream_graph::State,
    ) {
        self.ctx.media = Some(media);
        for entry in self.entries.values_mut() {
            entry.settings = Settings::new();
        }
        self.sync(state);
    }

    /// The browser engine `browser` sources and its presets (`youtube_chat`) open their
    /// pages in. Sources of those kinds built before have none, so they are built again.
    pub(crate) fn set_browser(&mut self, browser: Arc<dyn BrowserBackend>) {
        self.ctx.browser = Some(browser);
        for entry in self
            .entries
            .values_mut()
            .filter(|e| jstream_sources::opens_browser(&e.kind))
        {
            entry.settings = Settings::new();
        }
    }

    /// Gives the browser engine the program stage's GPU context to paint on, or takes
    /// it away (`None`) when the stage stops. A browser engine set after this gets the
    /// device from the next stage.
    pub(crate) fn use_gpu(&mut self, context: Option<&GpuContext>) {
        self.context = context.map(GpuContext::key);
        if let Some(browser) = &self.ctx.browser {
            browser.use_device(context);
        }
        for running in self.entries.values_mut().filter_map(|e| e.running.as_mut()) {
            running.shown_gpu = None;
        }
    }

    /// The audio of every running source that has its own, by source.
    pub(crate) fn audio(&self) -> BTreeMap<SourceId, Arc<dyn SourceAudio>> {
        self.entries
            .iter()
            .filter_map(|(id, entry)| Some((id.clone(), entry.running.as_ref()?.source.audio()?)))
            .collect()
    }

    /// The folder behind `collection` asset references: the collection file's.
    pub(crate) fn set_collection_folder(&mut self, folder: Option<PathBuf>) {
        if self.ctx.assets.collection != folder {
            self.ctx.assets.collection = folder;
            // Sources that read assets have to read them from the new folder.
            for entry in self.entries.values_mut() {
                entry.settings = Settings::new();
            }
            // And so do video filters' assets: the next sync loads them again.
            self.assets
                .retain(|key, _| key.root != AssetRoot::Collection);
            self.share_assets();
        }
    }

    /// Brings the live sources in line with the collection's: queues a build for every
    /// source that is new or whose settings changed, and drops the ones deleted.
    ///
    /// First it keeps the audio scopes of what the collection's sources draw and drops
    /// the others (`crate::audio::retain_scopes`), before any build is queued: a build
    /// for a source drawing a new bus or source then finds its scope, and one still
    /// running from before the edit can't make a scope the edit dropped again (#701).
    pub(crate) fn sync(&mut self, state: &jstream_graph::State) {
        if let Some(scopes) = &self.ctx.audio {
            crate::audio::retain_scopes(scopes, state);
        }
        let registry = Arc::clone(&self.registry);
        let before = self.entries.len();
        self.entries.retain(|id, _| {
            state
                .source(id)
                .is_some_and(|s| framed(&registry, s.kind()).is_some())
        });
        self.audio_changed |= self.entries.len() != before;
        if self.entries.len() != before {
            self.share_painters();
        }
        let mut jobs = Vec::new();
        for source in state.sources() {
            let Some(kind) = framed(&registry, source.kind()) else {
                continue;
            };
            let entry = self.entries.entry(source.id().clone()).or_insert(Entry {
                kind: String::new(),
                settings: Settings::new(),
                density: 1.0,
                ticket: 0,
                running: None,
            });
            if entry.kind == kind && entry.settings == *source.settings() {
                continue;
            }
            self.tickets += 1;
            entry.kind = kind.to_owned();
            entry.settings = source.settings().clone();
            entry.ticket = self.tickets;
            jobs.push(Job {
                source_id: source.id().clone(),
                kind: entry.kind.clone(),
                settings: entry.settings.clone(),
                density: entry.density,
                ticket: entry.ticket,
                registry: Arc::clone(&self.registry),
                ctx: self.ctx.clone(),
            });
        }
        let loads = self.sync_assets(state);
        if !jobs.is_empty() || !loads.is_empty() {
            self.start();
            let mut queue = lock(&self.worker.queue);
            queue.jobs.extend(jobs);
            queue.loads.extend(loads);
            self.worker.wake.notify_all();
        }
    }

    /// Brings the video filters' assets in line with the collection's: lets go of the
    /// ones no filter reads any more and returns a load for each new one. Every filter
    /// counts, enabled or not, so enabling one shows its asset at once.
    fn sync_assets(&mut self, state: &jstream_graph::State) -> Vec<AssetJob> {
        let wanted: HashSet<AssetKey> = state
            .sources()
            .flat_map(|source| source.filters().chain(FilterChain::Video).iter())
            .filter_map(|filter| filter_assets::wanted(filter.kind(), filter.settings()))
            .collect();
        let before = self.assets.len();
        self.assets.retain(|key, _| wanted.contains(key));
        if self.assets.len() != before {
            self.share_assets();
        }
        let mut loads = Vec::new();
        for key in wanted {
            if self.assets.contains_key(&key) {
                continue;
            }
            self.tickets += 1;
            self.assets.insert(
                key.clone(),
                AssetEntry {
                    ticket: self.tickets,
                    loaded: None,
                },
            );
            loads.push(AssetJob {
                key,
                ticket: self.tickets,
                ctx: self.ctx.clone(),
            });
        }
        loads
    }

    /// Installs a loaded asset if it is still the one wanted. Returns whether a plan
    /// may draw differently, so the caller re-plans. A failed load is logged, and its
    /// filters pass their input through until the setting changes.
    pub(crate) fn install_asset(&mut self, loaded: LoadedAsset) -> bool {
        let Some(entry) = self.assets.get_mut(&loaded.key) else {
            return false;
        };
        if entry.ticket != loaded.ticket {
            return false;
        }
        match loaded.result {
            Ok(asset) => {
                entry.loaded = Some(Arc::new(FilterAsset {
                    version: loaded.ticket,
                    loaded: asset,
                }));
                self.share_assets();
                true
            }
            Err(err) => {
                tracing::warn!(
                    path = %loaded.key.path,
                    root = ?loaded.key.root,
                    error = %err,
                    "video filter asset could not be loaded; its filter passes its input through"
                );
                false
            }
        }
    }

    /// Rebuilds what plans read from the loaded assets.
    fn share_assets(&mut self) {
        self.loaded = Arc::new(
            self.assets
                .iter()
                .filter_map(|(key, entry)| Some((key.clone(), Arc::clone(entry.loaded.as_ref()?))))
                .collect(),
        );
    }

    /// The loaded video filter assets, for a plan.
    pub(crate) fn filter_assets(&self) -> Arc<FilterAssets> {
        Arc::clone(&self.loaded)
    }

    /// Installs a build if it is still the one wanted. Returns whether what the source
    /// shows may have changed, so the caller re-plans.
    pub(crate) fn install(&mut self, built: Built) -> bool {
        let Some(entry) = self.entries.get_mut(&built.source_id) else {
            return false;
        };
        if entry.ticket != built.ticket {
            return false;
        }
        let plan = self.plan;
        self.audio_changed = true;
        let running = match built.result {
            Ok(source) => Some(Running {
                audio: source.sound(),
                source,
                ticket: built.ticket,
                start: self.worker.now(),
                shown: None,
                planned: plan,
                shown_gpu: None,
                extent: built.extent,
            }),
            // A kind that is described but not built yet draws what it drew before C1b:
            // a `display` source shows the pipeline's capture.
            Err(SourceError::NotBuilt { .. }) => None,
            Err(err) => {
                tracing::warn!(
                    source_id = %built.source_id,
                    kind = %entry.kind,
                    error = %err,
                    "source could not be started; it shows nothing"
                );
                None
            }
        };
        let painted = running
            .as_ref()
            .is_some_and(|r| r.source.painter().is_some())
            || entry
                .running
                .as_ref()
                .is_some_and(|r| r.source.painter().is_some());
        entry.running = running;
        if painted {
            self.share_painters();
        }
        true
    }

    /// Hands the thread every running source's painter and its time zero, after the
    /// set of painted sources or their timing changed.
    fn share_painters(&mut self) {
        let painters = self
            .entries
            .values()
            .filter_map(|e| e.running.as_ref())
            .filter_map(|r| Some((r.source.painter()?, r.start)))
            .collect();
        lock(&self.worker.queue).painters = painters;
    }

    /// The current time on the clock sources run on.
    pub(crate) fn now(&self) -> MediaTime {
        self.worker.now()
    }

    /// Draws each source in `scales` ([`Shot::scales`](crate::snapshot::Shot::scales))
    /// at the density its most enlarged item needs to stay sharp (ADR-0073), for a kind
    /// that draws at a density. Drawing again can take as long as building, so it is a
    /// build on the thread, at the new density, and the source shows its old frame until
    /// that is installed. Densities go up or down in quarter steps of a doubling, so a
    /// small change in scale builds nothing, and a source waits for the build it has in
    /// flight before it is asked for another: the plan that installs it measures again.
    pub(crate) fn densities(&mut self, scales: &BTreeMap<SourceId, f32>) {
        let mut jobs = Vec::new();
        for (id, &scale) in scales {
            let Some(entry) = self.entries.get_mut(id) else {
                continue;
            };
            let Some(running) = &entry.running else {
                continue;
            };
            if running.extent.is_none() || running.ticket != entry.ticket {
                continue;
            }
            let density = density_for(scale);
            if density == entry.density {
                continue;
            }
            self.tickets += 1;
            entry.density = density;
            entry.ticket = self.tickets;
            jobs.push(Job {
                source_id: id.clone(),
                kind: entry.kind.clone(),
                settings: entry.settings.clone(),
                density,
                ticket: entry.ticket,
                registry: Arc::clone(&self.registry),
                ctx: self.ctx.clone(),
            });
        }
        if !jobs.is_empty() {
            self.start();
            lock(&self.worker.queue).jobs.extend(jobs);
            self.worker.wake.notify_all();
        }
    }

    /// Starts a plan of the program mix: the [`look`](Self::look) calls until the next
    /// one are the sources it reaches.
    pub(crate) fn begin_plan(&mut self) {
        self.plan += 1;
    }

    /// What `source_id` shows at `now`: what the pipeline captures for a capture source,
    /// else its frame, numbered from `versions` when it is new, and when that frame is
    /// next replaced. `None` when the source has no live instance.
    ///
    /// A source the previous plan didn't reach, though an earlier one did or it was
    /// built before it, is shown again first ([`Source::show`]).
    pub(crate) fn look(
        &mut self,
        source_id: &SourceId,
        now: MediaTime,
        versions: &mut u64,
    ) -> Option<Look> {
        let plan = self.plan;
        let entry = self.entries.get_mut(source_id)?;
        let running = entry.running.as_mut()?;
        if let Some(spec) = running.source.capture() {
            // A window source with no window chosen shows nothing.
            let show_cursor = running.source.show_cursor();
            return (*spec != CaptureSpec::Window(None)).then(|| {
                Look::Capture(
                    CaptureKey::for_source(spec.clone(), source_id.clone())
                        .with_cursor(show_cursor),
                )
            });
        }
        let at = since(now, running.start);
        if running.planned + 1 < plan {
            running.source.show(at);
        }
        running.planned = plan;
        if let Some(context) = &self.context {
            let next = next_program_frame(now);
            if let Some(texture) = running.source.gpu_frame() {
                let frame = match &running.shown_gpu {
                    Some((shown, frame)) if *shown == texture => frame.clone(),
                    _ => {
                        *versions = versions.saturating_add(1);
                        let frame = Frame::from_gpu(texture.clone(), context.clone(), *versions);
                        running.shown_gpu = Some((texture, frame.clone()));
                        frame
                    }
                };
                return Some(Look::Frame(frame, next));
            }
            if jstream_sources::opens_browser(&entry.kind) {
                // Not painted yet: the stand-in, until a re-plan finds the first paint.
                let (frame, _) = Self::cpu_frame(source_id, running, at, now, versions)?;
                return Some(Look::Frame(frame, next));
            }
        }
        let (frame, next) = Self::cpu_frame(source_id, running, at, now, versions)?;
        Some(Look::Frame(frame, next))
    }

    /// [`Self::look`]'s frame from the source's CPU frame at `at`, its own time.
    fn cpu_frame(
        source_id: &SourceId,
        running: &mut Running,
        at: MediaTime,
        now: MediaTime,
        versions: &mut u64,
    ) -> Option<(Frame, Option<MediaTime>)> {
        let video = match running.source.frame(at) {
            Ok(video) => video,
            Err(err) => {
                tracing::warn!(source_id = %source_id, error = %err, "source produced no frame");
                return None;
            }
        };
        let frame = match &running.shown {
            Some((shown, frame)) if Arc::ptr_eq(shown, &video) => frame.clone(),
            _ => {
                *versions = versions.saturating_add(1);
                let frame = Frame::from_video(Arc::clone(&video), *versions)?;
                let frame = match running.extent {
                    Some(extent) => frame.placed_at(extent),
                    None => frame,
                };
                running.shown = Some((video, frame.clone()));
                frame
            }
        };
        let next = running
            .source
            .next_change(at)
            .and_then(|change| after(running.start, change, now.timebase()));
        Some((frame, next))
    }

    /// Ends a plan of the program mix: a source's sound is heard while the plan reached
    /// it.
    pub(crate) fn end_plan(&self) {
        let plan = self.plan;
        for running in self.entries.values().filter_map(|e| e.running.as_ref()) {
            if let Some(audio) = &running.audio {
                audio.set_audible(running.planned == plan);
            }
        }
    }

    /// Every live source's sound, if that changed since the last call.
    pub(crate) fn take_audio(&mut self) -> Option<BTreeMap<SourceId, LiveAudio>> {
        if !std::mem::take(&mut self.audio_changed) {
            return None;
        }
        let audio = self
            .entries
            .iter()
            .filter_map(|(id, entry)| {
                let running = entry.running.as_ref()?;
                let audio = running.audio.clone()?;
                Some((
                    id.clone(),
                    LiveAudio {
                        ticket: running.ticket,
                        start: running.start,
                        audio,
                    },
                ))
            })
            .collect();
        Some(audio)
    }

    /// Asks the thread to re-plan at `at`, or at no set time.
    pub(crate) fn wake_at(&mut self, at: Option<MediaTime>) {
        if at.is_some() {
            self.start();
        }
        let mut queue = lock(&self.worker.queue);
        if queue.wake != at {
            queue.wake = at;
            self.worker.wake.notify_all();
        }
    }

    /// The thread's queue, so a test can wait for the builds it queued.
    #[cfg(test)]
    pub(crate) fn worker(&self) -> Arc<Worker> {
        Arc::clone(&self.worker)
    }

    fn start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        let worker = Arc::clone(&self.worker);
        let host = self.host.clone();
        if let Err(err) = thread::Builder::new()
            .name("jstream-sources".into())
            .spawn(move || run(&worker, &host))
        {
            tracing::error!(error = %err, "starting the jstream-sources thread failed");
            self.started = false;
        }
    }
}

impl Drop for LiveSources {
    /// Ends the thread with the engine.
    fn drop(&mut self) {
        lock(&self.worker.queue).closed = true;
        self.worker.wake.notify_all();
    }
}

/// The kind of a source that `jstream-sources` may run: every built-in kind. Scene
/// sources are drawn by the program mix itself.
fn builtin(kind: &SourceKind) -> Option<&str> {
    match kind {
        SourceKind::Builtin { kind } => Some(kind),
        _ => None,
    }
}

/// The kind of a source that `jstream-sources` may run and that makes frames: every
/// built-in kind but the audio-only ones, which the program stage records instead
/// (`crate::audio_sources`).
fn framed<'a>(registry: &Registry, kind: &'a SourceKind) -> Option<&'a str> {
    builtin(kind).filter(|kind| registry.descriptor(kind).is_none_or(|d| d.video))
}

/// `now - start` in milliseconds, the time base sources count in; zero before the start.
fn since(now: MediaTime, start: MediaTime) -> MediaTime {
    now.checked_sub(start)
        .and_then(|t| t.rescale(Timebase::MILLIS, Rounding::Down))
        .unwrap_or_else(|_| MediaTime::zero(Timebase::MILLIS))
}

/// One program frame after `now`, when a source that paints on the GPU is planned again.
fn next_program_frame(now: MediaTime) -> Option<MediaTime> {
    let period = MediaTime::new(1, Timebase::new(FRAME_NUM, FRAME_DEN).ok()?)
        .rescale(now.timebase(), Rounding::Up)
        .ok()?;
    now.checked_add(period).ok()
}

/// The clock time `change` after `start`, rounded up so the frame is due by then.
fn after(start: MediaTime, change: MediaTime, timebase: Timebase) -> Option<MediaTime> {
    change
        .rescale(timebase, Rounding::Up)
        .and_then(|change| start.checked_add(change))
        .ok()
}

/// The thread's queue and the clock it waits on.
pub(crate) struct Worker {
    queue: Mutex<Queue>,
    wake: Condvar,
}

struct Queue {
    jobs: VecDeque<Job>,
    /// Video filter assets to load, after the builds queued before them.
    loads: VecDeque<AssetJob>,
    /// When to re-plan next.
    wake: Option<MediaTime>,
    /// Builds taken but not installed yet, so `settled` waits for them too.
    building: usize,
    /// The running sources that draw their frames as they come due, with each one's
    /// time zero: the thread paints them before every re-plan.
    painters: Vec<(Arc<dyn Painter>, MediaTime)>,
    clock: Arc<dyn MediaClock>,
    closed: bool,
}

impl Worker {
    fn new(clock: Arc<dyn MediaClock>) -> Self {
        Self {
            queue: Mutex::new(Queue {
                jobs: VecDeque::new(),
                loads: VecDeque::new(),
                wake: None,
                building: 0,
                painters: Vec::new(),
                clock,
                closed: false,
            }),
            wake: Condvar::new(),
        }
    }

    fn now(&self) -> MediaTime {
        let clock = Arc::clone(&lock(&self.queue).clock);
        clock.now()
    }

    /// Waits until no build is queued or running, up to `limit`. For tests.
    #[cfg(test)]
    pub(crate) fn settled(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut queue = lock(&self.queue);
        while !queue.jobs.is_empty() || !queue.loads.is_empty() || queue.building > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            queue = self
                .wake
                .wait_timeout(queue, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        true
    }
}

/// What the thread does next.
enum Next {
    Build(Job),
    Load(AssetJob),
    /// Paint these at this time, then re-plan.
    Replan(Vec<(Arc<dyn Painter>, MediaTime)>, MediaTime),
    Exit,
}

/// The `jstream-sources` thread: builds queued sources and re-plans when an animation's
/// frame is due, until the engine goes away.
fn run(worker: &Worker, host: &Weak<dyn LiveHost>) {
    loop {
        let next = {
            let mut queue = lock(&worker.queue);
            loop {
                if queue.closed {
                    break Next::Exit;
                }
                if let Some(job) = queue.jobs.pop_front() {
                    queue.building += 1;
                    break Next::Build(job);
                }
                if let Some(job) = queue.loads.pop_front() {
                    queue.building += 1;
                    break Next::Load(job);
                }
                let Some(due) = queue.wake else {
                    queue = worker
                        .wake
                        .wait(queue)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    continue;
                };
                let now = queue.clock.now();
                if now >= due {
                    queue.wake = None;
                    break Next::Replan(queue.painters.clone(), now);
                }
                let wait = until(now, due);
                queue = worker
                    .wake
                    .wait_timeout(queue, wait)
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
            }
        };
        match next {
            Next::Exit => return,
            Next::Replan(painters, now) => {
                // Drawing can take milliseconds, so it happens here, before the re-plan
                // takes the collection lock and asks for the painted frames.
                for (painter, start) in painters {
                    painter.paint(since(now, start));
                }
                match host.upgrade() {
                    Some(host) => host.replan(),
                    None => return,
                }
            }
            Next::Load(job) => {
                let loaded = LoadedAsset {
                    result: filter_assets::load(&job.key, &job.ctx),
                    key: job.key,
                    ticket: job.ticket,
                };
                let alive = host
                    .upgrade()
                    .map(|host| host.install_asset(loaded))
                    .is_some();
                lock(&worker.queue).building -= 1;
                worker.wake.notify_all();
                if !alive {
                    return;
                }
            }
            Next::Build(job) => {
                let mut result = job.registry.create(&job.kind, &job.settings, &job.ctx);
                // Drawn at its density here, off the collection lock, as it was created.
                let extent = result
                    .as_mut()
                    .ok()
                    .and_then(|source| source.set_density(job.density));
                let built = Built {
                    source_id: job.source_id,
                    ticket: job.ticket,
                    result,
                    extent,
                };
                let alive = host.upgrade().map(|host| host.install(built)).is_some();
                lock(&worker.queue).building -= 1;
                worker.wake.notify_all();
                if !alive {
                    return;
                }
            }
        }
    }
}

/// The density a source drawn `scale` times enlarged is drawn at: 1 up to a scale of 1,
/// then the next quarter step of a doubling at or above it (1.19, 1.41, 1.68, 2 and so
/// on), up to [`jstream_sources::MAX_DENSITY`].
fn density_for(scale: f32) -> f32 {
    if scale.is_nan() || scale <= 1.0 {
        return 1.0;
    }
    let steps = (scale.log2() * 4.0).ceil();
    2f32.powf(steps / 4.0)
        .clamp(1.0, jstream_sources::MAX_DENSITY)
}

/// How long to wait from `now` until `due`: at least a millisecond, so a wait never
/// spins, and at most a second, so a clock that is not wall time (a test's) is checked
/// again.
fn until(now: MediaTime, due: MediaTime) -> Duration {
    let micros = due
        .checked_sub(now)
        .and_then(|t| t.rescale(Timebase::MICROS, Rounding::Up))
        .map_or(1_000, |t| u64::try_from(t.ticks()).unwrap_or(0));
    Duration::from_micros(micros.clamp(1_000, 1_000_000))
}

/// A monotonic clock from the OS counter, in microseconds since it was made: the clock
/// sources run on until the host injects the platform's.
struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl MediaClock for SystemClock {
    fn timebase(&self) -> Timebase {
        Timebase::MICROS
    }

    fn now(&self) -> MediaTime {
        let micros = i64::try_from(self.origin.elapsed().as_micros()).unwrap_or(i64::MAX);
        MediaTime::new(micros, Timebase::MICROS)
    }
}

#[cfg(test)]
mod tests {
    use super::density_for;

    #[test]
    fn a_density_is_the_next_quarter_step_of_a_doubling() {
        for scale in [f32::NAN, -3.0, 0.0, 0.5, 1.0] {
            assert_eq!(density_for(scale), 1.0, "{scale}");
        }
        assert_eq!(density_for(1.01), 2f32.powf(0.25));
        assert_eq!(density_for(2.0), 2.0);
        assert_eq!(density_for(2.1), 2f32.powf(1.25));
        assert_eq!(density_for(6.0), 2f32.powf(2.75));
        assert_eq!(density_for(9.0), jstream_sources::MAX_DENSITY);
        assert_eq!(density_for(f32::INFINITY), jstream_sources::MAX_DENSITY);
    }
}
