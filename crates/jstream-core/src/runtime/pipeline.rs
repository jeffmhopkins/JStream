//! Starts and stops the program stage every running output shares, and each output's
//! own threads.
//!
//! Capture is the platform's mailbox, read by the render thread. The render thread
//! composites the program mix from the render snapshot the control side published
//! ([`RenderHub`]) and, because the fake GPU interop has no encode target, reads the
//! frame back and pushes CPU RGBA. What it lets go of (a replaced snapshot, GPU images
//! past their frame fence, a finished encoder session's input) goes to the program
//! stage's `jstream-collector` thread (R-RT-003), and so does a finished session's
//! input the mixer thread lets go of. The mixer thread polls the engine clock. Encode threads live inside the encoder sessions. The output thread muxes
//! FLV and publishes it, or muxes fragmented MP4 and writes it to a recording file
//! ([`Sink`]). None of those threads lock the engine's collection.
//!
//! The render and mixer threads catch a panic at their thread boundary, and every
//! fatal error on the render, mixer and output threads goes to the same channel, so
//! the publish watcher settles `failed` instead of leaving an output `active` with a
//! dead stage (R-RT-006). The output thread also reports there when the publish link
//! goes down and comes back, which the watcher turns into `reconnecting` and `active`.
//!
//! A stop gives the output thread [`STOP_DRAIN`] to send what the encoders have
//! queued. Past that deadline it abandons the rest, counts it as dropped and closes
//! the publish target, so a peer that has stopped reading cannot hold `output.stop`.
//! The stop waits for a publishing output thread only until that deadline plus
//! [`OUTPUT_GRACE`]: a publish target that blocks in a send without honouring the
//! deadline is left to its thread, which ends once the send returns (#138).

use std::any::Any;
use std::collections::BTreeMap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use jstream_audio::{BusControl, InputControl, Mixer, MixerConfig};
use jstream_encode::{
    AudioInput, AudioSession, PushError, SessionOptions, SessionRegistry, SessionStats, StreamEnd,
    SubscriberStats, Subscription, VideoInput, VideoSession,
};
use jstream_media::{
    AudioBuffer, AudioFormat, ChannelLayout, Codec, ColorInfo, ColorRange, ColorSpace, FrameSize,
    MediaTime, PixelFormat, Plane, Rounding, SampleFormat, Timebase, VideoFrame, VideoFrameInfo,
};
use jstream_platform::{
    AudioEncoderConfig, AudioInputTarget, AudioIo, AudioStream, Backends, Capture, CaptureStream,
    Codecs, HardwarePreference, MediaClock, RateControl, VideoEncoderConfig, VideoSurface,
};
use jstream_protocol::{
    EncoderProfile, EncoderProfileId, HardwarePreference as ProfileHardware, OutputId,
    RateControl as ProfileRate,
};
use jstream_render::{Compositor, OpenOptions, Reclaimed, RenderError, ResourceCache};

use crate::drops::{
    AudioBusDrops, AudioEncodeDrops, AudioInputDrops, DropGuard, DropSnapshot, FnDropSource,
    VideoEncodeDrops, capture_drops, output_packet_drops, render_tick_drops,
};
use crate::engine::Engine;
use crate::lock;
use crate::runtime::sink::{Destination, Sink, StreamInfo};
use crate::runtime::{
    Collector, Garbage, LinkState, LivePublish, OpenPublish, RenderHub, RenderReader,
};
use crate::snapshot::{self, CaptureFrame, GpuKey, Shot};

/// Base frame rate of this cut: one frame every 1/30 s.
const FRAME_NUM: u32 = 1;
const FRAME_DEN: u32 = 30;
const SAMPLE_RATE: u32 = 48_000;
/// How long `output.start` waits for the render thread to open the compositor on the
/// software adapter, unless the engine was given another bound. It only keeps a stuck
/// adapter from blocking the call forever; a slower open is not a pipeline fault.
pub(crate) const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
/// Composite errors in a row that the render thread rides out by repeating the last
/// frame. One more reports the output failed: one second at the base rate.
const MAX_DRAW_ERRORS: u32 = 30;
/// How long a stop lets the output thread send the packets still queued, counted from
/// the moment it sees the stop flag. What is left after that is dropped and counted, and
/// the publish target is told the same deadline ([`LivePublish::stop_by`]), so a stalled
/// peer cannot hold `output.stop` much longer than this.
pub(crate) const STOP_DRAIN: Duration = Duration::from_secs(2);
/// How long past the [`STOP_DRAIN`] deadline a stop still waits for a publishing output
/// thread: time to drop what is queued and close the target. A thread that has not
/// ended by then is blocked in the target, and the stop returns without it.
const OUTPUT_GRACE: Duration = Duration::from_millis(500);

/// Composites one frame on the render thread: the program mix of the render snapshot's
/// [`Shot`] at `size`, with `display` sources showing the capture (or, with no program
/// scene, the capture filling the frame, or black when there is none yet), read back as
/// RGBA8 into `out` (replacing its contents). Returns the GPU images its cache let go
/// of, for the collector. Created on the render thread and kept there, so it need not
/// be `Send`.
pub(crate) type DrawFn = Box<
    dyn FnMut(
        &Shot,
        FrameSize,
        Option<&CaptureFrame<'_>>,
        &mut Vec<u8>,
    ) -> Result<Option<Reclaimed>, String>,
>;

/// Opens the render thread's [`DrawFn`], on that thread. The engine's opens the wgpu
/// compositor on the software adapter ([`gpu_draw`]). Tests inject [`cpu_draw`] (or a
/// variant) so the pipeline runs without an adapter.
pub(crate) type OpenDraw = Arc<dyn Fn() -> Result<DrawFn, String> + Send + Sync>;

/// A CPU stand-in for [`gpu_draw`]: it copies the captured RGBA8 frame, or fills black
/// when there is none, which is what the GPU path draws for a collection with no scene.
/// It ignores the scenes. Tests use it to run the pipeline without a wgpu adapter
/// ([`Engine::with_cpu_compositor`](crate::Engine::with_cpu_compositor)).
pub(crate) fn cpu_draw() -> OpenDraw {
    Arc::new(|| {
        let draw: DrawFn = Box::new(
            |_: &Shot, size: FrameSize, capture: Option<&CaptureFrame<'_>>, out: &mut Vec<u8>| {
                out.clear();
                match capture {
                    Some(capture) => out.extend_from_slice(capture.rgba),
                    None => out.resize(size.width() as usize * size.height() as usize * 4, 0),
                }
                Ok(None)
            },
        );
        Ok(draw)
    })
}

/// A fake Annex-B encoder does not emit SPS/PPS. FLV and MP4 still need a decoder
/// configuration, so the muxers are given the mux crate's baseline sets.
const SPS: [u8; 5] = [0x67, 0x42, 0xC0, 0x1E, 0xDA];
const PPS: [u8; 4] = [0x68, 0xCE, 0x3C, 0x80];

/// Counters a caller can read while an output is running.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutputStats {
    /// Video frames the output's encoder session accepted. A shared session counts
    /// for every output that shares it.
    pub video: SessionStats,
    /// Audio blocks the output's encoder session accepted.
    pub audio: SessionStats,
    /// What the output's video subscriber has been given and lost.
    pub video_subscriber: SubscriberStats,
    /// What the output's audio subscriber has been given and lost.
    pub audio_subscriber: SubscriberStats,
    /// CPU readbacks on the render thread, which every running output shares. The fake
    /// GPU path has no encode target, so every encoded frame is one of these.
    pub readbacks: u64,
}

struct PipelineStats {
    video_subscriber: Mutex<SubscriberStats>,
    audio_subscriber: Mutex<SubscriberStats>,
}

impl PipelineStats {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            video_subscriber: Mutex::new(SubscriberStats::default()),
            audio_subscriber: Mutex::new(SubscriberStats::default()),
        })
    }
}

/// Backends and the publish target for one engine, plus the running outputs.
///
/// `gate` is taken after the collection lock when both are held. `output_stats`,
/// the in-flight wait and `Drop` take `gate` alone. Nothing takes the collection
/// lock while holding `gate`. `gate_cv` wakes a stop that is waiting for a start,
/// and a start that is waiting for a failed pipeline to finish joining. `starts`
/// runs one `output.start` at a time, so only one start ever opens the program stage.
pub(crate) struct MediaHost {
    pub(crate) backends: Mutex<Option<Backends>>,
    /// How the render thread composites. [`gpu_draw`] unless a test replaced it.
    pub(crate) draw: Mutex<OpenDraw>,
    /// How long a start waits for that compositor to open. [`OPEN_TIMEOUT`] unless a
    /// test raised it.
    pub(crate) open_timeout: Mutex<Duration>,
    pub(crate) publish: Mutex<Option<Box<dyn LivePublish>>>,
    /// Opens a target for a start when `publish` holds none.
    pub(crate) open_publish: Mutex<Option<OpenPublish>>,
    pub(crate) starts: Mutex<()>,
    pub(crate) gate: Mutex<Gate>,
    pub(crate) gate_cv: Condvar,
    pub(crate) next_generation: AtomicU64,
}

/// The running outputs, and the program stage they share. `pending` is a start that
/// has emitted `starting` and has not settled yet. Each output's `watcher` receives
/// its pipeline's failures and settles `failed`.
pub(crate) struct Gate {
    /// The program stage, while some running output holds it.
    program: Weak<Program>,
    /// Its failure fan-out, which says whether it has failed without keeping it alive.
    failures: Weak<Failures>,
    pub(crate) running: BTreeMap<OutputId, Running>,
    pub(crate) pending: Option<Pending>,
    pub(crate) watchers: BTreeMap<OutputId, JoinHandle<()>>,
    /// Outputs taken out of `running` that are still being joined. While the program
    /// stage is gone, a new start waits for this to reach 0 so it doesn't open the
    /// devices over a teardown.
    pub(crate) draining: u32,
}

impl Gate {
    /// Whether a start has to wait for a teardown before it opens the devices.
    /// A program stage that has failed is on its way down: its outputs settle
    /// `failed` and drop it, and the start waits for that rather than joining it.
    pub(crate) fn must_wait(&self) -> bool {
        (self.draining > 0 && self.program.strong_count() == 0)
            || self
                .failures
                .upgrade()
                .is_some_and(|failures| failures.failed())
    }

    /// The running program stage, to share.
    pub(crate) fn program(&self) -> Option<Arc<Program>> {
        self.program.upgrade()
    }

    /// Records the program stage a start launched.
    pub(crate) fn set_program(&mut self, program: &Arc<Program>) {
        self.program = Arc::downgrade(program);
        self.failures = Arc::downgrade(&program.failures);
    }
}

/// One `output.start` between the `starting` event and the settle.
pub(crate) struct Pending {
    pub(crate) output_id: OutputId,
    pub(crate) generation: u64,
    /// `output.stop` won this generation. The start must not settle `active`.
    pub(crate) cancel: bool,
}

impl MediaHost {
    pub(crate) fn new() -> Self {
        Self {
            backends: Mutex::new(None),
            draw: Mutex::new(gpu_draw()),
            open_timeout: Mutex::new(OPEN_TIMEOUT),
            publish: Mutex::new(None),
            open_publish: Mutex::new(None),
            starts: Mutex::new(()),
            gate: Mutex::new(Gate {
                program: Weak::new(),
                failures: Weak::new(),
                running: BTreeMap::new(),
                pending: None,
                watchers: BTreeMap::new(),
                draining: 0,
            }),
            gate_cv: Condvar::new(),
            next_generation: AtomicU64::new(1),
        }
    }
}

impl Drop for MediaHost {
    fn drop(&mut self) {
        let (running, watchers) = {
            let gate = self.gate.get_mut().unwrap_or_else(PoisonError::into_inner);
            (
                std::mem::take(&mut gate.running),
                std::mem::take(&mut gate.watchers),
            )
        };
        drop(running);
        for watcher in watchers.into_values() {
            if watcher.thread().id() != thread::current().id() {
                join("publish watcher", Some(watcher));
            }
        }
    }
}

/// The key that decides which outputs share an encoder session: the profile they
/// name (`None` for the engine's default) and its encoding when the session opened.
/// Every output consumes the program mix and none has a video chain yet, so the mix
/// and chain parts of ADR-0027's key are the same for all of them.
pub(crate) type EncodeKey = (Option<EncoderProfileId>, EncoderProfile);

/// The stage every running output shares: capture, the render thread compositing the
/// program, the audio devices and the mixer thread. It runs while any output holds it
/// and stops when the last one lets go.
///
/// The render and mixer threads push each frame and block into every encoder session
/// that is open. A session is handed to them through a channel, never a lock, and
/// they let go of it once it refuses input because it has finished. Their fatal errors
/// go to every running output ([`Failures`]).
pub(crate) struct Program {
    stop: Arc<AtomicBool>,
    render: Option<JoinHandle<()>>,
    /// Frees what the render and mixer threads let go of. It ends once both have.
    collector: Option<JoinHandle<()>>,
    mixer: Option<JoinHandle<()>>,
    forward: Option<JoinHandle<()>>,
    clock: Arc<dyn MediaClock>,
    codecs: Arc<dyn Codecs>,
    size: FrameSize,
    format: AudioFormat,
    /// How far the mixer's blocks run behind the clock; an output's stream origin
    /// sits that far back.
    latency: MediaTime,
    video_taps: Sender<VideoInput>,
    audio_taps: Sender<AudioInput>,
    failures: Arc<Failures>,
    /// Open encoder sessions by key, held weakly: each lives while an output holds it.
    sessions: Mutex<SessionRegistry<EncodeKey, Encoders>>,
    readbacks: Arc<AtomicU64>,
    /// Kept alive so the platform keeps calling the mixer producers. Behind a lock
    /// only so the program can be shared; nothing takes it.
    _streams: Mutex<Vec<Box<dyn AudioStream>>>,
    capture_drops: DropSnapshot,
    /// Declared after `_streams` (fields drop in order), so the input rings outlive
    /// both producers and the mixer and are freed here, not on a real-time thread
    /// (docs/RULES.md#R-RT-003).
    mic: InputControl,
    desktop: InputControl,
    bus: BusControl,
    drop_guard: Option<DropGuard>,
    ticker: Option<JoinHandle<()>>,
    /// Emits one last `stats.tick` once the threads have stopped and the sources are
    /// latched, so the run's final drops are reported when it ends.
    final_tick: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Program {
    /// The encoder sessions for `key`: the open ones, or new ones not yet fed. Call
    /// [`attach`](Self::attach) once the output has subscribed, so a new session's
    /// first packets go to it.
    fn encoders(&self, engine: &Engine, key: EncodeKey) -> Result<Arc<Encoders>, String> {
        lock(&self.sessions).get_or_open(key.clone(), || {
            let video = Arc::new(open_video(self.codecs.as_ref(), self.size, &key.1)?);
            let audio = Arc::new(open_audio_encoder(
                self.codecs.as_ref(),
                self.format,
                &key.1,
            )?);
            let ids = vec![
                engine.add_drop_source(VideoEncodeDrops::new(Arc::clone(&video))),
                engine.add_drop_source(AudioEncodeDrops::new(Arc::clone(&audio))),
            ];
            Ok(Encoders {
                key,
                video,
                audio,
                attached: AtomicBool::new(false),
                drop_guard: Some(DropGuard::new(engine.drop_ledger(), ids)),
            })
        })
    }

    /// Starts feeding `encoders` from the render and mixer threads, once.
    fn attach(&self, encoders: &Encoders) {
        if encoders.attached.swap(true, Ordering::AcqRel) {
            return;
        }
        // The threads hold the receivers until the program stops; a send fails only
        // after that, when nothing is fed anyway.
        let _ = self.video_taps.send(encoders.video.input());
        let _ = self.audio_taps.send(encoders.audio.input());
    }

    /// Where an output that starts now puts its stream's time zero: the mixer's
    /// latency before the clock, as the first block it will get is stamped.
    fn origin(&self) -> Result<MediaTime, String> {
        self.clock
            .now()
            .checked_sub(self.latency)
            .map_err(|err| format!("stream origin: {err}"))
    }

    /// Registers the program's stages and starts the once-a-second sampler.
    fn arm_stats(&mut self, engine: &Engine) {
        let ids = vec![
            engine.add_drop_source(self.capture_drops.clone()),
            engine.add_drop_source(AudioInputDrops::new(self.mic.clone())),
            engine.add_drop_source(AudioInputDrops::new(self.desktop.clone())),
            engine.add_drop_source(AudioBusDrops::new(self.bus.clone())),
        ];
        self.drop_guard = Some(DropGuard::new(engine.drop_ledger(), ids));
        self.ticker = engine.spawn_stats_ticker(Arc::clone(&self.stop));
        *lock(&self.final_tick) = Some(engine.stats_emitter());
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        join("render", self.render.take());
        join("mixer", self.mixer.take());
        // These end once the render and mixer threads have dropped their senders.
        join("collector", self.collector.take());
        join("program watch", self.forward.take());
        join("stats", self.ticker.take());
        // After the threads stop, so the latch sees their final counters.
        self.drop_guard.take();
        let tick = self
            .final_tick
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(tick) = tick {
            tick();
        }
    }
}

/// The outputs to tell when the program stage fails. A failure is kept, so an output
/// that registers after it is told at once.
#[derive(Default)]
struct Failures {
    inner: Mutex<FailState>,
}

#[derive(Default)]
struct FailState {
    reason: Option<String>,
    /// Each running output's failure channel, by output and start generation.
    outputs: Vec<(OutputId, u64, Sender<PipelineEvent>)>,
}

impl Failures {
    /// Sends the program's failures to this output until the returned registration
    /// drops. Its watcher waits for every sender to go, so an output leaving a program
    /// others still use has to take its sender with it.
    fn add(
        self: &Arc<Self>,
        output_id: OutputId,
        generation: u64,
        output: Sender<PipelineEvent>,
    ) -> Registration {
        let mut state = lock(&self.inner);
        if let Some(reason) = &state.reason {
            let _ = output.send(PipelineEvent::Failed(reason.clone()));
        }
        state.outputs.push((output_id.clone(), generation, output));
        Registration {
            failures: Arc::clone(self),
            output_id,
            generation,
        }
    }

    fn failed(&self) -> bool {
        lock(&self.inner).reason.is_some()
    }

    fn fail(&self, reason: String) {
        let mut state = lock(&self.inner);
        state
            .outputs
            .retain(|(_, _, output)| output.send(PipelineEvent::Failed(reason.clone())).is_ok());
        state.reason.get_or_insert(reason);
    }
}

/// An output's place in [`Failures`], given up on drop.
struct Registration {
    failures: Arc<Failures>,
    output_id: OutputId,
    generation: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        lock(&self.failures.inner)
            .outputs
            .retain(|(output_id, generation, _)| {
                (output_id, *generation) != (&self.output_id, self.generation)
            });
    }
}

/// Passes the program threads' fatal errors on to every output, on its own thread,
/// so the render and mixer threads never take the [`Failures`] lock.
fn forward_failures(events: Receiver<PipelineEvent>, failures: &Failures) {
    for event in events {
        if let PipelineEvent::Failed(reason) = event {
            failures.fail(reason);
        }
    }
}

/// One video and one audio encoder session, shared by every running output with the
/// same [`EncodeKey`]. The output that lets go of it last finishes it.
pub(crate) struct Encoders {
    key: EncodeKey,
    video: Arc<VideoSession>,
    audio: Arc<AudioSession>,
    /// Whether the program threads feed it yet.
    attached: AtomicBool,
    drop_guard: Option<DropGuard>,
}

impl Encoders {
    /// Drains both encoders and ends their subscriptions, so the last output gets
    /// every packet.
    fn finish(&self) {
        if let Err(err) = self.video.finish() {
            tracing::info!(error = %err, "video session finish failed");
        }
        if let Err(err) = self.audio.finish() {
            tracing::info!(error = %err, "audio session finish failed");
        }
    }

    /// The profile the sessions were opened with, or `None` for the default.
    pub(crate) fn profile_id(&self) -> Option<&EncoderProfileId> {
        self.key.0.as_ref()
    }

    /// Whether the video encoder is a hardware one.
    pub(crate) fn hardware(&self) -> bool {
        self.video.info().codec.hardware
    }
}

impl Drop for Encoders {
    fn drop(&mut self) {
        // Latches the sessions' drop counters once they are done.
        self.drop_guard.take();
    }
}

/// The threads and sessions of one running output. Dropping it stops them.
pub(crate) struct Running {
    output_id: OutputId,
    generation: u64,
    stop: Arc<AtomicBool>,
    /// When the stop's drain gives up. Set by the drop, just before `stop`, and read by
    /// the output thread once it sees `stop`.
    drain_by: Arc<OnceLock<Instant>>,
    /// Set once the output may leave as soon as nothing is queued for it.
    cut: Arc<AtomicBool>,
    output: Option<JoinHandle<()>>,
    /// Disconnects when the output thread ends. Set for a live publish target only: the
    /// drop then waits for the thread until the drain deadline plus [`OUTPUT_GRACE`],
    /// and no longer. A recording is always joined, so its file is in place when the
    /// stop returns.
    output_done: Option<Receiver<()>>,
    /// The output thread's end of the failure channel, closed by the drop so a thread
    /// it stopped waiting for does not keep the publish watcher waiting too.
    output_fail: FailLine,
    encoders: Option<Arc<Encoders>>,
    stats: Arc<PipelineStats>,
    /// Packets the output thread dropped itself: the muxer rejected them, or the stop's
    /// drain deadline ([`STOP_DRAIN`]) passed before they were sent. Distinct from the
    /// encode subscriber queues.
    output_drops: Arc<AtomicU64>,
    /// Tags the publish target dropped while its link was down, as it last reported.
    link_drops: Arc<AtomicU64>,
    drop_guard: Option<DropGuard>,
    /// Its failure channel's place in the program, given up once its thread is gone.
    registration: Option<Registration>,
    /// Dropped last: when this was the program's last output, that stops the render
    /// and mixer threads.
    program: Option<Arc<Program>>,
}

impl Running {
    pub(crate) fn output_id(&self) -> &OutputId {
        &self.output_id
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// The encoder sessions this output uses.
    pub(crate) fn encoders(&self) -> Option<&Arc<Encoders>> {
        self.encoders.as_ref()
    }

    pub(crate) fn stats(&self) -> OutputStats {
        OutputStats {
            video: self
                .encoders
                .as_ref()
                .map(|e| e.video.stats())
                .unwrap_or_default(),
            audio: self
                .encoders
                .as_ref()
                .map(|e| e.audio.stats())
                .unwrap_or_default(),
            video_subscriber: *lock(&self.stats.video_subscriber),
            audio_subscriber: *lock(&self.stats.audio_subscriber),
            readbacks: self
                .program
                .as_ref()
                .map_or(0, |p| p.readbacks.load(Ordering::Relaxed)),
        }
    }

    /// Registers this output's own packet drops.
    fn arm_stats(&mut self, engine: &Engine) {
        let own = Arc::clone(&self.output_drops);
        let link = Arc::clone(&self.link_drops);
        let ids = vec![engine.add_drop_source(FnDropSource::new(move || {
            output_packet_drops(
                own.load(Ordering::Relaxed)
                    .saturating_add(link.load(Ordering::Relaxed)),
            )
        }))];
        self.drop_guard = Some(DropGuard::new(engine.drop_ledger(), ids));
    }
}

impl Running {
    /// Joins the output thread. A publishing one is waited for only until `by`: past
    /// that it is blocked in a publish target that ignores the stop deadline, so it is
    /// detached and ends on its own once that call returns. What it drops after this
    /// is not in the run's final counts.
    fn join_output(&mut self, by: Instant) {
        let Some(handle) = self.output.take() else {
            return;
        };
        let ended = self.output_done.take().is_none_or(|done| {
            !matches!(
                done.recv_timeout(by.saturating_duration_since(Instant::now())),
                Err(RecvTimeoutError::Timeout)
            )
        });
        if ended {
            join("output", Some(handle));
        } else {
            tracing::warn!(
                "the publish target did not return by the stop deadline; \
                 leaving the output thread to end on its own"
            );
            drop(handle);
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // The deadline is fixed here, before the joins below, so they count against
        // it and `output.stop` returns by it plus `OUTPUT_GRACE` (#138).
        let deadline = Instant::now() + STOP_DRAIN;
        let _ = self.drain_by.set(deadline);
        self.stop.store(true, Ordering::Release);
        // The last output on a session drains it, so the output thread can send the
        // remaining packets and then see the streams end. An output leaving a session
        // others still use takes what is queued for it and goes.
        if let Some(encoders) = self.encoders.take()
            && let Some(encoders) = Arc::into_inner(encoders)
        {
            encoders.finish();
        }
        self.cut.store(true, Ordering::Release);
        self.join_output(deadline + OUTPUT_GRACE);
        self.output_fail.close();
        // After the thread stops, so the latch sees its final counters.
        self.drop_guard.take();
        self.registration.take();
        self.program.take();
    }
}

fn stop_and_join(stop: &AtomicBool, handles: Vec<JoinHandle<()>>) {
    stop.store(true, Ordering::Release);
    for handle in handles {
        join("pipeline", Some(handle));
    }
}

fn join(name: &str, handle: Option<JoinHandle<()>>) {
    if let Some(handle) = handle
        && let Err(err) = handle.join()
    {
        tracing::error!(thread = name, panic = ?err, "pipeline thread panicked");
    }
}

pub(crate) struct Parts {
    clock: Arc<dyn MediaClock>,
    capture: Arc<dyn Capture>,
    audio: Arc<dyn AudioIo>,
    codecs: Arc<dyn Codecs>,
    draw: OpenDraw,
    open_timeout: Duration,
    render: Arc<RenderHub>,
}

pub(crate) fn parts_from(
    backends: &Backends,
    draw: OpenDraw,
    open_timeout: Duration,
    render: Arc<RenderHub>,
) -> Parts {
    Parts {
        clock: Arc::clone(&backends.clock),
        capture: Arc::clone(&backends.capture),
        audio: Arc::clone(&backends.audio),
        codecs: Arc::clone(&backends.codecs),
        draw,
        open_timeout,
        render,
    }
}

/// The wgpu compositor on the software adapter, reading every frame back to the CPU.
/// Its [`ResourceCache`] keeps the uploads and nested-scene layers from tick to tick.
pub(crate) fn gpu_draw() -> OpenDraw {
    Arc::new(|| {
        let mut compositor =
            Compositor::open(OpenOptions::headless()).map_err(|err| render_open_error(&err))?;
        let mut cache = ResourceCache::<GpuKey>::new();
        let draw: DrawFn = Box::new(move |shot, size, capture, out| {
            cache.begin_frame();
            snapshot::compose(&mut compositor, &mut cache, shot, capture, size, true)
                .and_then(|output| copy_pixels(&output, out))
                .map_err(|err| err.to_string())?;
            // A failed frame skips this: what it retired waits for the next frame's
            // fence, which also covers it, so nothing is freed on this thread.
            Ok(cache.end_frame(&compositor))
        });
        Ok(draw)
    })
}

/// What the pipeline threads tell the publish watcher.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PipelineEvent {
    /// A fatal error: the watcher settles `failed`.
    Failed(String),
    /// The publish link went down and is being re-established: `reconnecting`.
    LinkDown,
    /// The publish link is up again: `active`.
    LinkUp,
}

/// Sends a fatal pipeline error to the publish watcher, which settles `failed`.
fn report(fail: &Sender<PipelineEvent>, reason: String) {
    tracing::error!(error = %reason, "pipeline failed");
    // The receiver is gone once the pipeline is being torn down; nothing to tell.
    let _ = fail.send(PipelineEvent::Failed(reason));
}

/// The output thread's sender to the publish watcher, which the stop can close. The
/// watcher ends when every sender is gone, and `output.stop` joins it, so an output
/// thread the stop has left behind must not hold one (#138). The lock is taken only
/// to send or close, never across a call into the publish target.
#[derive(Clone)]
struct FailLine(Arc<Mutex<Option<Sender<PipelineEvent>>>>);

impl FailLine {
    fn new(fail: Sender<PipelineEvent>) -> Self {
        Self(Arc::new(Mutex::new(Some(fail))))
    }

    /// Sends `event`, unless the line is closed.
    fn send(&self, event: PipelineEvent) {
        if let Some(fail) = lock(&self.0).as_ref() {
            // The receiver is gone once the pipeline is being torn down.
            let _ = fail.send(event);
        }
    }

    /// [`report`] on this line.
    fn report(&self, reason: String) {
        tracing::error!(error = %reason, "pipeline failed");
        self.send(PipelineEvent::Failed(reason));
    }

    /// Drops the sender. Later sends do nothing.
    fn close(&self) {
        drop(lock(&self.0).take());
    }
}

fn panic_message(panic: &(dyn Any + Send)) -> &str {
    if let Some(text) = panic.downcast_ref::<&str>() {
        text
    } else if let Some(text) = panic.downcast_ref::<String>() {
        text
    } else {
        "a non-string panic"
    }
}

/// Opens the program stage: the capture and the audio devices, then the render and
/// mixer threads, and waits for the compositor to open. On failure everything opened
/// is dropped again.
pub(crate) fn launch_program(engine: &Engine, parts: Parts) -> Result<Arc<Program>, String> {
    let (size, capture) = open_capture(&parts)?;
    let format = engine_format()?;
    // Bindings drop in reverse pattern order, so on an early return the streams (and
    // their producers) go before the input controls, and the rings are freed here
    // rather than on a real-time thread (docs/RULES.md#R-RT-003).
    let OpenedAudio {
        mic,
        desktop,
        mixer,
        streams,
    } = open_audio(&parts, format)?;
    let bus = mixer.bus();
    let period = frame_period(parts.clock.timebase())?;
    if period.ticks() <= 0 {
        return Err("the media clock is too coarse for the frame rate".into());
    }
    // The mixer timestamps its first block `latency` before the clock, so a stream's
    // origin sits that far back. Later blocks and frames stay at or after it.
    let latency = mixer_latency(parts.clock.as_ref(), &mixer)?;
    let capture_drops = DropSnapshot::new();
    let stop = Arc::new(AtomicBool::new(false));
    let readbacks = Arc::new(AtomicU64::new(0));
    let failures = Arc::new(Failures::default());
    let (ready_tx, ready_rx) = mpsc::channel();
    let (video_taps, video_inputs) = mpsc::channel();
    let (audio_taps, audio_inputs) = mpsc::channel();
    let (fail_tx, fail_rx) = mpsc::channel();

    let forward = spawn("jstream-program-watch", {
        let failures = Arc::clone(&failures);
        move || forward_failures(fail_rx, &failures)
    })?;
    let (collector, collector_thread) = match Collector::spawn() {
        Ok(collector) => collector,
        Err(err) => {
            drop(fail_tx);
            stop_and_join(&stop, vec![forward]);
            return Err(err);
        }
    };
    let mixer_collector = collector.clone();
    let render = spawn("jstream-render", {
        let job = RenderJob {
            capture,
            reader: RenderReader::new(Arc::clone(&parts.render), collector.clone()),
            collector,
            open: Arc::clone(&parts.draw),
            clock: Arc::clone(&parts.clock),
            inputs: video_inputs,
            size,
            period,
            stop: Arc::clone(&stop),
            ready: ready_tx,
            readbacks: Arc::clone(&readbacks),
            drops: capture_drops.clone(),
            fail: fail_tx.clone(),
        };
        move || render_thread(job)
    });
    // The render and mixer threads own the collector's senders, so the collector ends
    // once both have (or here, if they never started).
    let render = match render {
        Ok(handle) => handle,
        Err(err) => {
            drop(mixer_collector);
            drop(fail_tx);
            stop_and_join(&stop, vec![collector_thread, forward]);
            return Err(err);
        }
    };
    let mixer_thread = match spawn("jstream-mixer", {
        let clock = Arc::clone(&parts.clock);
        let stop = Arc::clone(&stop);
        let fail = fail_tx;
        let collector = mixer_collector;
        move || mixer_thread(mixer, clock, audio_inputs, format, stop, fail, &collector)
    }) {
        Ok(handle) => handle,
        Err(err) => {
            stop_and_join(&stop, vec![render, collector_thread, forward]);
            return Err(err);
        }
    };

    let mut program = Program {
        stop,
        render: Some(render),
        collector: Some(collector_thread),
        mixer: Some(mixer_thread),
        forward: Some(forward),
        clock: Arc::clone(&parts.clock),
        codecs: Arc::clone(&parts.codecs),
        size,
        format,
        latency,
        video_taps,
        audio_taps,
        failures,
        sessions: Mutex::new(SessionRegistry::new()),
        readbacks,
        _streams: Mutex::new(streams),
        capture_drops,
        mic,
        desktop,
        bus,
        drop_guard: None,
        ticker: None,
        final_tick: Mutex::new(None),
    };
    match ready_rx.recv_timeout(parts.open_timeout) {
        Ok(Ok(())) => {
            program.arm_stats(engine);
            Ok(Arc::new(program))
        }
        Ok(Err(reason)) => {
            drop(program);
            Err(reason)
        }
        Err(RecvTimeoutError::Timeout) => {
            drop(program);
            Err(format!(
                "the compositor did not open within {}s",
                parts.open_timeout.as_secs()
            ))
        }
        Err(RecvTimeoutError::Disconnected) => {
            drop(program);
            Err("the render thread stopped before the compositor opened".into())
        }
    }
}

/// Starts one output on `program`: its encoder sessions (shared with the running
/// outputs that have the same `key`), then the destination, then its output thread.
/// On failure the publish target is dropped: a one-shot target has to be set again,
/// an opener opens a new one on the next start. A recording opened before the failure
/// is removed, so its `.part` file doesn't block the next start.
pub(crate) fn launch(
    engine: &Engine,
    program: Arc<Program>,
    key: EncodeKey,
    destination: Destination,
    output_id: OutputId,
    generation: u64,
    fail_tx: Sender<PipelineEvent>,
) -> Result<Running, String> {
    let registration = program
        .failures
        .add(output_id.clone(), generation, fail_tx.clone());
    let encoders = program.encoders(engine, key)?;
    let origin = program.origin()?;
    // Set once the start has succeeded; until then a recording is provisional.
    let keep = Arc::new(AtomicBool::new(false));
    let info = stream_info(program.size, origin, &encoders.key.1);
    let publishing = matches!(destination, Destination::Publish(_));
    let sink = Sink::open(destination, &info, Arc::clone(&keep))?;
    let stop = Arc::new(AtomicBool::new(false));
    let drain_by = Arc::new(OnceLock::new());
    let cut = Arc::new(AtomicBool::new(false));
    let stats = PipelineStats::new();
    let output_drops = Arc::new(AtomicU64::new(0));
    let link_drops = Arc::new(AtomicU64::new(0));

    // Subscribe before a new session is fed, so its first packets (the first video
    // keyframe among them) are not encoded before the output thread is listening. A
    // session already running gives this output a keyframe on request.
    let video_sub = encoders.video.subscribe();
    let audio_sub = encoders.audio.subscribe();
    program.attach(&encoders);
    let (started_tx, started_rx) = mpsc::channel();
    let output_fail = FailLine::new(fail_tx);
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let output = spawn("jstream-output", {
        let stats = Arc::clone(&stats);
        let own = Arc::clone(&output_drops);
        let link = Arc::clone(&link_drops);
        let stop = Arc::clone(&stop);
        let drain_by = Arc::clone(&drain_by);
        let cut = Arc::clone(&cut);
        let fail = output_fail.clone();
        move || {
            // Dropped when this thread ends, even by a panic: `join_output` waits on it.
            let _done = done_tx;
            output_loop(OutputJob {
                sink,
                video: video_sub,
                audio: audio_sub,
                stats,
                output_drops: own,
                link_drops: link,
                stop,
                drain_by,
                cut,
                fail,
                started: started_tx,
            });
        }
    })?;
    // Returns once the destination has taken the stream's header, or refused it: a
    // refusal is then already on the failure channel, so the start sees it.
    let _ = started_rx.recv();
    let mut running = Running {
        output_id,
        generation,
        stop,
        drain_by,
        cut,
        output: Some(output),
        output_done: publishing.then_some(done_rx),
        output_fail,
        encoders: Some(encoders),
        stats,
        output_drops,
        link_drops,
        drop_guard: None,
        registration: Some(registration),
        program: Some(program),
    };
    running.arm_stats(engine);
    keep.store(true, Ordering::Release);
    Ok(running)
}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, String> {
    thread::Builder::new()
        .name(name.into())
        .spawn(body)
        .map_err(|err| format!("starting the {name} thread: {err}"))
}

fn engine_format() -> Result<AudioFormat, String> {
    AudioFormat::new(
        SAMPLE_RATE,
        ChannelLayout::Stereo,
        SampleFormat::F32Interleaved,
    )
    .map_err(|err| format!("engine audio format: {err}"))
}

fn frame_period(clock: Timebase) -> Result<MediaTime, String> {
    let base = Timebase::new(FRAME_NUM, FRAME_DEN).map_err(|err| format!("frame period: {err}"))?;
    MediaTime::new(1, base)
        .rescale(clock, Rounding::Nearest)
        .map_err(|err| format!("frame period on the media clock: {err}"))
}

fn open_capture(parts: &Parts) -> Result<(FrameSize, Box<dyn CaptureStream>), String> {
    let targets = parts
        .capture
        .targets()
        .map_err(|err| format!("listing capture targets: {err}"))?;
    let index = targets
        .iter()
        .position(|target| target.primary)
        .or_else(|| targets.first().map(|_| 0));
    let Some(index) = index else {
        return Err("no capture target".into());
    };
    let info = targets.into_iter().nth(index).ok_or("no capture target")?;
    let Some(size) = info.size else {
        return Err(format!("capture target `{}` has no size", info.name));
    };
    let stream = parts
        .capture
        .start(&info.target)
        .map_err(|err| format!("starting capture: {err}"))?;
    Ok((size, stream))
}

struct OpenedAudio {
    mixer: Mixer,
    streams: Vec<Box<dyn AudioStream>>,
    mic: InputControl,
    desktop: InputControl,
}

fn open_audio(parts: &Parts, format: AudioFormat) -> Result<OpenedAudio, String> {
    let mut mixer = Mixer::new(MixerConfig::default()).map_err(|err| format!("mixer: {err}"))?;
    let (mic, mic_control) = mixer
        .add_input(format)
        .map_err(|err| format!("microphone input: {err}"))?;
    let (desktop, desktop_control) = mixer
        .add_input(format)
        .map_err(|err| format!("loopback input: {err}"))?;
    let mic = parts
        .audio
        .open_input(&AudioInputTarget::DefaultInput, Box::new(mic))
        .map_err(|err| format!("opening the microphone: {err}"))?;
    let desktop = parts
        .audio
        .open_input(&AudioInputTarget::DefaultOutputLoopback, Box::new(desktop))
        .map_err(|err| format!("opening desktop audio: {err}"))?;
    if mic.format() != format || desktop.format() != format {
        return Err(format!(
            "audio device format does not match the mixer (microphone {:?}, desktop {:?}, mixer {:?})",
            mic.format(),
            desktop.format(),
            format
        ));
    }
    Ok(OpenedAudio {
        mixer,
        streams: vec![mic, desktop],
        mic: mic_control,
        desktop: desktop_control,
    })
}

/// The video encoder settings for `profile` at `size` and the base frame rate.
fn video_config(size: FrameSize, profile: &EncoderProfile) -> Result<VideoEncoderConfig, String> {
    let period =
        Timebase::new(FRAME_NUM, FRAME_DEN).map_err(|err| format!("frame period: {err}"))?;
    let video = &profile.video;
    let rate_control = match video.rate_control {
        ProfileRate::Cbr { kbps } => RateControl::Cbr { kbps },
        ProfileRate::Vbr { kbps, max_kbps } => RateControl::Vbr { kbps, max_kbps },
        _ => return Err("the encoder profile's rate control is not one this engine knows".into()),
    };
    let hardware = match video.hardware {
        ProfileHardware::Prefer => HardwarePreference::Prefer,
        ProfileHardware::Require => HardwarePreference::Require,
        _ => HardwarePreference::Avoid,
    };
    Ok(VideoEncoderConfig {
        codec: Codec::H264,
        size,
        input_format: PixelFormat::Rgba8,
        frame_period: period,
        rate_control,
        keyframe_interval: video
            .keyframe_interval_s
            .saturating_mul(FRAME_DEN / FRAME_NUM),
        hardware,
    })
}

fn open_video(
    codecs: &dyn Codecs,
    size: FrameSize,
    profile: &EncoderProfile,
) -> Result<VideoSession, String> {
    VideoSession::open(
        codecs,
        video_config(size, profile)?,
        SessionOptions::default(),
    )
    .map_err(|err| format!("opening the video encoder: {err}"))
}

fn open_audio_encoder(
    codecs: &dyn Codecs,
    format: AudioFormat,
    profile: &EncoderProfile,
) -> Result<AudioSession, String> {
    let config = AudioEncoderConfig {
        codec: Codec::Aac,
        input_format: format,
        kbps: profile.audio.kbps,
    };
    AudioSession::open(codecs, config, SessionOptions::default())
        .map_err(|err| format!("opening the audio encoder: {err}"))
}

/// How far behind the clock the mixer stamps its blocks, on the clock's timebase.
fn mixer_latency(clock: &dyn MediaClock, mixer: &Mixer) -> Result<MediaTime, String> {
    let config = mixer.config();
    let frames = i64::try_from(config.latency_frames)
        .map_err(|_| "mixer latency does not fit a timestamp".to_string())?;
    let sample =
        Timebase::per_second(config.sample_rate).map_err(|err| format!("mixer timebase: {err}"))?;
    MediaTime::new(frames, sample)
        .rescale(clock.timebase(), Rounding::Up)
        .map_err(|err| format!("mixer latency: {err}"))
}

fn stream_info(size: FrameSize, origin: MediaTime, profile: &EncoderProfile) -> StreamInfo {
    let video_kbps = match profile.video.rate_control {
        ProfileRate::Cbr { kbps } | ProfileRate::Vbr { kbps, .. } => kbps,
        _ => 0,
    };
    StreamInfo {
        size,
        origin,
        frame_rate: f64::from(FRAME_DEN) / f64::from(FRAME_NUM),
        video_kbps,
        audio_kbps: profile.audio.kbps,
        sample_rate: SAMPLE_RATE,
        sps: &SPS,
        pps: &PPS,
    }
}

struct RenderJob {
    capture: Box<dyn CaptureStream>,
    reader: RenderReader,
    /// Takes the input of a session that has finished, so the render thread never drops
    /// the last reference to it (R-RT-003).
    collector: Collector,
    open: OpenDraw,
    clock: Arc<dyn MediaClock>,
    /// The sessions to push frames into, as they are opened.
    inputs: Receiver<VideoInput>,
    size: FrameSize,
    period: MediaTime,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<(), String>>,
    readbacks: Arc<AtomicU64>,
    drops: DropSnapshot,
    fail: Sender<PipelineEvent>,
}

/// The render thread's boundary: a panic outside a tick (opening the compositor,
/// dropping the capture stream) is caught here and reported as fatal.
fn render_thread(job: RenderJob) {
    let fail = job.fail.clone();
    if let Err(panic) = panic::catch_unwind(AssertUnwindSafe(|| render_loop(job))) {
        report(
            &fail,
            format!("the render thread panicked: {}", panic_message(&*panic)),
        );
    }
}

/// What the render thread keeps between ticks. The buffers are reused, so a tick
/// allocates only the frame the encoder takes ownership of.
struct Frames {
    /// The latest capture, converted to RGBA8. Empty until the first usable frame.
    source: Vec<u8>,
    /// Counts the captures copied into `source`, so the compositor uploads a capture
    /// only when it is new.
    source_version: u64,
    /// The last frame composited in full. Repeated when a tick cannot draw.
    shown: Vec<u8>,
    /// Where a tick composites; swapped into `shown` when the draw succeeds.
    scratch: Vec<u8>,
    capture: jstream_platform::CaptureStats,
    warned_format: bool,
    capture_failing: bool,
}

impl Frames {
    /// Reads the capture mailbox. An error or a missing frame keeps the last one.
    fn read_capture(&mut self, capture: &mut dyn CaptureStream, size: FrameSize) {
        match capture.latest() {
            Ok(Some(surface)) => {
                self.capture_failing = false;
                if rgba_from_surface(&surface, size, &mut self.source) {
                    self.source_version += 1;
                } else if !self.warned_format {
                    self.warned_format = true;
                    tracing::warn!("capture frame is not CPU RGBA or BGRA at the encoder size");
                }
            }
            Ok(None) => {}
            Err(err) => {
                if !self.capture_failing {
                    self.capture_failing = true;
                    tracing::warn!(error = %err, "capture failed; repeating the last frame");
                }
            }
        }
        self.capture = capture.stats();
    }

    /// Composites `shot` into `scratch` and, on success, makes that the shown frame.
    /// Returns the GPU images the compositor let go of.
    fn draw(
        &mut self,
        draw: &mut DrawFn,
        shot: &Shot,
        size: FrameSize,
    ) -> Result<Option<Reclaimed>, String> {
        let capture = (!self.source.is_empty()).then_some(CaptureFrame {
            version: self.source_version,
            size,
            rgba: &self.source,
        });
        let freed = draw(shot, size, capture.as_ref(), &mut self.scratch)?;
        std::mem::swap(&mut self.scratch, &mut self.shown);
        Ok(freed)
    }
}

/// Composites on every base-rate tick of the media clock.
///
/// Each tick first adopts the render snapshot the control side published last, which
/// costs a pointer compare unless a new one arrived ([`RenderReader::adopt`]); the
/// thread never takes the collection lock. Its generation is stamped as the thread
/// starts and whenever it changes. The stopped marker is published when the loop ends,
/// and when a fatal error stops the drawing.
///
/// Recoverable: the capture mailbox failing or having no new frame repeats the last
/// capture, and a composite error repeats the last composited frame, up to
/// [`MAX_DRAW_ERRORS`] in a row. Fatal, reported on the failure channel: more composite
/// errors than that, a panic inside a tick (the compositor's state is unknown), and a
/// frame time that overflows. After a fatal error the thread keeps pushing the last
/// frame until the pipeline stops, so the stream does not freeze before teardown. A
/// tick that falls a whole period or more behind the clock skips to the current tick
/// and counts the ticks it skipped in `render_ticks` (R-RT-004), so video pts stay on
/// the clock that stamps audio.
///
/// Each frame goes to every encoder session the program has handed it. A session that
/// refuses input because it has finished is let go of; the thread runs until the
/// program stops. While it has no session it draws nothing.
fn render_loop(job: RenderJob) {
    let RenderJob {
        mut capture,
        mut reader,
        collector,
        open,
        clock,
        inputs,
        size,
        period,
        stop,
        ready,
        readbacks,
        drops,
        fail,
    } = job;
    let mut targets: Vec<VideoInput> = Vec::new();
    let mut draw = match open() {
        Ok(draw) => draw,
        Err(err) => {
            let _ = ready.send(Err(err));
            return;
        }
    };
    if ready.send(Ok(())).is_err() {
        return;
    }
    reader.start();
    let mut frames = Frames {
        source: Vec::new(),
        source_version: 0,
        shown: Vec::new(),
        scratch: Vec::new(),
        capture: jstream_platform::CaptureStats::default(),
        warned_format: false,
        capture_failing: false,
    };
    let mut next: Option<MediaTime> = None;
    let mut skipped: u64 = 0;
    let mut draw_errors: u32 = 0;
    // Set by a fatal error: stop drawing and repeat `shown` until the pipeline stops.
    let mut broken = false;
    while !stop.load(Ordering::Relaxed) {
        targets.extend(inputs.try_iter());
        if targets.is_empty() {
            // No session to draw for, so no tick is due and none counts as skipped;
            // the first tick after a session arrives is drawn at the clock. It still
            // adopts the latest snapshot, so a gesture never waits on it.
            next = None;
            if !broken {
                reader.adopt();
            }
            thread::sleep(Duration::from_millis(1));
            continue;
        }
        let now = clock.now();
        let due = next.unwrap_or(now);
        if now < due {
            thread::sleep(Duration::from_millis(1));
            continue;
        }
        let (due, missed) = match catch_up(due, now, period) {
            Ok(caught) => caught,
            Err(err) => {
                report(&fail, err);
                break;
            }
        };
        skipped = skipped.saturating_add(missed);
        if !broken {
            let tick = panic::catch_unwind(AssertUnwindSafe(|| {
                frames.read_capture(capture.as_mut(), size);
                let snapshot = reader.adopt();
                frames.draw(&mut draw, snapshot.shot(), size)
            }));
            match tick {
                Ok(Ok(freed)) => {
                    draw_errors = 0;
                    readbacks.fetch_add(1, Ordering::Relaxed);
                    if let Some(freed) = freed {
                        reader.free(freed);
                    }
                }
                Ok(Err(err)) => {
                    draw_errors += 1;
                    if draw_errors == 1 {
                        tracing::warn!(error = %err, "composite failed; repeating the last frame");
                    }
                    if draw_errors > MAX_DRAW_ERRORS {
                        broken = true;
                        // No longer adopting snapshots, so it holds no gesture up.
                        reader.stop();
                        report(
                            &fail,
                            format!("compositing failed {draw_errors} times in a row: {err}"),
                        );
                    }
                }
                Err(panic) => {
                    broken = true;
                    reader.stop();
                    report(
                        &fail,
                        format!("the render thread panicked: {}", panic_message(&*panic)),
                    );
                }
            }
        }
        drops.publish(capture_drops(frames.capture).saturating_add(render_tick_drops(skipped)));
        if !frames.shown.is_empty() {
            push_each(
                &mut targets,
                |input| push_frame(input, size, due, &frames.shown),
                |input| collector.free(Garbage::Video(input)),
            );
        }
        next = match due.checked_add(period) {
            Ok(next) => Some(next),
            Err(err) => {
                report(&fail, format!("frame time overflow: {err}"));
                break;
            }
        };
    }
}

/// The tick to draw when the clock reads `now` and the next tick was `due`, and how
/// many whole periods before it were missed. Not late by a period: `due`, none missed.
fn catch_up(due: MediaTime, now: MediaTime, period: MediaTime) -> Result<(MediaTime, u64), String> {
    let overflow = |err: jstream_media::MediaError| format!("frame time overflow: {err}");
    let late = now.checked_sub(due).map_err(overflow)?.ticks();
    let step = period.ticks();
    if step <= 0 || late < step {
        return Ok((due, 0));
    }
    let missed = late / step;
    let by = missed
        .checked_mul(step)
        .ok_or_else(|| "frame time overflow: skipped ticks".to_string())?;
    let tick = due
        .checked_add(MediaTime::new(by, period.timebase()))
        .map_err(overflow)?;
    Ok((tick, missed.unsigned_abs()))
}

fn render_open_error(err: &RenderError) -> String {
    if err.is_no_adapter() {
        err.to_string()
    } else {
        format!("opening the compositor: {err}")
    }
}

/// Copies a CPU RGBA8 or BGRA8 capture at `expected` into `out` as RGBA8, reusing its
/// allocation. `false`, leaving `out` as it was, when the frame is not usable.
fn rgba_from_surface(surface: &VideoSurface, expected: FrameSize, out: &mut Vec<u8>) -> bool {
    let VideoSurface::Cpu(frame) = surface else {
        return false;
    };
    let info = frame.info();
    if info.size != expected || !matches!(info.format, PixelFormat::Rgba8 | PixelFormat::Bgra8) {
        return false;
    }
    let width = info.size.width() as usize;
    let height = info.size.height() as usize;
    let Some(len) = width.checked_mul(height).and_then(|n| n.checked_mul(4)) else {
        return false;
    };
    let row_len = width * 4;
    for y in 0..height {
        match frame.row(0, y as u32) {
            Some(row) if row.len() >= row_len => {}
            _ => return false,
        }
    }
    out.resize(len, 0);
    for y in 0..height {
        let Some(row) = frame.row(0, y as u32) else {
            return false;
        };
        let dst = &mut out[y * row_len..(y + 1) * row_len];
        match info.format {
            PixelFormat::Rgba8 => dst.copy_from_slice(&row[..row_len]),
            _ => {
                for (d, s) in dst.chunks_exact_mut(4).zip(row[..row_len].chunks_exact(4)) {
                    d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
                }
            }
        }
    }
    true
}

/// Copies the readback of `output` into `out`, reusing its allocation.
fn copy_pixels(output: &jstream_render::Output, out: &mut Vec<u8>) -> Result<(), RenderError> {
    match output.pixels() {
        Some(snapshot) => {
            out.clear();
            out.extend_from_slice(snapshot.rgba());
            Ok(())
        }
        None => Err(RenderError::Readback {
            detail: "the compositor returned no pixels".into(),
        }),
    }
}

/// Pushes one frame. `false` when the session has closed.
///
/// The encoder session takes ownership of the frame's plane, so this is the one
/// allocation a tick makes per session: `VideoInput::push` has no way to hand buffers
/// back.
fn push_frame(input: &VideoInput, size: FrameSize, pts: MediaTime, rgba: &[u8]) -> bool {
    let stride = size.width() as usize * 4;
    let info = VideoFrameInfo {
        size,
        format: PixelFormat::Rgba8,
        color: ColorInfo {
            space: ColorSpace::Bt709,
            range: ColorRange::Full,
        },
        pts,
    };
    let frame = match VideoFrame::new(info, vec![Plane::new(rgba.to_vec(), stride)]) {
        Ok(frame) => frame,
        Err(err) => {
            tracing::warn!(error = %err, "encoded frame rejected");
            return true;
        }
    };
    match input.push(VideoSurface::Cpu(frame)) {
        Ok(()) | Err(PushError::Full) => true,
        Err(PushError::Closed) => false,
    }
}

/// Pushes into every session in `targets` with `push`, which says whether the session
/// still takes input. A finished one is taken out and given to `retire`, which hands it
/// to the collector rather than dropping it here: once its session has dropped, this is
/// the last reference to the session's shared state and input channel, and freeing them
/// is not a real-time thread's job (R-RT-003). The order of `targets` does not matter.
fn push_each<T>(targets: &mut Vec<T>, mut push: impl FnMut(&T) -> bool, mut retire: impl FnMut(T)) {
    let mut i = 0;
    while i < targets.len() {
        if push(&targets[i]) {
            i += 1;
        } else {
            retire(targets.swap_remove(i));
        }
    }
}

/// The mixer thread's boundary: a panic is caught and reported as fatal. The encoders
/// then get no more audio, so the outputs cannot keep running.
fn mixer_thread(
    mixer: Mixer,
    clock: Arc<dyn MediaClock>,
    inputs: Receiver<AudioInput>,
    format: AudioFormat,
    stop: Arc<AtomicBool>,
    fail: Sender<PipelineEvent>,
    collector: &Collector,
) {
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        mixer_loop(
            mixer,
            clock.as_ref(),
            &inputs,
            format,
            &stop,
            &fail,
            collector,
        );
    }));
    if let Err(panic) = result {
        report(
            &fail,
            format!("the mixer thread panicked: {}", panic_message(&*panic)),
        );
    }
}

/// Mixes on the media clock, and pushes each block into every encoder session the
/// program has handed it, letting go of one that has finished. A rejected block or a
/// poll error is fatal: both mean the mixer and the encoder disagree about the format,
/// which no retry fixes.
fn mixer_loop(
    mut mixer: Mixer,
    clock: &dyn MediaClock,
    inputs: &Receiver<AudioInput>,
    format: AudioFormat,
    stop: &AtomicBool,
    fail: &Sender<PipelineEvent>,
    collector: &Collector,
) {
    let samples_len = mixer.config().block_frames.saturating_mul(2);
    let mut samples = vec![0.0f32; samples_len];
    let mut targets: Vec<AudioInput> = Vec::new();
    'mix: while !stop.load(Ordering::Relaxed) {
        match mixer.poll(clock.now(), &mut samples) {
            Ok(Some(block)) => {
                targets.extend(inputs.try_iter());
                let mut rejected = None;
                // The mix itself does not allocate. `AudioInput::push` takes an owned
                // buffer and keeps it, so each block needs its own per session: this
                // clone is forced by that API. `Full` is counted by the session.
                // `Closed` means the session finished; the outputs on it report an
                // early end themselves.
                push_each(
                    &mut targets,
                    |input| match AudioBuffer::new(format, block.pts, samples.clone()) {
                        Ok(buffer) => !matches!(input.push(buffer), Err(PushError::Closed)),
                        Err(err) => {
                            rejected = Some(err);
                            true
                        }
                    },
                    |input| collector.free(Garbage::Audio(input)),
                );
                if let Some(err) = rejected {
                    report(fail, format!("mixer block rejected: {err}"));
                    break 'mix;
                }
            }
            Ok(None) => thread::sleep(Duration::from_millis(1)),
            Err(err) => {
                report(fail, format!("mixer poll failed: {err}"));
                break;
            }
        }
    }
}

struct OutputJob {
    sink: Sink,
    video: Subscription,
    audio: Subscription,
    stats: Arc<PipelineStats>,
    output_drops: Arc<AtomicU64>,
    link_drops: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    drain_by: Arc<OnceLock<Instant>>,
    cut: Arc<AtomicBool>,
    fail: FailLine,
    /// Told once the sink has started, or has failed to and reported why.
    started: Sender<()>,
}

/// Muxes both encoder streams and publishes or records them. A publish or write
/// error, or an encoder
/// stream that ends before the pipeline was told to stop (the encoder failed, or its
/// session closed), is fatal and goes to the failure channel. A lost link is not an
/// error: the publish target drops and counts tags while it reconnects, and this
/// loop reports each change of its [`LinkState`] on the same channel.
///
/// Once it sees the stop flag, the loop keeps sending what the encoders queued until
/// the stop's deadline (`drain_by`, [`STOP_DRAIN`] after the stop began), and tells the
/// publish target that deadline. When the deadline
/// passes, or a send fails while stopping, it drops the packets still queued, counts
/// them in `output_drops`, and finishes the target, which past the deadline closes
/// without its goodbye. A failure while stopping is logged, not reported: the output
/// is already on its way to `stopped`. An output leaving encoder sessions that other
/// outputs still use is `cut` instead of waiting for the streams to end: it sends what
/// is queued for it and finishes as soon as a pass finds nothing more.
fn output_loop(job: OutputJob) {
    let OutputJob {
        mut sink,
        video,
        audio,
        stats,
        output_drops,
        link_drops,
        stop,
        drain_by,
        cut,
        fail: fail_tx,
        started,
    } = job;
    if let Err(err) = sink.start() {
        fail_tx.report(err);
        let _ = started.send(());
        sink.finish();
        return;
    }
    let _ = started.send(());
    let mut video_done = false;
    let mut audio_done = false;
    let mut failed: Option<String> = None;
    let mut link = LinkState::Active;
    // Set when the loop first sees `stop`: when the drain gives up.
    let mut drain: Option<Instant> = None;
    let mut abandoned = false;
    // An end after `stop` is our own teardown; any other end is a failure.
    let ended = |kind: &str, end: StreamEnd| -> Option<String> {
        if stop.load(Ordering::Acquire) {
            tracing::info!(%end, "{kind} subscription ended");
            None
        } else {
            Some(format!("the {kind} encoder stream ended: {end}"))
        }
    };
    while failed.is_none() && (!video_done || !audio_done) {
        if drain.is_none() && stop.load(Ordering::Acquire) {
            // `drain_by` is set before `stop`; the fallback is for a stop that did not.
            let deadline = drain_by
                .get()
                .copied()
                .unwrap_or_else(|| Instant::now() + STOP_DRAIN);
            sink.stop_by(deadline);
            drain = Some(deadline);
        }
        if drain.is_some_and(|deadline| Instant::now() >= deadline) {
            abandoned = true;
            break;
        }
        // Read before this pass's receives, so a pass that finds nothing after the cut
        // has drained everything queued before it.
        let leaving = cut.load(Ordering::Acquire);
        let mut got = false;
        if !video_done {
            match video.recv_timeout(Duration::from_millis(2)) {
                Ok(Some(packet)) => {
                    got = true;
                    if let Err(err) = sink.forward(&packet, &output_drops) {
                        failed = Some(err);
                    }
                }
                Ok(None) => {}
                Err(end) => {
                    video_done = true;
                    failed = ended("video", end);
                }
            }
        }
        if failed.is_none() && !audio_done {
            match audio.recv_timeout(Duration::from_millis(2)) {
                Ok(Some(packet)) => {
                    got = true;
                    if let Err(err) = sink.forward(&packet, &output_drops) {
                        failed = Some(err);
                    }
                }
                Ok(None) => {}
                Err(end) => {
                    audio_done = true;
                    failed = ended("audio", end);
                }
            }
        }
        *lock(&stats.video_subscriber) = video.stats();
        *lock(&stats.audio_subscriber) = audio.stats();
        if failed.is_none()
            && let Err(err) = sink.poll()
        {
            failed = Some(err);
        }
        if failed.is_none() {
            link = report_link(&sink, link, &fail_tx);
        }
        link_drops.store(sink.dropped(), Ordering::Relaxed);
        if leaving && !got {
            break;
        }
    }
    if let Some(err) = failed {
        if stop.load(Ordering::Acquire) {
            tracing::warn!(error = %err, "publishing failed while stopping");
            abandoned = true;
        } else {
            fail_tx.report(err);
        }
    }
    if abandoned {
        let left = discard(&video, video_done).saturating_add(discard(&audio, audio_done));
        output_drops.fetch_add(left, Ordering::Relaxed);
        tracing::warn!(
            packets = left,
            "stop did not drain the output in time; dropped the packets still queued"
        );
    }
    link_drops.store(sink.dropped(), Ordering::Relaxed);
    sink.finish();
}

/// Takes and counts the packets queued on `sub` without sending them. Nothing when
/// the stream has already ended (`done`). An encoder still running can queue a few
/// more while this drains; they are taken and counted too, so the count can include
/// packets queued just after the deadline (#136 review N1).
fn discard(sub: &Subscription, done: bool) -> u64 {
    let mut count: u64 = 0;
    if done {
        return count;
    }
    while let Ok(Some(_)) = sub.try_recv() {
        count = count.saturating_add(1);
    }
    count
}

/// Tells the watcher when the publish link has changed from `last`, and returns
/// the link's state now. Giving up (`Failed`) is not reported here: the next send
/// or poll fails with it, and that error settles the output `failed`.
fn report_link(sink: &Sink, last: LinkState, fail: &FailLine) -> LinkState {
    let now = sink.link_state();
    let event = match (last, now) {
        (LinkState::Active, LinkState::Reconnecting) => PipelineEvent::LinkDown,
        (LinkState::Reconnecting, LinkState::Active) => PipelineEvent::LinkUp,
        _ => return now,
    };
    tracing::info!(link = ?now, "publish link changed");
    fail.send(event);
    now
}

#[cfg(test)]
mod tests {
    use super::{catch_up, push_each, video_config};
    use jstream_media::{FrameSize, MediaTime, Timebase};
    use jstream_platform::{HardwarePreference, RateControl};
    use jstream_protocol::{
        EncoderProfile, HardwarePreference as ProfileHardware, RateControl as ProfileRate,
    };

    /// Every target is pushed once; exactly the ones that refuse are taken out and
    /// retired (handed to the collector), and the rest stay.
    #[test]
    fn a_finished_target_is_retired_not_dropped() {
        let mut targets = vec![1, 2, 3, 4, 5];
        let mut pushed = Vec::new();
        let mut retired = Vec::new();
        push_each(
            &mut targets,
            |&target| {
                pushed.push(target);
                target % 2 == 1
            },
            |target| retired.push(target),
        );
        pushed.sort_unstable();
        targets.sort_unstable();
        retired.sort_unstable();
        assert_eq!(pushed, [1, 2, 3, 4, 5]);
        assert_eq!(targets, [1, 3, 5]);
        assert_eq!(retired, [2, 4]);
    }

    /// A profile maps onto the encoder config field by field; its keyframe interval in
    /// seconds becomes frames at the program's 30 fps.
    #[test]
    fn a_profile_maps_to_the_encoder_config() {
        let size = FrameSize::new(64, 36).expect("size");
        let config = video_config(size, &EncoderProfile::DEFAULT).expect("default");
        assert_eq!(config.rate_control, RateControl::Cbr { kbps: 2500 });
        assert_eq!(config.keyframe_interval, 60);
        assert_eq!(config.hardware, HardwarePreference::Avoid);

        let mut profile = EncoderProfile::DEFAULT;
        profile.video.rate_control = ProfileRate::Vbr {
            kbps: 12_000,
            max_kbps: 20_000,
        };
        profile.video.keyframe_interval_s = 4;
        profile.video.hardware = ProfileHardware::Require;
        let config = video_config(size, &profile).expect("vbr");
        assert_eq!(
            config.rate_control,
            RateControl::Vbr {
                kbps: 12_000,
                max_kbps: 20_000
            }
        );
        assert_eq!(config.keyframe_interval, 120);
        assert_eq!(config.hardware, HardwarePreference::Require);

        profile.video.hardware = ProfileHardware::Prefer;
        let config = video_config(size, &profile).expect("prefer");
        assert_eq!(config.hardware, HardwarePreference::Prefer);
        profile.video.rate_control = ProfileRate::Unknown;
        assert!(video_config(size, &profile).is_err());
    }

    fn us(ticks: i64) -> MediaTime {
        MediaTime::new(ticks, Timebase::MICROS)
    }

    /// Less than a period late keeps the tick. A period or more skips to the latest
    /// tick at or before the clock and counts the ones in between.
    #[test]
    fn a_late_tick_catches_up_to_the_clock() {
        let period = us(33_333);
        let (tick, missed) = catch_up(us(33_333), us(66_665), period).expect("on time");
        assert_eq!((tick.ticks(), missed), (33_333, 0));
        let (tick, missed) = catch_up(us(33_333), us(66_666), period).expect("one late");
        assert_eq!((tick.ticks(), missed), (66_666, 1));
        let (tick, missed) = catch_up(us(33_333), us(333_340), period).expect("nine late");
        assert_eq!((tick.ticks(), missed), (333_330, 9));
        assert!(
            catch_up(us(i64::MIN), us(i64::MAX), period).is_err(),
            "overflow"
        );
    }
}
