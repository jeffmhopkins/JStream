//! Preview streams (ADR-0045): the program mix, or one scene, as low-resolution
//! pictures, sent to the session that subscribed as binary frames
//! ([`jstream_protocol::BinaryFrame`]).
//!
//! **Scenes.** A stream of a scene draws that scene's own render snapshot, which the
//! engine plans beside the program mix's for every scene a live stream shows
//! ([`PreviewHub::scenes`]), after the program's plan has ended so it never makes a
//! source shown or heard. The program stage neither draws nor captures anything for
//! it; the hub captures its displays and windows (**Captures**). Until the engine has planned it the stream waits; once
//! the scene is deleted the engine ends it ([`PreviewHub::end_scene`]).
//!
//! **Sessions.** Each [`InProcessControl`](crate::InProcessControl) holds a
//! [`SessionPreviews`]: its binary channel (a bounded queue, read through
//! `Control::binary`) and its open streams by channel. `preview.subscribe` checks the
//! request, picks the codec and registers the stream with the engine's [`PreviewHub`];
//! `preview.unsubscribe` and the session's end drop it.
//!
//! **The hub.** One `jstream-preview` thread per engine, started by the first
//! subscription and ended when none is left, serves every stream. It owns its own
//! compositor, like `video.snapshot`'s thread, and never runs on the render thread or
//! takes part in the program stage, so a preview costs an output nothing. On each pass
//! it reads the published render snapshot and `rev` and the live values on the render
//! parameter channel (ADR-0032), and for each stream that is due draws the shot with
//! them at the stream's size, reads it back and encodes it. A stream is due at most
//! `fps` times a second, when the snapshot or the live values changed, when a keyframe was asked
//! for, or [`KEYFRAME_EVERY`] after its last keyframe, so a reader that lost frames
//! recovers and a still scene costs almost nothing.
//!
//! **Codecs.** `preview.subscribe` picks the first codec the client accepts that the
//! engine has: H.264 when the platform lists an H.264 encoder, JPEG always. H.264 runs
//! on a `jstream-encode` [`VideoSession`] of the platform's software encoder (so a
//! preview never takes a hardware session from an output), opened on the hub thread,
//! and reopened when the picture size changes; if it can't be opened, the stream falls
//! back to JPEG when the client accepts it, and ends otherwise. Every frame names its
//! codec. JPEG is this module's own baseline encoder ([`jpeg`]). The encoders and
//! pacing live on the hub thread alone ([`Work`]), so a stream shared with its session
//! holds only flags and its settings.
//!
//! **Captures.** The hub captures the displays and windows the program mix and each
//! streamed scene show itself,
//! on the CPU path, through the engine's capture backend ([`PreviewCapture`]), with the
//! program stage's [`RenderCaptures`] on a `jstream-capture` thread of its own, so a
//! display shows in the preview with no output running. While an output runs, its stage
//! captures the same display again: each capture belongs to one consumer. A webcam or
//! capture card opens once, so the preview leaves it to the program stage and draws it
//! as nothing, as `video.snapshot` draws every capture. The program's captures run only
//! while a stream shows the program. A stream that shows a capture is due when one of
//! the captures it shows took a new frame since its last picture.
//!
//! **Overload.** The hub never waits on a reader: the binary channel holds
//! [`BINARY_BUFFER`] frames, and a [`KeyframeGate`] drops the rest of a stream's
//! frames until a keyframe finds room, which the hub then forces. A stream whose reader
//! has no room is not drawn at all.

mod jpeg;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::task::{Context, Poll, Waker};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use futures_core::Stream;
use jstream_encode::{SessionOptions, Subscription, VideoInput, VideoSession};
use jstream_media::{
    Codec, ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, Plane, Rounding,
    Timebase, VideoFrame, VideoFrameInfo,
};
use jstream_platform::{
    CodecRole, Codecs, HardwarePreference, RateControl, VideoEncoderConfig, VideoSurface,
};
use jstream_protocol::{
    BinaryFrame, BinaryHeader, KeyframeGate, PreviewCodec, PreviewSettings, ProtocolError, Rev,
    SceneId, StreamKind,
};
use jstream_render::{Compositor, OpenOptions, ResourceCache};

use jstream_platform::Capture;
use jstream_sources::CaptureSpec;

use crate::chain::Chains;
use crate::lock;
use crate::runtime::{CaptureBackends, RenderCaptures, RenderSnapshot, spawn_captures};
use crate::snapshot::{self, CaptureKey, Captures, GpuKey, LiveParams, NoCaptures, Shot};

/// How many frames a session's binary channel holds for a reader that falls behind.
pub(crate) const BINARY_BUFFER: usize = 8;

/// The longest a stream goes without a keyframe, even when nothing changes.
pub(crate) const KEYFRAME_EVERY: Duration = Duration::from_secs(2);

/// JPEG quality, 1 to 100.
const JPEG_QUALITY: u8 = 75;

/// Draw errors in a row that a stream rides out by skipping the picture. One more
/// ends it.
const MAX_DRAW_ERRORS: u32 = 30;

/// The longest the hub sleeps between passes while streams are open.
const MAX_SLEEP: Duration = Duration::from_millis(100);

/// How long the hub waits for the encoder's packet after pushing a picture, before it
/// moves on to the other streams (the packet then goes out on the next pass).
const PACKET_WAIT: Duration = Duration::from_millis(20);

/// What the hub reads from the engine on every pass.
pub(crate) struct Current {
    /// The render snapshot published now: the program mix.
    pub(crate) program: Arc<RenderSnapshot>,
    /// What each scene with an open stream draws, once the engine has planned it.
    pub(crate) scenes: Arc<BTreeMap<SceneId, Arc<RenderSnapshot>>>,
    /// The live values (ADR-0032) on the render parameter channel now.
    pub(crate) live: Arc<LiveParams>,
    /// The `rev` the snapshots were published at.
    pub(crate) rev: Rev,
    /// The media clock now.
    pub(crate) at: MediaTime,
}

/// What the hub reads from the engine.
pub(crate) trait PreviewSource: Send + Sync {
    /// The snapshots, live values, `rev` and clock now.
    fn current(&self) -> Current;
}

/// Draws one picture for the hub: `shot` with `live` applied and the latest frame of
/// each display and window it shows from `captures`, at `size` and media time `at` (a
/// running transition samples it), read back as straight RGBA8 into `out` (replacing
/// what it held). Opened on the hub thread and kept there.
pub(crate) type PreviewDraw = Box<
    dyn FnMut(
        &Shot,
        &LiveParams,
        &dyn Captures,
        FrameSize,
        MediaTime,
        &mut Vec<u8>,
    ) -> Result<(), String>,
>;

/// Opens the hub's [`PreviewDraw`], on the hub thread.
pub(crate) type OpenPreviewDraw = Arc<dyn Fn() -> Result<PreviewDraw, String> + Send + Sync>;

/// The hub's draw: its own wgpu compositor, hardware adapter first with the software one
/// as the fallback (`OpenOptions::hardware`), drawing the program mix with
/// [`snapshot::compose`] and the hub's captures.
pub(crate) fn gpu_draw() -> OpenPreviewDraw {
    Arc::new(|| {
        let mut compositor =
            Compositor::open(OpenOptions::hardware()).map_err(|err| err.to_string())?;
        tracing::info!(
            adapter = %compositor.adapter_description(),
            "preview compositor opened"
        );
        let mut cache = ResourceCache::<GpuKey>::new();
        let mut chains = Chains::default();
        let draw: PreviewDraw = Box::new(move |shot, live, captures, size, at, out| {
            cache.begin_frame();
            let output = snapshot::compose(
                &mut compositor,
                (&mut cache, &mut chains),
                (shot, at),
                // A preview stream shows the program mix, never a video mix.
                snapshot::View::default(),
                live,
                captures,
                snapshot::DrawAt::read(size),
            );
            drop(cache.end_frame(&compositor));
            let output = output.map_err(|err| err.to_string())?;
            let pixels = output
                .pixels()
                .ok_or("the compositor returned no pixels")?
                .rgba();
            out.clear();
            out.extend_from_slice(pixels);
            Ok(())
        });
        Ok(draw)
    })
}

/// A CPU stand-in for [`gpu_draw`] that draws every picture black, for tests on a
/// machine with no adapter ([`Engine::with_cpu_compositor`](crate::Engine::with_cpu_compositor)).
pub(crate) fn cpu_draw() -> OpenPreviewDraw {
    Arc::new(|| {
        let draw: PreviewDraw = Box::new(|_, _, _, size, _, out| {
            out.clear();
            out.resize(size.width() as usize * size.height() as usize * 4, 0);
            Ok(())
        });
        Ok(draw)
    })
}

/// The frames waiting in one session's binary channel.
#[derive(Default)]
pub(crate) struct BinaryQueue {
    frames: VecDeque<BinaryFrame>,
    gate: KeyframeGate,
    waker: Option<Waker>,
    closed: bool,
}

impl BinaryQueue {
    /// Whether a frame would find room.
    fn has_room(&self) -> bool {
        !self.closed && self.frames.len() < BINARY_BUFFER
    }

    /// Queues `frame` unless the gate drops it. Returns whether its channel now waits
    /// for a keyframe (the frame or one before it was dropped), and the waker to wake.
    fn push(&mut self, frame: BinaryFrame) -> (bool, Option<Waker>) {
        if self.closed {
            return (false, None);
        }
        let channel = frame.header.channel;
        let room = self.frames.len() < BINARY_BUFFER;
        let waker = if self.gate.admit(&frame.header, room) {
            self.frames.push_back(frame);
            self.waker.take()
        } else {
            None
        };
        (self.gate.is_waiting(channel), waker)
    }

    fn close(&mut self) -> Option<Waker> {
        self.closed = true;
        self.waker.take()
    }
}

/// A shared [`BinaryQueue`].
type Sink = Arc<Mutex<BinaryQueue>>;

/// Wakes `waker`, if there is one, with no lock held.
fn wake(waker: Option<Waker>) {
    if let Some(waker) = waker {
        waker.wake();
    }
}

/// A session's binary channel, the stream behind `Control::binary`.
pub(crate) struct BinaryChannel(Sink);

impl Stream for BinaryChannel {
    type Item = BinaryFrame;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<BinaryFrame>> {
        let mut queue = lock(&self.0);
        if let Some(frame) = queue.frames.pop_front() {
            Poll::Ready(Some(frame))
        } else if queue.closed {
            Poll::Ready(None)
        } else {
            queue.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

/// What a subscription needs from the engine.
pub(crate) struct Opening {
    /// The platform's encoders, when the engine has backends.
    pub(crate) codecs: Option<Arc<dyn Codecs>>,
    /// The rev now, for an end frame sent before any picture.
    pub(crate) rev: Rev,
    /// The scene the stream shows, or `None` for the program mix.
    pub(crate) scene: Option<SceneId>,
}

/// One session's binary channel and preview streams.
#[derive(Default)]
pub(crate) struct SessionPreviews {
    binary: Mutex<Option<Sink>>,
    streams: Mutex<BTreeMap<u32, Arc<PreviewStream>>>,
}

impl SessionPreviews {
    /// Opens the binary channel, ending the one opened before and the streams on it.
    pub(crate) fn open_binary(&self) -> BinaryChannel {
        let sink: Sink = Arc::default();
        let old = lock(&self.binary).replace(Arc::clone(&sink));
        self.drop_streams();
        if let Some(old) = old {
            wake(lock(&old).close());
        }
        BinaryChannel(sink)
    }

    /// `preview.subscribe`: picks the first of the settings' codecs that can be opened
    /// and hands the stream to `hub`.
    pub(crate) fn subscribe(
        &self,
        hub: &PreviewHub,
        source: Weak<dyn PreviewSource>,
        channel: u32,
        settings: PreviewSettings,
        opening: Opening,
    ) -> Result<(), ProtocolError> {
        let Some(sink) = lock(&self.binary).clone() else {
            return Err(ProtocolError::ActionFailed {
                reason: "this session has no binary channel to send preview frames on".into(),
            });
        };
        let has_h264 = opening.codecs.as_ref().is_some_and(|codecs| {
            codecs.list().is_ok_and(|list| {
                list.iter()
                    .any(|info| info.codec == Codec::H264 && info.role == CodecRole::VideoEncoder)
            })
        });
        let Some(codec) = settings.codecs.iter().copied().find(|&codec| match codec {
            PreviewCodec::H264 => has_h264,
            PreviewCodec::Jpeg => true,
            _ => false,
        }) else {
            return Err(ProtocolError::ActionFailed {
                reason: "this engine can encode none of the requested preview codecs".into(),
            });
        };
        let stream = Arc::new(PreviewStream {
            id: hub.next_id(),
            channel,
            codec,
            settings,
            scene: opening.scene,
            codecs: opening.codecs,
            sink,
            keyframe: AtomicBool::new(true),
            closed: AtomicBool::new(false),
            ended: AtomicBool::new(false),
        });
        {
            // The checks and the insert under one lock, so two subscriptions in
            // flight can't both pass them.
            let mut streams = lock(&self.streams);
            streams.retain(|_, stream| !stream.ended.load(Ordering::Acquire));
            if streams.contains_key(&channel) {
                return Err(ProtocolError::invalid_argument(
                    "channel",
                    format!("this session already has preview channel {channel} open"),
                ));
            }
            if streams.len() >= PreviewSettings::MAX_PER_SESSION {
                return Err(ProtocolError::invalid_argument(
                    "channel",
                    format!(
                        "this session already has {} preview streams open",
                        PreviewSettings::MAX_PER_SESSION
                    ),
                ));
            }
            streams.insert(channel, Arc::clone(&stream));
        }
        tracing::debug!(channel, codec = ?codec, "preview stream opened");
        if let Err(reason) = hub.add(&stream, source) {
            lock(&self.streams).remove(&channel);
            stream.end(opening.rev, &reason);
            return Err(ProtocolError::Internal { reason });
        }
        Ok(())
    }

    /// `preview.unsubscribe`. Nothing more is sent on `channel` once this returns,
    /// except a frame the hub is already queueing. Returns whether the stream it closed
    /// showed a scene.
    pub(crate) fn unsubscribe(&self, channel: u32) -> bool {
        let Some(stream) = lock(&self.streams).remove(&channel) else {
            return false;
        };
        stream.closed.store(true, Ordering::Release);
        tracing::debug!(channel, "preview stream closed");
        stream.scene.is_some()
    }

    /// Ends every stream and the binary channel: the session is gone. Returns whether
    /// any stream it closed showed a scene.
    pub(crate) fn close(&self) -> bool {
        let scenes = self.drop_streams();
        if let Some(sink) = lock(&self.binary).take() {
            wake(lock(&sink).close());
        }
        scenes
    }

    fn drop_streams(&self) -> bool {
        let streams = std::mem::take(&mut *lock(&self.streams));
        let mut scenes = false;
        for stream in streams.into_values() {
            stream.closed.store(true, Ordering::Release);
            scenes |= stream.scene.is_some();
        }
        scenes
    }
}

/// One preview stream, shared by its session (which owns it) and the hub (which holds
/// it weakly).
pub(crate) struct PreviewStream {
    /// Unique per engine: the hub keeps the stream's [`Work`] under it.
    id: u64,
    channel: u32,
    /// The codec chosen at subscription; the hub may fall back from it ([`Work::codec`]).
    codec: PreviewCodec,
    settings: PreviewSettings,
    /// The scene it shows, or `None` for the program mix.
    scene: Option<SceneId>,
    /// The platform's encoders, when the engine has backends.
    codecs: Option<Arc<dyn Codecs>>,
    sink: Sink,
    /// The next picture must be a keyframe.
    keyframe: AtomicBool,
    /// Unsubscribed, or its session ended: send nothing more.
    closed: AtomicBool,
    /// The engine ended it and sent its end frame.
    ended: AtomicBool,
}

impl PreviewStream {
    /// Sends the end frame with `reason` and marks the stream ended.
    fn end(&self, rev: Rev, reason: &str) {
        if self.ended.swap(true, Ordering::AcqRel) {
            return;
        }
        tracing::info!(channel = self.channel, reason, "preview stream ended");
        if !self.closed.load(Ordering::Acquire) {
            let frame = BinaryFrame::end(StreamKind::Preview, self.channel, rev, reason);
            let (_, waker) = lock(&self.sink).push(frame);
            wake(waker);
        }
    }

    /// Queues a picture. Asks for a keyframe when the channel lost one.
    fn send(&self, frame: BinaryFrame) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let (waiting, waker) = lock(&self.sink).push(frame);
        wake(waker);
        if waiting {
            self.keyframe.store(true, Ordering::Release);
        }
    }

    fn live(&self) -> bool {
        !self.closed.load(Ordering::Acquire) && !self.ended.load(Ordering::Acquire)
    }
}

/// A stream's encoder and pacing, kept on the hub thread.
struct Work {
    /// The codec its pictures use now.
    codec: PreviewCodec,
    /// The H.264 session, for an H.264 stream; reopened when the picture size changes.
    h264: Option<H264>,
    started: Instant,
    /// The earliest the next picture may be drawn.
    next_at: Instant,
    last_key: Option<Instant>,
    /// The render snapshot and live values the last picture showed.
    shown: Option<(Arc<RenderSnapshot>, Arc<LiveParams>)>,
    draw_errors: u32,
    /// The newest capture frame version the last picture drew
    /// ([`HubCaptures::frames`]).
    frames: u64,
}

/// An H.264 stream's encoder session, its input and its packets.
struct H264 {
    /// Kept so the session (and its encode thread) lives as long as the stream.
    session: VideoSession,
    input: VideoInput,
    packets: Subscription,
    size: FrameSize,
    /// The presentation time (µs) and `rev` of each picture pushed and not yet seen
    /// back as a packet.
    pending: VecDeque<(i64, Rev)>,
}

impl Work {
    fn new(stream: &PreviewStream, now: Instant) -> Self {
        Self {
            codec: stream.codec,
            h264: None,
            started: now,
            next_at: now,
            last_key: None,
            shown: None,
            draw_errors: 0,
            frames: 0,
        }
    }
}

impl H264 {
    /// Opens the platform's H.264 encoder at `width` × `height`, software preferred.
    fn open(codecs: &dyn Codecs, width: u32, height: u32, fps: u32) -> Result<Self, String> {
        let size = FrameSize::new(width, height).map_err(|err| err.to_string())?;
        let frame_period = Timebase::new(1, fps).map_err(|err| err.to_string())?;
        // About 0.1 bit per pixel per frame: 640 × 360 at 15 fps is about 350 kbit/s.
        let kbps =
            (u64::from(width) * u64::from(height) * u64::from(fps) / 10_000).clamp(100, 4000);
        let kbps = u32::try_from(kbps).unwrap_or(4000);
        let config = VideoEncoderConfig {
            codec: Codec::H264,
            size,
            input_format: PixelFormat::Rgba8,
            frame_period,
            rate_control: RateControl::Vbr {
                kbps,
                max_kbps: kbps * 2,
            },
            keyframe_interval: fps.saturating_mul(KEYFRAME_EVERY.as_secs() as u32),
            hardware: HardwarePreference::Avoid,
        };
        let options = SessionOptions {
            input_capacity: 2,
            subscriber_capacity: BINARY_BUFFER * 2,
            ..SessionOptions::default()
        };
        let session = VideoSession::open(codecs, config, options)
            .map_err(|err| format!("opening the H.264 encoder: {err}"))?;
        let input = session.input();
        let packets = session.subscribe();
        Ok(Self {
            session,
            input,
            packets,
            size,
            pending: VecDeque::new(),
        })
    }
}

/// The engine's preview thread and the streams it serves.
#[derive(Default)]
pub(crate) struct PreviewHub {
    inner: Arc<HubInner>,
}

#[derive(Default)]
struct HubInner {
    state: Mutex<HubState>,
    wake: Condvar,
    /// The next stream's [`PreviewStream::id`].
    next_id: AtomicU64,
}

#[derive(Default)]
struct HubState {
    streams: Vec<Weak<PreviewStream>>,
    /// The thread, while it runs or until it is joined.
    thread: Option<JoinHandle<()>>,
    running: bool,
    /// The engine is going away.
    stop: bool,
    /// How the hub draws; [`gpu_draw`] unless a test set another.
    draw: Option<OpenPreviewDraw>,
    /// What the hub captures with, once the engine has backends.
    capture: Option<PreviewCapture>,
}

/// The engine's capture backends, which the hub captures displays and windows with.
#[derive(Clone)]
pub(crate) struct PreviewCapture {
    pub(crate) capture: Arc<dyn Capture>,
    /// Never opened by the hub: a capture of [`CaptureSpec::Device`] is left out.
    pub(crate) devices: Arc<dyn jstream_platform::VideoDevices>,
    pub(crate) codecs: Arc<dyn Codecs>,
    /// The media clock's timebase, which [`PreviewSource::current`]'s time is on.
    pub(crate) clock: Timebase,
}

impl PreviewCapture {
    /// Starts the `jstream-capture` thread, on the CPU path: the hub's compositor is not
    /// on the platform's GPU context. The sizes it reports go nowhere; the program stage
    /// reports the sizes `source.sizes` shows.
    fn spawn(&self) -> Result<RenderCaptures, String> {
        let (sizes, _) = std::sync::mpsc::channel();
        spawn_captures(
            CaptureBackends {
                cpu: Arc::clone(&self.capture),
                gpu: None,
                devices: Arc::clone(&self.devices),
                codecs: Arc::clone(&self.codecs),
            },
            self.clock,
            sizes,
        )
    }
}

impl PreviewHub {
    /// Replaces how the hub draws, for tests. Takes effect when the thread next starts.
    pub(crate) fn set_draw(&self, draw: OpenPreviewDraw) {
        lock(&self.inner.state).draw = Some(draw);
    }

    /// Gives the hub the engine's capture backends. Takes effect when the thread next
    /// starts.
    pub(crate) fn set_capture(&self, capture: PreviewCapture) {
        lock(&self.inner.state).capture = Some(capture);
    }

    fn next_id(&self) -> u64 {
        self.inner.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// The scenes the live streams show, which the engine plans for them.
    pub(crate) fn scenes(&self) -> BTreeSet<SceneId> {
        lock(&self.inner.state)
            .streams
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|stream| stream.live())
            .filter_map(|stream| stream.scene.clone())
            .collect()
    }

    /// Ends every stream of `scene_id` with `reason`.
    pub(crate) fn end_scene(&self, scene_id: &SceneId, rev: Rev, reason: &str) {
        let streams: Vec<Arc<PreviewStream>> = lock(&self.inner.state)
            .streams
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|stream| stream.scene.as_ref() == Some(scene_id))
            .collect();
        for stream in streams {
            stream.end(rev, reason);
        }
    }

    /// Adds `stream`, starting the thread if it isn't running.
    fn add(
        &self,
        stream: &Arc<PreviewStream>,
        source: Weak<dyn PreviewSource>,
    ) -> Result<(), String> {
        let finished = {
            let mut state = lock(&self.inner.state);
            state.streams.push(Arc::downgrade(stream));
            if state.running {
                self.inner.wake.notify_all();
                return Ok(());
            }
            let draw = state.draw.clone().unwrap_or_else(gpu_draw);
            let capture = state.capture.clone();
            let inner = Arc::clone(&self.inner);
            let handle = thread::Builder::new()
                .name("jstream-preview".into())
                .spawn(move || run(&inner, &source, &draw, capture.as_ref()))
                .map_err(|err| format!("starting the preview thread: {err}"))?;
            state.running = true;
            state.thread.replace(handle)
        };
        // A thread that ended on its own has nothing left to do; this only reaps it.
        if let Some(finished) = finished
            && finished.join().is_err()
        {
            tracing::error!("preview thread panicked");
        }
        Ok(())
    }
}

impl Drop for PreviewHub {
    fn drop(&mut self) {
        let handle = {
            let mut state = lock(&self.inner.state);
            state.stop = true;
            state.thread.take()
        };
        self.inner.wake.notify_all();
        if let Some(handle) = handle
            && handle.thread().id() != thread::current().id()
            && handle.join().is_err()
        {
            tracing::error!("preview thread panicked");
        }
    }
}

/// The hub thread: passes over the live streams until none is left, the engine is
/// gone, or the hub is dropped. Its captures stop when it ends.
fn run(
    inner: &HubInner,
    source: &Weak<dyn PreviewSource>,
    open: &OpenPreviewDraw,
    capture: Option<&PreviewCapture>,
) {
    let mut canvas = Canvas {
        open,
        draw: None,
        rgba: Vec::new(),
        jpeg: Vec::new(),
    };
    let mut captures = HubCaptures {
        capture,
        running: None,
        wanted: BTreeSet::new(),
        of: None,
    };
    let mut works: HashMap<u64, Work> = HashMap::new();
    loop {
        let streams: Vec<Arc<PreviewStream>> = {
            let mut state = lock(&inner.state);
            let mut live = Vec::new();
            state.streams.retain(|weak| match weak.upgrade() {
                Some(stream) if stream.live() => {
                    live.push(stream);
                    true
                }
                _ => false,
            });
            if live.is_empty() || state.stop {
                state.running = false;
                return;
            }
            live
        };
        let Some(current) = source.upgrade().map(|source| source.current()) else {
            lock(&inner.state).running = false;
            return;
        };
        let Current {
            program,
            scenes,
            live,
            rev,
            at,
        } = current;
        let now = Instant::now();
        let mut wake_at = now + MAX_SLEEP;
        // A closed stream's encoder is dropped (and its thread joined) here.
        works.retain(|id, _| streams.iter().any(|stream| stream.id == *id));
        // The program's captures only while a stream shows the program.
        let shows_program = streams.iter().any(|stream| stream.scene.is_none());
        captures.tick((shows_program.then_some(&program), &scenes), at);
        let shown: &dyn Captures = match &captures.running {
            Some(running) => running,
            None => &NoCaptures,
        };
        for stream in &streams {
            let work = works
                .entry(stream.id)
                .or_insert_with(|| Work::new(stream, now));
            let snapshot = match &stream.scene {
                None => &program,
                Some(scene_id) => match scenes.get(scene_id) {
                    Some(snapshot) => snapshot,
                    // Not planned yet: the engine re-plans once the stream is open.
                    None => {
                        let period = Duration::from_secs(1) / stream.settings.fps;
                        wake_at = wake_at.min(now + period);
                        continue;
                    }
                },
            };
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                serve(
                    stream,
                    work,
                    (snapshot, &live, shown, captures.frames(snapshot)),
                    rev,
                    (now, at),
                    &mut canvas,
                )
            }));
            match result {
                Ok(Some(next)) => wake_at = wake_at.min(next),
                Ok(None) => {}
                Err(_) => {
                    // The compositor may be what panicked: open a new one next time.
                    canvas.draw = None;
                    stream.end(rev, "the preview thread panicked");
                }
            }
        }
        drop(streams);
        drop(program);
        drop(scenes);
        drop(live);
        let state = lock(&inner.state);
        if state.stop {
            continue;
        }
        let timeout = wake_at.saturating_duration_since(Instant::now());
        // A new stream wakes the thread; so does the hub's drop.
        drop(inner.wake.wait_timeout(state, timeout));
    }
}

/// The displays and windows the hub captures: what the program mix and the scenes with
/// a stream show of them, on a `jstream-capture` thread started the first time one is
/// shown.
struct HubCaptures<'a> {
    /// `None` when the engine has no backends, or the thread could not be started:
    /// nothing is captured.
    capture: Option<&'a PreviewCapture>,
    running: Option<RenderCaptures>,
    /// What the snapshots `of` show: their displays and windows.
    wanted: BTreeSet<CaptureKey>,
    of: Option<(Option<Arc<RenderSnapshot>>, Scenes)>,
}

/// The scenes' snapshots, as [`Current::scenes`] holds them.
type Scenes = Arc<BTreeMap<SceneId, Arc<RenderSnapshot>>>;

impl HubCaptures<'_> {
    /// Starts and stops captures to follow the program's snapshot (`None` while no
    /// stream shows the program) and the scenes', and reads their latest frames. Never
    /// waits.
    fn tick(&mut self, (program, scenes): (Option<&Arc<RenderSnapshot>>, &Scenes), now: MediaTime) {
        let Some(capture) = self.capture else {
            return;
        };
        let same = self.of.as_ref().is_some_and(|(p, s)| {
            Arc::ptr_eq(s, scenes)
                && match (p, program) {
                    (Some(p), Some(program)) => Arc::ptr_eq(p, program),
                    (None, None) => true,
                    _ => false,
                }
        });
        if !same {
            self.wanted = program
                .into_iter()
                .chain(scenes.values())
                .flat_map(|snapshot| snapshot.shot().captures())
                .filter(|key| captured(key))
                .cloned()
                .collect();
            self.of = Some((program.cloned(), Arc::clone(scenes)));
        }
        if self.running.is_none() && !self.wanted.is_empty() {
            match capture.spawn() {
                Ok(running) => self.running = Some(running),
                Err(err) => {
                    tracing::warn!(error = %err, "the preview captures nothing");
                    self.capture = None;
                    self.wanted.clear();
                    return;
                }
            }
        }
        if let Some(running) = &mut self.running {
            running.tick(&self.wanted, now);
        }
    }

    /// The newest frame version among the captures `snapshot` shows, when it shows one
    /// the hub runs: a picture of it is due when that has moved since it was drawn.
    /// Versions only grow, across every capture the hub runs.
    fn frames(&self, snapshot: &RenderSnapshot) -> Option<u64> {
        let running = self.running.as_ref()?;
        let mut shown = snapshot
            .shot()
            .captures()
            .iter()
            .filter(|key| captured(key))
            .peekable();
        shown.peek()?;
        Some(
            shown
                .filter_map(|key| running.frame(key).map(|frame| frame.version))
                .max()
                .unwrap_or_default(),
        )
    }
}

/// Whether the hub captures `key`: a display or window, never a video device.
fn captured(key: &CaptureKey) -> bool {
    !matches!(key.spec, CaptureSpec::Device(_))
}

/// What the hub thread draws and encodes with, shared by every stream it serves.
struct Canvas<'a> {
    /// Opens the compositor when there is none.
    open: &'a OpenPreviewDraw,
    /// The compositor, once opened; dropped after a panic so the next pass reopens it.
    draw: Option<PreviewDraw>,
    /// The last picture drawn, RGBA8.
    rgba: Vec<u8>,
    /// The last JPEG encoded.
    jpeg: Vec<u8>,
}

/// One pass over one stream: forwards the packets its encoder produced, and draws and
/// encodes a picture if it is due. Returns when it is next due, or `None` once it ended.
fn serve(
    stream: &PreviewStream,
    work: &mut Work,
    (snapshot, live, captures, frames): (
        &Arc<RenderSnapshot>,
        &Arc<LiveParams>,
        &dyn Captures,
        Option<u64>,
    ),
    rev: Rev,
    (now, at): (Instant, MediaTime),
    canvas: &mut Canvas<'_>,
) -> Option<Instant> {
    let Canvas {
        open,
        draw,
        rgba,
        jpeg,
    } = canvas;
    if let Err(reason) = forward(stream, work, None) {
        stream.end(rev, &reason);
        return None;
    }
    let period = Duration::from_secs(1) / stream.settings.fps;
    if now < work.next_at {
        return Some(work.next_at);
    }
    // A running transition changes the picture with time, not with the snapshot, and so
    // can a video filter (a scroll, a video delay) and a capture.
    let changed = snapshot.shot().changes_with_time()
        || frames.is_some_and(|frames| work.frames != frames)
        || !work
            .shown
            .as_ref()
            .is_some_and(|(shot, values)| Arc::ptr_eq(shot, snapshot) && Arc::ptr_eq(values, live));
    let stale = work
        .last_key
        .is_none_or(|at| now.saturating_duration_since(at) >= KEYFRAME_EVERY);
    let keyframe = stale || stream.keyframe.load(Ordering::Acquire);
    if !(changed || keyframe) || !lock(&stream.sink).has_room() {
        // Check again next period: the scene, or the reader, may have moved on.
        return Some(now + period);
    }
    work.next_at = now + period;
    let shot = snapshot.shot();
    let canvas = shot.canvas().map_or((16, 16), |c| (c.width(), c.height()));
    let (width, height) = stream.settings.picture_size(canvas.0, canvas.1);
    let Ok(size) = FrameSize::new(width, height) else {
        stream.end(rev, "the preview picture has no size");
        return None;
    };
    if draw.is_none() {
        match open() {
            Ok(opened) => *draw = Some(opened),
            Err(reason) => {
                stream.end(rev, &format!("opening the preview compositor: {reason}"));
                return None;
            }
        }
    }
    let drawing = draw.as_mut()?;
    if let Err(reason) = drawing(shot, live, captures, size, at, rgba) {
        work.draw_errors += 1;
        tracing::debug!(channel = stream.channel, error = %reason, "preview picture skipped");
        if work.draw_errors > MAX_DRAW_ERRORS {
            stream.end(rev, &format!("drawing the preview failed: {reason}"));
            return None;
        }
        return Some(work.next_at);
    }
    work.draw_errors = 0;
    work.shown = Some((Arc::clone(snapshot), Arc::clone(live)));
    work.frames = frames.unwrap_or_default();
    stream.keyframe.store(false, Ordering::Release);
    let pts_us =
        i64::try_from(now.saturating_duration_since(work.started).as_micros()).unwrap_or(i64::MAX);
    if work.codec == PreviewCodec::H264
        && let Err(reason) = open_h264(stream, work, size)
    {
        if !stream.settings.codecs.contains(&PreviewCodec::Jpeg) {
            stream.end(rev, &reason);
            return None;
        }
        tracing::warn!(channel = stream.channel, error = %reason, "preview falls back to JPEG");
        work.codec = PreviewCodec::Jpeg;
    }
    let sent = match work.codec {
        PreviewCodec::Jpeg => {
            if jpeg::encode(width, height, rgba, JPEG_QUALITY, jpeg) {
                stream.send(picture(
                    stream,
                    PreviewCodec::Jpeg,
                    true,
                    size,
                    pts_us,
                    rev,
                    jpeg,
                ));
                Ok(())
            } else {
                Err("encoding the JPEG failed".to_owned())
            }
        }
        _ => push_h264(stream, work, size, pts_us, rev, keyframe, rgba),
    };
    if let Err(reason) = sent {
        stream.end(rev, &reason);
        return None;
    }
    if keyframe {
        work.last_key = Some(now);
    }
    Some(work.next_at)
}

/// Opens the stream's H.264 session at `size`, unless it is open at that size.
fn open_h264(stream: &PreviewStream, work: &mut Work, size: FrameSize) -> Result<(), String> {
    if work.h264.as_ref().is_some_and(|h264| h264.size == size) {
        return Ok(());
    }
    // Dropped here: the old session's encode thread is joined on this thread.
    work.h264 = None;
    let codecs = stream
        .codecs
        .as_ref()
        .ok_or("this engine has no H.264 encoder")?;
    work.h264 = Some(H264::open(
        codecs.as_ref(),
        size.width(),
        size.height(),
        stream.settings.fps,
    )?);
    Ok(())
}

/// Pushes one picture into the stream's open H.264 session, then waits briefly for its
/// packet.
fn push_h264(
    stream: &PreviewStream,
    work: &mut Work,
    size: FrameSize,
    pts_us: i64,
    rev: Rev,
    keyframe: bool,
    rgba: &[u8],
) -> Result<(), String> {
    let h264 = work.h264.as_mut().ok_or("the H.264 encoder is not open")?;
    if keyframe {
        h264.session.request_keyframe();
    }
    let info = VideoFrameInfo {
        size,
        format: PixelFormat::Rgba8,
        color: ColorInfo {
            space: ColorSpace::Bt709,
            range: ColorRange::Full,
        },
        pts: MediaTime::new(pts_us, Timebase::MICROS),
    };
    let stride = size.width() as usize * 4;
    let frame = VideoFrame::new(info, vec![Plane::new(rgba.to_vec(), stride)])
        .map_err(|err| format!("building the preview frame: {err}"))?;
    match h264.input.push(VideoSurface::Cpu(frame)) {
        Ok(()) => h264.pending.push_back((pts_us, rev)),
        // The encoder is behind: this picture is lost, so the next one is a keyframe.
        Err(jstream_encode::PushError::Full) => stream.keyframe.store(true, Ordering::Release),
        Err(err) => return Err(format!("the H.264 encoder stopped: {err}")),
    }
    forward(stream, work, Some(PACKET_WAIT))
}

/// Sends the packets the stream's H.264 session has produced, waiting up to `wait` for
/// the first.
fn forward(stream: &PreviewStream, work: &mut Work, wait: Option<Duration>) -> Result<(), String> {
    let Some(h264) = work.h264.as_mut() else {
        return Ok(());
    };
    let size = h264.size;
    let mut first = wait;
    loop {
        let packet = match first.take() {
            Some(wait) => h264.packets.recv_timeout(wait),
            None => h264.packets.try_recv(),
        };
        let packet = match packet {
            Ok(Some(packet)) => packet,
            Ok(None) => return Ok(()),
            Err(end) => return Err(format!("the H.264 encoder stopped: {end}")),
        };
        let pts_us = packet
            .pts()
            .rescale(Timebase::MICROS, Rounding::Nearest)
            .map_or(0, MediaTime::ticks);
        // Pictures come back in order; the packet shows the latest one at or before it.
        let mut rev = Rev(0);
        while let Some(&(pts, picture_rev)) = h264.pending.front() {
            if pts > pts_us {
                break;
            }
            rev = picture_rev;
            h264.pending.pop_front();
        }
        stream.send(picture(
            stream,
            PreviewCodec::H264,
            packet.is_keyframe(),
            size,
            pts_us,
            rev,
            packet.data(),
        ));
    }
}

/// A picture frame for `stream`.
fn picture(
    stream: &PreviewStream,
    codec: PreviewCodec,
    keyframe: bool,
    size: FrameSize,
    pts_us: i64,
    rev: Rev,
    payload: &[u8],
) -> BinaryFrame {
    BinaryFrame {
        header: BinaryHeader {
            kind: StreamKind::Preview,
            channel: stream.channel,
            codec: Some(codec),
            keyframe,
            end: false,
            width: u16::try_from(size.width()).unwrap_or(u16::MAX),
            height: u16::try_from(size.height()).unwrap_or(u16::MAX),
            pts_us,
            rev,
        },
        payload: Arc::from(payload),
    }
}
