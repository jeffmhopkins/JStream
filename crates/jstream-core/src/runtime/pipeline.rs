//! Starts and stops one output's threads.
//!
//! Capture is the platform's mailbox, read by the render thread. The render thread
//! composites one scene and, because the fake GPU interop has no encode target,
//! reads the frame back and pushes CPU RGBA. The mixer thread polls the engine
//! clock. Encode threads live inside the encoder sessions. The output thread muxes
//! FLV and publishes it. None of those threads lock the engine's collection.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use jstream_audio::{Mixer, MixerConfig};
use jstream_encode::{
    AudioInput, AudioSession, PushError, SessionOptions, SessionStats, SubscriberStats,
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

use crate::lock;
use crate::runtime::LivePublish;

/// Base frame rate of this cut: one frame every 1/30 s.
const FRAME_NUM: u32 = 1;
const FRAME_DEN: u32 = 30;
/// Two seconds of frames between keyframes.
const KEYFRAME_INTERVAL: u32 = 60;
const VIDEO_KBPS: u32 = 2500;
const AUDIO_KBPS: u32 = 160;
const SAMPLE_RATE: u32 = 48_000;
/// How long `output.start` waits for the software adapter.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

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
    video: VideoSession,
    audio: AudioSession,
    /// Kept alive so the platform keeps calling the mixer producers.
    _streams: Vec<Box<dyn AudioStream>>,
    stats: Arc<PipelineStats>,
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
}

pub(crate) fn parts_from(backends: &Backends) -> Parts {
    Parts {
        clock: Arc::clone(&backends.clock),
        capture: Arc::clone(&backends.capture),
        audio: Arc::clone(&backends.audio),
        codecs: Arc::clone(&backends.codecs),
    }
}

/// Opens the devices and encoders, then the threads. On failure the publish
/// target is dropped; the host has to set another one.
pub(crate) fn launch(
    parts: Parts,
    publish: Box<dyn LivePublish>,
    output_id: OutputId,
    generation: u64,
    fail_tx: Sender<String>,
) -> Result<Running, String> {
    let (size, capture) = open_capture(&parts)?;
    let format = engine_format()?;
    let (mixer, streams) = open_audio(&parts, format)?;
    let period = frame_period(parts.clock.timebase())?;
    let video = open_video(&parts, size)?;
    let audio = open_audio_encoder(&parts, format)?;
    // The mixer timestamps its first block `latency` before the clock, so the FLV
    // origin sits that far back. Later blocks and frames stay at or after it.
    let origin = stream_origin(parts.clock.as_ref(), &mixer)?;
    let tagger = tagger(size, origin)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stats = PipelineStats::new();
    let (ready_tx, ready_rx) = mpsc::channel();

    let render = spawn("jstream-render", {
        let input = video.input();
        let clock = Arc::clone(&parts.clock);
        let stop = Arc::clone(&stop);
        let readbacks = Arc::clone(&stats);
        move || {
            render_loop(RenderJob {
                capture,
                clock,
                input,
                size,
                period,
                stop,
                ready: ready_tx,
                stats: readbacks,
            });
        }
    })?;
    let mixer_thread = match spawn("jstream-mixer", {
        let input = audio.input();
        let clock = Arc::clone(&parts.clock);
        let stop = Arc::clone(&stop);
        move || mixer_loop(mixer, clock, input, format, stop)
    }) {
        Ok(handle) => handle,
        Err(err) => {
            stop_and_join(&stop, vec![render]);
            return Err(err);
        }
    };
    let output = match spawn("jstream-output", {
        let video_sub = video.subscribe();
        let audio_sub = audio.subscribe();
        let stats = Arc::clone(&stats);
        move || output_loop(publish, tagger, video_sub, audio_sub, stats, fail_tx)
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
    };
    match ready_rx.recv_timeout(OPEN_TIMEOUT) {
        Ok(Ok(())) => Ok(running),
        Ok(Err(reason)) => {
            drop(running);
            Err(reason)
        }
        Err(RecvTimeoutError::Timeout) => {
            drop(running);
            Err("the compositor did not open within 10s".into())
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

fn open_audio(
    parts: &Parts,
    format: AudioFormat,
) -> Result<(Mixer, Vec<Box<dyn AudioStream>>), String> {
    let mut mixer = Mixer::new(MixerConfig::default()).map_err(|err| format!("mixer: {err}"))?;
    let (mic, _) = mixer
        .add_input(format)
        .map_err(|err| format!("microphone input: {err}"))?;
    let (desktop, _) = mixer
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
    Ok((mixer, vec![mic, desktop]))
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
    clock: Arc<dyn MediaClock>,
    input: VideoInput,
    size: FrameSize,
    period: MediaTime,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<(), String>>,
    stats: Arc<PipelineStats>,
}

fn render_loop(job: RenderJob) {
    let RenderJob {
        mut capture,
        clock,
        input,
        size,
        period,
        stop,
        ready,
        stats,
    } = job;
    let mut compositor = match Compositor::open(OpenOptions::headless()) {
        Ok(compositor) => compositor,
        Err(err) => {
            let _ = ready.send(Err(render_open_error(&err)));
            return;
        }
    };
    if ready.send(Ok(())).is_err() {
        return;
    }
    let mut last: Option<Vec<u8>> = None;
    let mut next: Option<MediaTime> = None;
    let mut warned = false;
    while !stop.load(Ordering::Relaxed) {
        let now = clock.now();
        let due = next.unwrap_or(now);
        if now < due {
            thread::sleep(Duration::from_millis(1));
            continue;
        }
        match capture.latest() {
            Ok(Some(surface)) => match rgba_from_surface(&surface, size) {
                Some(rgba) => last = Some(rgba),
                None if !warned => {
                    warned = true;
                    tracing::warn!("capture frame is not CPU RGBA or BGRA at the encoder size");
                }
                None => {}
            },
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(error = %err, "capture stream failed");
                break;
            }
        }
        let drawn = match last.as_deref() {
            Some(rgba) => draw_image(&mut compositor, size, rgba),
            None => draw_black(&mut compositor, size),
        };
        let pixels = match drawn {
            Ok(pixels) => pixels,
            Err(err) => {
                tracing::warn!(error = %err, "composite failed");
                break;
            }
        };
        stats.readbacks.fetch_add(1, Ordering::Relaxed);
        if !push_frame(&input, size, due, &pixels) {
            break;
        }
        next = match due.checked_add(period) {
            Ok(next) => Some(next),
            Err(err) => {
                tracing::warn!(error = %err, "frame time overflow");
                break;
            }
        };
    }
}

fn render_open_error(err: &RenderError) -> String {
    if err.is_no_adapter() {
        err.to_string()
    } else {
        format!("opening the compositor: {err}")
    }
}

fn rgba_from_surface(surface: &VideoSurface, expected: FrameSize) -> Option<Vec<u8>> {
    let VideoSurface::Cpu(frame) = surface else {
        return None;
    };
    let info = frame.info();
    if info.size != expected {
        return None;
    }
    let width = info.size.width() as usize;
    let height = info.size.height() as usize;
    let mut out = vec![0u8; width.checked_mul(height)?.checked_mul(4)?];
    match info.format {
        PixelFormat::Rgba8 => {
            for y in 0..height {
                let row = frame.row(0, y as u32)?;
                let start = y * width * 4;
                let n = (width * 4).min(row.len());
                out[start..start + n].copy_from_slice(&row[..n]);
            }
        }
        PixelFormat::Bgra8 => {
            for y in 0..height {
                let row = frame.row(0, y as u32)?;
                for x in 0..width {
                    let i = x * 4;
                    if i + 3 >= row.len() {
                        return None;
                    }
                    let o = (y * width + x) * 4;
                    out[o] = row[i + 2];
                    out[o + 1] = row[i + 1];
                    out[o + 2] = row[i];
                    out[o + 3] = row[i + 3];
                }
            }
        }
        _ => return None,
    }
    Some(out)
}

fn draw_image(comp: &mut Compositor, size: FrameSize, rgba: &[u8]) -> Result<Vec<u8>, RenderError> {
    let image = comp.upload_rgba8(size.width(), size.height(), rgba)?;
    let items = [Item {
        image: &image,
        placement: Placement::IDENTITY,
        crop: Crop::NONE,
        opacity: 1.0,
        visible: true,
    }];
    readback(comp, size, &items)
}

fn draw_black(comp: &mut Compositor, size: FrameSize) -> Result<Vec<u8>, RenderError> {
    readback(comp, size, &[])
}

fn readback(
    comp: &mut Compositor,
    size: FrameSize,
    items: &[Item<'_>],
) -> Result<Vec<u8>, RenderError> {
    let scene = Scene {
        canvas: size,
        background: Color::BLACK,
        items,
    };
    let output = comp.render(&scene, true)?;
    match output.pixels() {
        Some(snapshot) => Ok(snapshot.rgba().to_vec()),
        None => Err(RenderError::Readback {
            detail: "the compositor returned no pixels".into(),
        }),
    }
}

/// Pushes one frame. `false` when the session has closed.
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

fn mixer_loop(
    mut mixer: Mixer,
    clock: Arc<dyn MediaClock>,
    input: AudioInput,
    format: AudioFormat,
    stop: Arc<AtomicBool>,
) {
    let samples_len = mixer.config().block_frames.saturating_mul(2);
    let mut samples = vec![0.0f32; samples_len];
    while !stop.load(Ordering::Relaxed) {
        match mixer.poll(clock.now(), &mut samples) {
            Ok(Some(block)) => {
                let buffer = match AudioBuffer::new(format, block.pts, samples.clone()) {
                    Ok(buffer) => buffer,
                    Err(err) => {
                        tracing::error!(error = %err, "mixer block rejected");
                        break;
                    }
                };
                // `Full` is counted by the session. The mix itself did not allocate;
                // the encoder queue takes an owned buffer.
                if matches!(input.push(buffer), Err(PushError::Closed)) {
                    break;
                }
            }
            Ok(None) => thread::sleep(Duration::from_millis(1)),
            Err(err) => {
                tracing::error!(error = %err, "mixer poll failed");
                break;
            }
        }
    }
}

fn output_loop(
    mut publish: Box<dyn LivePublish>,
    mut tagger: FlvTagger,
    video: Subscription,
    audio: Subscription,
    stats: Arc<PipelineStats>,
    fail_tx: Sender<String>,
) {
    let started = match tagger.start() {
        Ok(tags) => publish_tags(&mut publish, &tags),
        Err(err) => Err(format!("FLV start: {err}")),
    };
    if let Err(err) = started {
        tracing::error!(error = %err, "FLV start failed");
        let _ = fail_tx.send(err);
        return;
    }
    let mut video_done = false;
    let mut audio_done = false;
    let mut failed: Option<String> = None;
    while failed.is_none() && (!video_done || !audio_done) {
        if !video_done {
            match video.recv_timeout(Duration::from_millis(2)) {
                Ok(Some(packet)) => {
                    if let Err(err) = forward(&mut publish, &mut tagger, &packet) {
                        tracing::error!(error = %err, "publish failed");
                        failed = Some(err);
                    }
                }
                Ok(None) => {}
                Err(end) => {
                    tracing::info!(%end, "video subscription ended");
                    video_done = true;
                }
            }
        }
        if failed.is_none() && !audio_done {
            match audio.recv_timeout(Duration::from_millis(2)) {
                Ok(Some(packet)) => {
                    if let Err(err) = forward(&mut publish, &mut tagger, &packet) {
                        tracing::error!(error = %err, "publish failed");
                        failed = Some(err);
                    }
                }
                Ok(None) => {}
                Err(end) => {
                    tracing::info!(%end, "audio subscription ended");
                    audio_done = true;
                }
            }
        }
        *lock(&stats.video_subscriber) = video.stats();
        *lock(&stats.audio_subscriber) = audio.stats();
        if failed.is_none()
            && let Err(err) = publish.poll()
        {
            tracing::error!(error = %err, "publish poll failed");
            failed = Some(err.to_string());
        }
    }
    if let Some(err) = failed {
        let _ = fail_tx.send(err);
    }
    if let Err(err) = publish.finish() {
        tracing::error!(error = %err, "publish finish failed");
    }
}

/// Muxes one packet and sends its tags. A packet the muxer rejects is dropped. A
/// destination that stops accepting tags ends the output loop.
fn forward(
    publish: &mut Box<dyn LivePublish>,
    tagger: &mut FlvTagger,
    packet: &jstream_media::Packet,
) -> Result<(), String> {
    match tagger.push(packet) {
        Ok(tags) => publish_tags(publish, &tags),
        Err(err) => {
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
