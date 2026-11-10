//! Live sources: one running [`Source`] from `jstream-sources` per source in the
//! collection whose kind produces frames, and the `jstream-sources` thread that builds
//! them and wakes the program mix when an animation's frame is due.
//!
//! A source is built when it is created and built again when its settings change; a
//! deleted source's instance goes with it. A source built again starts its time again,
//! unless its kind keeps the old instance's time zero for that change
//! (`SourceKind::keeps_time`: a `timer` keeps counting through a change of its look)
//! or it is built again only for a new density. Building reads and decodes files, so it never
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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use jstream_media::{FrameSize, MediaTime, Rounding, Timebase, VideoFrame};
use jstream_platform::{ContextKey, GpuContext, GpuFrame, MediaClock, MediaDecoders};
use jstream_protocol::{AssetRoot, BusId, FilterChain, SceneId, Settings, SourceId, SourceKind};
use jstream_sources::{
    AudioScope, AudioScopes, BrowserBackend, BusLevels, CaptureSpec, Context, MediaAudio, PageView,
    Painter, Registry, Source, SourceAudio, SourceError,
};
use serde_json::Value;

use crate::effect_packs::{PackEffects, PackSources};
use crate::filter_assets::{self, AssetKey, FilterAsset, FilterAssets, LoadError, Loaded};
use crate::lock;
use crate::pack_source;
use crate::profile::SourceMeters;
use crate::runtime::{frame_period, on_frame_grid};
use crate::slot::{FrameSlot, Stored};
use crate::snapshot::{CaptureKey, Frame, Look};
use crate::still::Stills;

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
    /// When the last plan asked to be re-planned ([`Self::wake_at`]), and when a granted
    /// page has an update still to send ([`Self::feed_pages`]): the thread wakes at the
    /// earlier.
    plan_wake: Option<MediaTime>,
    page_wake: Option<MediaTime>,
    ctx: Context,
    /// Numbers the builds, so a result installs only if nothing changed the source
    /// since it was asked for.
    tickets: u64,
    /// Counts the builds installed, so what is planned from the running sources alone
    /// (the warm scenes' captures) is planned again when one changes.
    installs: u64,
    /// The sources the warm scenes draw (ADR-0117 §7): they never rest, so showing one
    /// finds a page loaded and painting.
    warm: BTreeSet<SourceId>,
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
    /// The GPU context the browser engine paints on: the engine's (ADR-0117 §5), which
    /// the program stage draws on too, or `None` when there is none.
    context: Option<ContextKey>,
    /// Every asset a video filter of the collection reads, and what loading it gave.
    assets: HashMap<AssetKey, AssetEntry>,
    /// The loaded ones, as plans read them: rebuilt when one is installed or let go, so
    /// a plan takes them with no copy.
    loaded: Arc<FilterAssets>,
    /// What each source's paints, builds and plans' looks cost, and the plans, for
    /// `profile.get` (ADR-0103). The thread holds it too.
    meters: Arc<SourceMeters>,
    /// The thread that holds and lets go of shown stills' pixels (#1139).
    stills: Stills,
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
    /// The time zero that build keeps, on the media clock: the running instance's, for
    /// a build at another density or for a settings change its kind counts on through
    /// (`SourceKind::keeps_time`, a `timer`'s style); `None` starts it at its install.
    anchor: Option<MediaTime>,
    running: Option<Running>,
}

impl Entry {
    /// The time zero a build queued now would keep: the one the build in flight keeps,
    /// if there is one (`None` when it starts again, which a later build mustn't undo),
    /// else the running instance's.
    fn time_zero(&self) -> Option<MediaTime> {
        match &self.running {
            Some(running) if running.ticket == self.ticket => Some(running.start),
            _ => self.anchor,
        }
    }
}

/// A built source and the frame it showed last.
struct Running {
    source: Box<dyn Source>,
    /// The build it came from.
    ticket: u64,
    /// Its sound, if it has any.
    audio: Option<MediaAudio>,
    /// Its time zero on the media clock: when it was installed, or the time zero of the
    /// instance before it that its build kept ([`Entry::anchor`]).
    start: MediaTime,
    shown: Option<(Arc<VideoFrame>, Frame)>,
    /// The still image it shows and the frame plans show it by, for a source whose one
    /// frame is a [`jstream_sources::Still`]: its pixels are held only while a draw
    /// needs them ([`crate::still`]).
    shown_still: Option<(Arc<jstream_sources::Still>, Frame)>,
    /// The last plan that reached it, or the plan current when it was built, so the
    /// first plan to reach it after that counts as showing it again when one between
    /// didn't.
    planned: u64,
    /// The last plan any mix looked at it in (the program's, a video mix's or a
    /// preview's), or the plan current when it was built.
    looked: u64,
    /// Whether it was told no mix draws it ([`Source::set_drawn`], #1071).
    resting: bool,
    /// Whether it was cued to be shown, on a warm scene no mix drew ([`Source::cue`],
    /// ADR-0117 §7), and not looked at since.
    cued: bool,
    /// `!resting`, shared with the thread's [`Painted`] entry: the thread paints a
    /// painter only while some mix draws it.
    drawn: Arc<AtomicBool>,
    /// The texture it showed last, for a source that paints on the GPU.
    shown_gpu: Option<(GpuFrame, Frame)>,
    /// A painter's frame slot (ADR-0117 §3), and the frame plans show it by: the thread
    /// stores each new frame of the same size there, with no re-plan.
    slot: Option<(Arc<FrameSlot>, Option<Frame>)>,
    /// Its own size, which its frames are placed at, for a kind that draws them at a
    /// density ([`Source::set_density`]); `None` for the others, which are never built
    /// again for one.
    extent: Option<FrameSize>,
    /// The size `source.sizes` reports for it (ADR-0022 §4): its first frame's, from the
    /// build, then each new frame's a plan takes. Its extent for a kind drawn at a
    /// density. `None` for a capture source, whose frames reach only the render thread.
    size: Option<FrameSize>,
}

/// One source to build.
pub(crate) struct Job {
    source_id: SourceId,
    kind: String,
    settings: Settings,
    density: f32,
    ticket: u64,
    /// The time zero it keeps ([`Entry::anchor`]), so the thread paints it for the
    /// right time before it is installed.
    anchor: Option<MediaTime>,
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
    /// Its size, from its first frame, asked on the thread too ([`first_size`]).
    size: Option<FrameSize>,
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
        let meters = Arc::new(SourceMeters::default());
        Self {
            registry: Arc::new(Registry::builtin()),
            ctx: Context::default(),
            entries: BTreeMap::new(),
            plan_wake: None,
            page_wake: None,
            tickets: 0,
            installs: 0,
            warm: BTreeSet::new(),
            worker: Arc::new(Worker::new(
                Arc::new(SystemClock::new()),
                Arc::clone(&meters),
            )),
            host,
            started: false,
            plan: 0,
            audio_changed: false,
            context: None,
            assets: HashMap::new(),
            loaded: Arc::new(FilterAssets::new()),
            meters,
            stills: Stills::default(),
        }
    }

    /// What each source's paints, builds and plans' looks cost, and the plans
    /// (ADR-0103).
    pub(crate) fn meters(&self) -> Arc<SourceMeters> {
        Arc::clone(&self.meters)
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
        for entry in self.entries.values_mut() {
            entry.anchor = entry.anchor.map(|_| now);
            if let Some(source) = entry.running.as_mut() {
                source.start = now;
                running = true;
            }
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

    /// The MIDI inputs the `midi_*` sources draw (ADR-0137), which the JTunnel receiver
    /// feeds.
    pub(crate) fn set_midi(&mut self, midi: Arc<jstream_sources::MidiInputs>) {
        // Their clock moves the filters that follow the tempo (ADR-0148).
        self.loaded = Arc::new(
            (*self.loaded)
                .clone()
                .with_tempo(crate::tempo::Tempo(Arc::clone(&midi))),
        );
        self.ctx.midi = Some(midi);
    }

    /// The MIDI inputs the `midi_*` sources draw.
    pub(crate) fn midi(&self) -> Option<Arc<jstream_sources::MidiInputs>> {
        self.ctx.midi.clone()
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
    /// pages in, painting on `context`, the engine's GPU context (ADR-0117 §5), which it is
    /// given before any page opens. Sources of those kinds built before have none, so
    /// they are built again.
    pub(crate) fn set_browser(
        &mut self,
        browser: Arc<dyn BrowserBackend>,
        context: Option<&GpuContext>,
    ) {
        browser.use_device(context);
        self.context = context.map(GpuContext::key);
        for running in self.entries.values_mut().filter_map(|e| e.running.as_mut()) {
            running.shown_gpu = None;
        }
        self.ctx.browser = Some(browser);
        for entry in self
            .entries
            .values_mut()
            .filter(|e| jstream_sources::opens_browser(&e.kind))
        {
            entry.settings = Settings::new();
        }
    }

    /// Gives the browser engine the GPU context to paint on: the engine's, as the browser
    /// engine is set and again once a lost one is replaced, or `None` when there is
    /// none. The context it already paints on is not given again, so pages keep their
    /// frames (ADR-0117 §5).
    pub(crate) fn use_gpu(&mut self, context: Option<&GpuContext>) {
        let key = context.map(GpuContext::key);
        if key == self.context {
            return;
        }
        self.context = key;
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

    /// Takes the program's base frame rate. A page that follows it (`fps` 0, #1405) is
    /// built again at a new one, so it paints as often as the program draws.
    fn sync_frame_rate(&mut self, fps: u32) {
        if self.ctx.frame_rate == fps {
            return;
        }
        self.ctx.frame_rate = fps;
        for entry in self.entries.values_mut() {
            if follows_program_rate(&entry.kind, &entry.settings) {
                entry.settings = Settings::new();
            }
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
        self.sync_heard(state);
        self.sync_frame_rate(state.base_fps());
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
        self.meters.retain(|id| state.source(id).is_some());
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
                anchor: None,
                running: None,
            });
            let settings = built_from(kind, source.settings());
            if entry.kind == kind && entry.settings == settings {
                continue;
            }
            let keeps = entry.kind == kind && registry.keeps_time(kind, &entry.settings, &settings);
            entry.anchor = if keeps { entry.time_zero() } else { None };
            self.tickets += 1;
            entry.kind = kind.to_owned();
            entry.settings = settings;
            entry.ticket = self.tickets;
            jobs.push(Job {
                source_id: source.id().clone(),
                kind: entry.kind.clone(),
                settings: entry.settings.clone(),
                density: entry.density,
                ticket: entry.ticket,
                anchor: entry.anchor,
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

    /// Finds the scope each shader source that hears audio hears (ADR-0093), after the
    /// scopes were kept for the collection: plans hand it to the program each frame.
    fn sync_heard(&mut self, state: &jstream_graph::State) {
        let heard: BTreeMap<SourceId, Arc<AudioScope>> = match &self.ctx.audio {
            Some(scopes) => state
                .sources()
                .filter(|source| match source.kind() {
                    SourceKind::Builtin { kind } => pack_source::is_pack_kind(kind),
                    _ => false,
                })
                .filter(|source| source.settings().get("audio").is_some())
                .filter_map(|source| {
                    let scope = jstream_sources::heard_scope(scopes, source.settings())?;
                    Some((source.id().clone(), scope))
                })
                .collect(),
            None => BTreeMap::new(),
        };
        let same = heard.len() == self.loaded.heard().len()
            && heard
                .iter()
                .zip(self.loaded.heard().iter())
                .all(|((a, x), (b, y))| a == b && Arc::ptr_eq(x, y));
        if !same {
            self.loaded = Arc::new((*self.loaded).clone().with_heard(Arc::new(heard)));
        }
        // And the scope each audio motion modifier hears (ADR-0100).
        let motion_heard = match &self.ctx.audio {
            Some(scopes) => {
                crate::reactive::heard_scopes(scopes, crate::audio::heard_by_motion(state))
            }
            None => BTreeMap::new(),
        };
        let same = motion_heard.len() == self.loaded.motion_heard().len()
            && motion_heard
                .iter()
                .zip(self.loaded.motion_heard().iter())
                .all(|((a, x), (b, y))| a == b && Arc::ptr_eq(x, y));
        if !same {
            self.loaded = Arc::new(
                (*self.loaded)
                    .clone()
                    .with_motion_heard(Arc::new(motion_heard)),
            );
        }
    }

    /// Brings the video filters' assets in line with the collection's: lets go of the
    /// ones no filter reads any more and returns a load for each new one. Every filter
    /// counts, enabled or not, so enabling one shows its asset at once.
    fn sync_assets(&mut self, state: &jstream_graph::State) -> Vec<AssetJob> {
        // Every video chain that can read an asset: each source's, each scene's, each
        // item's effects (ADR-0065), the master stage's and each output's (ADR-0080).
        let stage = state.master().chain(FilterChain::Video).iter().chain(
            state
                .outputs()
                .flat_map(|output| output.filters().chain(FilterChain::Video).iter()),
        );
        let sources = state
            .sources()
            .flat_map(|source| source.filters().chain(FilterChain::Video).iter());
        let scenes = state.scenes().iter().flat_map(|scene| {
            let effects = scene.items().iter().flat_map(|item| item.effects().iter());
            scene
                .filters()
                .chain(FilterChain::Video)
                .iter()
                .chain(effects)
        });
        let wanted: HashSet<AssetKey> = sources
            .chain(scenes)
            .chain(stage)
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

    /// The first loaded asset again, as a finished load of the version wanted now, for
    /// a test that replays an install.
    #[cfg(test)]
    pub(crate) fn reload_asset(&self) -> Option<LoadedAsset> {
        let (key, entry) = self.assets.iter().next()?;
        Some(LoadedAsset {
            key: key.clone(),
            ticket: entry.ticket,
            result: Ok(entry.loaded.as_ref()?.loaded.clone()),
        })
    }

    /// Rebuilds what plans read from the loaded assets, keeping everything else they
    /// read: the pack effects and shader sources, the audio scopes and the MIDI inputs
    /// (#1453 review B1).
    fn share_assets(&mut self) {
        let images = self
            .assets
            .iter()
            .filter_map(|(key, entry)| Some((key.clone(), Arc::clone(entry.loaded.as_ref()?))))
            .collect();
        self.loaded = Arc::new((*self.loaded).clone().with_images(images));
    }

    /// Hands plans the effect packs' effects and shader sources as now loaded
    /// ([`crate::effect_packs`]).
    pub(crate) fn set_effects(&mut self, effects: PackEffects, sources: PackSources) {
        self.loaded = Arc::new(
            (*self.loaded)
                .clone()
                .with_effects(effects)
                .with_sources(sources),
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
        self.installs += 1;
        let running = match built.result {
            Ok(source) => Some(Running {
                slot: source
                    .painter()
                    .and_then(|painter| FrameSlot::new(painter.frame()))
                    .map(|slot| (slot, None)),
                audio: source.sound(),
                source,
                ticket: built.ticket,
                start: entry.anchor.unwrap_or_else(|| self.worker.now()),
                shown: None,
                shown_still: None,
                planned: plan,
                looked: plan,
                resting: false,
                cued: false,
                drawn: Arc::new(AtomicBool::new(true)),
                shown_gpu: None,
                extent: built.extent,
                size: built.size,
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
            .iter()
            .filter_map(|(id, e)| Some((id, e.running.as_ref()?)))
            .filter_map(|(id, r)| {
                Some(Painted {
                    source_id: id.clone(),
                    painter: r.source.painter()?,
                    start: r.start,
                    drawn: Arc::clone(&r.drawn),
                    slot: r.slot.as_ref().map(|(slot, _)| Arc::clone(slot)),
                })
            })
            .collect();
        let mut queue = lock(&self.worker.queue);
        queue.painters = painters;
        // Painted from now on, on the thread's own wake.
        queue.paint_wake = Some(queue.clock.now());
        drop(queue);
        self.worker.wake.notify_all();
    }

    /// The current time on the clock sources run on.
    pub(crate) fn now(&self) -> MediaTime {
        self.worker.now()
    }

    /// Starts source `source_id` again from its beginning (`source.restart`, ADR-0115):
    /// [`Source::restart`] on its running instance, at `at`. A cue passes the cue's time,
    /// so a late tick starts a media file at the cue rather than when the tick ran; a
    /// call from a client passes the clock's now. `at` is brought onto the source
    /// clock's time base. A source with no running instance has nothing to restart.
    pub(crate) fn restart(&mut self, source_id: &SourceId, at: MediaTime) {
        let now = at
            .rescale(self.now().timebase(), Rounding::Down)
            .unwrap_or_else(|_| self.now());
        if let Some(running) = self
            .entries
            .get_mut(source_id)
            .and_then(|entry| entry.running.as_mut())
        {
            running.source.restart(since(now, running.start));
        }
    }

    /// Tells each running source whose page has the page API (ADR-0102) what it may read
    /// now: whether it is shown on program and on preview, nested scenes included, the
    /// two scenes, the shared values, and each bus's levels (`buses`, read only when a
    /// page is granted). Each source sends its page only what changed,
    /// so this runs after anything that can change one of them.
    pub(crate) fn feed_pages(
        &mut self,
        state: &jstream_graph::State,
        program: Option<&SceneId>,
        preview: Option<&SceneId>,
        values: &BTreeMap<String, Value>,
        buses: &dyn Fn() -> BTreeMap<BusId, BusLevels>,
    ) {
        let granted = |entry: &Entry| entry.running.as_ref().is_some_and(|r| r.source.page_api());
        if !self.entries.values().any(granted) {
            if self.page_wake.take().is_some() {
                self.set_wake();
            }
            return;
        }
        // Read only when a page is granted: a few atomic loads per bus.
        let buses = buses();
        let on_program = program.map(|scene| crate::activity::shown_sources(state, scene));
        let on_preview = preview.map(|scene| crate::activity::shown_sources(state, scene));
        let scene = |scene_id: Option<&SceneId>| {
            let scene = state.scene(scene_id?)?;
            Some((scene.id().as_str(), scene.name()))
        };
        let now = self.now();
        let mut due = None;
        for (source_id, entry) in &mut self.entries {
            let Some(running) = entry.running.as_mut().filter(|r| r.source.page_api()) else {
                continue;
            };
            let at = running.source.page_view(&PageView {
                now,
                program: on_program
                    .as_ref()
                    .is_some_and(|shown| shown.contains(source_id)),
                preview: on_preview
                    .as_ref()
                    .is_some_and(|shown| shown.contains(source_id)),
                program_scene: scene(program),
                preview_scene: scene(preview),
                values,
                buses: &buses,
            });
            due = crate::snapshot::earliest(due, at);
        }
        // An update held back to keep to one per page frame goes on the plan that wakes
        // the thread when it is due, shown or not.
        if self.page_wake != due {
            self.page_wake = due;
            self.set_wake();
        }
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
            // A pack source's stand-in is placed at its size at any density: a build
            // would only restart its program.
            if running.extent.is_none()
                || running.ticket != entry.ticket
                || pack_source::is_pack_kind(&entry.kind)
            {
                continue;
            }
            let density = density_for(scale);
            if density == entry.density {
                continue;
            }
            // Drawing it finer changes nothing it shows, so it keeps its time zero.
            entry.anchor = Some(running.start);
            self.tickets += 1;
            entry.density = density;
            entry.ticket = self.tickets;
            jobs.push(Job {
                source_id: id.clone(),
                kind: entry.kind.clone(),
                settings: entry.settings.clone(),
                density,
                ticket: entry.ticket,
                anchor: entry.anchor,
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

    /// The size `source.sizes` reports for `source_id`'s live instance, if it runs and
    /// has made a frame.
    pub(crate) fn size(&self, source_id: &SourceId) -> Option<FrameSize> {
        self.entries.get(source_id)?.running.as_ref()?.size
    }

    /// Whether a mix (the program's, a video mix's or a preview's) looked at
    /// `source_id`'s running instance in the latest plan: it is drawn anyway, so a warm
    /// scene that draws it holds nothing more for it (ADR-0150).
    pub(crate) fn reached(&self, source_id: &SourceId) -> bool {
        self.entries
            .get(source_id)
            .and_then(|entry| entry.running.as_ref())
            .is_some_and(|running| running.looked == self.plan)
    }

    /// How many builds have been installed: it changes whenever a running source does.
    pub(crate) fn installs(&self) -> u64 {
        self.installs
    }

    /// The capture the program stage runs for `source_id`, if it is a capture source
    /// that captures something, as [`look`](Self::look) names it.
    pub(crate) fn capture_key(&self, source_id: &SourceId) -> Option<CaptureKey> {
        let source = &self.entries.get(source_id)?.running.as_ref()?.source;
        let spec = source.capture()?;
        (*spec != CaptureSpec::Window(None)).then(|| {
            CaptureKey::for_source(spec.clone(), source_id.clone())
                .with_cursor(source.show_cursor())
        })
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
        self.look_as(source_id, now, versions, true)
    }

    /// [`Self::look`] for a plan that is not the program mix's (a scene's preview
    /// stream): the source is not reached by it, so it is neither shown again nor heard
    /// for it (ADR-0028).
    pub(crate) fn look_quiet(
        &mut self,
        source_id: &SourceId,
        now: MediaTime,
        versions: &mut u64,
    ) -> Option<Look> {
        self.look_as(source_id, now, versions, false)
    }

    fn look_as(
        &mut self,
        source_id: &SourceId,
        now: MediaTime,
        versions: &mut u64,
        reach: bool,
    ) -> Option<Look> {
        let started = Instant::now();
        let look = self.look_timed(source_id, now, versions, reach);
        if look.is_some() {
            self.meters.looked(source_id, started.elapsed());
        }
        look
    }

    fn look_timed(
        &mut self,
        source_id: &SourceId,
        now: MediaTime,
        versions: &mut u64,
        reach: bool,
    ) -> Option<Look> {
        if self
            .entries
            .get(source_id)?
            .running
            .as_ref()?
            .source
            .capture()
            .is_some()
        {
            // A window source with no window chosen shows nothing.
            return self.capture_key(source_id).map(Look::Capture);
        }
        let plan = self.plan;
        let entry = self.entries.get_mut(source_id)?;
        let running = entry.running.as_mut()?;
        let at = since(now, running.start);
        // A granted page's clock anchor, when one is due (ADR-0102 decision 4).
        running.source.page_clock(now);
        // Cued, it plays on from where it was held, unless the program shows it again
        // now, which plays it from there too.
        if std::mem::take(&mut running.cued) && !(reach && running.planned + 1 < plan) {
            running.source.uncue(at);
        }
        if reach {
            if running.planned + 1 < plan {
                running.source.show(at);
            }
            running.planned = plan;
        }
        running.looked = plan;
        if std::mem::take(&mut running.resting) {
            running.source.set_drawn(true);
            running.drawn.store(true, Ordering::Relaxed);
            if let Some(painter) = running.source.painter() {
                // Not painted while it rested: painted for now before it is shown, so
                // it never shows the frame it had when it was last drawn.
                let started = Instant::now();
                painter.paint(at);
                self.meters.painted(source_id, started.elapsed());
            }
        }
        if let Some(context) = &self.context {
            let next = next_program_frame(now);
            if let Some(texture) = running.source.gpu_frame() {
                let frame = match &running.shown_gpu {
                    Some((shown, frame)) if *shown == texture => frame.clone(),
                    _ => {
                        *versions = versions.saturating_add(1);
                        let frame = Frame::from_gpu(texture.clone(), context.clone(), *versions);
                        running.size = Some(running.extent.unwrap_or(texture.info().size));
                        running.shown_gpu = Some((texture, frame.clone()));
                        frame
                    }
                };
                return Some(Look::frame(frame, next));
            }
            if jstream_sources::opens_browser(&entry.kind) {
                // Not painted yet: the stand-in, until a re-plan finds the first paint.
                // A page that paints on the CPU hands its paints over through its frame
                // slot instead, with no re-plan (ADR-0117 §3).
                let (frame, own) =
                    Self::cpu_frame(source_id, running, (at, now), versions, &self.stills)?;
                let next = if running.slot.is_some() { own } else { next };
                return Some(Look::frame(frame, next));
            }
        }
        let (frame, next) = Self::cpu_frame(source_id, running, (at, now), versions, &self.stills)?;
        Some(Look::frame(frame, next))
    }

    /// [`Self::look`]'s frame from the source's CPU frame at `at`, its own time.
    /// A painter's frame through its slot ([`Running::slot`]): the frame plans show it
    /// by, kept while the painter keeps its size, and a new one at a new size. `None`
    /// for a source with no slot, or a frame the slot can't hold (not packed RGBA8),
    /// which is shown as any other source's from then on.
    fn slotted(
        running: &mut Running,
        video: &Arc<VideoFrame>,
        versions: &mut u64,
    ) -> Option<Frame> {
        let Running {
            slot: slotted,
            extent,
            size,
            source,
            ..
        } = running;
        let (slot, shown) = slotted.as_mut()?;
        // The painter's frame as it is when stored, not `video`: the sources thread may
        // have painted and stored a newer one since it was read.
        let painter = source.painter();
        let resized = slot.store_latest(|| {
            painter
                .as_ref()
                .map_or_else(|| Arc::clone(video), |painter| painter.frame())
        }) == Stored::Refused;
        if resized && !slot.replace(Arc::clone(video)) {
            *slotted = None;
            return None;
        }
        match shown {
            Some(frame) if !resized => Some(frame.clone()),
            _ => {
                *versions = versions.saturating_add(1);
                let frame = Frame::from_slot(Arc::clone(slot), *versions);
                let frame = match *extent {
                    Some(extent) => frame.placed_at(extent),
                    None => frame,
                };
                *size = Some(extent.unwrap_or(video.info().size));
                *shown = Some(frame.clone());
                Some(frame)
            }
        }
    }

    fn cpu_frame(
        source_id: &SourceId,
        running: &mut Running,
        (at, now): (MediaTime, MediaTime),
        versions: &mut u64,
        stills: &Stills,
    ) -> Option<(Frame, Option<MediaTime>)> {
        if let Some(still) = running.source.still() {
            // Never decoded here: the stills thread holds its pixels for the draws.
            let frame = match &running.shown_still {
                Some((shown, frame)) if Arc::ptr_eq(shown, &still) => frame.clone(),
                _ => {
                    *versions = versions.saturating_add(1);
                    let size = still.size();
                    let shown = stills.show(Arc::clone(&still));
                    let frame = Frame::from_still(shown, *versions);
                    let frame = match running.extent {
                        Some(extent) => frame.placed_at(extent),
                        None => frame,
                    };
                    running.size = Some(running.extent.unwrap_or(size));
                    running.shown_still = Some((still, frame.clone()));
                    frame
                }
            };
            return Some((
                frame
                    .with_model(running.source.model())
                    .with_light(running.source.light()),
                None,
            ));
        }
        let video = match running.source.frame(at) {
            Ok(video) => video,
            Err(err) => {
                tracing::warn!(source_id = %source_id, error = %err, "source produced no frame");
                return None;
            }
        };
        if let Some(frame) = Self::slotted(running, &video, versions) {
            // The thread paints it and stores its new frames on its own wake.
            return Some((frame, None));
        }
        let frame = match &running.shown {
            Some((shown, frame)) if Arc::ptr_eq(shown, &video) => frame.clone(),
            _ => {
                *versions = versions.saturating_add(1);
                let frame = Frame::from_video(Arc::clone(&video), *versions)?;
                let frame = match running.extent {
                    Some(extent) => frame.placed_at(extent),
                    None => frame,
                };
                running.size = Some(running.extent.unwrap_or(video.info().size));
                running.shown = Some((video, frame.clone()));
                frame
            }
        };
        let next = running
            .source
            .next_change(at)
            .and_then(|change| after(running.start, change, now.timebase()));
        // A model source's frame is its model's stand-in (ADR-0107): the item draws the
        // model in its place.
        let frame = frame
            .with_model(running.source.model())
            .with_light(running.source.light());
        Some((frame, next))
    }

    /// After a plan of the program mix and every mix and preview drawn with it: tells
    /// each source none of them looked at that nothing draws it, so a kind that costs
    /// something while drawn (a `browser` page) can rest (#1071), and the thread stops
    /// painting a scope, timer or chat source no mix draws. A source looked at again is
    /// told it is drawn, and a painter painted for now, as it is looked at
    /// ([`Self::look`]).
    ///
    /// A source a warm scene draws never rests (ADR-0117 §7). One no mix looked at and
    /// the program didn't reach is cued instead ([`Source::cue`]): a media file that
    /// restarts when shown holds its start, so the switch to its scene shows it at once.
    pub(crate) fn rest_undrawn(&mut self) {
        let plan = self.plan;
        let warm = &self.warm;
        let mut cued = false;
        for (id, entry) in &mut self.entries {
            let Some(running) = entry.running.as_mut() else {
                continue;
            };
            if warm.contains(id) {
                if running.planned != plan && running.looked != plan && !running.cued {
                    running.source.cue();
                    running.cued = true;
                    cued |= running.source.painter().is_some();
                }
                continue;
            }
            if running.looked != plan && !running.resting {
                running.resting = true;
                running.drawn.store(false, Ordering::Relaxed);
                running.source.set_drawn(false);
            }
        }
        if cued {
            // The thread takes a cued file's first frame as soon as it is decoded.
            self.wake_painters();
        }
    }

    /// Has the thread paint the painters drawn now, then as they ask.
    fn wake_painters(&self) {
        let mut queue = lock(&self.worker.queue);
        let now = queue.clock.now();
        queue.paint_wake = crate::snapshot::earliest(queue.paint_wake, Some(now));
        self.worker.wake.notify_all();
    }

    /// Keeps `sources`, which the warm scenes draw (ADR-0117 §7), from resting: one
    /// resting now is told it is drawn again, and the thread paints it from now on. One
    /// that leaves the set rests at the next [`rest_undrawn`](Self::rest_undrawn) if no
    /// mix looked at it.
    pub(crate) fn set_warm(&mut self, sources: &BTreeSet<SourceId>) {
        if self.warm == *sources {
            return;
        }
        self.warm.clone_from(sources);
        let mut painted = false;
        for (_, entry) in self
            .entries
            .iter_mut()
            .filter(|(id, _)| sources.contains(*id))
        {
            let Some(running) = entry.running.as_mut() else {
                continue;
            };
            if std::mem::take(&mut running.resting) {
                running.source.set_drawn(true);
                running.drawn.store(true, Ordering::Relaxed);
                painted |= running.source.painter().is_some();
            }
        }
        if painted {
            self.wake_painters();
        }
    }

    /// The first pixel of the frame `source_id`'s painter holds now, for tests.
    #[cfg(test)]
    pub(crate) fn painted_pixel(&self, source_id: &SourceId) -> Option<[u8; 4]> {
        let painter = self
            .entries
            .get(source_id)?
            .running
            .as_ref()?
            .source
            .painter()?;
        let frame = painter.frame();
        crate::slot::rgba(&frame).get(..4)?.try_into().ok()
    }

    /// Whether `source_id`'s live instance rests, for tests.
    #[cfg(test)]
    pub(crate) fn resting(&self, source_id: &SourceId) -> Option<bool> {
        Some(self.entries.get(source_id)?.running.as_ref()?.resting)
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

    /// Asks the thread to re-plan at `at`, or at no set time, for the plan: it also wakes
    /// for a granted page's held update ([`Self::feed_pages`]).
    ///
    /// The time is rounded to the program's frame grid at `fps` ([`replan_time`]), so
    /// animations out of phase with each other wake the thread once a frame at most.
    pub(crate) fn wake_at(&mut self, at: Option<MediaTime>, fps: u32) {
        self.plan_wake = at.map(|at| replan_time(at, fps));
        let kicked = {
            let mut queue = lock(&self.worker.queue);
            queue.fps = fps;
            // A plan may have shown a painter again that the thread stopped painting.
            let kick = queue.paint_wake.is_none() && !queue.painters.is_empty();
            if kick {
                queue.paint_wake = Some(queue.clock.now());
            }
            kick
        };
        if kicked {
            self.start();
            self.worker.wake.notify_all();
        }
        self.set_wake();
    }

    fn set_wake(&mut self) {
        let at = crate::snapshot::earliest(self.plan_wake, self.page_wake);
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

/// Just after `now`, when a source that paints on the GPU is planned again: the plan's
/// wake rounds it to the next frame on the grid ([`replan_time`]).
fn next_program_frame(now: MediaTime) -> Option<MediaTime> {
    now.checked_add(MediaTime::new(1, now.timebase())).ok()
}

/// When to re-plan for a frame change due at `at`, at `fps`: a quarter period before the
/// first program tick at least that long after `at`, so every change due before a tick
/// shares one plan, finished by the tick (#1138). A change due within that quarter
/// before a tick is shown a tick later, as one whose plan took that long already was.
/// `at` itself when the frame grid can't be worked out.
fn replan_time(at: MediaTime, fps: u32) -> MediaTime {
    let rounded = || {
        let period = frame_period(at.timebase(), fps).ok()?;
        let lead = MediaTime::new(period.ticks() / 4, at.timebase());
        let tick = on_frame_grid(at.checked_add(lead).ok()?, period)?;
        tick.checked_sub(lead).ok()
    };
    rounded().unwrap_or(at)
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
    /// Where the thread counts its paints and builds (ADR-0103).
    meters: Arc<SourceMeters>,
}

struct Queue {
    jobs: VecDeque<Job>,
    /// Video filter assets to load, after the builds queued before them.
    loads: VecDeque<AssetJob>,
    /// When to re-plan next.
    wake: Option<MediaTime>,
    /// When to paint the painters with frame slots next, which needs no plan.
    paint_wake: Option<MediaTime>,
    /// The collection's base frame rate, whose frame grid paints are timed to.
    fps: u32,
    /// Builds taken but not installed yet, so `settled` waits for them too.
    building: usize,
    /// The running sources that draw their frames as they come due, with each one's
    /// time zero: the thread paints them before every re-plan.
    painters: Vec<Painted>,
    clock: Arc<dyn MediaClock>,
    closed: bool,
}

impl Worker {
    fn new(clock: Arc<dyn MediaClock>, meters: Arc<SourceMeters>) -> Self {
        Self {
            meters,
            queue: Mutex::new(Queue {
                jobs: VecDeque::new(),
                loads: VecDeque::new(),
                wake: None,
                paint_wake: None,
                fps: jstream_protocol::DEFAULT_BASE_FPS,
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

/// A running source that draws its own frames, as the thread paints it.
#[derive(Clone)]
struct Painted {
    source_id: SourceId,
    painter: Arc<dyn Painter>,
    /// Its time zero.
    start: MediaTime,
    /// Whether a mix drew it in the latest plans ([`Running::drawn`]).
    drawn: Arc<AtomicBool>,
    /// Where its new frames go without a re-plan ([`Running::slot`]); `None` for a
    /// painter whose frames aren't packed RGBA8, whose frames reach the picture only
    /// through a re-plan.
    slot: Option<Arc<FrameSlot>>,
}

/// What the thread does next.
enum Next {
    Build(Job),
    Load(AssetJob),
    /// Paint these at this time, then re-plan if the plan is due (or a painter's frame
    /// changed size).
    Tick {
        painters: Vec<Painted>,
        now: MediaTime,
        replan: bool,
        fps: u32,
    },
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
                let Some(due) = crate::snapshot::earliest(queue.wake, queue.paint_wake) else {
                    queue = worker
                        .wake
                        .wait(queue)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    continue;
                };
                let now = queue.clock.now();
                if now >= due {
                    let replan = queue.wake.is_some_and(|wake| now >= wake);
                    if replan {
                        queue.wake = None;
                    }
                    queue.paint_wake = None;
                    break Next::Tick {
                        painters: queue.painters.clone(),
                        now,
                        replan,
                        fps: queue.fps,
                    };
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
            Next::Tick {
                painters,
                now,
                mut replan,
                fps,
            } => {
                // Drawing can take milliseconds, so it happens here, before a re-plan
                // takes the collection lock and asks for the painted frames.
                // A source no mix drew in the latest plans is skipped.
                let mut next = None;
                for painted in painters.iter().filter(|p| p.drawn.load(Ordering::Relaxed)) {
                    let started = Instant::now();
                    let at = since(now, painted.start);
                    painted.painter.paint(at);
                    worker.meters.painted(&painted.source_id, started.elapsed());
                    // A painter with a slot hands its new frame to the compositors there
                    // (ADR-0117 §3); only one of a new size needs a plan to place it.
                    let Some(slot) = &painted.slot else {
                        continue;
                    };
                    replan |= slot.store_latest(|| painted.painter.frame()) == Stored::Refused;
                    let due = painted
                        .painter
                        .next_change(at)
                        .and_then(|change| after(painted.start, change, now.timebase()));
                    next = crate::snapshot::earliest(next, due);
                }
                {
                    let mut queue = lock(&worker.queue);
                    let next = next.map(|due| replan_time(due, fps));
                    queue.paint_wake = crate::snapshot::earliest(queue.paint_wake, next);
                }
                if replan {
                    match host.upgrade() {
                        Some(host) => host.replan(),
                        None => return,
                    }
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
                let started = Instant::now();
                let mut result = if pack_source::is_pack_kind(&job.kind)
                    && job.registry.descriptor(&job.kind).is_none()
                {
                    pack_source::StandIn::new(&job.settings)
                        .map(|stand_in| Box::new(stand_in) as Box<dyn Source>)
                } else {
                    job.registry.create(&job.kind, &job.settings, &job.ctx)
                };
                // Drawn at its density here, off the collection lock, as it was created.
                let extent = result
                    .as_mut()
                    .ok()
                    .and_then(|source| source.set_density(job.density));
                // A source that keeps an earlier time zero is painted for the time it
                // will show once installed, so its first plan doesn't show its zero's
                // frame (a `timer` built again for a style change keeps its count).
                if let (Ok(source), Some(anchor)) = (result.as_ref(), job.anchor)
                    && let Some(painter) = source.painter()
                {
                    painter.paint(since(worker.now(), anchor));
                }
                let size = match result.as_mut() {
                    Ok(source) => extent.or_else(|| first_size(source.as_mut())),
                    Err(_) => None,
                };
                worker.meters.built(&job.source_id, started.elapsed());
                let built = Built {
                    source_id: job.source_id,
                    ticket: job.ticket,
                    result,
                    extent,
                    size,
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

/// A source's size before any plan shows it: its frame at time zero, which a plan would
/// show first, so asking for it changes nothing a plan sees. `None` for a capture
/// source and for one that makes no frame.
fn first_size(source: &mut dyn Source) -> Option<FrameSize> {
    if source.capture().is_some() {
        return None;
    }
    if let Some(still) = source.still() {
        // Its size, without taking the pixels it holds for the first plan.
        return Some(still.size());
    }
    let frame = source.frame(MediaTime::zero(Timebase::MILLIS)).ok()?;
    Some(frame.info().size)
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

/// The settings a source of `kind` is built from, which a change to builds it again: all of
/// them, but a pack source's stand-in is built from its size alone, as its program takes
/// the other settings live.
/// Whether a source of `kind` built from `settings` paints at the program's frame rate:
/// a `browser` page with an `fps` of 0.
fn follows_program_rate(kind: &str, settings: &Settings) -> bool {
    kind == "browser" && settings.get("fps").and_then(serde_json::Value::as_u64) == Some(0)
}

fn built_from(kind: &str, settings: &Settings) -> Settings {
    if pack_source::is_pack_kind(kind) {
        pack_source::stand_in_settings(settings)
    } else {
        settings.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{built_from, density_for};
    use jstream_protocol::Settings;
    use serde_json::json;

    /// A shader source's stand-in is built again for a new size only; any other kind for
    /// any change.
    #[test]
    fn only_a_size_change_builds_a_shader_source_again() {
        let of = |pairs: &[(&str, serde_json::Value)]| -> Settings {
            pairs
                .iter()
                .map(|(id, value)| ((*id).to_owned(), value.clone()))
                .collect()
        };
        let before = of(&[
            ("width", json!(64)),
            ("height", json!(32)),
            ("speed", json!(1.0)),
        ]);
        let faster = of(&[
            ("width", json!(64)),
            ("height", json!(32)),
            ("speed", json!(2.0)),
        ]);
        let wider = of(&[
            ("width", json!(128)),
            ("height", json!(32)),
            ("speed", json!(1.0)),
        ]);
        assert_eq!(
            built_from("glow/ramp", &before),
            built_from("glow/ramp", &faster)
        );
        assert_ne!(
            built_from("glow/ramp", &before),
            built_from("glow/ramp", &wider)
        );
        assert_ne!(built_from("color", &before), built_from("color", &faster));
    }

    #[test]
    fn a_density_is_the_next_quarter_step_of_a_doubling() {
        for scale in [f32::NAN, -3.0, 0.0, 0.5, 1.0] {
            assert_eq!(density_for(scale), 1.0, "{scale}");
        }
        assert_eq!(density_for(1.01), 2f32.powf(0.25));
        assert_eq!(density_for(2.0), 2.0);
        assert_eq!(density_for(2.1), 2f32.powf(1.25));
        assert_eq!(density_for(6.0), 2f32.powf(2.75));
        assert_eq!(density_for(9.0), 2f32.powf(3.25));
        assert_eq!(density_for(33.0), jstream_sources::MAX_DENSITY);
        assert_eq!(density_for(f32::INFINITY), jstream_sources::MAX_DENSITY);
    }

    /// Changes due anywhere in one frame share one re-plan, a quarter frame before the
    /// tick that shows them, and never before they are due (#1138).
    #[test]
    fn re_plans_snap_to_the_frame_grid() {
        use super::replan_time;
        use crate::runtime::{frame_period, on_frame_grid};
        use jstream_media::{MediaTime, Timebase};

        let at = |ns: i64| MediaTime::new(ns, Timebase::NANOS);
        let period = frame_period(Timebase::NANOS, 30).expect("period");
        let p = period.ticks();
        assert_eq!(p, 33_333_333);
        assert_eq!(on_frame_grid(at(0), period), Some(at(0)));
        assert_eq!(on_frame_grid(at(1), period), Some(at(p)));
        assert_eq!(on_frame_grid(at(p), period), Some(at(p)));
        assert_eq!(on_frame_grid(at(p + 1), period), Some(at(2 * p)));

        let lead = p / 4;
        let wake = at(2 * p - lead);
        // Out of phase: everything due after the previous frame's wake up to this one.
        for due in [p - lead + 1, p, p + 1, p + p / 2, 2 * p - lead] {
            assert_eq!(replan_time(at(due), 30), wake, "due at {due}");
        }
        // Due after the wake: planned for the next tick.
        assert_eq!(replan_time(at(2 * p - lead + 1), 30), at(3 * p - lead));
        // A faster base rate has a finer grid.
        let fast = frame_period(Timebase::NANOS, 60).expect("period").ticks();
        assert_eq!(replan_time(at(1), 60), at(fast - fast / 4));
    }
}
