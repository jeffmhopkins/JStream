//! Starts and stops one output's threads.
//!
//! Capture is the platform's mailbox, read by the render thread. The render thread
//! composites one scene and, because the fake GPU interop has no encode target,
//! reads the frame back and pushes CPU RGBA. The mixer thread polls the engine
//! clock. Encode threads live inside the encoder sessions. The output thread muxes
//! FLV and publishes it. None of those threads lock the engine's collection.
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

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use jstream_audio::{BusControl, InputControl, Mixer, MixerConfig};
use jstream_encode::{
    AudioInput, AudioSession, PushError, SessionOptions, SessionStats, StreamEnd, SubscriberStats,
    Subscription, VideoInput, VideoSession,
};
use jstream_media::{
    AudioBuffer, AudioFormat, ChannelLayout, Codec, ColorInfo, ColorRange, ColorSpace, FrameSize,
    MediaTime, PixelFormat, Plane, Rounding, SampleFormat, Timebase, VideoFrame, VideoFrameInfo,
};
use jstream_mux::aac::{AOT_AAC_LC, AudioSpecificConfig};
use jstream_mux::flv::{AudioTrack, FlvConfig, FlvTagger, MetadataStyle, VideoTrack};
use jstream_mux::h264::AvcDecoderConfig;
use jstream_platform::{
    AudioEncoderConfig, AudioInputTarget, AudioIo, AudioStream, Backends, Capture, CaptureStream,
    Codecs, HardwarePreference, MediaClock, RateControl, VideoEncoderConfig, VideoSurface,
};
use jstream_protocol::OutputId;
use jstream_render::{Color, Compositor, Crop, Item, OpenOptions, Placement, RenderError, Scene};

use crate::drops::{
    AudioBusDrops, AudioEncodeDrops, AudioInputDrops, DropGuard, DropSnapshot, FnDropSource,
    VideoEncodeDrops, capture_drops, output_packet_drops, render_tick_drops,
};
use crate::engine::Engine;
use crate::lock;
use crate::runtime::{LinkState, LivePublish};

/// Base frame rate of this cut: one frame every 1/30 s.
const FRAME_NUM: u32 = 1;
const FRAME_DEN: u32 = 30;
/// Two seconds of frames between keyframes.
const KEYFRAME_INTERVAL: u32 = 60;
const VIDEO_KBPS: u32 = 2500;
const AUDIO_KBPS: u32 = 160;
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

/// Composites one frame on the render thread: the captured RGBA8 frame, or black when
/// there is none yet, at `size`, read back as RGBA8 into `out` (replacing its contents).
/// Created on the render thread and kept there, so it need not be `Send`.
pub(crate) type DrawFn =
    Box<dyn FnMut(FrameSize, Option<&[u8]>, &mut Vec<u8>) -> Result<(), String>>;

/// Opens the render thread's [`DrawFn`], on that thread. The engine's opens the wgpu
/// compositor on the software adapter ([`gpu_draw`]). Tests inject [`cpu_draw`] (or a
/// variant) so the pipeline runs without an adapter.
pub(crate) type OpenDraw = Arc<dyn Fn() -> Result<DrawFn, String> + Send + Sync>;

/// A CPU stand-in for [`gpu_draw`]: it copies the captured RGBA8 frame, or fills black
/// when there is none, which is what this cut's one-item composite produces. Tests use
/// it to run the pipeline without a wgpu adapter
/// ([`Engine::with_cpu_compositor`](crate::Engine::with_cpu_compositor)).
pub(crate) fn cpu_draw() -> OpenDraw {
    Arc::new(|| {
        let draw: DrawFn = Box::new(|size: FrameSize, rgba: Option<&[u8]>, out: &mut Vec<u8>| {
            out.clear();
            match rgba {
                Some(rgba) => out.extend_from_slice(rgba),
                None => out.resize(size.width() as usize * size.height() as usize * 4, 0),
            }
            Ok(())
        });
        Ok(draw)
    })
}

/// A fake Annex-B encoder does not emit SPS/PPS. FLV still needs a decoder
/// configuration, so the tagger is given the mux crate's baseline sets.
const SPS: [u8; 5] = [0x67, 0x42, 0xC0, 0x1E, 0xDA];
const PPS: [u8; 4] = [0x68, 0xCE, 0x3C, 0x80];

/// Counters a caller can read while an output is running.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutputStats {
    /// Video frames the encoder session accepted.
    pub video: SessionStats,
    /// Audio blocks the encoder session accepted.
    pub audio: SessionStats,
    /// What the output's video subscriber has been given and lost.
    pub video_subscriber: SubscriberStats,
    /// What the output's audio subscriber has been given and lost.
    pub audio_subscriber: SubscriberStats,
    /// CPU readbacks on the render thread. The fake GPU path has no encode target,
    /// so every encoded frame is one of these.
    pub readbacks: u64,
}

struct PipelineStats {
    video_subscriber: Mutex<SubscriberStats>,
    audio_subscriber: Mutex<SubscriberStats>,
    readbacks: AtomicU64,
}

impl PipelineStats {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            video_subscriber: Mutex::new(SubscriberStats::default()),
            audio_subscriber: Mutex::new(SubscriberStats::default()),
            readbacks: AtomicU64::new(0),
        })
    }
}

/// Backends and the publish target for one engine, plus the pipeline if it is up.
///
/// `gate` is taken after the collection lock when both are held. `output_stats`,
/// the in-flight wait and `Drop` take `gate` alone. Nothing takes the collection
/// lock while holding `gate`. `gate_cv` wakes a stop that is waiting for a start,
/// and a start that is waiting for a failed pipeline to finish joining.
pub(crate) struct MediaHost {
    pub(crate) backends: Mutex<Option<Backends>>,
    /// How the render thread composites. [`gpu_draw`] unless a test replaced it.
    pub(crate) draw: Mutex<OpenDraw>,
    /// How long a start waits for that compositor to open. [`OPEN_TIMEOUT`] unless a
    /// test raised it.
    pub(crate) open_timeout: Mutex<Duration>,
    pub(crate) publish: Mutex<Option<Box<dyn LivePublish>>>,
    pub(crate) gate: Mutex<Gate>,
    pub(crate) gate_cv: Condvar,
    pub(crate) next_generation: AtomicU64,
}

/// The pipeline slot. `pending` is a start that has emitted `starting` and has not
/// settled yet. `watcher` receives a publish failure and settles `failed`.
pub(crate) struct Gate {
    pub(crate) running: Option<Running>,
    pub(crate) pending: Option<Pending>,
    pub(crate) watcher: Option<JoinHandle<()>>,
    /// `Running` was taken and is still being joined. A new start waits this out.
    pub(crate) draining: bool,
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
            gate: Mutex::new(Gate {
                running: None,
                pending: None,
                watcher: None,
                draining: false,
            }),
            gate_cv: Condvar::new(),
            next_generation: AtomicU64::new(1),
        }
    }
}

impl Drop for MediaHost {
    fn drop(&mut self) {
        let (running, watcher) = {
            let gate = self.gate.get_mut().unwrap_or_else(PoisonError::into_inner);
            (gate.running.take(), gate.watcher.take())
        };
        drop(running);
        if let Some(watcher) = watcher
            && watcher.thread().id() != thread::current().id()
        {
            join("publish watcher", Some(watcher));
        }
    }
}

/// Threads and sessions of one running output. Dropping it stops them.
pub(crate) struct Running {
    output_id: OutputId,
    generation: u64,
    stop: Arc<AtomicBool>,
    render: Option<JoinHandle<()>>,
    mixer: Option<JoinHandle<()>>,
    output: Option<JoinHandle<()>>,
    video: Arc<VideoSession>,
    audio: Arc<AudioSession>,
    /// Kept alive so the platform keeps calling the mixer producers.
    _streams: Vec<Box<dyn AudioStream>>,
    stats: Arc<PipelineStats>,
    capture_drops: DropSnapshot,
    mic: InputControl,
    desktop: InputControl,
    bus: BusControl,
    /// Packets the output thread dropped itself: the muxer rejected them, or the stop's
    /// drain deadline ([`STOP_DRAIN`]) passed before they were sent. Distinct from the
    /// encode subscriber queues.
    output_drops: Arc<AtomicU64>,
    /// Tags the publish target dropped while its link was down, as it last reported.
    link_drops: Arc<AtomicU64>,
    drop_guard: Option<DropGuard>,
    ticker: Option<JoinHandle<()>>,
    /// Emits one last `stats.tick` once the threads have stopped and the sources are
    /// latched, so the run's final drops are reported when it ends.
    final_tick: Option<Box<dyn FnOnce() + Send>>,
}

impl Running {
    pub(crate) fn output_id(&self) -> &OutputId {
        &self.output_id
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn stats(&self) -> OutputStats {
        OutputStats {
            video: self.video.stats(),
            audio: self.audio.stats(),
            video_subscriber: *lock(&self.stats.video_subscriber),
            audio_subscriber: *lock(&self.stats.audio_subscriber),
            readbacks: self.stats.readbacks.load(Ordering::Relaxed),
        }
    }

    /// Registers this run's stages and starts the once-a-second sampler.
    ///
    /// Dropping the run removes the sources, which latches their totals, and joins
    /// the sampler.
    pub(crate) fn arm_stats(&mut self, engine: &Engine) {
        let own = Arc::clone(&self.output_drops);
        let link = Arc::clone(&self.link_drops);
        let ids = vec![
            engine.add_drop_source(self.capture_drops.clone()),
            engine.add_drop_source(VideoEncodeDrops::new(Arc::clone(&self.video))),
            engine.add_drop_source(AudioEncodeDrops::new(Arc::clone(&self.audio))),
            engine.add_drop_source(AudioInputDrops::new(self.mic.clone())),
            engine.add_drop_source(AudioInputDrops::new(self.desktop.clone())),
            engine.add_drop_source(AudioBusDrops::new(self.bus.clone())),
            engine.add_drop_source(FnDropSource::new(move || {
                output_packet_drops(
                    own.load(Ordering::Relaxed)
                        .saturating_add(link.load(Ordering::Relaxed)),
                )
            })),
        ];
        self.drop_guard = Some(DropGuard::new(engine.drop_ledger(), ids));
        self.ticker = engine.spawn_stats_ticker(Arc::clone(&self.stop));
        self.final_tick = Some(engine.stats_emitter());
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        join("render", self.render.take());
        join("mixer", self.mixer.take());
        // Finish after the producers have stopped pushing, so the output thread
        // can drain the remaining packets and then see the streams end.
        if let Err(err) = self.video.finish() {
            tracing::info!(error = %err, "video session finish failed");
        }
        if let Err(err) = self.audio.finish() {
            tracing::info!(error = %err, "audio session finish failed");
        }
        join("output", self.output.take());
        join("stats", self.ticker.take());
        // After the threads stop, so the latch sees their final counters.
        self.drop_guard.take();
        if let Some(tick) = self.final_tick.take() {
            tick();
        }
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
}

pub(crate) fn parts_from(backends: &Backends, draw: OpenDraw, open_timeout: Duration) -> Parts {
    Parts {
        clock: Arc::clone(&backends.clock),
        capture: Arc::clone(&backends.capture),
        audio: Arc::clone(&backends.audio),
        codecs: Arc::clone(&backends.codecs),
        draw,
        open_timeout,
    }
}

/// The wgpu compositor on the software adapter, reading every frame back to the CPU.
pub(crate) fn gpu_draw() -> OpenDraw {
    Arc::new(|| {
        let mut compositor =
            Compositor::open(OpenOptions::headless()).map_err(|err| render_open_error(&err))?;
        let draw: DrawFn = Box::new(move |size, rgba, out| {
            let drawn = match rgba {
                Some(rgba) => draw_image(&mut compositor, size, rgba, out),
                None => readback(&mut compositor, size, &[], out),
            };
            drawn.map_err(|err| err.to_string())
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

fn panic_message(panic: &(dyn Any + Send)) -> &str {
    if let Some(text) = panic.downcast_ref::<&str>() {
        text
    } else if let Some(text) = panic.downcast_ref::<String>() {
        text
    } else {
        "a non-string panic"
    }
}

/// Opens the devices and encoders, then the threads. On failure the publish
/// target is dropped; the host has to set another one.
pub(crate) fn launch(
    parts: Parts,
    publish: Box<dyn LivePublish>,
    output_id: OutputId,
    generation: u64,
    fail_tx: Sender<PipelineEvent>,
) -> Result<Running, String> {
    let (size, capture) = open_capture(&parts)?;
    let format = engine_format()?;
    let OpenedAudio {
        mixer,
        streams,
        mic,
        desktop,
    } = open_audio(&parts, format)?;
    let bus = mixer.bus();
    let period = frame_period(parts.clock.timebase())?;
    if period.ticks() <= 0 {
        return Err("the media clock is too coarse for the frame rate".into());
    }
    let video = Arc::new(open_video(&parts, size)?);
    let audio = Arc::new(open_audio_encoder(&parts, format)?);
    let capture_drops = DropSnapshot::new();
    let output_drops = Arc::new(AtomicU64::new(0));
    let link_drops = Arc::new(AtomicU64::new(0));
    // The mixer timestamps its first block `latency` before the clock, so the FLV
    // origin sits that far back. Later blocks and frames stay at or after it.
    let origin = stream_origin(parts.clock.as_ref(), &mixer)?;
    let tagger = tagger(size, origin)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stats = PipelineStats::new();
    let (ready_tx, ready_rx) = mpsc::channel();

    // Subscribe before any producer starts, so the first packets (the first video
    // keyframe among them) are not encoded before the output thread is listening.
    let video_sub = video.subscribe();
    let audio_sub = audio.subscribe();
    let render = spawn("jstream-render", {
        let job = RenderJob {
            capture,
            open: Arc::clone(&parts.draw),
            clock: Arc::clone(&parts.clock),
            input: video.input(),
            size,
            period,
            stop: Arc::clone(&stop),
            ready: ready_tx,
            stats: Arc::clone(&stats),
            drops: capture_drops.clone(),
            fail: fail_tx.clone(),
        };
        move || render_thread(job)
    })?;
    let mixer_thread = match spawn("jstream-mixer", {
        let input = audio.input();
        let clock = Arc::clone(&parts.clock);
        let stop = Arc::clone(&stop);
        let fail = fail_tx.clone();
        move || mixer_thread(mixer, clock, input, format, stop, fail)
    }) {
        Ok(handle) => handle,
        Err(err) => {
            stop_and_join(&stop, vec![render]);
            return Err(err);
        }
    };
    let output = match spawn("jstream-output", {
        let stats = Arc::clone(&stats);
        let own = Arc::clone(&output_drops);
        let link = Arc::clone(&link_drops);
        let stop = Arc::clone(&stop);
        move || {
            output_loop(OutputJob {
                publish,
                tagger,
                video: video_sub,
                audio: audio_sub,
                stats,
                output_drops: own,
                link_drops: link,
                stop,
                fail: fail_tx,
            });
        }
    }) {
        Ok(handle) => handle,
        Err(err) => {
            stop_and_join(&stop, vec![render, mixer_thread]);
            return Err(err);
        }
    };

    let running = Running {
        output_id,
        generation,
        stop,
        render: Some(render),
        mixer: Some(mixer_thread),
        output: Some(output),
        video,
        audio,
        _streams: streams,
        stats,
        capture_drops,
        mic,
        desktop,
        bus,
        output_drops,
        link_drops,
        drop_guard: None,
        ticker: None,
        final_tick: None,
    };
    match ready_rx.recv_timeout(parts.open_timeout) {
        Ok(Ok(())) => Ok(running),
        Ok(Err(reason)) => {
            drop(running);
            Err(reason)
        }
        Err(RecvTimeoutError::Timeout) => {
            drop(running);
            Err(format!(
                "the compositor did not open within {}s",
                parts.open_timeout.as_secs()
            ))
        }
        Err(RecvTimeoutError::Disconnected) => {
            drop(running);
            Err("the render thread stopped before the compositor opened".into())
        }
    }
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

fn open_video(parts: &Parts, size: FrameSize) -> Result<VideoSession, String> {
    let period =
        Timebase::new(FRAME_NUM, FRAME_DEN).map_err(|err| format!("frame period: {err}"))?;
    let config = VideoEncoderConfig {
        codec: Codec::H264,
        size,
        input_format: PixelFormat::Rgba8,
        frame_period: period,
        rate_control: RateControl::Cbr { kbps: VIDEO_KBPS },
        keyframe_interval: KEYFRAME_INTERVAL,
        hardware: HardwarePreference::Avoid,
    };
    VideoSession::open(parts.codecs.as_ref(), config, SessionOptions::default())
        .map_err(|err| format!("opening the video encoder: {err}"))
}

fn open_audio_encoder(parts: &Parts, format: AudioFormat) -> Result<AudioSession, String> {
    let config = AudioEncoderConfig {
        codec: Codec::Aac,
        input_format: format,
        kbps: AUDIO_KBPS,
    };
    AudioSession::open(parts.codecs.as_ref(), config, SessionOptions::default())
        .map_err(|err| format!("opening the audio encoder: {err}"))
}

fn stream_origin(clock: &dyn MediaClock, mixer: &Mixer) -> Result<MediaTime, String> {
    let config = mixer.config();
    let frames = i64::try_from(config.latency_frames)
        .map_err(|_| "mixer latency does not fit a timestamp".to_string())?;
    let sample =
        Timebase::per_second(config.sample_rate).map_err(|err| format!("mixer timebase: {err}"))?;
    let latency = MediaTime::new(frames, sample)
        .rescale(clock.timebase(), Rounding::Up)
        .map_err(|err| format!("mixer latency: {err}"))?;
    clock
        .now()
        .checked_sub(latency)
        .map_err(|err| format!("stream origin: {err}"))
}

fn tagger(size: FrameSize, origin: MediaTime) -> Result<FlvTagger, String> {
    let avc = AvcDecoderConfig::new(vec![SPS.to_vec()], vec![PPS.to_vec()])
        .map_err(|err| format!("AVC decoder config: {err}"))?;
    let aac = AudioSpecificConfig::new(AOT_AAC_LC, SAMPLE_RATE, 2)
        .map_err(|err| format!("AAC config: {err}"))?;
    let config = FlvConfig {
        video: Some(VideoTrack {
            width: size.width(),
            height: size.height(),
            frame_rate: Some(f64::from(FRAME_DEN) / f64::from(FRAME_NUM)),
            bitrate_kbps: Some(VIDEO_KBPS),
            avc: Some(avc),
        }),
        audio: Some(AudioTrack {
            config: aac,
            bitrate_kbps: Some(AUDIO_KBPS),
        }),
        origin: Some(origin),
        metadata_style: MetadataStyle::File,
        encoder: Some("jstream".into()),
    };
    FlvTagger::new(config).map_err(|err| format!("FLV tagger: {err}"))
}

struct RenderJob {
    capture: Box<dyn CaptureStream>,
    open: OpenDraw,
    clock: Arc<dyn MediaClock>,
    input: VideoInput,
    size: FrameSize,
    period: MediaTime,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<(), String>>,
    stats: Arc<PipelineStats>,
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
                if !rgba_from_surface(&surface, size, &mut self.source) && !self.warned_format {
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

    /// Composites into `scratch` and, on success, makes that the shown frame.
    fn draw(&mut self, draw: &mut DrawFn, size: FrameSize) -> Result<(), String> {
        let source = (!self.source.is_empty()).then_some(self.source.as_slice());
        draw(size, source, &mut self.scratch)?;
        std::mem::swap(&mut self.scratch, &mut self.shown);
        Ok(())
    }
}

/// Composites on every base-rate tick of the media clock.
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
fn render_loop(job: RenderJob) {
    let RenderJob {
        mut capture,
        open,
        clock,
        input,
        size,
        period,
        stop,
        ready,
        stats,
        drops,
        fail,
    } = job;
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
    let mut frames = Frames {
        source: Vec::new(),
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
                frames.draw(&mut draw, size)
            }));
            match tick {
                Ok(Ok(())) => {
                    draw_errors = 0;
                    stats.readbacks.fetch_add(1, Ordering::Relaxed);
                }
                Ok(Err(err)) => {
                    draw_errors += 1;
                    if draw_errors == 1 {
                        tracing::warn!(error = %err, "composite failed; repeating the last frame");
                    }
                    if draw_errors > MAX_DRAW_ERRORS {
                        broken = true;
                        report(
                            &fail,
                            format!("compositing failed {draw_errors} times in a row: {err}"),
                        );
                    }
                }
                Err(panic) => {
                    broken = true;
                    report(
                        &fail,
                        format!("the render thread panicked: {}", panic_message(&*panic)),
                    );
                }
            }
        }
        drops.publish(capture_drops(frames.capture).saturating_add(render_tick_drops(skipped)));
        if !frames.shown.is_empty() && !push_frame(&input, size, due, &frames.shown) {
            break;
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

fn draw_image(
    comp: &mut Compositor,
    size: FrameSize,
    rgba: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), RenderError> {
    let image = comp.upload_rgba8(size.width(), size.height(), rgba)?;
    let items = [Item {
        image: &image,
        placement: Placement::IDENTITY,
        crop: Crop::NONE,
        opacity: 1.0,
        visible: true,
    }];
    readback(comp, size, &items, out)
}

/// Renders `items` and copies the readback into `out`, reusing its allocation.
fn readback(
    comp: &mut Compositor,
    size: FrameSize,
    items: &[Item<'_>],
    out: &mut Vec<u8>,
) -> Result<(), RenderError> {
    let scene = Scene {
        canvas: size,
        background: Color::BLACK,
        items,
    };
    let output = comp.render(&scene, true)?;
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
/// allocation a tick makes: `VideoInput::push` has no way to hand buffers back.
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

/// The mixer thread's boundary: a panic is caught and reported as fatal. The encoder
/// then gets no more audio, so the output cannot keep running.
fn mixer_thread(
    mixer: Mixer,
    clock: Arc<dyn MediaClock>,
    input: AudioInput,
    format: AudioFormat,
    stop: Arc<AtomicBool>,
    fail: Sender<PipelineEvent>,
) {
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        mixer_loop(mixer, clock.as_ref(), &input, format, &stop, &fail);
    }));
    if let Err(panic) = result {
        report(
            &fail,
            format!("the mixer thread panicked: {}", panic_message(&*panic)),
        );
    }
}

/// Mixes on the media clock. A rejected block or a poll error is fatal: both mean
/// the mixer and the encoder disagree about the format, which no retry fixes.
fn mixer_loop(
    mut mixer: Mixer,
    clock: &dyn MediaClock,
    input: &AudioInput,
    format: AudioFormat,
    stop: &AtomicBool,
    fail: &Sender<PipelineEvent>,
) {
    let samples_len = mixer.config().block_frames.saturating_mul(2);
    let mut samples = vec![0.0f32; samples_len];
    while !stop.load(Ordering::Relaxed) {
        match mixer.poll(clock.now(), &mut samples) {
            Ok(Some(block)) => {
                // The mix itself does not allocate. `AudioInput::push` takes an owned
                // buffer and keeps it, so each block needs its own: this clone is
                // forced by that API.
                let buffer = match AudioBuffer::new(format, block.pts, samples.clone()) {
                    Ok(buffer) => buffer,
                    Err(err) => {
                        report(fail, format!("mixer block rejected: {err}"));
                        break;
                    }
                };
                // `Full` is counted by the session. `Closed` means the encoder ended,
                // which the output thread reports.
                if matches!(input.push(buffer), Err(PushError::Closed)) {
                    break;
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
    publish: Box<dyn LivePublish>,
    tagger: FlvTagger,
    video: Subscription,
    audio: Subscription,
    stats: Arc<PipelineStats>,
    output_drops: Arc<AtomicU64>,
    link_drops: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    fail: Sender<PipelineEvent>,
}

/// Muxes both encoder streams and publishes them. A publish error, or an encoder
/// stream that ends before the pipeline was told to stop (the encoder failed, or its
/// session closed), is fatal and goes to the failure channel. A lost link is not an
/// error: the publish target drops and counts tags while it reconnects, and this
/// loop reports each change of its [`LinkState`] on the same channel.
///
/// Once it sees the stop flag, the loop keeps sending what the encoders queued for at
/// most [`STOP_DRAIN`], and tells the publish target that deadline. When the deadline
/// passes, or a send fails while stopping, it drops the packets still queued, counts
/// them in `output_drops`, and finishes the target, which past the deadline closes
/// without its goodbye. A failure while stopping is logged, not reported: the output
/// is already on its way to `stopped`.
fn output_loop(job: OutputJob) {
    let OutputJob {
        mut publish,
        mut tagger,
        video,
        audio,
        stats,
        output_drops,
        link_drops,
        stop,
        fail: fail_tx,
    } = job;
    let started = match tagger.start() {
        Ok(tags) => publish_tags(&mut publish, &tags),
        Err(err) => Err(format!("FLV start: {err}")),
    };
    if let Err(err) = started {
        report(&fail_tx, err);
        if let Err(err) = publish.finish() {
            tracing::error!(error = %err, "publish finish failed");
        }
        return;
    }
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
            let deadline = Instant::now() + STOP_DRAIN;
            publish.stop_by(deadline);
            drain = Some(deadline);
        }
        if drain.is_some_and(|deadline| Instant::now() >= deadline) {
            abandoned = true;
            break;
        }
        if !video_done {
            match video.recv_timeout(Duration::from_millis(2)) {
                Ok(Some(packet)) => {
                    if let Err(err) = forward(&mut publish, &mut tagger, &packet, &output_drops) {
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
                    if let Err(err) = forward(&mut publish, &mut tagger, &packet, &output_drops) {
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
            && let Err(err) = publish.poll()
        {
            failed = Some(format!("publish poll failed: {err}"));
        }
        if failed.is_none() {
            link = report_link(publish.as_ref(), link, &fail_tx);
        }
        link_drops.store(publish.dropped(), Ordering::Relaxed);
    }
    if let Some(err) = failed {
        if stop.load(Ordering::Acquire) {
            tracing::warn!(error = %err, "publishing failed while stopping");
            abandoned = true;
        } else {
            report(&fail_tx, err);
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
    link_drops.store(publish.dropped(), Ordering::Relaxed);
    if let Err(err) = publish.finish() {
        tracing::error!(error = %err, "publish finish failed");
    }
}

/// Takes and counts the packets queued on `sub` without sending them. Nothing when
/// the stream has already ended (`done`).
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
fn report_link(
    publish: &dyn LivePublish,
    last: LinkState,
    fail: &Sender<PipelineEvent>,
) -> LinkState {
    let now = publish.link_state();
    let event = match (last, now) {
        (LinkState::Active, LinkState::Reconnecting) => PipelineEvent::LinkDown,
        (LinkState::Reconnecting, LinkState::Active) => PipelineEvent::LinkUp,
        _ => return now,
    };
    tracing::info!(link = ?now, "publish link changed");
    // The receiver is gone once the pipeline is being torn down; nothing to tell.
    let _ = fail.send(event);
    now
}

/// Muxes one packet and sends its tags. A packet the muxer rejects is dropped. A
/// destination that stops accepting tags ends the output loop.
fn forward(
    publish: &mut Box<dyn LivePublish>,
    tagger: &mut FlvTagger,
    packet: &jstream_media::Packet,
    output_drops: &AtomicU64,
) -> Result<(), String> {
    match tagger.push(packet) {
        Ok(tags) => publish_tags(publish, &tags),
        Err(err) => {
            output_drops.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(error = %err, "dropping a packet the muxer rejected");
            Ok(())
        }
    }
}

fn publish_tags(
    publish: &mut Box<dyn LivePublish>,
    tags: &[jstream_mux::flv::FlvTag],
) -> Result<(), String> {
    for tag in tags {
        publish
            .send_tag(tag)
            .map_err(|err| format!("sending an FLV tag: {err}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::catch_up;
    use jstream_media::{MediaTime, Timebase};

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
