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
//! The thread re-plans at the earliest such time, so an animated image advances on the
//! clock; a still frame is planned once.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use jstream_media::{MediaTime, Rounding, Timebase, VideoFrame};
use jstream_platform::MediaClock;
use jstream_protocol::{Settings, SourceId, SourceKind};
use jstream_sources::{CaptureSpec, Context, Registry, Source, SourceError};

use crate::lock;
use crate::snapshot::{Frame, Look};

/// What the `jstream-sources` thread calls back into: the engine, held weakly so the
/// thread never keeps it alive.
pub(crate) trait LiveHost: Send + Sync {
    /// Installs a built source and re-plans the program mix.
    fn install(&self, built: Built);
    /// Re-plans the program mix, because an animation's next frame is due.
    fn replan(&self);
}

/// The live sources of one engine. Lives in the engine state, under the collection lock.
pub(crate) struct LiveSources {
    registry: Arc<Registry>,
    ctx: Context,
    entries: BTreeMap<SourceId, Entry>,
    /// Numbers the builds, so a result installs only if nothing changed the source
    /// since it was asked for.
    tickets: u64,
    worker: Arc<Worker>,
    host: Weak<dyn LiveHost>,
    /// Whether the thread has been started.
    started: bool,
}

/// What the engine wants for one source, and what it runs now.
struct Entry {
    kind: String,
    settings: Settings,
    /// The build asked for last. Only its result is installed.
    ticket: u64,
    running: Option<Running>,
}

/// A built source and the frame it showed last.
struct Running {
    source: Box<dyn Source>,
    /// When it was built, on the media clock: its time zero.
    start: MediaTime,
    shown: Option<(Arc<VideoFrame>, Frame)>,
}

/// One source to build.
pub(crate) struct Job {
    source_id: SourceId,
    kind: String,
    settings: Settings,
    ticket: u64,
    registry: Arc<Registry>,
    ctx: Context,
}

/// A finished build, for [`LiveSources::install`].
pub(crate) struct Built {
    source_id: SourceId,
    ticket: u64,
    result: Result<Box<dyn Source>, SourceError>,
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
        }
    }

    /// The clock animations run on: the platform's media clock once the host injects
    /// backends, else a monotonic clock of the engine's own.
    pub(crate) fn set_clock(&mut self, clock: Arc<dyn MediaClock>) {
        let now = clock.now();
        let mut queue = lock(&self.worker.queue);
        queue.clock = clock;
        // Times on the old clock mean nothing on the new one: restart the timing, and
        // re-plan now if a re-plan was due, so the next one is timed on this clock.
        if queue.wake.is_some() {
            queue.wake = Some(now);
            self.worker.wake.notify_all();
        }
        drop(queue);
        for running in self.entries.values_mut().filter_map(|e| e.running.as_mut()) {
            running.start = now;
        }
    }

    /// The folder behind `collection` asset references: the collection file's.
    pub(crate) fn set_collection_folder(&mut self, folder: Option<PathBuf>) {
        if self.ctx.assets.collection != folder {
            self.ctx.assets.collection = folder;
            // Sources that read assets have to read them from the new folder.
            for entry in self.entries.values_mut() {
                entry.settings = Settings::new();
            }
        }
    }

    /// Brings the live sources in line with the collection's: queues a build for every
    /// source that is new or whose settings changed, and drops the ones deleted.
    pub(crate) fn sync(&mut self, state: &jstream_graph::State) {
        let registry = Arc::clone(&self.registry);
        self.entries.retain(|id, _| {
            state
                .source(id)
                .is_some_and(|s| framed(&registry, s.kind()).is_some())
        });
        let mut jobs = Vec::new();
        for source in state.sources() {
            let Some(kind) = framed(&registry, source.kind()) else {
                continue;
            };
            let entry = self.entries.entry(source.id().clone()).or_insert(Entry {
                kind: String::new(),
                settings: Settings::new(),
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
                ticket: entry.ticket,
                registry: Arc::clone(&self.registry),
                ctx: self.ctx.clone(),
            });
        }
        if !jobs.is_empty() {
            self.start();
            let mut queue = lock(&self.worker.queue);
            queue.jobs.extend(jobs);
            self.worker.wake.notify_all();
        }
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
        entry.running = match built.result {
            Ok(source) => Some(Running {
                source,
                start: self.worker.now(),
                shown: None,
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
        true
    }

    /// The current time on the clock sources run on.
    pub(crate) fn now(&self) -> MediaTime {
        self.worker.now()
    }

    /// What `source_id` shows at `now`: what the pipeline captures for a capture source,
    /// else its frame, numbered from `versions` when it is new, and when that frame is
    /// next replaced. `None` when the source has no live instance.
    pub(crate) fn look(
        &mut self,
        source_id: &SourceId,
        now: MediaTime,
        versions: &mut u64,
    ) -> Option<Look> {
        let running = self.entries.get_mut(source_id)?.running.as_mut()?;
        if let Some(spec) = running.source.capture() {
            // A window source with no window chosen shows nothing.
            return (*spec != CaptureSpec::Window(None)).then(|| Look::Capture(spec.clone()));
        }
        let at = since(now, running.start);
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
                running.shown = Some((video, frame.clone()));
                frame
            }
        };
        let next = running
            .source
            .next_change(at)
            .and_then(|change| after(running.start, change, now.timebase()));
        Some(Look::Frame(frame, next))
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
    /// When to re-plan next.
    wake: Option<MediaTime>,
    /// Builds taken but not installed yet, so `settled` waits for them too.
    building: usize,
    clock: Arc<dyn MediaClock>,
    closed: bool,
}

impl Worker {
    fn new(clock: Arc<dyn MediaClock>) -> Self {
        Self {
            queue: Mutex::new(Queue {
                jobs: VecDeque::new(),
                wake: None,
                building: 0,
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
        while !queue.jobs.is_empty() || queue.building > 0 {
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
    Replan,
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
                    break Next::Replan;
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
            Next::Replan => match host.upgrade() {
                Some(host) => host.replan(),
                None => return,
            },
            Next::Build(job) => {
                let result = job.registry.create(&job.kind, &job.settings, &job.ctx);
                let built = Built {
                    source_id: job.source_id,
                    ticket: job.ticket,
                    result,
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
