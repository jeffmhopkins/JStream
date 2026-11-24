//! Preview streams (ADR-0045): the program mix, or one scene, as low-resolution
//! pictures, sent to the session that subscribed as binary frames
//! ([`jstream_protocol::BinaryFrame`]).
//!
//! **Scenes.** A stream of a scene draws that scene's own render snapshot, which the
//! engine plans beside the program mix's for every scene a live stream shows
//! ([`PreviewHub::scenes`]), after the program's plan has ended so it never makes a
//! source shown or heard. Once the engine has backends the program stage draws it as a
//! monitored scene (**The program stage**); otherwise the hub draws it and captures its
//! displays, windows and webcams (**Captures**). Until the
//! engine has planned it the stream waits; once the scene is deleted the engine ends it
//! ([`PreviewHub::end_scene`]).
//!
//! **Sessions.** Each [`InProcessControl`](crate::InProcessControl) holds a
//! [`SessionPreviews`]: its binary channel (a bounded queue, read through
//! `Control::binary`) and its open streams by channel. `preview.subscribe` checks the
//! request, picks the codec and registers the stream with the engine's [`PreviewHub`];
//! `preview.unsubscribe` and the session's end drop it. `preview.show` changes the scene
//! an open stream shows (**Moving a stream**), and `preview.request_keyframe` sets its keyframe
//! flag, so a client whose decoder fell behind resumes on the next picture.
//!
//! **Moving a stream.** A stream's scene is the one thing about it that changes while it
//! is open. The hub moves it on its first pass after the engine has planned the new
//! scene, and until then goes on sending the old one. A staged H.264 stream whose
//! [`SharedEncode`] it has alone, and whose new picture no other session encodes, keeps
//! that session: its render-thread tap is cut and a tap of the new picture handed over
//! in its place, so the encoder, any hardware session it holds and the stream's sender
//! carry on, and the session is asked for a keyframe. Otherwise the stream opens again
//! on the new picture, joining its session if one is open. The hub's own streams just
//! draw the new scene, as a keyframe.
//!
//! **The program stage.** A stream of the program or of a scene is drawn by the program
//! stage (ADR-0117 §6): the hub holds the stage while such a stream is open, opening it
//! when no output has, and hands it the stream's H.264 encoder session, or for JPEG and
//! PNG a picture queue ([`Staged`]); the stage's render thread draws the program, or the
//! scene as a monitored scene, at the stream's size and feeds it, and a sender thread
//! per stream forwards the packets, or encodes and sends the pictures that differ from
//! the last one sent (or after [`KEYFRAME_EVERY`], or when a keyframe is asked for).
//! Source and draft streams, and every stream of an engine without backends, are the
//! hub's own.
//!
//! **The hub.** One `jstream-preview` thread per engine, started by the first
//! subscription and ended when none is left, serves every stream. For the streams it
//! draws itself it owns a compositor, like `video.snapshot`'s thread, never on the
//! render thread. On each pass
//! it reads the published render snapshot and `rev` and the live values on the render
//! parameter channel (ADR-0032), and for each stream that is due draws the shot with
//! them at the stream's size, reads it back and encodes it. A stream is due at most
//! `fps` times a second, when the snapshot or the live values changed, when a keyframe was asked
//! for, or [`KEYFRAME_EVERY`] after its last keyframe, so a reader that lost frames
//! recovers and a still scene costs almost nothing.
//!
//! **Codecs.** `preview.subscribe` picks the first codec the client accepts that the
//! engine has: H.264 when the platform lists an H.264 encoder, JPEG and PNG always.
//! H.264 runs on a `jstream-encode` [`VideoSession`], opened on the hub thread and
//! reopened when the picture size changes: a staged stream's on a hardware encoder while
//! one of the stage's [`HardwareSlot`]s is free, which an output takes back, and every
//! other on the platform's software encoder. Each asks for about half a bit per pixel,
//! up to 1080p at 30 fps in software and 1080p at 60 fps on hardware; if it can't be opened, the
//! stream falls back to JPEG when the client accepts it, and ends otherwise. Every frame
//! names its codec. JPEG is this module's own baseline encoder ([`jpeg`]). PNG, which is
//! lossless, is for a client taking a screenshot (ADR-0096), with `video.snapshot`'s
//! encoder. The encoders and pacing live on the hub thread alone ([`Work`]), so a
//! stream shared with its session holds only flags and its settings.
//!
//! **Captures.** The hub captures the displays, windows, webcams and capture cards the
//! program mix and each streamed scene show itself, on the CPU path, through the
//! engine's backends ([`PreviewCapture`]), with the program stage's [`RenderCaptures`]
//! on a `jstream-capture` thread of its own, so they show in the preview with no output
//! running. While an output runs, its stage captures the same display again: each
//! display capture belongs to one consumer. A webcam or capture card opens once, so
//! the engine's video-device backend shares that one open between the program stage
//! and the hub (`runtime/devices.rs`). `video.snapshot` still draws every capture as
//! nothing. The program's captures run only
//! while a stream shows the program. A stream that shows a capture is due when one of
//! the captures it shows took a new frame since its last picture.
//!
//! **Overload.** The hub never waits on a reader: the binary channel holds
//! [`BINARY_BUFFER`] frames, and a [`KeyframeGate`] drops the rest of a stream's
//! frames until a keyframe finds room, which the hub then forces. A stream whose reader
//! has no room is not drawn at all.

mod draft;
mod jpeg;

pub(crate) use draft::plan as plan_draft;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
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
    BinaryFrame, BinaryHeader, KeyframeGate, PreviewCodec, PreviewSettings, ProfileStream,
    ProtocolError, Rev, SceneId, SourceId, StreamKind,
};
use jstream_render::{Compositor, OpenOptions, ResourceCache};

use jstream_platform::Capture;

use crate::chain::Chains;
use crate::lock;
use crate::profile::{self, DrawMeter, StreamGuard, StreamMeter, StreamRegistry, StreamTotals};
use crate::runtime::{
    CaptureBackends, PictureOutput, Program, RenderCaptures, RenderSnapshot, SharedGpu,
    output_size, picture_queue, spawn_captures,
};
use crate::snapshot::{self, CaptureKey, Captures, GpuKey, LiveParams, NoCaptures, Shot};

/// How many frames a session's binary channel holds for a reader that falls behind.
pub(crate) const BINARY_BUFFER: usize = 8;

/// The longest a stream goes without a keyframe, even when nothing changes.
pub(crate) const KEYFRAME_EVERY: Duration = Duration::from_secs(2);

/// JPEG quality, 1 to 100: near-lossless, as a local preview's bytes cost little and
/// an editing view must show text sharply (#1203).
const JPEG_QUALITY: u8 = 95;

/// The highest average H.264 bit rate a preview stream asks of a software encoder, in
/// kbit/s: 1080p at 30 fps at about half a bit per pixel.
const MAX_H264_KBPS: u64 = 32_000;

/// The highest average H.264 bit rate a preview stream asks of a hardware encoder, in
/// kbit/s: 1080p at 60 fps at about half a bit per pixel, which a hardware encoder costs
/// no more CPU to make. Over a software encoder's cap it keeps a 60 fps canvas's text
/// and fine detail as sharp as a 30 fps one's.
const MAX_HARDWARE_H264_KBPS: u64 = 64_000;

/// The average bit rate, in kbit/s, of a preview of `width` × `height` at `fps`: about
/// half a bit per pixel, so text and fine detail survive (#1203), up to `max`. The
/// stream stays on this machine or the local network, where 30 Mbit/s for 1080p at
/// 30 fps costs little. 640 × 360 at 15 fps is about 1.7 Mbit/s.
fn preview_kbps((width, height): (u32, u32), fps: u32, max: u64) -> u32 {
    let kbps = (u64::from(width) * u64::from(height) * u64::from(fps) / 2_000).clamp(100, max);
    u32::try_from(kbps).unwrap_or(u32::MAX)
}

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
    /// What each source with an open stream draws alone, once the engine has planned it.
    pub(crate) sources: Arc<BTreeMap<SourceId, Arc<RenderSnapshot>>>,
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

    /// A meter for a run of the hub thread, which times its pictures for
    /// `profile.get` (ADR-0103). Without one the hub's pictures are timed for no one.
    fn meter(&self) -> DrawMeter {
        DrawMeter::default()
    }
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
        &mut DrawMeter,
    ) -> Result<(), String>,
>;

/// Opens the hub's [`PreviewDraw`], on the hub thread.
pub(crate) type OpenPreviewDraw = Arc<dyn Fn() -> Result<PreviewDraw, String> + Send + Sync>;

/// The hub's draw: a wgpu compositor on the engine's GPU context (ADR-0117 §5), so it
/// draws the browser pages painted there, or, when the engine has none, on a device of
/// its own, hardware adapter first with the software one as the fallback
/// (`OpenOptions::hardware`). It draws the program mix with [`snapshot::compose`] and the
/// hub's captures.
pub(crate) fn gpu_draw(gpu: Option<Arc<SharedGpu>>) -> OpenPreviewDraw {
    Arc::new(move || {
        let context = gpu.as_ref().and_then(|gpu| gpu.context());
        let mut compositor = match &context {
            // Its errors stay its own, apart from the stage's on the context (#1268).
            Some(context) => Compositor::on_context(context).keeping_own_errors(),
            None => Compositor::open(OpenOptions::hardware()).map_err(|err| err.to_string())?,
        };
        tracing::info!(
            adapter = %compositor.adapter_description(),
            shared = context.is_some(),
            "preview compositor opened"
        );
        let mut cache = ResourceCache::<GpuKey>::new();
        // A still drawn small holds only the texels it is drawn with (#1139).
        cache.draw_uploads_small(true);
        let mut chains = Chains::default();
        let draw: PreviewDraw = Box::new(move |shot, live, captures, size, at, out, meter| {
            cache.begin_frame();
            let output = snapshot::compose_metered(
                &mut compositor,
                (&mut cache, &mut chains, &mut *meter),
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
        let draw: PreviewDraw = Box::new(|_, _, _, size, _, out, _| {
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

    /// Queues `frame` unless the gate drops it. Returns whether it was queued, whether
    /// its channel now waits for a keyframe (the frame or one before it was dropped),
    /// and the waker to wake.
    fn push(&mut self, frame: BinaryFrame) -> (bool, bool, Option<Waker>) {
        if self.closed {
            return (false, false, None);
        }
        let channel = frame.header.channel;
        let room = self.frames.len() < BINARY_BUFFER;
        let admitted = self.gate.admit(&frame.header, room);
        let waker = if admitted {
            self.frames.push_back(frame);
            self.waker.take()
        } else {
            None
        };
        (admitted, self.gate.is_waiting(channel), waker)
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
    /// The collection's source the stream shows alone, in place of the program mix.
    pub(crate) source_id: Option<SourceId>,
    /// What a stream of a source the collection doesn't hold draws ([`plan_draft`]),
    /// in place of the program mix or a scene.
    pub(crate) draft: Option<Arc<RenderSnapshot>>,
    /// Where its H.264 sessions are metered for `profile.get` (protocol 0.92); `None`
    /// counts them for no one.
    pub(crate) streams: Option<StreamRegistry>,
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
            PreviewCodec::Jpeg | PreviewCodec::Png => true,
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
            scene: Mutex::new(opening.scene),
            source_id: opening.source_id,
            draft: opening.draft,
            codecs: opening.codecs,
            sink,
            keyframe: AtomicBool::new(true),
            closed: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            counts: Arc::default(),
            streams: opening.streams,
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
        stream.planned()
    }

    /// `preview.show`: the stream on `channel` shows `scene` (the program mix for
    /// `None`) from its next picture, which is a keyframe. The hub moves it on its next
    /// pass, once the engine has planned `scene`; until then it goes on showing what it
    /// showed. Returns whether what the engine plans for the streams changed.
    ///
    /// # Errors
    /// `invalid_argument` on `channel` for a channel not open, or a stream of a source.
    pub(crate) fn show(
        &self,
        hub: &PreviewHub,
        channel: u32,
        scene: Option<SceneId>,
    ) -> Result<bool, ProtocolError> {
        let stream = self.open(channel)?;
        if stream.source_id.is_some() || stream.draft.is_some() {
            return Err(ProtocolError::invalid_argument(
                "channel",
                format!(
                    "preview channel {channel} shows a source, which `preview.show` can't change"
                ),
            ));
        }
        let changed = {
            let mut shown = lock(&stream.scene);
            let changed = *shown != scene;
            if changed {
                tracing::debug!(channel, scene = ?scene, "preview stream shows another picture");
                *shown = scene;
            }
            changed
        };
        if changed {
            stream.keyframe.store(true, Ordering::Release);
            hub.wake();
        }
        Ok(changed)
    }

    /// `preview.request_keyframe`: the next picture on `channel` is a keyframe. Nothing for a
    /// channel not open.
    pub(crate) fn keyframe(&self, hub: &PreviewHub, channel: u32) {
        if let Some(stream) = lock(&self.streams).get(&channel) {
            stream.keyframe.store(true, Ordering::Release);
            hub.wake();
        }
    }

    /// The live stream on `channel`.
    fn open(&self, channel: u32) -> Result<Arc<PreviewStream>, ProtocolError> {
        lock(&self.streams)
            .get(&channel)
            .filter(|stream| stream.live())
            .cloned()
            .ok_or_else(|| {
                ProtocolError::invalid_argument(
                    "channel",
                    format!("this session has no preview channel {channel} open"),
                )
            })
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
            scenes |= stream.planned();
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
    /// The scene it shows, or `None` for the program mix; `preview.show` changes it.
    scene: Mutex<Option<SceneId>>,
    /// The collection's source it shows alone instead.
    source_id: Option<SourceId>,
    /// The draft source it shows instead, planned once at the subscription.
    draft: Option<Arc<RenderSnapshot>>,
    /// The platform's encoders, when the engine has backends.
    codecs: Option<Arc<dyn Codecs>>,
    sink: Sink,
    /// The next picture must be a keyframe.
    keyframe: AtomicBool,
    /// Unsubscribed, or its session ended: send nothing more.
    closed: AtomicBool,
    /// The engine ended it and sent its end frame.
    ended: AtomicBool,
    /// The pictures it sent and lost, for `profile.get`.
    counts: Arc<SentCounts>,
    /// Where its H.264 sessions are metered.
    streams: Option<StreamRegistry>,
}

/// The pictures a stream queued for its client and those its queue refused, as running
/// totals.
#[derive(Debug, Default)]
struct SentCounts {
    sent: AtomicU64,
    bytes: AtomicU64,
    lost: AtomicU64,
}

impl SentCounts {
    fn count(&self, admitted: bool, bytes: usize) {
        if admitted {
            self.sent.fetch_add(1, Ordering::Relaxed);
            self.bytes
                .fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
        } else {
            self.lost.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn read(&self) -> (u64, u64, u64) {
        (
            self.sent.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
            self.lost.load(Ordering::Relaxed),
        )
    }
}

/// Meters an H.264 session of `stream` in `profile.get`, counting from
/// now: its encoder's frames, then the pictures the stream queued and lost. A session
/// reopened at a new size is a new meter.
fn meter_h264(stream: &PreviewStream, session: &VideoSession) -> Option<StreamGuard> {
    let streams = stream.streams.as_ref()?;
    let (fps, target_kbps) = profile::streams::encoder_target(session.info(), None);
    let handle = session.stats_handle();
    let encoder = profile::streams::encoder_of(session.stats_handle());
    let counts = Arc::clone(&stream.counts);
    let base = handle.stats().unwrap_or_default();
    let (base_sent, base_bytes, base_lost) = counts.read();
    let last = Mutex::new(base);
    Some(streams.add(StreamMeter {
        stream: ProfileStream::Preview {
            channel: stream.channel,
            scene_id: stream.scene(),
            source_id: stream.source_id.clone(),
        },
        fps,
        target_kbps,
        encoder,
        read: Box::new(move || {
            let video = match handle.stats() {
                Some(stats) => {
                    *lock(&last) = stats;
                    stats
                }
                None => *lock(&last),
            };
            let (sent, bytes, lost) = counts.read();
            let since = |now: u64, then: u64| now.saturating_sub(then);
            let encoder_drops = |stats: &jstream_encode::SessionStats| {
                stats
                    .dropped_queue_full
                    .saturating_add(stats.dropped_encoder_busy)
                    .saturating_add(stats.subscriber_drops)
            };
            StreamTotals {
                frames_in: since(
                    video.received.saturating_add(video.dropped_queue_full),
                    base.received.saturating_add(base.dropped_queue_full),
                ),
                encoded: since(video.encoded, base.encoded),
                frames_out: since(sent, base_sent),
                dropped: since(encoder_drops(&video), encoder_drops(&base))
                    .saturating_add(since(lost, base_lost)),
                late: since(video.late, base.late),
                encode_nanos: since(video.encode_nanos, base.encode_nanos),
                bytes: since(bytes, base_bytes),
            }
        }),
    }))
}

impl PreviewStream {
    /// Whether the engine plans what it shows for it: a scene, or a source alone.
    fn planned(&self) -> bool {
        lock(&self.scene).is_some() || self.source_id.is_some()
    }

    /// The scene it shows now, or `None` for the program mix (or a source).
    fn scene(&self) -> Option<SceneId> {
        lock(&self.scene).clone()
    }

    /// Whether it shows the program mix.
    fn shows_program(&self) -> bool {
        !self.planned() && self.draft.is_none()
    }

    /// Sends the end frame with `reason` and marks the stream ended.
    fn end(&self, rev: Rev, reason: &str) {
        if self.ended.swap(true, Ordering::AcqRel) {
            return;
        }
        tracing::info!(channel = self.channel, reason, "preview stream ended");
        if !self.closed.load(Ordering::Acquire) {
            let frame = BinaryFrame::end(StreamKind::Preview, self.channel, rev, reason);
            let (_, _, waker) = lock(&self.sink).push(frame);
            wake(waker);
        }
    }

    /// Queues a picture. Asks for a keyframe when the channel lost one.
    fn send(&self, frame: BinaryFrame) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let bytes = frame.payload.len();
        let (admitted, waiting, waker) = lock(&self.sink).push(frame);
        wake(waker);
        self.counts.count(admitted, bytes);
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
    /// The last `pts_us` the stream sent, from the hub or from any stage's sender, so
    /// its times never go back when it moves between them ([`StreamClock`]).
    last_pts: Arc<AtomicI64>,
    /// The earliest the next picture may be drawn.
    next_at: Instant,
    last_key: Option<Instant>,
    /// The render snapshot and live values the last picture showed.
    shown: Option<(Arc<RenderSnapshot>, Arc<LiveParams>)>,
    draw_errors: u32,
    /// The newest capture frame version the last picture drew
    /// ([`HubCaptures::frames`]).
    frames: u64,
    /// What the frame slots the last picture drew held ([`Shot::slot_stamp`]).
    slots: u64,
    /// The session the program stage feeds, while it draws this stream.
    staged: Option<Staged>,
    /// The stage couldn't take this JPEG or PNG stream: the hub draws it from now on,
    /// rather than ask again on every pass. It lasts for the stream's life, so after a
    /// one-off failure the hub keeps drawing it until the client subscribes again.
    unstaged: bool,
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
    /// Its meter in `profile.get`, dropped with it.
    profiled: Option<StreamGuard>,
}

impl Work {
    fn new(stream: &PreviewStream, now: Instant) -> Self {
        Self {
            codec: stream.codec,
            h264: None,
            started: now,
            last_pts: Arc::new(AtomicI64::new(0)),
            next_at: now,
            last_key: None,
            shown: None,
            draw_errors: 0,
            frames: 0,
            slots: 0,
            staged: None,
            unstaged: false,
        }
    }
}

impl H264 {
    /// Opens the platform's H.264 encoder at `width` × `height`, as `hardware` says,
    /// for frames pushed in `input_format`.
    fn open(
        codecs: &dyn Codecs,
        (width, height): (u32, u32),
        fps: u32,
        input_format: PixelFormat,
        hardware: HardwarePreference,
    ) -> Result<Self, String> {
        let size = FrameSize::new(width, height).map_err(|err| err.to_string())?;
        let frame_period = Timebase::new(1, fps).map_err(|err| err.to_string())?;
        let max = match hardware {
            HardwarePreference::Prefer | HardwarePreference::Require => MAX_HARDWARE_H264_KBPS,
            HardwarePreference::Avoid => MAX_H264_KBPS,
        };
        let kbps = preview_kbps((width, height), fps, max);
        let config = VideoEncoderConfig {
            codec: Codec::H264,
            size,
            input_format,
            frame_period,
            rate_control: RateControl::Vbr {
                kbps,
                max_kbps: kbps * 2,
            },
            keyframe_interval: fps.saturating_mul(KEYFRAME_EVERY.as_secs() as u32),
            hardware,
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
            profiled: None,
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

/// The engine's capture backends, which the hub captures with.
#[derive(Clone)]
pub(crate) struct PreviewCapture {
    pub(crate) capture: Arc<dyn Capture>,
    /// Shared with the program stage: a device opens once (`SharedVideoDevices`).
    pub(crate) devices: Arc<dyn jstream_platform::VideoDevices>,
    pub(crate) codecs: Arc<dyn Codecs>,
    /// The media clock's timebase, which [`PreviewSource::current`]'s time is on.
    pub(crate) clock: Timebase,
    /// Opens or shares the program stage that draws the program for H.264 streams.
    pub(crate) stage: Option<StageOpener>,
    /// The engine's GPU context, which the hub's compositor opens on.
    pub(crate) gpu: Arc<SharedGpu>,
}

/// Gives the hub the program stage to draw the program on (ADR-0117 §6): the running one,
/// or one it opens now. `Ok(None)` when it can't be had yet (an output is starting).
pub(crate) type StageOpener = Arc<dyn Fn() -> Result<Option<Arc<Program>>, String> + Send + Sync>;

/// How soon the hub asks again for a stage it couldn't have yet, and after a stage that
/// failed to open.
const STAGE_RETRY: Duration = Duration::from_millis(200);
const STAGE_RETRY_FAILED: Duration = Duration::from_secs(5);

/// A program or scene stream fed by the program stage (ADR-0117 §6): the stage's render
/// thread draws it at the stream's size on the engine's one compositor and pushes it
/// into this H.264 session, or into a JPEG or PNG stream's picture queue; a
/// `jstream-preview-send` thread sends its packets, or encodes and sends its pictures,
/// as they come. The hub only opens it, asks for keyframes and closes it.
struct Staged {
    /// The encoder session its packets come from, which other streams of the same
    /// picture may share ([`SharedEncode`]). `None` for a JPEG or PNG stream.
    session: Option<Arc<SharedEncode>>,
    sender: Option<JoinHandle<()>>,
    /// Set before the session is closed, so the sender knows its end was asked for.
    closing: Arc<AtomicBool>,
    codec: PreviewCodec,
    size: FrameSize,
    /// The scene the stage draws for it, or `None` for the program mix.
    scene: Option<SceneId>,
    stage: Weak<Program>,
    /// Its meter in `profile.get`, dropped with it.
    _profiled: Option<StreamGuard>,
}

/// The most hardware H.264 sessions the previews of a program stage hold at once. Each
/// is one distinct picture (a scene or the program, at a size and rate), however many
/// streams show it ([`SharedEncode`]). Four covers the views a studio layout shows at
/// once (the canvas, the program monitor, a Next monitor and a projector), and stays
/// under the session limits consumer GPUs have had. Nothing relies on getting them: an
/// output that misses its encoder takes every preview session back.
const PREVIEW_HARDWARE_SESSIONS: u32 = 4;

/// The hardware H.264 sessions the previews of a program stage may hold (#1691), up to
/// [`PREVIEW_HARDWARE_SESSIONS`]. A program or scene stream the stage draws tries the
/// hardware encoder while one is free, so the canvas doesn't cost a CPU core on a
/// software encoder. Any other stream encodes in software.
///
/// Outputs come first (ADR-0080 §3): an output whose own open did not get a hardware
/// encoder [`reclaim`](Self::reclaim)s them all, the streams holding them reopen in
/// software, and the output opens again. The stage owns the slot, so its outputs and
/// the hub see the same one.
#[derive(Clone, Debug, Default)]
pub(crate) struct HardwareSlot(Arc<SlotShared>);

#[derive(Debug, Default)]
struct SlotShared {
    state: Mutex<SlotState>,
    /// Woken when the slot is given up.
    freed: Condvar,
}

#[derive(Debug, Default)]
struct SlotState {
    /// The preview hardware sessions that hold one.
    taken: u32,
    /// Outputs opening now that want them back: no stream takes one meanwhile.
    reclaiming: u32,
}

/// One hardware session of the slot, taken until it is dropped.
#[derive(Debug)]
struct HardwareTaken(Arc<SlotShared>);

/// An output's hold on a slot it reclaimed, until its own encoder has opened.
#[derive(Debug)]
pub(crate) struct Reclaimed(Arc<SlotShared>);

impl HardwareSlot {
    /// A hardware session, if fewer than [`PREVIEW_HARDWARE_SESSIONS`] are held and no
    /// output is reclaiming them.
    fn take(&self) -> Option<HardwareTaken> {
        let mut state = lock(&self.0.state);
        (state.taken < PREVIEW_HARDWARE_SESSIONS && state.reclaiming == 0).then(|| {
            state.taken += 1;
            HardwareTaken(Arc::clone(&self.0))
        })
    }

    /// Whether an output wants the hardware sessions back from the streams that hold
    /// them.
    fn asked_back(&self) -> bool {
        let state = lock(&self.0.state);
        state.taken > 0 && state.reclaiming > 0
    }

    /// How many hardware sessions preview streams hold, for the count of hardware
    /// sessions in use.
    pub(crate) fn taken(&self) -> usize {
        usize::try_from(lock(&self.0.state).taken).unwrap_or(usize::MAX)
    }

    /// Takes the hardware sessions back for an output: asks the streams holding them
    /// to reopen in software and waits up to `wait` for every one to close. `None`
    /// when no stream held one, or when they were not all given up in time; otherwise
    /// no stream takes one until the returned hold is dropped.
    pub(crate) fn reclaim(&self, wait: Duration) -> Option<Reclaimed> {
        let mut state = lock(&self.0.state);
        if state.taken == 0 {
            return None;
        }
        state.reclaiming += 1;
        let hold = Reclaimed(Arc::clone(&self.0));
        let deadline = Instant::now() + wait;
        while state.taken > 0 {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                // The lock goes before `hold`, whose drop takes it.
                drop(state);
                return None;
            };
            state = match self.0.freed.wait_timeout(state, left) {
                Ok((state, _)) => state,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
        drop(state);
        Some(hold)
    }
}

impl Drop for HardwareTaken {
    fn drop(&mut self) {
        let mut state = lock(&self.0.state);
        state.taken = state.taken.saturating_sub(1);
        drop(state);
        self.0.freed.notify_all();
    }
}

impl Drop for Reclaimed {
    fn drop(&mut self) {
        let mut state = lock(&self.0.state);
        state.reclaiming = state.reclaiming.saturating_sub(1);
    }
}

/// One H.264 encoder session of a program stage's previews, shared by every stream that
/// shows the same picture: the same scene (or the program) at the same size and rate.
/// The stage draws and reads that picture back once, the session encodes it once, and
/// each stream sends the packets on from its own subscription, which starts on a
/// keyframe the session is asked for. Several views of one scene so cost one encode.
/// It closes when its last stream lets go of it.
struct SharedEncode {
    session: VideoSession,
    /// The scene the stage draws into it, or `None` for the program mix. A stream that
    /// has it alone moves it to another scene with `preview.show` ([`Self::show`]).
    scene: Mutex<Option<SceneId>>,
    /// Set to let the render thread's tap of the session go, when the session moves to
    /// another scene.
    cut: Mutex<Arc<AtomicBool>>,
    /// The stage draws it every `divisor`th base tick.
    divisor: u32,
    size: FrameSize,
    fps: u32,
    /// Which session this is, unique per process, for tests of what reopens.
    #[cfg(test)]
    serial: u64,
    /// The stage's hardware session, while this encoder is a hardware one.
    hardware: Option<HardwareTaken>,
}

impl SharedEncode {
    /// Whether it holds a hardware session an output wants back: its streams then
    /// open again, in software, and no stream joins it.
    fn gives_way(&self, slot: &HardwareSlot) -> bool {
        self.hardware.is_some() && slot.asked_back()
    }

    /// Moves it to `scene` on `stage` (`preview.show`): the render thread lets go of
    /// its tap of the old picture before it is handed the tap of the new one, so no
    /// tick pushes both, and the session is asked for a keyframe, which the new
    /// picture's first frame becomes. The encoder, and any hardware session it holds,
    /// stay open, and the stream's reader sees one stream go on.
    fn show(&self, stage: &Program, scene: Option<SceneId>) {
        let mut cut = lock(&self.cut);
        cut.store(true, Ordering::Release);
        *cut = stage.attach_preview(self.size, self.divisor, scene.clone(), self.session.input());
        *lock(&self.scene) = scene;
        self.session.request_keyframe();
    }
}

/// The [`SharedEncode`]s open on a program stage, which owns this list.
#[derive(Default)]
pub(crate) struct SharedEncodes(Mutex<Vec<Weak<SharedEncode>>>);

impl std::fmt::Debug for SharedEncodes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedEncodes").finish_non_exhaustive()
    }
}

impl SharedEncodes {
    /// The open session for `scene` at `size` and `fps`, unless it is giving its
    /// hardware session back to an output.
    fn find(
        &self,
        scene: Option<&SceneId>,
        size: FrameSize,
        fps: u32,
        slot: &HardwareSlot,
    ) -> Option<Arc<SharedEncode>> {
        let mut open = lock(&self.0);
        open.retain(|encode| encode.strong_count() > 0);
        open.iter().filter_map(Weak::upgrade).find(|encode| {
            lock(&encode.scene).as_ref() == scene
                && encode.size == size
                && encode.fps == fps
                && !encode.gives_way(slot)
        })
    }

    fn add(&self, encode: &Arc<SharedEncode>) {
        lock(&self.0).push(Arc::downgrade(encode));
    }

    /// Each open session's identity and the scene it shows, for tests of what moves.
    #[cfg(test)]
    pub(crate) fn shown(&self) -> Vec<(u64, Option<SceneId>)> {
        lock(&self.0)
            .iter()
            .filter_map(Weak::upgrade)
            .map(|encode| (encode.serial, lock(&encode.scene).clone()))
            .collect()
    }

    /// How many sessions are open, for tests.
    #[cfg(test)]
    pub(crate) fn open(&self) -> usize {
        lock(&self.0)
            .iter()
            .filter(|encode| encode.strong_count() > 0)
            .count()
    }
}

/// Opens a staged stream's H.264 encoder: on hardware while `slot` is free, falling back
/// to software when no hardware encoder opens, or in software while another stream holds
/// the slot. The slot stays taken while the session is a hardware one.
fn open_staged_h264(
    codecs: &dyn Codecs,
    size: (u32, u32),
    fps: u32,
    format: PixelFormat,
    slot: &HardwareSlot,
) -> Result<(H264, Option<HardwareTaken>), String> {
    if let Some(taken) = slot.take() {
        match H264::open(codecs, size, fps, format, HardwarePreference::Prefer) {
            Ok(h264) if h264.session.info().codec.hardware => return Ok((h264, Some(taken))),
            // `Prefer` fell back to software inside the platform: the slot is free again.
            // A software encoder keeps the software cap, which only a rate above 1080p30
            // reaches: such a session opens again, in software.
            Ok(h264)
                if preview_kbps(size, fps, MAX_HARDWARE_H264_KBPS)
                    == preview_kbps(size, fps, MAX_H264_KBPS) =>
            {
                return Ok((h264, None));
            }
            Ok(h264) => drop(h264),
            // Linux's `Prefer` has no software fallback when VA-API refuses. A platform
            // that lets go of a closed session late can also refuse the open that
            // follows a resize; software until the next open is harmless.
            Err(reason) => tracing::info!(
                error = %reason,
                "no hardware H.264 encoder for the preview; it encodes in software"
            ),
        }
    }
    H264::open(codecs, size, fps, format, HardwarePreference::Avoid).map(|h264| (h264, None))
}

/// The format the program stage's render thread pushes a preview's pictures of `size`
/// in: NV12, which it converts to on the GPU before the readback and the encoder takes
/// as it is, or RGBA8 for a size NV12 can't hold (an odd width or height).
fn staged_format(size: FrameSize) -> PixelFormat {
    if size.width().is_multiple_of(2) && size.height().is_multiple_of(2) {
        PixelFormat::Nv12
    } else {
        PixelFormat::Rgba8
    }
}

impl Staged {
    /// Opens `stream`'s encoder at `size`, hands its input to `stage`, and starts the
    /// sender. `rev` is the collection's `rev` now, which each picture carries.
    fn open(
        stream: &Arc<PreviewStream>,
        codecs: &dyn Codecs,
        size: FrameSize,
        stage: &Arc<Program>,
        rev: &Arc<AtomicU64>,
        clock: StreamClock,
        slot: &HardwareSlot,
    ) -> Result<Self, String> {
        // The stage draws every `divisor`th base tick, so the encoder is told the rate
        // it gets: never above the base rate, which a 60 fps ask of a 30 fps stage is.
        let divisor = (stage.base_fps / stream.settings.fps.max(1)).max(1);
        let fps = (stage.base_fps / divisor).max(1);
        let encodes = stage.preview_encodes();
        // Another stream of the same picture already has it encoded: this one takes
        // the same packets, from the keyframe its subscription asks for.
        let scene = stream.scene();
        let (session, packets) = match encodes.find(scene.as_ref(), size, fps, slot) {
            Some(session) => {
                let packets = session.session.subscribe();
                (session, packets)
            }
            None => {
                let (
                    H264 {
                        session, packets, ..
                    },
                    hardware,
                ) = open_staged_h264(
                    codecs,
                    (size.width(), size.height()),
                    fps,
                    staged_format(size),
                    slot,
                )?;
                let cut = stage.attach_preview(size, divisor, scene.clone(), session.input());
                let session = Arc::new(SharedEncode {
                    session,
                    scene: Mutex::new(scene.clone()),
                    cut: Mutex::new(cut),
                    divisor,
                    #[cfg(test)]
                    serial: {
                        static SERIAL: AtomicU64 = AtomicU64::new(0);
                        SERIAL.fetch_add(1, Ordering::Relaxed)
                    },
                    size,
                    fps,
                    hardware,
                });
                encodes.add(&session);
                (session, packets)
            }
        };
        let profiled = meter_h264(stream, &session.session);
        let closing = Arc::new(AtomicBool::new(false));
        let sender = {
            let stream = Arc::clone(stream);
            let closing = Arc::clone(&closing);
            let rev = Arc::clone(rev);
            thread::Builder::new()
                .name("jstream-preview-send".into())
                .spawn(move || {
                    let mut clock = clock;
                    send_packets(&stream, &packets, size, &rev, &closing, &mut clock);
                })
                .map_err(|err| format!("starting the preview sender: {err}"))?
        };
        Ok(Self {
            session: Some(session),
            sender: Some(sender),
            closing,
            codec: PreviewCodec::H264,
            size,
            scene,
            stage: Arc::downgrade(stage),
            _profiled: profiled,
        })
    }

    /// Hands `stage` a picture queue for `stream` at `size`, and starts the thread that
    /// encodes each picture in `codec` (JPEG or PNG) and sends it.
    fn open_pictures(
        stream: &Arc<PreviewStream>,
        codec: PreviewCodec,
        size: FrameSize,
        stage: &Arc<Program>,
        rev: &Arc<AtomicU64>,
        clock: StreamClock,
    ) -> Result<Self, String> {
        let (input, pictures) = picture_queue();
        let divisor = (stage.base_fps / stream.settings.fps.max(1)).max(1);
        let scene = stream.scene();
        stage.attach_pictures(size, divisor, scene.clone(), input);
        let closing = Arc::new(AtomicBool::new(false));
        let sender = {
            let stream = Arc::clone(stream);
            let closing = Arc::clone(&closing);
            let rev = Arc::clone(rev);
            thread::Builder::new()
                .name("jstream-preview-send".into())
                .spawn(move || {
                    let mut clock = clock;
                    send_pictures(&stream, codec, &pictures, &rev, &closing, &mut clock);
                })
                .map_err(|err| format!("starting the preview sender: {err}"))?
        };
        Ok(Self {
            session: None,
            sender: Some(sender),
            closing,
            codec,
            size,
            scene,
            stage: Arc::downgrade(stage),
            _profiled: None,
        })
    }

    /// Whether its encoder holds one of the stage's hardware sessions while an output
    /// wants them back: the stream then opens again, in software.
    fn gives_way(&self, slot: &HardwareSlot) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.gives_way(slot))
    }

    /// Moves its encoder to `scene` (`preview.show`) when no other stream shares it and
    /// no open session of `scene`'s picture can be joined instead; `false` when it has to
    /// open again for `scene`, which joins or opens that session (a JPEG or PNG stream
    /// always does, which costs no encoder). Its packets go on from the same
    /// subscription, so its sender, its clock and its reader carry on.
    fn show(
        &mut self,
        stream: &PreviewStream,
        stage: &Arc<Program>,
        scene: &Option<SceneId>,
        slot: &HardwareSlot,
    ) -> bool {
        let Some(session) = self.session.as_ref() else {
            return false;
        };
        let alone = Arc::strong_count(session) == 1;
        if !alone
            || stage
                .preview_encodes()
                .find(scene.as_ref(), session.size, session.fps, slot)
                .is_some()
        {
            return false;
        }
        session.show(stage, scene.clone());
        self.scene.clone_from(scene);
        // `profile.get` names the scene it shows now.
        self._profiled = None;
        self._profiled = meter_h264(stream, &session.session);
        true
    }

    /// Whether it is open in `codec` at `size` on `stage`.
    fn is(&self, codec: PreviewCodec, size: FrameSize, stage: &Arc<Program>) -> bool {
        self.codec == codec
            && self.size == size
            && self
                .stage
                .upgrade()
                .is_some_and(|open| Arc::ptr_eq(&open, stage))
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::Release);
        // The stream lets go of the session. If it was the last one on it, the session
        // finishes, the render thread lets go of its input and the sender's
        // subscription ends; otherwise the sender sees `closing` within its wait
        // (`PACKET_WAIT`).
        // A picture stream's sender (no session) sees `closing` within its longer wait
        // and ends by itself; joining it would hold the hub up for that wait.
        if self.session.take().is_none() {
            return;
        }
        if let Some(sender) = self.sender.take()
            && sender.join().is_err()
        {
            tracing::error!("preview sender panicked");
        }
    }
}

/// A staged stream's picture times (`pts_us`), which go on increasing across the stages
/// and sizes it is drawn at, as the hub's own do: the first picture a sender sends is
/// timed by how long the stream has been open, and each later one by how far its media
/// time is past that first one's. None goes back past the last time the stream sent,
/// from this sender, an earlier stage's or the hub (`Work::last_pts`, shared).
struct StreamClock {
    /// When the stream opened (the hub's `Work::started`).
    started: Instant,
    /// The first picture's media time (µs) and the time it was sent at.
    first: Option<(i64, i64)>,
    /// The last `pts_us` the stream sent, shared with its other senders.
    last: Arc<AtomicI64>,
}

impl StreamClock {
    fn new(started: Instant, last: Arc<AtomicI64>) -> Self {
        Self {
            started,
            first: None,
            last,
        }
    }

    /// The stream's clock for a sender opened now.
    fn of(work: &Work) -> Self {
        Self::new(work.started, Arc::clone(&work.last_pts))
    }

    /// The `pts_us` of a picture whose media time is `pts` (µs).
    fn at(&mut self, pts: i64) -> i64 {
        let started = self.started;
        let (base, offset) = *self.first.get_or_insert_with(|| {
            let open = started.elapsed().as_micros();
            (pts, i64::try_from(open).unwrap_or(i64::MAX))
        });
        not_back(&self.last, offset.saturating_add(pts.saturating_sub(base)))
    }
}

/// `pts_us`, or the last time sent through `last` if that is later, recorded as the
/// last.
fn not_back(last: &AtomicI64, pts_us: i64) -> i64 {
    last.fetch_max(pts_us, Ordering::AcqRel).max(pts_us)
}

/// The sender thread of a [`Staged`] stream: sends each packet as it comes, timed by
/// `clock`, until the stream closes it. A session that ends without being closed ends
/// the stream.
fn send_packets(
    stream: &PreviewStream,
    packets: &Subscription,
    size: FrameSize,
    rev: &AtomicU64,
    closing: &AtomicBool,
    clock: &mut StreamClock,
) {
    // The session may outlive this stream, when other streams share it, so the wait
    // for a packet is bounded and `closing` ends it.
    while !closing.load(Ordering::Acquire) {
        match packets.recv_timeout(PACKET_WAIT) {
            Ok(None) => {}
            Ok(Some(packet)) => {
                let pts = packet
                    .pts()
                    .rescale(Timebase::MICROS, Rounding::Nearest)
                    .map_or(0, MediaTime::ticks);
                stream.send(picture(
                    stream,
                    PreviewCodec::H264,
                    packet.is_keyframe(),
                    size,
                    clock.at(pts),
                    Rev(rev.load(Ordering::Acquire)),
                    packet.data(),
                ));
            }
            Err(end) => {
                if !closing.load(Ordering::Acquire) {
                    stream.end(
                        Rev(rev.load(Ordering::Acquire)),
                        &format!("the H.264 encoder stopped: {end}"),
                    );
                }
                return;
            }
        }
    }
}

/// The thread of a [`Staged`] JPEG or PNG stream: encodes and sends each picture the
/// stage pushes, timed by `clock`, as the hub would draw it: when it
/// differs from the last one sent, a keyframe was asked for, or [`KEYFRAME_EVERY`]
/// passed, and only while the stream's queue has room. Ends when the stream closes it
/// or the stage lets go of the queue (the hub then opens it on the next stage).
fn send_pictures(
    stream: &PreviewStream,
    codec: PreviewCodec,
    pictures: &PictureOutput,
    rev: &AtomicU64,
    closing: &AtomicBool,
    clock: &mut StreamClock,
) {
    // The stage's own buffer, shared: a frame repeated is the same buffer again.
    let mut last: Arc<Vec<u8>> = Arc::default();
    let mut sent: Option<Instant> = None;
    let mut encoded = Vec::new();
    while !closing.load(Ordering::Acquire) {
        let shot = match pictures.recv(PACKET_WAIT * 5) {
            Ok(shot) => shot,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let stale = sent.is_none_or(|at| at.elapsed() >= KEYFRAME_EVERY);
        let asked = stream.keyframe.load(Ordering::Acquire);
        let same = Arc::ptr_eq(&shot.rgba, &last) || *shot.rgba == *last;
        if (same && !stale && !asked) || !lock(&stream.sink).has_room() {
            continue;
        }
        let pts = shot
            .pts
            .rescale(Timebase::MICROS, Rounding::Nearest)
            .map_or(0, MediaTime::ticks);
        let (width, height) = (shot.size.width(), shot.size.height());
        let payload = match codec {
            PreviewCodec::Png => snapshot::encode_png(width, height, &shot.rgba),
            _ if jpeg::encode(width, height, &shot.rgba, JPEG_QUALITY, &mut encoded) => {
                Ok(std::mem::take(&mut encoded))
            }
            _ => Err("encoding the JPEG failed".to_owned()),
        };
        if closing.load(Ordering::Acquire) {
            return;
        }
        let rev = Rev(rev.load(Ordering::Acquire));
        match payload {
            Ok(payload) => {
                stream.keyframe.store(false, Ordering::Release);
                stream.send(picture(
                    stream,
                    codec,
                    true,
                    shot.size,
                    clock.at(pts),
                    rev,
                    &payload,
                ));
                encoded = payload;
                sent = Some(Instant::now());
                last = shot.rgba;
            }
            Err(reason) => {
                stream.end(rev, &reason);
                return;
            }
        }
    }
}

/// Whether `stream` is one the program stage draws for, given its `work` so far: a
/// stream of the program or of a scene, in H.264 when the engine has an encoder, or in
/// JPEG or PNG (ADR-0117 §6).
fn stageable(stream: &PreviewStream, work: Option<&Work>, can_stage: bool) -> bool {
    can_stage
        && stream.source_id.is_none()
        && stream.draft.is_none()
        && work.is_none_or(|work| !work.unstaged)
        && match work.map_or(stream.codec, |work| work.codec) {
            PreviewCodec::H264 => stream.codecs.is_some(),
            _ => true,
        }
}

/// Opens, or keeps, `stream`'s [`Staged`] session at its picture size of `canvas` (the
/// program's, or its scene's), and passes a keyframe request on. An H.264 session
/// opens on hardware while the stage's [`HardwareSlot`] is free, and opens again, in
/// software, when an output wants the slot back.
fn serve_staged(
    stream: &Arc<PreviewStream>,
    work: &mut Work,
    stage: &Arc<Program>,
    (canvas, scene): ((u32, u32), Option<SceneId>),
    rev: &Arc<AtomicU64>,
) -> Result<(), String> {
    let (width, height) = stream.settings.picture_size(canvas.0, canvas.1);
    let size = FrameSize::new(width, height).map_err(|err| err.to_string())?;
    let slot = stage.preview_hardware();
    if let Some(staged) = work.staged.as_mut()
        && staged.scene != scene
        && staged.is(work.codec, size, stage)
        && !staged.gives_way(slot)
        && staged.show(stream, stage, &scene, slot)
    {
        tracing::info!(
            channel = stream.channel,
            scene = ?scene,
            "a preview stream's encoder moves to another picture"
        );
        stream.keyframe.store(false, Ordering::Release);
    }
    if !work.staged.as_ref().is_some_and(|staged| {
        staged.scene == scene && staged.is(work.codec, size, stage) && !staged.gives_way(slot)
    }) {
        // The old session (or the hub's own) closes first.
        work.staged = None;
        work.h264 = None;
        // Which stream the stage draws, and how, so a canvas that shows the wrong
        // picture can be matched to its stream in the log (#1562).
        tracing::info!(
            channel = stream.channel,
            scene = ?scene,
            codec = ?work.codec,
            width,
            height,
            "the program stage draws a preview stream"
        );
        work.staged = Some(if work.codec == PreviewCodec::H264 {
            let codecs = stream
                .codecs
                .as_ref()
                .ok_or("this engine has no H.264 encoder")?;
            let staged = Staged::open(
                stream,
                codecs.as_ref(),
                size,
                stage,
                rev,
                StreamClock::of(work),
                slot,
            )?;
            // A new session starts on a keyframe.
            stream.keyframe.store(false, Ordering::Release);
            staged
        } else {
            Staged::open_pictures(stream, work.codec, size, stage, rev, StreamClock::of(work))?
        });
    }
    // A picture stream's request is its sender's to take (`send_pictures`).
    if let Some(session) = work
        .staged
        .as_ref()
        .and_then(|staged| staged.session.as_ref())
        && stream.keyframe.swap(false, Ordering::AcqRel)
    {
        // Every stream on the session gets the keyframe; it costs them nothing.
        session.session.request_keyframe();
    }
    Ok(())
}

impl PreviewCapture {
    /// Starts the `jstream-capture` thread, on the CPU path: the hub's compositor is not
    /// on the platform's GPU context. The sizes it reports go nowhere; the program stage
    /// reports the sizes `source.sizes` shows.
    pub(crate) fn spawn(&self) -> Result<RenderCaptures, String> {
        let (sizes, _) = std::sync::mpsc::channel();
        spawn_captures(
            CaptureBackends {
                cpu: Arc::clone(&self.capture),
                gpu: None,
                devices: Arc::clone(&self.devices),
                gpu_devices: None,
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

    /// Draws every stream on the hub's own compositor from the next start of the
    /// thread, for tests of that path.
    #[cfg(test)]
    pub(crate) fn clear_stage(&self) {
        if let Some(capture) = lock(&self.inner.state).capture.as_mut() {
            capture.stage = None;
        }
    }

    /// Asks every open stream for a keyframe, as a channel that lost a frame does.
    #[cfg(test)]
    pub(crate) fn ask_keyframes(&self) {
        for stream in lock(&self.inner.state)
            .streams
            .iter()
            .filter_map(Weak::upgrade)
        {
            stream.keyframe.store(true, Ordering::Release);
        }
    }

    fn next_id(&self) -> u64 {
        self.inner.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// The rate of each live stream, for tests of the engine's cap.
    #[cfg(test)]
    pub(crate) fn rates(&self) -> Vec<u32> {
        lock(&self.inner.state)
            .streams
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|stream| stream.live())
            .map(|stream| stream.settings.fps)
            .collect()
    }

    /// The scenes the live streams show, which the engine plans for them.
    pub(crate) fn scenes(&self) -> BTreeSet<SceneId> {
        lock(&self.inner.state)
            .streams
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|stream| stream.live())
            .filter_map(|stream| stream.scene())
            .collect()
    }

    /// The collection's sources the live streams show alone, which the engine plans
    /// for them.
    pub(crate) fn sources(&self) -> BTreeSet<SourceId> {
        lock(&self.inner.state)
            .streams
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|stream| stream.live())
            .filter_map(|stream| stream.source_id.clone())
            .collect()
    }

    /// Ends every stream of the source `source_id` with `reason`.
    pub(crate) fn end_source(&self, source_id: &SourceId, rev: Rev, reason: &str) {
        let streams: Vec<Arc<PreviewStream>> = lock(&self.inner.state)
            .streams
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|stream| stream.source_id.as_ref() == Some(source_id))
            .collect();
        for stream in streams {
            stream.end(rev, reason);
        }
    }

    /// Ends every stream of `scene_id` with `reason`.
    pub(crate) fn end_scene(&self, scene_id: &SceneId, rev: Rev, reason: &str) {
        let streams: Vec<Arc<PreviewStream>> = lock(&self.inner.state)
            .streams
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|stream| stream.scene().as_ref() == Some(scene_id))
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
            let draw = state.draw.clone().unwrap_or_else(|| {
                gpu_draw(
                    state
                        .capture
                        .as_ref()
                        .map(|capture| Arc::clone(&capture.gpu)),
                )
            });
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

impl PreviewHub {
    /// Wakes the thread for a pass now, so a stream's new picture or keyframe doesn't
    /// wait for its next due time.
    pub(crate) fn wake(&self) {
        self.inner.wake.notify_all();
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
        meter: source
            .upgrade()
            .map(|source| source.meter())
            .unwrap_or_default(),
    };
    let mut captures = HubCaptures {
        capture,
        running: None,
        wanted: BTreeSet::new(),
        of: None,
    };
    let mut works: HashMap<u64, Work> = HashMap::new();
    // The program stage while a program stream is drawn on it, and when to ask again
    // for one that couldn't be had.
    let opener = capture.and_then(|capture| capture.stage.as_ref());
    let mut stage: Option<Arc<Program>> = None;
    let mut stage_at = Instant::now();
    let rev_now = Arc::new(AtomicU64::new(0));
    let no_scenes: Scenes = Arc::default();
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
            sources,
            live,
            rev,
            at,
        } = current;
        let now = Instant::now();
        let mut wake_at = now + MAX_SLEEP;
        // A closed stream's encoder is dropped (and its thread joined) here.
        works.retain(|id, _| streams.iter().any(|stream| stream.id == *id));
        rev_now.store(rev.0, Ordering::Release);
        let program_canvas = program
            .shot()
            .canvas()
            .map_or((16, 16), |c| (c.width(), c.height()));
        let wants_stage = streams
            .iter()
            .any(|stream| stageable(stream, works.get(&stream.id), opener.is_some()));
        stage = follow_stage(
            stage,
            opener,
            wants_stage,
            program_canvas,
            now,
            &mut stage_at,
        );
        // The program's captures only while a stream the hub draws itself shows it, and
        // the scenes' likewise: the stage captures what it draws.
        let shows_program = streams.iter().any(|stream| {
            stream.shows_program() && !stageable(stream, works.get(&stream.id), stage.is_some())
        });
        let shows_scenes = streams.iter().any(|stream| {
            stream.scene().is_some() && !stageable(stream, works.get(&stream.id), stage.is_some())
        });
        // A source's own snapshot is captured as a draft's is.
        let drafts: Vec<Arc<RenderSnapshot>> = streams
            .iter()
            .filter_map(|stream| match &stream.source_id {
                Some(source_id) => sources.get(source_id).cloned(),
                None => stream.draft.clone(),
            })
            .collect();
        let hub_scenes = if shows_scenes { &scenes } else { &no_scenes };
        captures.tick((shows_program.then_some(&program), hub_scenes, &drafts), at);
        let shown: &dyn Captures = match &captures.running {
            Some(running) => running,
            None => &NoCaptures,
        };
        for stream in &streams {
            let work = works
                .entry(stream.id)
                .or_insert_with(|| Work::new(stream, now));
            if let Some(stage) = stage
                .as_ref()
                .filter(|_| stageable(stream, Some(work), true))
            {
                let scene = stream.scene();
                let canvas = match &scene {
                    None => Some(program_canvas),
                    // A planned scene with no canvas (the collection has none) draws at
                    // 16 × 16, as the hub's own drawing of it does (`serve`).
                    Some(scene_id) => scenes.get(scene_id).map(|scene| {
                        scene
                            .shot()
                            .canvas()
                            .map_or((16, 16), |c| (c.width(), c.height()))
                    }),
                };
                let Some(canvas) = canvas else {
                    // Not planned yet: the engine re-plans once the stream is open.
                    let period = Duration::from_secs(1) / stream.settings.fps;
                    wake_at = wake_at.min(now + period);
                    continue;
                };
                match serve_staged(stream, work, stage, (canvas, scene), &rev_now) {
                    Ok(()) => {
                        let period = Duration::from_secs(1) / stream.settings.fps;
                        wake_at = wake_at.min(now + period);
                        continue;
                    }
                    Err(reason) if work.codec != PreviewCodec::H264 => {
                        tracing::warn!(channel = stream.channel, error = %reason, "the preview thread draws this stream: the program stage couldn't take it");
                        work.unstaged = true;
                    }
                    Err(reason) if stream.settings.codecs.contains(&PreviewCodec::Jpeg) => {
                        tracing::warn!(channel = stream.channel, error = %reason, "preview falls back to JPEG");
                        work.codec = PreviewCodec::Jpeg;
                    }
                    Err(reason) => {
                        stream.end(rev, &reason);
                        continue;
                    }
                }
            }
            // Drawn by the hub from here: the stage no longer feeds it.
            work.staged = None;
            let scene = stream.scene();
            let planned = match (&stream.source_id, &scene) {
                (Some(source_id), _) => Some(sources.get(source_id)),
                (None, Some(scene_id)) => Some(scenes.get(scene_id)),
                (None, None) => None,
            };
            let snapshot = match (&stream.draft, planned) {
                (Some(draft), _) => draft,
                (None, None) => &program,
                (None, Some(planned)) => match planned {
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
        drop(drafts);
        drop(program);
        drop(scenes);
        drop(sources);
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

/// The program stage to hold from now on: kept while a stream wants it, opened when one
/// does and none is held (at most every [`STAGE_RETRY`]), and let go of otherwise. A
/// stage only the hub holds is let go of once the canvas differs from its size, so the
/// next one opens at the new canvas, as an output's start would.
fn follow_stage(
    stage: Option<Arc<Program>>,
    opener: Option<&StageOpener>,
    wanted: bool,
    canvas: (u32, u32),
    now: Instant,
    retry_at: &mut Instant,
) -> Option<Arc<Program>> {
    let opener = opener.filter(|_| wanted)?;
    if let Some(stage) = stage {
        let resized = output_size(canvas).is_ok_and(|size| size != stage.size());
        if !(resized && Arc::strong_count(&stage) == 1) {
            return Some(stage);
        }
        drop(stage);
        *retry_at = now;
    }
    if now < *retry_at {
        return None;
    }
    match opener() {
        Ok(Some(stage)) => Some(stage),
        Ok(None) => {
            *retry_at = now + STAGE_RETRY;
            None
        }
        Err(err) => {
            tracing::warn!(error = %err, "the preview draws the program itself: the program stage did not open");
            *retry_at = now + STAGE_RETRY_FAILED;
            None
        }
    }
}

/// The displays, windows and video devices the hub captures: what the program mix, the
/// scenes with a stream and the drafts with a stream show of them, on a `jstream-capture`
/// thread started the first time one is shown.
struct HubCaptures<'a> {
    /// `None` when the engine has no backends, or the thread could not be started:
    /// nothing is captured.
    capture: Option<&'a PreviewCapture>,
    running: Option<RenderCaptures>,
    /// What the snapshots `of` show: their displays, windows and video devices.
    wanted: BTreeSet<CaptureKey>,
    of: Option<Shown>,
}

/// The scenes' snapshots, as [`Current::scenes`] holds them.
type Scenes = Arc<BTreeMap<SceneId, Arc<RenderSnapshot>>>;

/// The snapshots [`HubCaptures::wanted`] was read from: the program's (while a stream
/// shows it), the scenes' and the drafts'.
type Shown = (
    Option<Arc<RenderSnapshot>>,
    Scenes,
    Vec<Arc<RenderSnapshot>>,
);

impl HubCaptures<'_> {
    /// Starts and stops captures to follow the program's snapshot (`None` while no
    /// stream shows the program), the scenes' and the drafts', and reads their latest
    /// frames. Never waits.
    fn tick(
        &mut self,
        (program, scenes, drafts): (
            Option<&Arc<RenderSnapshot>>,
            &Scenes,
            &[Arc<RenderSnapshot>],
        ),
        now: MediaTime,
    ) {
        let Some(capture) = self.capture else {
            return;
        };
        let same = self.of.as_ref().is_some_and(|(p, s, d)| {
            Arc::ptr_eq(s, scenes)
                && d.len() == drafts.len()
                && d.iter().zip(drafts).all(|(a, b)| Arc::ptr_eq(a, b))
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
                .chain(drafts)
                .flat_map(|snapshot| snapshot.shot().captures())
                .cloned()
                .collect();
            self.of = Some((program.cloned(), Arc::clone(scenes), drafts.to_vec()));
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

    /// The newest frame version among the captures `snapshot` shows, when it shows one:
    /// a picture of it is due when that has moved since it was drawn. Versions only
    /// grow, across every capture the hub runs.
    fn frames(&self, snapshot: &RenderSnapshot) -> Option<u64> {
        let running = self.running.as_ref()?;
        let shown = snapshot.shot().captures();
        (!shown.is_empty()).then(|| {
            shown
                .iter()
                .filter_map(|key| running.frame(key).map(|frame| frame.version))
                .max()
                .unwrap_or_default()
        })
    }
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
    /// Times each picture for `profile.get` (ADR-0103).
    meter: DrawMeter,
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
        meter,
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
    // can a video filter (a scroll, a video delay) and a capture. The slots' stamp is
    // read before the draw: a frame a slot takes during it is drawn again next time.
    let slots = snapshot.shot().slot_stamp();
    // So does a scene camera's live aim while a drive or a glide moves it (ADR-0152),
    // which the parameter channel doesn't republish as it moves.
    let changed = snapshot.shot().changes_with_time()
        || live.aims_move()
        || frames.is_some_and(|frames| work.frames != frames)
        || slots != work.slots
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
    let started = Instant::now();
    let drawn = drawing(shot, live, captures, size, at, rgba, meter);
    if drawn.is_ok() {
        meter.submitted(jstream_render::submits_on_this_thread());
        meter.end_frame(started.elapsed(), Instant::now(), drop);
    } else {
        meter.discard_frame();
    }
    if let Err(reason) = drawn {
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
    work.slots = slots;
    stream.keyframe.store(false, Ordering::Release);
    let pts_us = not_back(
        &work.last_pts,
        i64::try_from(now.saturating_duration_since(work.started).as_micros()).unwrap_or(i64::MAX),
    );
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
        PreviewCodec::Png => snapshot::encode_png(width, height, rgba).map(|png| {
            stream.send(picture(
                stream,
                PreviewCodec::Png,
                true,
                size,
                pts_us,
                rev,
                &png,
            ));
        }),
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
    // This thread pushes the RGBA8 it read back itself.
    let mut h264 = H264::open(
        codecs.as_ref(),
        (size.width(), size.height()),
        stream.settings.fps,
        PixelFormat::Rgba8,
        // The hub's own drawing (a source, a draft, or no program stage) stays in
        // software: only a staged stream takes the hub's hardware slot.
        HardwarePreference::Avoid,
    )?;
    h264.profiled = meter_h264(stream, &h264.session);
    work.h264 = Some(h264);
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

#[cfg(test)]
mod clock_tests {
    use super::{StreamClock, not_back};
    use std::sync::Arc;
    use std::sync::atomic::AtomicI64;
    use std::time::{Duration, Instant};

    #[test]
    fn a_staged_streams_times_go_on_from_when_it_opened_and_never_back() {
        let opened = Instant::now() - Duration::from_secs(5);
        let last = Arc::new(AtomicI64::new(0));
        let mut first = StreamClock::new(opened, Arc::clone(&last));
        let start = first.at(1_000);
        assert!(
            start >= 5_000_000,
            "timed by how long the stream is open: {start}"
        );
        assert_eq!(first.at(41_000), start + 40_000, "then by media time");
        assert_eq!(first.at(21_000), start + 40_000, "never back");
        // A later stage's sender, its media time starting anew, goes on from there.
        std::thread::sleep(Duration::from_millis(5));
        let mut next = StreamClock::new(opened, Arc::clone(&last));
        assert!(
            next.at(0) >= start + 5_000,
            "after the first sender's first picture"
        );
    }

    /// A stream that moves to a later stage, or back to the hub, sends no time before the
    /// last one it sent, whichever sent it (#1437 review).
    #[test]
    fn a_stream_moved_to_another_sender_goes_on_from_the_last_time_sent() {
        let opened = Instant::now();
        let last = Arc::new(AtomicI64::new(0));
        let mut first = StreamClock::new(opened, Arc::clone(&last));
        let start = first.at(0);
        // Media time running ahead of the wall clock: far past what the stream's age says.
        let ahead = first.at(10_000_000);
        assert_eq!(ahead, start + 10_000_000);
        // The next stage's sender opens now, its first picture timed by the stream's age.
        let mut next = StreamClock::new(opened, Arc::clone(&last));
        assert_eq!(next.at(0), ahead, "not back to the stream's age");
        // Nor does the hub, timing by the stream's age too.
        assert_eq!(not_back(&last, 1_000), ahead);
        assert_eq!(not_back(&last, ahead + 1), ahead + 1);
    }
}

#[cfg(test)]
mod format_tests {
    use super::staged_format;
    use jstream_media::{FrameSize, PixelFormat};

    /// A staged preview's encoder is opened for NV12, which the render thread converts
    /// to on the GPU, unless its size has an odd side, which NV12 can't hold.
    #[test]
    fn a_staged_preview_encodes_nv12_at_an_even_size() {
        let size = |width, height| FrameSize::new(width, height).expect("size");
        assert_eq!(staged_format(size(960, 540)), PixelFormat::Nv12);
        assert_eq!(staged_format(size(961, 540)), PixelFormat::Rgba8);
        assert_eq!(staged_format(size(960, 541)), PixelFormat::Rgba8);
    }
}

#[cfg(test)]
mod hardware_tests {
    use super::{HardwareSlot, open_staged_h264};
    use jstream_media::{Packet, PixelFormat};
    use jstream_platform::fake::FakeCodecs;
    use jstream_platform::{
        AudioEncoder, AudioEncoderConfig, CodecInfo, Codecs, HardwarePreference, PlatformError,
        VideoDecoder, VideoDecoderConfig, VideoEncoder, VideoEncoderConfig, VideoSurface,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// The fake encoder, reported as a hardware one when opened with `Prefer`, or
    /// refused then when `refuse` is set. With `one`, it has a single hardware session
    /// to give, and `Prefer` opens software while that one is open, as an encoder at
    /// its session limit does. Records each preference it was opened with.
    #[derive(Default)]
    struct HardwareCodecs {
        refuse: bool,
        one: Option<Arc<AtomicUsize>>,
        asked: Mutex<Vec<HardwarePreference>>,
    }

    struct Hardware {
        inner: Box<dyn VideoEncoder>,
        info: CodecInfo,
        /// The count of open hardware sessions it is one of.
        open: Option<Arc<AtomicUsize>>,
    }

    impl Drop for Hardware {
        fn drop(&mut self) {
            if let Some(open) = &self.open {
                open.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }

    impl VideoEncoder for Hardware {
        fn info(&self) -> &CodecInfo {
            &self.info
        }
        fn can_accept(&self) -> bool {
            self.inner.can_accept()
        }
        fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
            self.inner.encode(frame)
        }
        fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            self.inner.poll(out)
        }
        fn request_keyframe(&mut self) -> Result<(), PlatformError> {
            self.inner.request_keyframe()
        }
        fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            self.inner.finish(out)
        }
    }

    impl Codecs for HardwareCodecs {
        fn list(&self) -> Result<Vec<CodecInfo>, PlatformError> {
            FakeCodecs.list()
        }
        fn video_encoder(
            &self,
            config: &VideoEncoderConfig,
        ) -> Result<Box<dyn VideoEncoder>, PlatformError> {
            self.asked.lock().expect("asked").push(config.hardware);
            if config.hardware != HardwarePreference::Prefer {
                return FakeCodecs.video_encoder(config);
            }
            if self.refuse {
                return Err(PlatformError::Unsupported {
                    what: "no hardware encoder here".into(),
                });
            }
            let inner = FakeCodecs.video_encoder(config)?;
            if let Some(open) = &self.one
                && open
                    .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
            {
                return Ok(inner);
            }
            let mut info = inner.info().clone();
            info.hardware = true;
            Ok(Box::new(Hardware {
                inner,
                info,
                open: self.one.clone(),
            }))
        }
        fn audio_encoder(
            &self,
            config: &AudioEncoderConfig,
        ) -> Result<Box<dyn AudioEncoder>, PlatformError> {
            FakeCodecs.audio_encoder(config)
        }
        fn video_decoder(
            &self,
            config: &VideoDecoderConfig,
        ) -> Result<Box<dyn VideoDecoder>, PlatformError> {
            FakeCodecs.video_decoder(config)
        }
    }

    fn open(
        codecs: &HardwareCodecs,
        slot: &HardwareSlot,
    ) -> (super::H264, Option<super::HardwareTaken>) {
        open_staged_h264(codecs, (640, 360), 30, PixelFormat::Nv12, slot).expect("opens")
    }

    /// A staged preview encodes on the hardware encoder rather than a software one that
    /// costs a CPU core (#1691), and so do the next distinct pictures, up to the cap;
    /// past it a preview is software, and a freed session is taken again.
    #[test]
    fn staged_previews_encode_on_hardware_up_to_the_cap() {
        let codecs = HardwareCodecs::default();
        let slot = HardwareSlot::default();
        let mut held: Vec<_> = (0..super::PREVIEW_HARDWARE_SESSIONS)
            .map(|_| open(&codecs, &slot))
            .collect();
        assert!(
            held.iter()
                .all(|(h264, taken)| h264.session.info().codec.hardware && taken.is_some()),
            "each is on hardware and holds a session of the slot"
        );
        assert_eq!(slot.taken(), held.len());
        let (over, none) = open(&codecs, &slot);
        assert!(
            !over.session.info().codec.hardware,
            "past the cap is software"
        );
        assert!(none.is_none());
        held.pop();
        let (again, taken) = open(&codecs, &slot);
        assert!(
            again.session.info().codec.hardware,
            "the freed session is taken again"
        );
        assert!(taken.is_some());
        assert_eq!(
            codecs.asked.lock().expect("asked").last(),
            Some(&HardwarePreference::Prefer)
        );
    }

    /// A machine with no hardware encoder (VA-API's `Prefer` refuses rather than fall
    /// back) still gets its preview, in software, and leaves the slot free.
    #[test]
    fn a_refused_hardware_encoder_falls_back_to_software() {
        let codecs = HardwareCodecs {
            refuse: true,
            ..HardwareCodecs::default()
        };
        let slot = HardwareSlot::default();
        let (h264, held) = open(&codecs, &slot);
        assert!(!h264.session.info().codec.hardware);
        assert!(held.is_none());
        assert!(slot.take().is_some(), "the slot is free");
        assert_eq!(
            *codecs.asked.lock().expect("asked"),
            [HardwarePreference::Prefer, HardwarePreference::Avoid]
        );
    }

    /// Outputs come first (ADR-0080 §3): on an encoder with one hardware session to
    /// give, an output whose open found it held by a preview stream takes it back. The
    /// stream opens again in software, and the output's next open is on hardware.
    #[test]
    fn an_output_takes_the_hardware_session_back_from_a_preview() {
        let codecs = HardwareCodecs {
            one: Some(Arc::default()),
            ..HardwareCodecs::default()
        };
        let slot = HardwareSlot::default();
        let (preview, held) = open(&codecs, &slot);
        assert!(preview.session.info().codec.hardware && held.is_some());
        assert_eq!(slot.taken(), 1, "counted as a hardware session in use");
        let output = || {
            super::H264::open(
                &codecs,
                (1280, 720),
                30,
                PixelFormat::Nv12,
                HardwarePreference::Prefer,
            )
            .expect("opens")
        };
        // The one session is the preview's, so the output's open lands on software.
        let first = output();
        assert!(!first.session.info().codec.hardware);
        drop(first);
        std::thread::scope(|scope| {
            let started = scope.spawn(|| {
                let hold = slot
                    .reclaim(Duration::from_secs(10))
                    .expect("given up in time");
                let again = output();
                drop(hold);
                again
            });
            // The preview thread's next look at its stream.
            let deadline = Instant::now() + Duration::from_secs(10);
            while !slot.asked_back() {
                assert!(Instant::now() < deadline, "the output never asked");
                std::thread::sleep(Duration::from_millis(1));
            }
            // As `Staged` drops them: the session, then the slot.
            drop(preview);
            drop(held);
            let (software, none) = open(&codecs, &slot);
            assert!(!software.session.info().codec.hardware);
            assert!(none.is_none(), "the stream holds no slot in software");
            let output = started.join().expect("the output's open");
            assert!(
                output.session.info().codec.hardware,
                "the output got the session"
            );
        });
        assert_eq!(slot.taken(), 0);
    }

    /// A slot no stream holds has nothing to take back, and one not given up in time
    /// is left with its stream, which can be asked again.
    #[test]
    fn a_reclaim_of_a_free_or_stuck_slot_returns_at_once_or_at_its_wait() {
        let slot = HardwareSlot::default();
        assert!(slot.reclaim(Duration::from_secs(10)).is_none(), "free");
        let held = slot.take().expect("free");
        assert!(slot.reclaim(Duration::from_millis(20)).is_none(), "stuck");
        assert!(!slot.asked_back(), "no output is waiting any more");
        drop(held);
        assert!(slot.take().is_some(), "and the slot can be taken again");
    }
}

#[cfg(test)]
mod rate_tests {
    use super::{MAX_H264_KBPS, MAX_HARDWARE_H264_KBPS, preview_kbps};

    /// A preview asks for about half a bit per pixel: a software encoder up to 1080p at
    /// 30 fps, a hardware one up to 1080p at 60 fps, so a 60 fps canvas on hardware
    /// isn't squeezed to a quarter bit per pixel.
    #[test]
    fn a_hardware_preview_keeps_half_a_bit_per_pixel_at_1080p60() {
        let full = (1920, 1080);
        assert_eq!(preview_kbps(full, 30, MAX_H264_KBPS), 31_104);
        assert_eq!(preview_kbps(full, 30, MAX_HARDWARE_H264_KBPS), 31_104);
        assert_eq!(preview_kbps(full, 60, MAX_H264_KBPS), 32_000, "capped");
        assert_eq!(preview_kbps(full, 60, MAX_HARDWARE_H264_KBPS), 62_208);
        assert_eq!(preview_kbps((640, 360), 15, MAX_H264_KBPS), 1_728);
        assert_eq!(preview_kbps((16, 16), 1, MAX_H264_KBPS), 100, "a floor");
    }
}
