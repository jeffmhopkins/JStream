//! Starts and stops the program stage every running output shares, and each output's
//! own threads.
//!
//! Each capture is a platform mailbox, read by the render thread: one per display and
//! window the program mix shows, started and stopped as it changes ([`RenderCaptures`],
//! on the `jstream-capture` thread). When the platform's
//! GPU interop opens a device and the capture backend can deliver frames on it, the
//! render thread composites on that device and samples each captured texture where it
//! is; otherwise it composites on a device of its own and uploads CPU frames
//! ([`open_capture`]). It composites the program mix from the render snapshot the
//! control side published ([`RenderHub`]). An encoder session that takes GPU frames
//! gets each frame converted to NV12 in one of the platform's encode targets, with no
//! CPU copy; the other sessions and the virtual cameras get it read back as CPU RGBA,
//! and a mix none of them is fed is not read back. What it lets go of (a replaced snapshot,
//! GPU images past their frame fence, a finished encoder session's input) goes to the
//! program stage's `jstream-collector` thread (R-RT-003), and so does a finished
//! session's input the mixer thread lets go of. The mixer thread polls the engine
//! clock. Encode threads live inside the encoder sessions. The output thread muxes
//! FLV and publishes it, or muxes fragmented MP4 and writes it to a recording file
//! ([`Sink`]). A virtual camera has no encoder session: the render thread sends the
//! program frames to its camera thread through a [`VcamTap`], over bounded channels,
//! and the camera thread publishes them. None of those threads lock the engine's
//! collection.
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
//! deadline is left to its thread, which ends once the send returns (#138). The stop
//! then drops and counts what is still queued for that thread itself, so the final
//! `stats.tick` has it (#385).

use std::any::Any;
use std::collections::BTreeMap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use jstream_audio::{BusControl, Mixer, MixerConfig, OutputTapId};
use jstream_encode::{
    AudioInput, AudioSession, PushError, SessionOptions, SessionRegistry, SessionStats, StreamEnd,
    SubscriberStats, Subscription, VideoInput, VideoPath, VideoSession,
};
use jstream_media::{
    AudioBuffer, AudioFormat, ChannelLayout, Codec, ColorInfo, ColorRange, ColorSpace, FrameSize,
    MediaTime, PixelFormat, Plane, Rounding, SampleFormat, Timebase, VideoFrame, VideoFrameInfo,
};
use jstream_platform::wgpu;
use jstream_platform::{
    AudioEncoderConfig, AudioIo, Backends, Capture, Codecs, GpuContext, GpuFrame, GpuInterop,
    HardwarePreference, MediaClock, RateControl, VcamFormat, VideoDevices, VideoEncoderConfig,
    VideoSurface,
};
use jstream_protocol::{
    EncoderProfile, EncoderProfileId, HardwarePreference as ProfileHardware, OutputId, OutputKind,
    RateControl as ProfileRate, VideoCodec, VideoMixId,
};
use jstream_render::{
    Compositor, Nv12Converter, Nv12Pass, Nv12Target, OpenOptions, Reclaimed, RenderError,
    ResourceCache,
};
use jstream_sources::AudioScopes;

use crate::audio::{MasterMetering, MixerHandles, ScopeTaps, TapFeed};
use crate::audio_sources::{AudioInputs, Wanted};
use crate::chain::Chains;
use crate::drops::{
    AudioBusDrops, AudioEncodeDrops, DropGuard, DropSnapshot, FnDropSource, VideoEncodeDrops,
    capture_drops, encode_frame_drops, output_packet_drops, render_tick_drops,
};
use crate::engine::Engine;
use crate::lock;
use crate::runtime::captures::{self, RenderCaptures};
use crate::runtime::sink::{Destination, Sink, StreamInfo};
use crate::runtime::vcam::{VcamOut, VcamSlot, VcamTap, camera_channels, run_camera};
use crate::runtime::{
    Collector, Garbage, LinkState, LivePublish, OpenPublish, RenderHub, RenderReader,
};
use crate::snapshot::{self, CaptureImage, Captures, GpuKey, LiveParams, Shot, View};

/// Base frame rate of this cut: one frame every 1/30 s.
pub(crate) const FRAME_NUM: u32 = 1;
pub(crate) const FRAME_DEN: u32 = 30;
const SAMPLE_RATE: u32 = 48_000;
/// How long `output.start` waits for the render thread to open the compositor, on the
/// platform's GPU context or, without one, hardware first with the software adapter as
/// the fallback, unless the engine
/// was given another bound. It only keeps a stuck adapter from blocking the call
/// forever; a slower open is not a pipeline fault.
pub(crate) const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
/// Composite errors in a row that the render thread rides out by repeating the last
/// frame. One more reports the output failed: one second at the base rate.
const MAX_DRAW_ERRORS: u32 = 30;
/// Composite errors in a row on the platform's GPU context after which the render thread
/// leaves it ([`Switch`]), well before [`MAX_DRAW_ERRORS`] would fail the output. A
/// third of a second at the base rate, so one bad frame does not cost the GPU path.
const GPU_FALLBACK_ERRORS: u32 = 10;
/// Device losses within [`LOSS_WINDOW`] that end the drawing: the render thread opens a
/// new device after each of the first two, and fails the output at the third
/// (#349's decision: a GPU that keeps resetting is not retried for ever).
const MAX_DEVICE_LOSSES: usize = 3;
/// How far back [`MAX_DEVICE_LOSSES`] counts.
const LOSS_WINDOW: Duration = Duration::from_secs(60);
/// How long a stop lets the output thread send the packets still queued, counted from
/// the moment it sees the stop flag. What is left after that is dropped and counted, and
/// the publish target is told the same deadline ([`LivePublish::stop_by`]), so a stalled
/// peer cannot hold `output.stop` much longer than this.
pub(crate) const STOP_DRAIN: Duration = Duration::from_secs(2);
/// How long past the [`STOP_DRAIN`] deadline a stop still waits for a publishing output
/// thread: time to drop what is queued and close the target. A thread that has not
/// ended by then is blocked in the target, and the stop returns without it.
const OUTPUT_GRACE: Duration = Duration::from_millis(500);

/// Composites one tick on the render thread: each [`MixFrame`] that is `due`, from the
/// render snapshot's [`Shot`] at the frame's size, with the live values in [`LiveParams`]
/// applied and `display` and `window` sources showing their captures (or, with no scene
/// to draw, the primary display's filling the canvas fitted to the frame, or black when
/// there is none yet), read back as RGBA8 into its `out` (replacing its contents) when
/// the frame asks for the `readback`, and converted into its `nv12` encode target when
/// it has one, as it is at the tick's time on the media clock (a running transition
/// samples it). Returns the GPU images its cache let go of, for the collector. Opened on
/// the render thread, except the one a fallback from the GPU path opens on a thread of
/// its own so it can be given a deadline ([`Switch`]), and then used only on the render
/// thread.
pub(crate) type DrawFn = Box<
    dyn Send
        + FnMut(
            &Shot,
            &LiveParams,
            &dyn Captures,
            &mut [MixFrame],
            MediaTime,
        ) -> Result<Option<Reclaimed>, DrawError>,
>;

/// Why a [`DrawFn`] drew nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DrawError {
    /// This frame failed; the next may not. Counted toward [`MAX_DRAW_ERRORS`].
    Failed(String),
    /// The compositor's device is lost (`Compositor::lost`): nothing it holds comes back,
    /// so the render thread opens a new one at once ([`Switch`], #349).
    Lost(String),
}

impl From<String> for DrawError {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

impl std::fmt::Display for DrawError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(error) => f.write_str(error),
            Self::Lost(why) => write!(f, "the GPU device was lost ({why})"),
        }
    }
}

/// One picture the render thread draws for the encoder sessions it feeds: a video mix
/// (or the program, for `None`) at a size, every `divisor`th base tick.
#[derive(Debug)]
pub(crate) struct MixFrame {
    /// The mix it shows. `None` is the program mix of an output that shows no mix.
    pub(crate) mix: Option<VideoMixId>,
    /// Its size in pixels.
    pub(crate) size: FrameSize,
    /// It is drawn on every `divisor`th base tick, or on the first one after that when
    /// the render thread skipped it ([`Frames::schedule`]).
    pub(crate) divisor: u32,
    /// The output whose own video chain it runs through, or `None` when it is shared.
    /// It is drawn on its own even when another frame shows the same mix.
    pub(crate) owner: Option<OutputId>,
    /// Whether this tick draws it.
    pub(crate) due: bool,
    /// The first base tick it is due on again.
    pub(crate) next: u64,
    /// Whether the privacy screen covers it on this tick (ADR-0027 §4).
    pub(crate) privacy: bool,
    /// Whether this tick reads it back into `out`: a session on the CPU path or a
    /// virtual camera is fed it. Without, `out` is left as it was.
    pub(crate) readback: bool,
    /// Where it is read back: RGBA8 at `size` once a draw succeeds.
    pub(crate) out: Vec<u8>,
    /// The platform's NV12 encode target on the compositor's device to convert it into
    /// this tick (ARCH › GPU pipeline, step 4), for the sessions on the GPU path.
    pub(crate) nv12: Option<GpuFrame>,
}

impl MixFrame {
    /// A frame of `mix` not yet drawn or due.
    fn new(
        mix: Option<VideoMixId>,
        size: FrameSize,
        divisor: u32,
        owner: Option<OutputId>,
    ) -> Self {
        Self {
            mix,
            size,
            divisor,
            owner,
            due: false,
            next: 0,
            privacy: false,
            readback: false,
            out: Vec::new(),
            nv12: None,
        }
    }
}

/// Encode targets the render thread converts one feed into. A target is written again
/// only once the ring holds its only clone (`jstream-platform` README › GPU frames):
/// every frame queued from it has been read or dropped. A session queues up to
/// [`SessionOptions::input_capacity`] frames and its encoder reads one more, so with
/// one session keeping up there is always a free target; when none is free, the tick's
/// frame is dropped for the feed's GPU sessions ([`EncodeRing::next`]).
fn encode_ring_len() -> usize {
    SessionOptions::default().input_capacity + 2
}

/// Opens the render thread's [`DrawFn`], on that thread, on the platform's GPU context
/// when the pipeline has one. The engine's opens the wgpu compositor on that context,
/// or without one on a hardware adapter first ([`gpu_draw`]). Tests inject [`cpu_draw`]
/// (or a variant) so the pipeline runs without an adapter.
pub(crate) type OpenDraw = Arc<dyn Fn(Option<&GpuContext>) -> Result<DrawFn, String> + Send + Sync>;

/// A CPU stand-in for [`gpu_draw`]: each due frame copies the first capture the shot
/// places when that is an RGBA8 frame, scaled nearest-neighbour to the frame's size, or
/// fills black, which is what the GPU path draws for a collection with no scene. It
/// ignores the scenes and the mixes, draws only the frames that ask for the readback,
/// and never writes an encode target. It can't blur, so under the privacy screen it
/// draws black, as the GPU path does when its blur fails. Tests use it to run the
/// pipeline without a wgpu adapter
/// ([`Engine::with_cpu_compositor`](crate::Engine::with_cpu_compositor)).
pub(crate) fn cpu_draw() -> OpenDraw {
    Arc::new(|_: Option<&GpuContext>| {
        let draw: DrawFn = Box::new(
            |shot: &Shot,
             _: &LiveParams,
             captures: &dyn Captures,
             frames: &mut [MixFrame],
             _: MediaTime| {
                let first = shot
                    .captures()
                    .first()
                    .and_then(|spec| captures.frame(spec));
                for frame in frames
                    .iter_mut()
                    .filter(|frame| frame.due && frame.readback)
                {
                    let (size, private, out) = (frame.size, frame.privacy, &mut frame.out);
                    out.clear();
                    match first.map(|capture| capture.image) {
                        _ if private => {
                            out.resize(size.width() as usize * size.height() as usize * 4, 0)
                        }
                        Some(CaptureImage::Rgba { size: own, rgba }) if own == size => {
                            out.extend_from_slice(rgba);
                        }
                        Some(CaptureImage::Rgba { size: own, rgba }) => {
                            scale_nearest(own, rgba, size, out);
                        }
                        _ => out.resize(size.width() as usize * size.height() as usize * 4, 0),
                    }
                }
                Ok(None)
            },
        );
        Ok(draw)
    })
}

/// A capture of `own` size scaled to fill `size`, nearest neighbour, appended to `out`:
/// what the GPU path's scale looks like closely enough for tests.
fn scale_nearest(own: FrameSize, rgba: &[u8], size: FrameSize, out: &mut Vec<u8>) {
    let (cw, ch) = (own.width() as usize, own.height() as usize);
    let (w, h) = (size.width() as usize, size.height() as usize);
    out.reserve(w * h * 4);
    for y in 0..h {
        let row = (y * ch / h) * cw;
        for x in 0..w {
            let at = (row + x * cw / w) * 4;
            out.extend_from_slice(&rgba[at..at + 4]);
        }
    }
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
    /// CPU readbacks on the render thread, which every running output shares: one per
    /// mix drawn while a session on the CPU path or a virtual camera is fed it. A session
    /// on the GPU path gets NV12 encode targets instead, so with only those running this
    /// stays put.
    pub readbacks: u64,
    /// New capture frames the render thread took from the mailbox on the GPU path: each
    /// a texture shared with the platform, composited with no CPU copy. Zero on the CPU
    /// path.
    pub gpu_captures: u64,
    /// Times the render thread left the platform's GPU context mid-run because
    /// compositing kept failing on it (#451), or because its device was lost (#349): at
    /// most one per program stage.
    pub gpu_fallbacks: u64,
    /// Times the render thread's GPU device was lost (#349): a driver reset after a
    /// hang, Windows' TDR, a removed GPU. After each of the first two within a minute it
    /// opens the compositor on a new device and the output keeps streaming; the third
    /// fails the output.
    pub gpu_device_losses: u64,
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
    /// The virtual-camera sink, taken out of the backends. Empty when the platform has
    /// none, or while a virtual-camera output runs.
    pub(crate) vcam: VcamSlot,
    /// Whether the backends had a virtual-camera sink, so an empty `vcam` slot means it
    /// is in use.
    pub(crate) has_vcam: AtomicBool,
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
    /// The output's kind when the start checked its key (#343): what
    /// [`Engine::output_kind`](crate::Engine::output_kind) answers for this output
    /// until the start settles, so a retarget that lands while the host's opener runs
    /// can't give it the new server's URL with the old server's key.
    pub(crate) kind: Option<OutputKind>,
}

impl MediaHost {
    pub(crate) fn new() -> Self {
        Self {
            backends: Mutex::new(None),
            draw: Mutex::new(gpu_draw()),
            open_timeout: Mutex::new(OPEN_TIMEOUT),
            publish: Mutex::new(None),
            open_publish: Mutex::new(None),
            vcam: Arc::new(Mutex::new(None)),
            has_vcam: AtomicBool::new(false),
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

/// The picture a video encoder session is fed: the video mix an output shows, with the
/// mix's size and rate when the session opened. An output that shows no mix gets the
/// program at the program stage's size (the canvas, [`output_size`]) and the base rate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MixKey {
    /// The mix, or `None` for the program.
    pub(crate) mix: Option<VideoMixId>,
    /// The frame size, or `None` for the program stage's.
    pub(crate) size: Option<FrameSize>,
    /// Frames are drawn on every `divisor`th base tick.
    pub(crate) divisor: u32,
    /// The output whose own video chain the picture runs through, or `None` when the
    /// output has no chain and shares the picture with every output on the same mix.
    pub(crate) owner: Option<OutputId>,
}

impl MixKey {
    /// What an output that shows no mix is fed.
    pub(crate) const PROGRAM: MixKey = MixKey {
        mix: None,
        size: None,
        divisor: 1,
        owner: None,
    };
}

/// The key that decides which outputs share an encoder session (ADR-0027 §3): the mix
/// they show, the profile they name (`None` for the engine's default) with its encoding
/// when the session opened, and, for an output with its own video chain, that output.
/// An output with an audio chain of its own shares the video session but encodes its
/// own audio ([`OwnAudio`]).
pub(crate) type EncodeKey = (MixKey, Option<EncoderProfileId>, EncoderProfile);

/// How one output encodes: the sessions it shares, and whether its audio is its own.
pub(crate) struct OutputEncode {
    pub(crate) key: EncodeKey,
    /// Whether the output has an audio chain of its own, so an output tap and an audio
    /// encode only it reads.
    pub(crate) own_audio: bool,
}

/// Undoes something once its output has stopped.
pub(crate) type Release = Box<dyn FnOnce() + Send>;

/// An audio encode the mixer thread feeds: from the master bus, or from one output's
/// tap (ADR-0027's per-output chain).
pub(crate) struct AudioTap {
    input: AudioInput,
    /// The output tap it reads, or `None` for the master bus.
    tap: Option<OutputTapId>,
    /// The first time it takes: a tap's blocks run its chain's latency early, and the
    /// stream's muxer refuses packets before its origin.
    from: MediaTime,
    /// Whether the tap has given a block yet. A tap that gave one and then has none was
    /// removed, so its encode is let go of.
    seen: std::cell::Cell<bool>,
}

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
    video_taps: Sender<(Feed, VideoTap)>,
    audio_taps: Sender<AudioTap>,
    /// Virtual cameras for the render thread to publish to.
    vcam_taps: Sender<VcamTap>,
    failures: Arc<Failures>,
    /// Open encoder sessions by key, held weakly: each lives while an output holds it.
    sessions: Mutex<SessionRegistry<EncodeKey, Encoders>>,
    readbacks: Arc<AtomicU64>,
    gpu_captures: Arc<AtomicU64>,
    gpu_fallbacks: Arc<AtomicU64>,
    gpu_losses: Arc<AtomicU64>,
    /// Set by the render thread once it leaves the platform's context (a fallback or a
    /// lost device), and never cleared: from then on a new video session opens on the
    /// CPU path, since the render thread has no encode targets for it (#699).
    off_context: Arc<AtomicBool>,
    /// The mixer's inputs, one per audio source, and the thread that follows the
    /// collection. Closed in `drop` once the mixer thread has stopped. Behind a lock
    /// only so the program can be shared.
    audio: Mutex<AudioInputs>,
    capture_drops: DropSnapshot,
    bus: BusControl,
    drop_guard: Option<DropGuard>,
    ticker: Option<JoinHandle<()>>,
    /// Emits one last `stats.tick` once the threads have stopped and the sources are
    /// latched, so the run's final drops are reported when it ends.
    final_tick: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Stops the engine routing the collection's audio into this program's mixer.
    detach_audio: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Takes the stage's GPU context away from live sources that paint on it, first
    /// when the stage stops. `None` on the CPU path.
    gpu_sources: Option<GpuSourcesGuard>,
    /// The platform's GPU context the render thread composites on, which a video
    /// session opens on when the codecs can take its encode targets. Shared with the
    /// render thread, which lets go of its own once its frames, compositor and captures
    /// are gone. Last, so it goes after the sessions and their encode targets.
    context: Option<Arc<GpuContext>>,
}

impl Program {
    /// The encoder sessions for `key`: the open ones, or new ones not yet fed. Call
    /// [`attach`](Self::attach) once the output has subscribed, so a new session's
    /// first packets go to it.
    fn encoders(&self, engine: &Engine, key: EncodeKey) -> Result<Arc<Encoders>, String> {
        lock(&self.sessions).get_or_open(key.clone(), || {
            let feed = Feed {
                mix: key.0.mix.clone(),
                size: key.0.size.unwrap_or(self.size),
                divisor: key.0.divisor,
                owner: key.0.owner.clone(),
            };
            let video = Arc::new(open_video(
                self.codecs.as_ref(),
                self.session_context(),
                &feed,
                &key.2,
            )?);
            let ids = vec![engine.add_drop_source(VideoEncodeDrops::new(Arc::clone(&video)))];
            Ok(Encoders {
                key,
                feed,
                video,
                audio: Mutex::new(None),
                attached: AtomicBool::new(false),
                drop_guard: Some(DropGuard::new(engine.drop_ledger(), ids)),
            })
        })
    }

    /// The context a new video session may open on: none once the render thread has
    /// left it, so a session opened after a fallback reads back on the CPU path instead
    /// of asking for encode targets the render thread can't give, which would fail every
    /// output on the stage (#699).
    fn session_context(&self) -> Option<&GpuContext> {
        self.context
            .as_deref()
            .filter(|_| !self.off_context.load(Ordering::Acquire))
    }

    /// The master bus's audio encode on `encoders`, shared by every output on them
    /// without an audio chain of its own. Opened by the first such output, and fed by
    /// the mixer thread from then on.
    fn shared_audio(
        &self,
        engine: &Engine,
        encoders: &Encoders,
        origin: MediaTime,
    ) -> Result<Arc<AudioSession>, String> {
        let mut audio = lock(&encoders.audio);
        if let Some(open) = audio.as_ref() {
            return Ok(Arc::clone(&open.session));
        }
        let open = self.open_audio(engine, &encoders.key.2, None, origin)?;
        let session = Arc::clone(&open.session);
        *audio = Some(open);
        Ok(session)
    }

    /// Opens an audio encode for `profile`, for the mixer thread to feed from the master
    /// bus, or from `tap` from `origin` on, once [`fed`](AudioEncode::feed).
    fn open_audio(
        &self,
        engine: &Engine,
        profile: &EncoderProfile,
        tap: Option<OutputTapId>,
        origin: MediaTime,
    ) -> Result<AudioEncode, String> {
        let session = Arc::new(open_audio_encoder(
            self.codecs.as_ref(),
            self.format,
            profile,
        )?);
        let ids = vec![engine.add_drop_source(AudioEncodeDrops::new(Arc::clone(&session)))];
        let pending = AudioTap {
            input: session.input(),
            tap,
            from: origin,
            seen: std::cell::Cell::new(false),
        };
        Ok(AudioEncode {
            session,
            pending: Mutex::new(Some(pending)),
            drop_guard: Some(DropGuard::new(engine.drop_ledger(), ids)),
        })
    }

    /// Starts feeding `encoders`' video from the render thread, once.
    fn attach(&self, encoders: &Encoders) {
        if encoders.attached.swap(true, Ordering::AcqRel) {
            return;
        }
        // The thread holds the receiver until the program stops; a send fails only
        // after that, when nothing is fed anyway.
        let tap = VideoTap {
            input: encoders.video.input(),
            gpu: *encoders.video.path() == VideoPath::Gpu,
        };
        let _ = self.video_taps.send((encoders.feed.clone(), tap));
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
            engine.add_drop_source(AudioBusDrops::new(self.bus.clone())),
        ];
        self.drop_guard = Some(DropGuard::new(engine.drop_ledger(), ids));
        self.ticker = engine.spawn_stats_ticker(Arc::clone(&self.stop));
        *lock(&self.final_tick) = Some(engine.stats_emitter());
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        // The browser engine stops painting on the context before the render thread
        // lets go of it.
        self.gpu_sources.take();
        let detach = self
            .detach_audio
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(detach) = detach {
            detach();
        }
        self.stop.store(true, Ordering::Release);
        join("render", self.render.take());
        join("mixer", self.mixer.take());
        // Each input's stream stops before its ring's last handle goes, and its drops
        // are latched before the last tick.
        self.audio
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .close();
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

/// Live sources painting on the program stage's GPU context ([`Engine::use_gpu`]). The
/// program holds it in a [`GpuSourcesGuard`]; the render thread holds a clone, to take
/// the context away from them when it leaves the GPU path ([`Switch`]).
#[derive(Clone)]
struct GpuSources(Arc<Mutex<Option<GpuRelease>>>);

/// What [`Engine::use_gpu`] returns: takes the context away from the sources.
type GpuRelease = Box<dyn FnOnce() + Send + Sync>;

impl GpuSources {
    fn new(release: GpuRelease) -> Self {
        Self(Arc::new(Mutex::new(Some(release))))
    }

    /// Takes the context away from the sources, once. It takes the collection lock, so
    /// the render thread never calls it itself. The lock is held while it runs, so a
    /// program that stops waits for a release already under way, and the next program's
    /// context is never taken away by this one's.
    fn release(&self) {
        let mut release = lock(&self.0);
        if let Some(release) = release.take() {
            release();
        }
    }
}

/// The program's [`GpuSources`]: dropping it takes the context away from them.
struct GpuSourcesGuard(GpuSources);

impl Drop for GpuSourcesGuard {
    fn drop(&mut self) {
        self.0.release();
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

/// What the render thread draws for one video encoder session: a mix (the program for
/// `None`) at a size, on every `divisor`th base tick. Sessions with the same feed are
/// fed the same frames.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Feed {
    mix: Option<VideoMixId>,
    size: FrameSize,
    divisor: u32,
    /// The output whose own chain it runs through; `None` is shared.
    owner: Option<OutputId>,
}

/// One video encoder session, and the master bus's audio encode once an output without
/// an audio chain of its own uses them, shared by every running output with the same
/// [`EncodeKey`]. The output that lets go of them last finishes them.
pub(crate) struct Encoders {
    key: EncodeKey,
    /// What the render thread draws for the video session.
    feed: Feed,
    video: Arc<VideoSession>,
    audio: Mutex<Option<AudioEncode>>,
    /// Whether the render thread feeds the video yet.
    attached: AtomicBool,
    drop_guard: Option<DropGuard>,
}

impl Encoders {
    /// Drains the encoders and ends their subscriptions, so the last output gets every
    /// packet.
    fn finish(&self) {
        if let Err(err) = self.video.finish() {
            tracing::info!(error = %err, "video session finish failed");
        }
        if let Some(audio) = lock(&self.audio).as_ref() {
            audio.finish();
        }
    }

    /// The profile the sessions were opened with, or `None` for the default.
    pub(crate) fn profile_id(&self) -> Option<&EncoderProfileId> {
        self.key.1.as_ref()
    }

    /// Whether the sessions belong to one output because it has its own video chain,
    /// rather than being shared by every output with the same mix and profile.
    pub(crate) fn dedicated(&self) -> bool {
        self.key.0.owner.is_some()
    }

    /// Whether the video encoder is a hardware one now: false once a hardware encoder
    /// that refused its first frame fell back to software (#443).
    pub(crate) fn hardware(&self) -> bool {
        self.video.codec().hardware
    }
}

impl Drop for Encoders {
    fn drop(&mut self) {
        // Latches the sessions' drop counters once they are done.
        self.drop_guard.take();
    }
}

/// One audio encoder session the mixer thread feeds, and its drop counters.
struct AudioEncode {
    session: Arc<AudioSession>,
    /// Its input, until it is handed to the mixer thread.
    pending: Mutex<Option<AudioTap>>,
    drop_guard: Option<DropGuard>,
}

impl AudioEncode {
    /// Hands the input to `program`'s mixer thread, once.
    fn feed(&self, program: &Program) {
        if let Some(tap) = lock(&self.pending).take() {
            // The mixer thread holds the receiver until the program stops; a send fails
            // only after that, when nothing is fed anyway.
            let _ = program.audio_taps.send(tap);
        }
    }

    /// Drains the encoder and ends its subscriptions.
    fn finish(&self) {
        if let Err(err) = self.session.finish() {
            tracing::info!(error = %err, "audio session finish failed");
        }
    }
}

impl Drop for AudioEncode {
    fn drop(&mut self) {
        // Latches the session's drop counters once it is done.
        self.drop_guard.take();
    }
}

/// The audio encode of an output with an audio chain of its own (ADR-0027): an output
/// tap on the master bus runs the chain, and only this output reads the encode. Its
/// drop removes the tap.
pub(crate) struct OwnAudio {
    encode: AudioEncode,
    /// Takes the tap out of the engine's audio routing, which removes it from the mixer.
    release: Option<Release>,
}

impl Drop for OwnAudio {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
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
    /// The audio encode it reads: the master bus's shared one, or its own.
    audio: Option<Arc<AudioSession>>,
    /// Its own audio encode, for an output with an audio chain of its own.
    own_audio: Option<OwnAudio>,
    stats: Arc<PipelineStats>,
    /// Packets the output thread dropped itself: the muxer rejected them, or the stop's
    /// drain deadline ([`STOP_DRAIN`]) passed before they were sent. Distinct from the
    /// encode subscriber queues.
    output_drops: Arc<AtomicU64>,
    /// Tags the publish target dropped while its link was down, as it last reported.
    link_drops: Arc<AtomicU64>,
    /// The output thread's encoder subscriptions, while the thread holds them. The drop
    /// counts what is queued there itself when it stops waiting for the thread (#385).
    /// Weak, so a thread that ended unsubscribes at once. Empty for a camera, which has
    /// no encoder queues.
    queues: Weak<Queues>,
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
                .audio
                .as_ref()
                .map(|audio| audio.stats())
                .unwrap_or_default(),
            video_subscriber: *lock(&self.stats.video_subscriber),
            audio_subscriber: *lock(&self.stats.audio_subscriber),
            readbacks: self
                .program
                .as_ref()
                .map_or(0, |p| p.readbacks.load(Ordering::Relaxed)),
            gpu_captures: self
                .program
                .as_ref()
                .map_or(0, |p| p.gpu_captures.load(Ordering::Relaxed)),
            gpu_fallbacks: self
                .program
                .as_ref()
                .map_or(0, |p| p.gpu_fallbacks.load(Ordering::Relaxed)),
            gpu_device_losses: self
                .program
                .as_ref()
                .map_or(0, |p| p.gpu_losses.load(Ordering::Relaxed)),
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
    /// Joins the output thread, and says whether it ended. A publishing one is waited
    /// for only until `by`: past that it is blocked in a publish target that ignores
    /// the stop deadline, so it is detached and ends on its own once that call returns.
    fn join_output(&mut self, by: Instant) -> bool {
        let Some(handle) = self.output.take() else {
            return true;
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
        ended
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
        // Only this output reads its own audio encode.
        if let Some(own) = &self.own_audio {
            own.encode.finish();
        }
        self.cut.store(true, Ordering::Release);
        let ended = self.join_output(deadline + OUTPUT_GRACE);
        self.output_fail.close();
        // After the output thread, which may still be draining it; removes the tap.
        self.own_audio.take();
        self.audio.take();
        // A thread left behind has not counted what is still queued for it, and won't
        // before the latch below. The stop drops and counts that itself, and latches
        // while it still holds the queues, so a count the thread makes is either wholly
        // in the latch or covers only packets queued after it (#385).
        let queues = if ended { None } else { self.queues.upgrade() };
        let abandoned = queues
            .as_ref()
            .map(|queues| queues.abandon(false, false, &self.output_drops));
        // After the thread stops, or after the count above, so the latch sees the
        // final counters.
        self.drop_guard.take();
        if let Some((left, held)) = abandoned {
            drop(held);
            tracing::warn!(
                packets = left,
                "dropped the packets still queued for the output thread left behind"
            );
        }
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
    video_devices: Arc<dyn VideoDevices>,
    audio: Arc<dyn AudioIo>,
    codecs: Arc<dyn Codecs>,
    gpu: Arc<dyn GpuInterop>,
    draw: OpenDraw,
    open_timeout: Duration,
    render: Arc<RenderHub>,
    audio_sources: Arc<Wanted>,
    /// The collection's canvas (width, height) when the program stage opens: the output
    /// size follows it ([`output_size`]).
    canvas: (u32, u32),
    /// Whether the privacy screen is on: the engine sets it, the render thread reads it
    /// every tick, with no lock (ADR-0027 §4).
    privacy: Arc<AtomicBool>,
}

pub(crate) fn parts_from(
    backends: &Backends,
    draw: OpenDraw,
    open_timeout: Duration,
    render: Arc<RenderHub>,
    audio_sources: Arc<Wanted>,
    canvas: (u32, u32),
    privacy: Arc<AtomicBool>,
) -> Parts {
    Parts {
        canvas,
        clock: Arc::clone(&backends.clock),
        capture: Arc::clone(&backends.capture),
        video_devices: Arc::clone(&backends.video_devices),
        audio: Arc::clone(&backends.audio),
        codecs: Arc::clone(&backends.codecs),
        gpu: Arc::clone(&backends.gpu),
        draw,
        open_timeout,
        render,
        audio_sources,
        privacy,
    }
}

/// The wgpu compositor, reading every frame back to the CPU: on the platform's GPU
/// context when there is one, else on a hardware adapter, or the software one when there
/// is none (`OpenOptions::hardware`; WARP missed the 1/30 s budget at 720p, about 10 fps,
/// #433). Its [`ResourceCache`] keeps
/// the uploads, the imported capture textures and the nested-scene layers from tick to
/// tick. A draw that fails on a lost device says so ([`DrawError::Lost`]).
pub(crate) fn gpu_draw() -> OpenDraw {
    open_gpu_draw(None)
}

/// Runs before each draw on the compositor it is about to draw with. Tests use it to
/// lose the device mid-run (`gpu_draw_with`); [`gpu_draw`] has none.
pub(crate) type DrawHook = Arc<dyn Fn(&Compositor) + Send + Sync>;

/// [`gpu_draw`], calling `hook` before each draw.
#[cfg(test)]
pub(crate) fn gpu_draw_with(hook: DrawHook) -> OpenDraw {
    open_gpu_draw(Some(hook))
}

fn open_gpu_draw(hook: Option<DrawHook>) -> OpenDraw {
    Arc::new(move |context: Option<&GpuContext>| {
        let hook = hook.clone();
        let mut compositor = match context {
            Some(context) => Compositor::on_context(context),
            None => {
                Compositor::open(OpenOptions::hardware()).map_err(|err| render_open_error(&err))?
            }
        };
        tracing::info!(
            adapter = %compositor.adapter_description(),
            platform = context.is_some(),
            "render compositor opened"
        );
        let mut cache = ResourceCache::<GpuKey>::new();
        let mut chains = Chains::default();
        let mut nv12 = Nv12Passes::default();
        let draw: DrawFn = Box::new(move |shot, live, captures, frames, at| {
            if let Some(hook) = &hook {
                hook(&compositor);
            }
            // One frame of the cache for every mix of the tick, so the uploads and
            // nested-scene layers they share are kept for all of them. A mix not due on
            // this tick claims its layers, and its privacy blur's, without drawing, so
            // they aren't retired and reallocated when it is due again.
            cache.begin_frame();
            for frame in frames.iter().filter(|frame| !frame.due) {
                let claimed = snapshot::claim_layers(
                    &mut compositor,
                    (&mut cache, &mut chains),
                    shot,
                    frame.mix.as_ref(),
                    frame.size,
                )
                .and_then(|()| snapshot::privacy_layers(&compositor, &mut cache, frame.size));
                claimed.map_err(|err| draw_error(&compositor, &err))?;
            }
            for frame in frames.iter_mut().filter(|frame| frame.due) {
                let view = View {
                    mix: frame.mix.as_ref(),
                    privacy: Some(frame.privacy),
                };
                let drawn = snapshot::compose(
                    &mut compositor,
                    (&mut cache, &mut chains),
                    (shot, at),
                    view,
                    live,
                    captures,
                    snapshot::DrawAt {
                        size: frame.size,
                        readback: frame.readback,
                    },
                )
                .and_then(|output| {
                    if frame.readback {
                        copy_pixels(&output, &mut frame.out)?;
                    }
                    match &frame.nv12 {
                        Some(target) => nv12.submit(&compositor, output.texture(), target),
                        None => Ok(()),
                    }
                });
                drawn.map_err(|err| draw_error(&compositor, &err))?;
            }
            // A failed frame skips this: what it retired waits for the next frame's
            // fence, which also covers it, so nothing is freed on this thread.
            Ok(cache.end_frame(&compositor))
        });
        Ok(draw)
    })
}

/// The NV12 conversion of the compositor's output into encode targets, built on the
/// first target. One pass is bound per target and kept while the output texture it
/// reads stays the same, so a steady tick allocates nothing. A pass is found by its
/// target's [`FrameSync`](jstream_platform::FrameSync) hold, which it watches without
/// holding (the ring's free check counts the holds), and is let go of once every clone
/// of its target is gone: feeds come and go with their rings.
#[derive(Default)]
struct Nv12Passes {
    converter: Option<Nv12Converter>,
    /// Each target's hold, the output texture its pass reads, and the pass.
    passes: Vec<(
        Weak<dyn jstream_platform::FrameSync>,
        wgpu::Texture,
        Nv12Pass,
    )>,
}

impl Nv12Passes {
    /// Converts `output` into `frame` in a submit of its own, after the composite's.
    fn submit(
        &mut self,
        compositor: &Compositor,
        output: &wgpu::Texture,
        frame: &GpuFrame,
    ) -> Result<(), RenderError> {
        self.passes.retain(|(sync, _, _)| sync.strong_count() > 0);
        let converter = self
            .converter
            .get_or_insert_with(|| Nv12Converter::new(compositor.device()));
        let sync = frame.sync();
        let at = match self
            .passes
            .iter()
            .position(|(held, _, _)| std::ptr::addr_eq(held.as_ptr(), Arc::as_ptr(sync)))
        {
            Some(at) if self.passes[at].1 == *output => at,
            found => {
                // In the target's own colour: the platform's is the engine's default,
                // BT.709 limited range.
                let pass = converter.bind(
                    output,
                    &Nv12Target::nv12_texture(frame.texture())?,
                    frame.info().color,
                )?;
                let entry = (Arc::downgrade(sync), output.clone(), pass);
                match found {
                    Some(at) => {
                        self.passes[at] = entry;
                        at
                    }
                    None => {
                        self.passes.push(entry);
                        self.passes.len() - 1
                    }
                }
            }
        };
        converter.submit(compositor.queue(), &self.passes[at].2, frame)
    }
}

/// `err` from drawing on `compositor`, as a [`DrawError::Lost`] when the device is gone:
/// a lost device's draws fail on its invalid resources, and wgpu reports the loss itself
/// on the poll `Compositor::lost` makes.
fn draw_error(compositor: &Compositor, err: &RenderError) -> DrawError {
    match compositor.lost() {
        Some(why) => DrawError::Lost(why),
        None => DrawError::Failed(err.to_string()),
    }
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
pub(crate) struct FailLine(Arc<Mutex<Option<Sender<PipelineEvent>>>>);

impl FailLine {
    pub(crate) fn new(fail: Sender<PipelineEvent>) -> Self {
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
    pub(crate) fn report(&self, reason: String) {
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
    let size = output_size(parts.canvas)?;
    // `context` is bound first, so on an early return it drops last, after the stream,
    // as in the render loop.
    let OpenedCapture { context, backends } = open_capture(&parts)?;
    let context = context.map(Arc::new);
    // Browser pages paint on the context while the stage runs. Bound before anything
    // else that can fail, so an early return takes the context away again.
    let gpu_sources = context
        .as_deref()
        .map(|context| GpuSourcesGuard(GpuSources::new(engine.use_gpu(context))));
    let captures = captures::spawn(backends, parts.clock.timebase())?;
    let format = engine_format()?;
    let mixer = Mixer::new(MixerConfig::default()).map_err(|err| format!("mixer: {err}"))?;
    // On an early return this closes the inputs, each stream before its ring's last
    // handle, so no ring is freed on a real-time thread (docs/RULES.md#R-RT-003).
    let audio = AudioInputs::start(
        mixer.control(),
        Arc::clone(&parts.audio),
        Arc::clone(&parts.video_devices),
        Arc::clone(&parts.clock),
        Arc::clone(&parts.audio_sources),
        engine.drop_ledger(),
    )?;
    let bus = mixer.bus();
    let period = frame_period(parts.clock.timebase())?;
    if period.ticks() <= 0 {
        return Err("the media clock is too coarse for the frame rate".into());
    }
    // The mixer timestamps its first block `latency` before the clock, so a stream's
    // origin sits that far back. Later blocks and frames stay at or after it.
    let latency = mixer_latency(parts.clock.as_ref(), &mixer)?;
    let mixer_control = mixer.control();
    let (metering, master_meters) = MasterMetering::new(mixer.config().sample_rate)?;
    let capture_drops = DropSnapshot::new();
    let stop = Arc::new(AtomicBool::new(false));
    let readbacks = Arc::new(AtomicU64::new(0));
    let gpu_captures = Arc::new(AtomicU64::new(0));
    let gpu_fallbacks = Arc::new(AtomicU64::new(0));
    let gpu_losses = Arc::new(AtomicU64::new(0));
    let off_context = Arc::new(AtomicBool::new(false));
    let failures = Arc::new(Failures::default());
    let (ready_tx, ready_rx) = mpsc::channel();
    let (video_taps, video_inputs) = mpsc::channel();
    let (audio_taps, audio_inputs) = mpsc::channel();
    let (vcam_taps, vcam_inputs) = mpsc::channel();
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
            context: context.clone(),
            gpu: Arc::clone(&parts.gpu),
            captures,
            gpu_sources: gpu_sources.as_ref().map(|guard| guard.0.clone()),
            reader: RenderReader::new(Arc::clone(&parts.render), collector.clone()),
            collector,
            open: Arc::clone(&parts.draw),
            open_timeout: parts.open_timeout,
            clock: Arc::clone(&parts.clock),
            inputs: video_inputs,
            cams: vcam_inputs,
            size,
            period,
            stop: Arc::clone(&stop),
            privacy: Arc::clone(&parts.privacy),
            ready: ready_tx,
            readbacks: Arc::clone(&readbacks),
            gpu_captures: Arc::clone(&gpu_captures),
            gpu_fallbacks: Arc::clone(&gpu_fallbacks),
            gpu_losses: Arc::clone(&gpu_losses),
            off_context: Arc::clone(&off_context),
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
    let scope_taps = Arc::new(ScopeTaps::default());
    let mixer_thread = match spawn("jstream-mixer", {
        let clock = Arc::clone(&parts.clock);
        let stop = Arc::clone(&stop);
        let fail = fail_tx;
        let collector = mixer_collector;
        let out = MixOut {
            inputs: audio_inputs,
            format,
            scopes: engine.audio_scopes(),
            taps: Arc::clone(&scope_taps),
        };
        let mixing = Mixing { mixer, metering };
        move || mixer_thread(mixing, clock, &out, stop, fail, &collector)
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
        vcam_taps,
        failures,
        sessions: Mutex::new(SessionRegistry::new()),
        readbacks,
        gpu_captures,
        gpu_fallbacks,
        gpu_losses,
        off_context,
        audio: Mutex::new(audio),
        capture_drops,
        bus,
        drop_guard: None,
        ticker: None,
        final_tick: Mutex::new(None),
        detach_audio: Mutex::new(None),
        gpu_sources,
        context,
    };
    match ready_rx.recv_timeout(parts.open_timeout) {
        Ok(Ok(())) => {
            program.arm_stats(engine);
            let detach = engine.attach_mixer(MixerHandles {
                control: mixer_control,
                master: program.bus.clone(),
                meters: master_meters,
                scopes: engine.audio_scopes(),
                taps: scope_taps,
            });
            *lock(&program.detach_audio) = Some(detach);
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
/// outputs that have the same `key`; with `own_audio`, an audio encode of its own from
/// an output tap that runs its audio chain), then the destination, then its output
/// thread.
/// On failure the publish target is dropped: a one-shot target has to be set again,
/// an opener opens a new one on the next start. A recording opened before the failure
/// is removed, so its `.part` file doesn't block the next start.
pub(crate) fn launch(
    engine: &Engine,
    program: Arc<Program>,
    encode: OutputEncode,
    destination: Destination,
    output_id: OutputId,
    generation: u64,
    fail_tx: Sender<PipelineEvent>,
) -> Result<Running, String> {
    let registration = program
        .failures
        .add(output_id.clone(), generation, fail_tx.clone());
    let OutputEncode { key, own_audio } = encode;
    let encoders = program.encoders(engine, key)?;
    let origin = program.origin()?;
    let own_audio = if own_audio {
        let (tap, release) = engine.add_output_tap(&output_id)?;
        match program.open_audio(engine, &encoders.key.2, Some(tap), origin) {
            Ok(encode) => Some(OwnAudio {
                encode,
                release: Some(release),
            }),
            Err(err) => {
                release();
                return Err(err);
            }
        }
    } else {
        None
    };
    let audio = match &own_audio {
        Some(own) => Arc::clone(&own.encode.session),
        None => program.shared_audio(engine, &encoders, origin)?,
    };
    // Set once the start has succeeded; until then a recording is provisional.
    let keep = Arc::new(AtomicBool::new(false));
    let info = stream_info(&encoders.feed, origin, &encoders.key.2)?;
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
    let queues = Arc::new(Queues {
        video: Mutex::new(encoders.video.subscribe()),
        audio: Mutex::new(audio.subscribe()),
        abandon: Mutex::new(()),
    });
    let shared_queues = Arc::downgrade(&queues);
    program.attach(&encoders);
    match &own_audio {
        Some(own) => own.encode.feed(&program),
        None => {
            if let Some(shared) = lock(&encoders.audio).as_ref() {
                shared.feed(&program);
            }
        }
    }
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
                queues,
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
        audio: Some(audio),
        own_audio,
        stats,
        output_drops,
        link_drops,
        queues: shared_queues,
        drop_guard: None,
        registration: Some(registration),
        program: Some(program),
    };
    running.arm_stats(engine);
    keep.store(true, Ordering::Release);
    Ok(running)
}

/// Starts a virtual-camera output on `program`: starts the camera at `canvas`, as BGRA8
/// at the base frame rate, hands the render thread a tap that sends it every program
/// frame, and starts the camera thread that publishes them. No encoder session opens.
/// On failure `vcam` drops, which stops the camera and returns its sink to the engine.
/// The stop sets the output's stop flag and waits for the camera thread to stop the
/// camera, as long as it waits for any output thread.
pub(crate) fn launch_vcam(
    program: Arc<Program>,
    mut vcam: VcamOut,
    canvas: FrameSize,
    output_id: OutputId,
    generation: u64,
    fail_tx: Sender<PipelineEvent>,
) -> Result<Running, String> {
    let registration = program
        .failures
        .add(output_id.clone(), generation, fail_tx.clone());
    vcam.start(VcamFormat {
        size: canvas,
        pixel_format: PixelFormat::Bgra8,
        frame_period: Timebase::new(FRAME_NUM, FRAME_DEN)
            .map_err(|err| format!("frame period: {err}"))?,
    })?;
    let stop = Arc::new(AtomicBool::new(false));
    let output_drops = Arc::new(AtomicU64::new(0));
    let (tap, frames, spare) = camera_channels(Arc::clone(&stop), Arc::clone(&output_drops));
    // The tap goes first: a tap the render thread can't take drops here, with `vcam`
    // still on this thread. A camera thread that fails to start drops its channel
    // ends, and the tap leaves.
    program
        .vcam_taps
        .send(tap)
        .map_err(|_| "the render thread has stopped".to_string())?;
    let fail = FailLine::new(fail_tx);
    let (done_tx, done_rx) = mpsc::channel();
    let output = {
        let (stop, fail) = (Arc::clone(&stop), fail.clone());
        spawn("jstream-vcam", move || {
            run_camera(vcam, canvas, frames, spare, stop, fail, done_tx);
        })?
    };
    Ok(Running {
        output_id,
        generation,
        stop,
        drain_by: Arc::new(OnceLock::new()),
        cut: Arc::new(AtomicBool::new(false)),
        output: Some(output),
        output_done: Some(done_rx),
        output_fail: fail,
        encoders: None,
        audio: None,
        own_audio: None,
        stats: PipelineStats::new(),
        output_drops,
        link_drops: Arc::new(AtomicU64::new(0)),
        queues: Weak::new(),
        drop_guard: None,
        registration: Some(registration),
        program: Some(program),
    })
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

/// The size the program stage renders and encodes at: the canvas, rounded down to even
/// sizes (NV12, the encoders' input, halves both for chroma), and at least 2x2. Each
/// capture keeps its own size and is scaled by its item's placement (#433).
fn output_size((width, height): (u32, u32)) -> Result<FrameSize, String> {
    let even = |edge: u32| (edge & !1).max(2);
    FrameSize::new(even(width), even(height)).map_err(|err| format!("output size: {err}"))
}

/// The capture backends, and the platform's GPU context when the render thread
/// composites on it.
pub(crate) struct OpenedCapture {
    pub(crate) backends: captures::Backends,
    pub(crate) context: Option<GpuContext>,
}

/// Picks the capture path (ARCH › GPU pipeline, step 6). Nothing is captured yet: the
/// render thread starts what its snapshots capture ([`RenderCaptures`]).
///
/// The GPU path needs the platform's GPU interop to open a device and the capture
/// backend to deliver frames on it ([`Capture::on_context`]). Its streams' frames are
/// textures the render thread samples where they are. When the interop has no device
/// (the fakes, CI, a machine without one), the render thread opens its own and captures
/// deliver CPU frames. When there is a device but the backend has no GPU path, or a GPU
/// capture does not start, captures deliver CPU frames and the render thread still
/// composites on the platform's device. Each fallback is logged.
pub(crate) fn open_capture(parts: &Parts) -> Result<OpenedCapture, String> {
    let context = match parts.gpu.open_device() {
        Ok(context) => Some(context),
        Err(err) => {
            tracing::info!(error = %err, "no GPU interop; compositing on the render thread's own device");
            None
        }
    };
    let gpu = context
        .as_ref()
        .and_then(|context| gpu_capture(parts.capture.as_ref(), context));
    Ok(OpenedCapture {
        backends: captures::Backends {
            cpu: Arc::clone(&parts.capture),
            gpu,
            devices: Arc::clone(&parts.video_devices),
            codecs: Arc::clone(&parts.codecs),
        },
        context,
    })
}

/// The capture backend on `context`, or `None`, logged, when it has no GPU path.
fn gpu_capture(capture: &dyn Capture, context: &GpuContext) -> Option<Arc<dyn Capture>> {
    match capture.on_context(context) {
        Ok(Some(capture)) => Some(capture),
        Ok(None) => {
            tracing::info!("the capture backend has no GPU path; capturing CPU frames");
            None
        }
        Err(err) => {
            tracing::warn!(error = %err, "capture refused the GPU context; capturing CPU frames");
            None
        }
    }
}

/// The codec `profile` encodes video in. HEVC and AV1 map here once the protocol's
/// `VideoCodec` names them; the encoders (`MfCodecs`) and the FLV sink already take them.
fn video_codec(profile: &EncoderProfile) -> Result<Codec, String> {
    match profile.video.codec {
        VideoCodec::H264 => Ok(Codec::H264),
        _ => Err("the encoder profile's video codec is not one this engine knows".into()),
    }
}

/// The video encoder settings for `profile` at `feed`'s size and rate.
fn video_config(feed: &Feed, profile: &EncoderProfile) -> Result<VideoEncoderConfig, String> {
    let period = Timebase::new(FRAME_NUM.saturating_mul(feed.divisor), FRAME_DEN)
        .map_err(|err| format!("frame period: {err}"))?;
    let video = &profile.video;
    let rate_control = match video.rate_control {
        ProfileRate::Cbr { kbps } => RateControl::Cbr { kbps },
        ProfileRate::Vbr { kbps, max_kbps } => RateControl::Vbr { kbps, max_kbps },
        _ => return Err("the encoder profile's rate control is not one this engine knows".into()),
    };
    let hardware = match video.hardware {
        ProfileHardware::Avoid => HardwarePreference::Avoid,
        ProfileHardware::Require => HardwarePreference::Require,
        _ => HardwarePreference::Prefer,
    };
    Ok(VideoEncoderConfig {
        codec: video_codec(profile)?,
        size: feed.size,
        input_format: PixelFormat::Rgba8,
        frame_period: period,
        rate_control,
        // Frames per keyframe interval at the mix's rate, at least one.
        keyframe_interval: video
            .keyframe_interval_s
            .saturating_mul(FRAME_DEN)
            .checked_div(FRAME_NUM.saturating_mul(feed.divisor))
            .unwrap_or(1)
            .max(1),
        hardware,
    })
}

/// Opens the video session on the platform's GPU context when the codecs take its
/// encode targets there, and otherwise for CPU frames (ARCH › GPU pipeline, step 6).
/// A fallback from a context is logged.
fn open_video(
    codecs: &dyn Codecs,
    context: Option<&GpuContext>,
    feed: &Feed,
    profile: &EncoderProfile,
) -> Result<VideoSession, String> {
    let session = VideoSession::open_on(
        codecs,
        context,
        video_config(feed, profile)?,
        SessionOptions::default(),
    )
    .map_err(|err| format!("opening the video encoder: {err}"))?;
    match session.path() {
        VideoPath::Gpu => tracing::info!("the video encoder takes GPU frames"),
        VideoPath::Cpu {
            fallback: Some(why),
        } => tracing::info!(reason = %why, "the video encoder takes CPU frames"),
        VideoPath::Cpu { fallback: None } => {}
    }
    Ok(session)
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

fn stream_info(
    feed: &Feed,
    origin: MediaTime,
    profile: &EncoderProfile,
) -> Result<StreamInfo, String> {
    let video_kbps = match profile.video.rate_control {
        ProfileRate::Cbr { kbps } | ProfileRate::Vbr { kbps, .. } => kbps,
        _ => 0,
    };
    Ok(StreamInfo {
        codec: video_codec(profile)?,
        size: feed.size,
        origin,
        frame_rate: f64::from(FRAME_DEN) / f64::from(FRAME_NUM.saturating_mul(feed.divisor)),
        video_kbps,
        audio_kbps: profile.audio.kbps,
        sample_rate: SAMPLE_RATE,
        sps: &SPS,
        pps: &PPS,
    })
}

/// A video session the render thread pushes frames into, and which kind it takes.
pub(crate) struct VideoTap {
    pub(crate) input: VideoInput,
    /// GPU frames: NV12 encode targets on the render thread's context
    /// ([`VideoPath::Gpu`]). Otherwise CPU RGBA8.
    gpu: bool,
}

struct RenderJob {
    /// The captures, whose streams and backends use the platform's GPU context: dropped
    /// before it, here and in the render loop.
    captures: RenderCaptures,
    /// The platform's GPU context the compositor opens on, or `None` for its own device.
    /// The render loop drops it last, after the encode targets, the compositor and the
    /// captures, also when it has left the GPU path.
    context: Option<Arc<GpuContext>>,
    /// Allocates the encode targets on `context`.
    gpu: Arc<dyn GpuInterop>,
    /// The live sources painting on `context`, to take it away from them when the thread
    /// leaves the GPU path. `None` without a context.
    gpu_sources: Option<GpuSources>,
    reader: RenderReader,
    /// Takes the input of a session that has finished, so the render thread never drops
    /// the last reference to it (R-RT-003).
    collector: Collector,
    open: OpenDraw,
    /// How long a fallback from the GPU path may take to open the compositor on a device
    /// of its own: the bound `output.start` gives the first open.
    open_timeout: Duration,
    clock: Arc<dyn MediaClock>,
    /// The sessions to push frames into, as they are opened, with what each is fed.
    inputs: Receiver<(Feed, VideoTap)>,
    /// The virtual cameras to publish frames to, as their outputs start.
    cams: Receiver<VcamTap>,
    size: FrameSize,
    period: MediaTime,
    stop: Arc<AtomicBool>,
    /// Whether the privacy screen is on, read at every tick.
    privacy: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<(), String>>,
    readbacks: Arc<AtomicU64>,
    gpu_captures: Arc<AtomicU64>,
    gpu_fallbacks: Arc<AtomicU64>,
    gpu_losses: Arc<AtomicU64>,
    /// Set once the thread leaves the platform's context ([`Program::session_context`]).
    off_context: Arc<AtomicBool>,
    drops: DropSnapshot,
    fail: Sender<PipelineEvent>,
}

/// The render thread's boundary: a panic outside a tick (opening the compositor,
/// stopping the captures) is caught here and reported as fatal.
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
/// allocates only the frames the encoders take ownership of.
struct Frames {
    /// The pictures drawn for the sessions, one per [`Feed`]: where a tick composites.
    mixes: Vec<MixFrame>,
    /// For each of `mixes`, the last frame composited in full, repeated when a tick
    /// cannot draw; swapped with the mix's `out` when a draw succeeds.
    shown: Vec<Vec<u8>>,
    /// For each of `mixes`, the sessions it feeds.
    targets: Vec<Vec<VideoTap>>,
    /// For each of `mixes`, its encode targets, for the sessions on the GPU path.
    encode: Vec<MixEncode>,
    /// Whether a virtual camera needs the program at the program stage's size, so that feed is
    /// drawn even with no session on it.
    program_held: bool,
}

impl Frames {
    /// Turns the last drawn frame of every due mix opaque black, in place, so a tick
    /// that could not draw under the privacy screen repeats nothing it showed before.
    /// Its sessions on the GPU path miss the tick's frame instead, since their last
    /// encode target may hold a frame the screen didn't cover. Returns how many
    /// sessions missed it, for [`encode_frame_drops`].
    fn blacken_due(&mut self) -> u64 {
        let mut missed: u64 = 0;
        for ((frame, shown), (taps, encode)) in self
            .mixes
            .iter()
            .zip(&mut self.shown)
            .zip(self.targets.iter().zip(&mut self.encode))
        {
            if frame.due {
                for pixel in shown.chunks_exact_mut(4) {
                    pixel.copy_from_slice(&[0, 0, 0, 255]);
                }
                let gpu_taps = taps.iter().filter(|tap| tap.gpu).count() as u64;
                if gpu_taps > 0 && !encode.starved {
                    encode.starved = true;
                    missed = missed.saturating_add(gpu_taps);
                }
            }
        }
        missed
    }

    /// Feeds `input` the frames of `feed`, drawn alongside an existing session's with
    /// the same feed.
    fn add(&mut self, feed: Feed, tap: VideoTap) {
        let found = self.mixes.iter().position(|frame| {
            frame.mix == feed.mix
                && frame.size == feed.size
                && frame.divisor == feed.divisor
                && frame.owner == feed.owner
        });
        match found {
            Some(at) => self.targets[at].push(tap),
            None => {
                self.mixes
                    .push(MixFrame::new(feed.mix, feed.size, feed.divisor, feed.owner));
                self.shown.push(Vec::new());
                self.targets.push(vec![tap]);
                self.encode.push(MixEncode::default());
            }
        }
    }

    /// Stops drawing the mixes that feed no session any more, handing their buffers to
    /// `free` so the render thread frees nothing (R-RT-003).
    fn prune(&mut self, mut free: impl FnMut(Garbage)) {
        let mut at = 0;
        while at < self.targets.len() {
            let held = self.program_held && is_program(&self.mixes[at], self.mixes[at].size);
            if self.targets[at].is_empty() && !held {
                free(Garbage::Inputs(self.targets.swap_remove(at)));
                free(Garbage::Pixels(self.mixes.swap_remove(at).out));
                free(Garbage::Pixels(self.shown.swap_remove(at)));
                // `shown` is a clone of one of the ring's targets.
                let MixEncode { ring, shown, .. } = self.encode.swap_remove(at);
                drop(shown);
                free(Garbage::Targets(ring.slots));
            } else {
                at += 1;
            }
        }
    }

    /// Keeps the program at the program stage's `size` drawn on every tick while `held`, for the
    /// virtual cameras, adding its frame if no session has it yet. Once no longer held,
    /// [`prune`](Self::prune) drops it like any feed with no session left.
    fn hold_program(&mut self, held: bool, size: FrameSize) {
        self.program_held = held;
        if held && self.program_frame(size).is_none() {
            self.mixes.push(MixFrame::new(None, size, 1, None));
            self.shown.push(Vec::new());
            self.targets.push(Vec::new());
            self.encode.push(MixEncode::default());
        }
    }

    /// Where the program at the program stage's `size`, drawn on every tick, is kept.
    fn program_frame(&self, size: FrameSize) -> Option<usize> {
        self.mixes.iter().position(|frame| is_program(frame, size))
    }

    /// Marks the mixes due on base tick `index` and returns how many are. A mix is due
    /// once `index` reaches its `next` tick, which is then `divisor` ticks on, so an
    /// overrun that skips its tick delays it rather than starving it, and it never gets
    /// frames closer together than its divisor. Each is covered by the privacy screen
    /// when `privacy`.
    fn schedule(&mut self, index: u64, privacy: bool) -> u64 {
        let mut due = 0;
        for frame in &mut self.mixes {
            frame.privacy = privacy;
            frame.due = index >= frame.next;
            if frame.due {
                frame.next = index.saturating_add(u64::from(frame.divisor.max(1)));
            }
            due += u64::from(frame.due);
        }
        due
    }

    /// Whether a session takes this program's GPU frames (encode targets).
    fn feeds_gpu(&self) -> bool {
        self.targets.iter().flatten().any(|tap| tap.gpu)
    }

    /// Sets out what each due mix is drawn into on this tick: a readback while a session
    /// on the CPU path is fed it (or, for the program at the stage's `size`, while a
    /// virtual camera runs, as `cams` says), and the next free encode target of its ring
    /// while a session on the GPU path is. A mix whose ring has none free gets no
    /// target, and its GPU sessions miss the tick's frame. Returns how many sessions
    /// missed it, for [`encode_frame_drops`].
    ///
    /// # Errors
    /// A ring that cannot be allocated ([`EncodeRing::next`]), which is fatal. No mix
    /// gets a target then.
    fn prepare(
        &mut self,
        gpu: &dyn GpuInterop,
        context: Option<&GpuContext>,
        size: FrameSize,
        cams: bool,
    ) -> Result<u64, String> {
        let program = if cams { self.program_frame(size) } else { None };
        let mut missed: u64 = 0;
        for (at, ((frame, taps), encode)) in self
            .mixes
            .iter_mut()
            .zip(&self.targets)
            .zip(&mut self.encode)
            .enumerate()
        {
            frame.readback = program == Some(at) || taps.iter().any(|tap| !tap.gpu);
            encode.starved = false;
            let gpu_taps = taps.iter().filter(|tap| tap.gpu).count() as u64;
            if !frame.due || gpu_taps == 0 {
                continue;
            }
            match encode
                .ring
                .next(gpu, context, frame.size, encode.shown.as_ref())
            {
                Ok(Some(target)) => frame.nv12 = Some(target),
                Ok(None) => {
                    encode.starved = true;
                    missed = missed.saturating_add(gpu_taps);
                }
                Err(err) => {
                    for frame in &mut self.mixes {
                        frame.nv12 = None;
                    }
                    return Err(err);
                }
            }
        }
        Ok(missed)
    }

    /// Composites the due mixes of `shot` as it is at `at`, with the live values `live`
    /// applied, into what [`prepare`](Self::prepare) set out, and, on success, makes each
    /// the shown frame:
    /// its readback swapped into `shown`, and its encode target kept as the mix's
    /// `shown` one. Returns the GPU images the compositor let go of.
    fn draw(
        &mut self,
        draw: &mut DrawFn,
        shot: &Shot,
        live: &LiveParams,
        captures: &RenderCaptures,
        at: MediaTime,
    ) -> Result<Option<Reclaimed>, DrawError> {
        let drawn = draw(shot, live, captures, &mut self.mixes, at);
        for ((frame, shown), encode) in self
            .mixes
            .iter_mut()
            .zip(&mut self.shown)
            .zip(&mut self.encode)
        {
            let target = frame.nv12.take();
            if frame.due && drawn.is_ok() {
                if frame.readback {
                    std::mem::swap(&mut frame.out, shown);
                }
                if let Some(target) = target {
                    encode.shown = Some(target);
                }
            }
        }
        drawn
    }
}

/// Composites on every base-rate tick of the media clock.
///
/// Each tick first adopts the render snapshot the control side published last, then the
/// live values on the parameter channel, which costs two pointer compares unless a new
/// one arrived ([`RenderReader::adopt`]); the thread never takes the collection lock. Its generation is stamped as the thread
/// starts and whenever it changes. The stopped marker is published when the loop ends,
/// and when a fatal error stops the drawing.
///
/// Recoverable: the capture mailbox failing or having no new frame repeats the last
/// capture, and a composite error repeats the last composited frame, up to
/// [`MAX_DRAW_ERRORS`] in a row. On the platform's context, [`GPU_FALLBACK_ERRORS`] in a
/// row leave the GPU path once ([`Switch`]): while the compositor opens on a device of
/// its own, the thread repeats the last frames, and the count starts again after it.
/// No fallback starts while a session takes the context's GPU frames
/// ([`Frames::feeds_gpu`]): the errors then run on to [`MAX_DRAW_ERRORS`].
/// Fatal, reported on the failure channel: more composite errors than
/// [`MAX_DRAW_ERRORS`], a lost context device while a session takes its GPU frames, an
/// encode ring that cannot be had ([`EncodeRing::next`]), a fallback whose compositor
/// does not open, panics or does not open in time, a panic inside a tick (the compositor's state is unknown), and a frame
/// time that overflows. After a fatal error the thread keeps pushing the last
/// frame until the pipeline stops, so the stream does not freeze before teardown. A
/// tick that falls a whole period or more behind the clock skips to the current tick
/// and counts the ticks it skipped in `render_ticks` (R-RT-004), so video pts stay on
/// the clock that stamps audio.
///
/// Each session the program hands the thread names its [`Feed`]: the mix to draw, its
/// size, and every how many base ticks. The thread draws each feed once per due tick,
/// all of them in one frame of the compositor's cache, and pushes it to every session
/// with that feed; a feed whose mix the collection has deleted draws the program. Every
/// virtual camera's tap gets the program at the program stage's size on every tick, so while a
/// camera runs that feed is drawn even with no session on it. A session that refuses
/// input because it has finished is let go of, and so is a tap whose output is stopping
/// or whose camera thread has ended, both to the collector; a feed with no session (and
/// for the program, no camera) left stops being drawn. The thread runs until the program
/// stops. While it has no session and no camera it draws nothing.
fn render_loop(job: RenderJob) {
    // `context` is bound first, so on an early return it drops last, after the captures.
    let RenderJob {
        context,
        gpu,
        mut captures,
        mut gpu_sources,
        mut reader,
        collector,
        open,
        open_timeout,
        clock,
        inputs,
        cams: cam_inputs,
        size,
        period,
        stop,
        privacy,
        ready,
        readbacks,
        gpu_captures,
        gpu_fallbacks,
        gpu_losses,
        off_context,
        drops,
        fail,
    } = job;
    let mut cams: Vec<VcamTap> = Vec::new();
    let mut draw = match open(context.as_deref()) {
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
    captures.set_on_gpu(context.is_some());
    // Whether the compositor is on the context, so it can still leave it.
    let mut on_context = context.is_some();
    // A fallback from the GPU path that is opening: no drawing until it is done.
    let mut switch: Option<Switch> = None;
    let mut frames = Frames {
        mixes: Vec::new(),
        shown: Vec::new(),
        targets: Vec::new(),
        encode: Vec::new(),
        program_held: false,
    };
    let mut next: Option<MediaTime> = None;
    // Base ticks since the thread started, skipped ones included: the mixes with a
    // divisor draw on every `divisor`th, or later when an overrun skips one.
    let mut index: u64 = 0;
    let mut skipped: u64 = 0;
    // Frames GPU sessions missed because their mix's every encode target was held, or
    // because a covered tick's draw failed (the privacy screen fails closed).
    let mut encode_held: u64 = 0;
    let mut draw_errors: u32 = 0;
    // When the device was lost, within the last `LOSS_WINDOW`: at most
    // `MAX_DEVICE_LOSSES`, as the third ends the drawing.
    let mut losses: Vec<Instant> = Vec::with_capacity(MAX_DEVICE_LOSSES);
    // Set by a fatal error: stop drawing and repeat `shown` until the pipeline stops. A
    // session that arrives afterwards with a new feed has nothing drawn to repeat, so
    // it gets no frames until the pipeline (already failing) stops.
    let mut broken = false;
    while !stop.load(Ordering::Relaxed) {
        for (feed, tap) in inputs.try_iter() {
            frames.add(feed, tap);
        }
        cams.extend(cam_inputs.try_iter());
        frames.hold_program(!cams.is_empty(), size);
        frames.prune(|garbage| collector.free(garbage));
        if frames.mixes.is_empty() {
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
        index = index.saturating_add(missed);
        let private = privacy.load(Ordering::Acquire);
        let drawn = frames.schedule(index, private);
        index = index.saturating_add(1);
        // Whether this tick's due frames were drawn; if not, the last ones repeat.
        let mut fresh = false;
        // Encode targets only while compositing on the context: after a fallback a
        // session on the GPU path has none, which fails the pipeline.
        if !broken && switch.is_none() && drawn > 0 {
            let on = context.as_deref().filter(|_| on_context);
            match frames.prepare(gpu.as_ref(), on, size, !cams.is_empty()) {
                Ok(missed) => encode_held = encode_held.saturating_add(missed),
                Err(err) => {
                    broken = true;
                    reader.stop();
                    report(&fail, err);
                }
            }
        }
        if broken {
            // Repeating the shown frames, below.
        } else if let Some(pending) = &switch {
            // The last frames repeat while the compositor opens; snapshots are still
            // adopted, so a gesture never waits on the fallback.
            let adopted = panic::catch_unwind(AssertUnwindSafe(|| {
                reader.adopt();
            }));
            let done = match adopted {
                Ok(()) => pending.poll(Instant::now()),
                Err(panic) => Some(Err(format!("panicked: {}", panic_message(&*panic)))),
            };
            if let Some(done) = done {
                let (why, leave_gpu) = (pending.why.clone(), pending.leave_gpu);
                switch = None;
                let switched = done.and_then(|own| {
                    panic::catch_unwind(AssertUnwindSafe(|| {
                        if leave_gpu {
                            captures.leave_gpu();
                        }
                        draw = own;
                    }))
                    .map_err(|panic| format!("panicked: {}", panic_message(&*panic)))
                });
                match switched {
                    Ok(()) => {
                        match &why {
                            SwitchWhy::Failing { cause, errors } => tracing::warn!(
                                error = %cause,
                                errors,
                                "compositing kept failing on the GPU path; fell back to CPU \
                                 capture on the render thread's own device"
                            ),
                            SwitchWhy::Lost { cause } => tracing::warn!(
                                reason = %cause,
                                platform = leave_gpu,
                                "the GPU device was lost; the compositor reopened on a new \
                                 device and the output kept streaming"
                            ),
                        }
                        if leave_gpu {
                            gpu_fallbacks.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(fell) => {
                        broken = true;
                        reader.stop();
                        report(&fail, why.failed(&fell));
                    }
                }
            }
        } else if drawn > 0 {
            let tick = panic::catch_unwind(AssertUnwindSafe(|| {
                reader.adopt();
                let (snapshot, live) = reader.held();
                let shot = snapshot.shot();
                // The shot's captures cover every mix's passes, so a display or window
                // only a video mix shows is started too.
                captures.tick(shot.captures(), now);
                frames.draw(&mut draw, shot, live, &captures, due)
            }));
            match tick {
                Ok(Ok(freed)) => {
                    fresh = true;
                    draw_errors = 0;
                    let read = frames
                        .mixes
                        .iter()
                        .filter(|frame| frame.due && frame.readback)
                        .count();
                    readbacks.fetch_add(read as u64, Ordering::Relaxed);
                    if let Some(freed) = freed {
                        reader.free(freed);
                    }
                }
                Ok(Err(DrawError::Lost(cause))) => {
                    // Nothing the compositor holds comes back, so it is replaced at once,
                    // without waiting for the error count (#349).
                    let at = Instant::now();
                    gpu_losses.fetch_add(1, Ordering::Relaxed);
                    losses.retain(|lost: &Instant| at.duration_since(*lost) < LOSS_WINDOW);
                    losses.push(at);
                    let why = SwitchWhy::Lost { cause };
                    let started = if losses.len() >= MAX_DEVICE_LOSSES {
                        Err(format!(
                            "it was lost {} times within {}s, so the engine stopped \
                             opening new devices",
                            losses.len(),
                            LOSS_WINDOW.as_secs()
                        ))
                    } else if on_context && frames.feeds_gpu() {
                        // The encode targets are the lost context's, and a compositor on
                        // a device of its own couldn't write new ones.
                        Err("an encoder takes the context's GPU frames, so the program \
                             can't leave it"
                            .to_owned())
                    } else {
                        let leave_gpu = on_context;
                        let sources = if leave_gpu { gpu_sources.take() } else { None };
                        Switch::start(&open, sources, open_timeout, why.clone(), leave_gpu)
                    };
                    on_context = false;
                    off_context.store(true, Ordering::Release);
                    draw_errors = 0;
                    match started {
                        Ok(started) => switch = Some(started),
                        Err(fell) => {
                            broken = true;
                            reader.stop();
                            report(&fail, why.failed(&fell));
                        }
                    }
                }
                Ok(Err(DrawError::Failed(err))) => {
                    draw_errors += 1;
                    if draw_errors == 1 {
                        tracing::warn!(error = %err, "composite failed; repeating the last frame");
                    }
                    // The CPU path has no encode targets for a session that takes the
                    // context's GPU frames, so while one runs there is no fallback: the
                    // errors run on to MAX_DRAW_ERRORS, as without one.
                    if on_context && draw_errors >= GPU_FALLBACK_ERRORS && !frames.feeds_gpu() {
                        on_context = false;
                        off_context.store(true, Ordering::Release);
                        let why = SwitchWhy::Failing {
                            cause: err,
                            errors: draw_errors,
                        };
                        let started = Switch::start(
                            &open,
                            gpu_sources.take(),
                            open_timeout,
                            why.clone(),
                            true,
                        );
                        match started {
                            Ok(started) => {
                                switch = Some(started);
                                draw_errors = 0;
                            }
                            Err(fell) => {
                                broken = true;
                                reader.stop();
                                report(&fail, why.failed(&fell));
                            }
                        }
                    } else if draw_errors > MAX_DRAW_ERRORS {
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
        } else {
            // No mix is due on this tick; it still adopts the latest snapshot, so a
            // gesture never waits on a mix with a divisor.
            reader.adopt();
        }
        if private && !fresh {
            // The privacy screen fails closed: a frame it didn't cover is never repeated,
            // black goes out instead, and GPU sessions get no frame (ADR-0027 §4).
            encode_held = encode_held.saturating_add(frames.blacken_due());
        }
        drops.publish(
            capture_drops(captures.stats())
                .saturating_add(render_tick_drops(skipped))
                .saturating_add(encode_frame_drops(encode_held)),
        );
        gpu_captures.store(captures.gpu_captures(), Ordering::Relaxed);
        for (((frame, shown), targets), encode) in frames
            .mixes
            .iter()
            .zip(&mut frames.shown)
            .zip(&mut frames.targets)
            .zip(&mut frames.encode)
        {
            // A missed frame is this tick's alone: a broken stage repeats the last one.
            let starved = std::mem::take(&mut encode.starved);
            if frame.due {
                push_each(
                    targets,
                    |tap| match (tap.gpu, &encode.shown) {
                        // Every target was held, or a covered draw failed: the frame
                        // is dropped, not repeated.
                        (true, _) if starved => true,
                        (true, Some(target)) => push_gpu(&tap.input, target, due),
                        (false, _) if !shown.is_empty() => {
                            push_frame(&tap.input, frame.size, due, shown)
                        }
                        _ => true,
                    },
                    |tap| collector.free(Garbage::Video(tap.input)),
                );
            }
        }
        // A camera whose output is stopping, or whose thread has ended, goes to the
        // collector.
        let program = frames.program_frame(size).map(|at| &frames.shown[at]);
        let mut at = 0;
        while at < cams.len() {
            let cam = &mut cams[at];
            let stays = match program {
                Some(shown) if !shown.is_empty() => cam.push(size, due, shown),
                _ => cam.stay(),
            };
            if stays {
                at += 1;
            } else {
                collector.free(Garbage::Vcam(cams.swap_remove(at)));
            }
        }
        next = match due.checked_add(period) {
            Ok(next) => Some(next),
            Err(err) => {
                report(&fail, format!("frame time overflow: {err}"));
                break;
            }
        };
    }
    // The held frames, the compositor and the streams all use the context's device: they
    // go before the context, as its platform state requires (README › GPU frames).
    captures.release_frames();
    drop(frames);
    drop(draw);
    captures.stop();
    drop(context);
}

/// A fallback from the GPU path mid-run (#451), opening: compositing kept failing on the
/// platform's context, because the captures' GPU frames failed to import or because the
/// context's device went away. A thread of its own takes the context away from the live
/// sources painting on it (which takes the collection lock) and opens the compositor
/// without the context (`open(None)`), so the render thread keeps repeating the last
/// frames meanwhile and can give up at a deadline: an adapter that hangs while opening
/// fails the output instead of freezing it (#577). A panic there comes back as an
/// error. When the deadline has passed the thread is left to finish on its own, and
/// drops what it opened there; none of it uses the context. Starting it spawns that
/// thread, once per program stage.
///
/// Once it is done, the render thread moves the captures to the CPU path
/// ([`RenderCaptures::leave_gpu`], which lets go of the held frames first) and puts the
/// new compositor in place of the old one, which goes on the render thread: it is owned
/// there and never shared, so it is not the collector's to free (R-RT-003). The context
/// itself stays until the render loop ends, after the captures have stopped, as its
/// platform state requires (README › GPU frames).
///
/// A lost device (#349) starts one too, at once: on the context it leaves the GPU path as
/// above, and on the render thread's own device it opens the compositor on a new one
/// (`open(None)` again, so the hardware adapter or, if that is gone, the software one).
/// The captures are already on the CPU path then, and stay there.
struct Switch {
    done: Receiver<Result<DrawFn, String>>,
    by: Instant,
    timeout: Duration,
    why: SwitchWhy,
    /// Whether the compositor was on the platform's context, so the captures move to the
    /// CPU path once it is done ([`RenderCaptures::leave_gpu`]).
    leave_gpu: bool,
}

/// What set a [`Switch`] off.
#[derive(Clone, Debug)]
enum SwitchWhy {
    /// Compositing on the platform's context kept failing: the last error, and how many
    /// there were in a row.
    Failing { cause: String, errors: u32 },
    /// The compositor's device was lost: why, as wgpu said.
    Lost { cause: String },
}

impl SwitchWhy {
    /// Why the output failed, when the switch did not finish: `fell`.
    fn failed(&self, fell: &str) -> String {
        match self {
            Self::Failing { cause, errors } => format!(
                "compositing failed {errors} times in a row on the GPU path ({cause}), and \
                 falling back failed: {fell}"
            ),
            Self::Lost { cause } => format!(
                "the GPU was lost ({cause}), and the compositor could not reopen on a new \
                 device: {fell}"
            ),
        }
    }
}

impl Switch {
    fn start(
        open: &OpenDraw,
        gpu_sources: Option<GpuSources>,
        timeout: Duration,
        why: SwitchWhy,
        leave_gpu: bool,
    ) -> Result<Self, String> {
        let (sent, done) = mpsc::channel();
        let open = Arc::clone(open);
        let by = Instant::now() + timeout;
        spawn("jstream-gpu-fallback", move || {
            let opened = panic::catch_unwind(AssertUnwindSafe(|| {
                if let Some(sources) = gpu_sources {
                    sources.release();
                }
                open(None)
            }))
            .unwrap_or_else(|panic| Err(format!("panicked: {}", panic_message(&*panic))));
            // Gone once the render thread gave up; what opened then drops here.
            let _ = sent.send(opened);
        })?;
        Ok(Self {
            done,
            by,
            timeout,
            why,
            leave_gpu,
        })
    }

    /// The compositor the fallback opened, or why it failed, once it is done or out of
    /// time. `None` while it is still opening.
    fn poll(&self, now: Instant) -> Option<Result<DrawFn, String>> {
        match self.done.try_recv() {
            Ok(done) => Some(done),
            Err(TryRecvError::Empty) if now < self.by => None,
            Err(TryRecvError::Empty) => Some(Err(format!(
                "the compositor did not open on a device of its own within {}s",
                self.timeout.as_secs()
            ))),
            Err(TryRecvError::Disconnected) => {
                Some(Err("the fallback thread stopped before it was done".into()))
            }
        }
    }
}

/// Whether `frame` is the program at the program stage's `size` on every base tick: what an
/// output with no mix, and every virtual camera, is fed.
fn is_program(frame: &MixFrame, size: FrameSize) -> bool {
    frame.mix.is_none() && frame.size == size && frame.divisor == 1 && frame.owner.is_none()
}

/// One mix's encode targets, for the sessions on the GPU path it feeds.
#[derive(Default)]
struct MixEncode {
    ring: EncodeRing,
    /// The target the last frame was converted into, repeated as `shown` is.
    shown: Option<GpuFrame>,
    /// Whether every target was held on this tick, or a covered tick's draw failed, so
    /// its GPU sessions miss the frame rather than get the last one again.
    starved: bool,
}

/// The encode targets of one feed ([`encode_ring_len`] of them), used in turn.
#[derive(Default)]
struct EncodeRing {
    slots: Vec<GpuFrame>,
    next: usize,
}

impl EncodeRing {
    /// The target to convert the next frame into, allocating the ring on first use:
    /// the next one only the ring holds, besides `shown` (the last frame converted, held
    /// to repeat it). `None` when a queued or encoding frame holds every target: the
    /// caller drops the frame rather than rewrite one (R-RT-004).
    ///
    /// # Errors
    /// No context, which a session on the GPU path cannot be opened without, or the
    /// platform refusing a target. Either is fatal: the session cannot be fed.
    fn next(
        &mut self,
        gpu: &dyn GpuInterop,
        context: Option<&GpuContext>,
        size: FrameSize,
        shown: Option<&GpuFrame>,
    ) -> Result<Option<GpuFrame>, String> {
        // Checked on every call: targets left from before a fallback are the context's,
        // which the compositor on its own device can't write.
        let context = context.ok_or("a GPU encoder session without the platform's GPU context")?;
        if self.slots.is_empty() {
            for _ in 0..encode_ring_len() {
                let target = gpu
                    .encode_target(context, size)
                    .map_err(|err| format!("allocating an encode target: {err}"))?;
                self.slots.push(target);
            }
        }
        let len = self.slots.len();
        for step in 0..len {
            let at = (self.next + step) % len;
            let sync = self.slots[at].sync();
            let shown = shown.is_some_and(|frame| Arc::ptr_eq(frame.sync(), sync));
            if Arc::strong_count(sync) == 1 + usize::from(shown) {
                self.next = (at + 1) % len;
                return Ok(Some(self.slots[at].clone()));
            }
        }
        Ok(None)
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

/// Pushes the encode target `frame` holds, stamped `pts`. `false` when the session has
/// closed.
fn push_gpu(input: &VideoInput, frame: &GpuFrame, pts: MediaTime) -> bool {
    match input.push(VideoSurface::Gpu(frame.clone().with_pts(pts))) {
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

/// Where the mixer thread's blocks go.
struct MixOut {
    /// The encoder sessions the program hands the thread.
    inputs: Receiver<AudioTap>,
    /// The blocks' format.
    format: AudioFormat,
    /// What the `waveform` and `spectrum` sources draw: the program mix's scope gets
    /// each block.
    scopes: Arc<AudioScopes>,
    /// The bus and source scopes, each fed its bus's or input's block.
    taps: Arc<ScopeTaps>,
}

/// The mixer thread's boundary: a panic is caught and reported as fatal. The encoders
/// then get no more audio, so the outputs cannot keep running.
fn mixer_thread(
    mixing: Mixing,
    clock: Arc<dyn MediaClock>,
    out: &MixOut,
    stop: Arc<AtomicBool>,
    fail: Sender<PipelineEvent>,
    collector: &Collector,
) {
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        mixer_loop(mixing, clock.as_ref(), out, &stop, &fail, collector);
    }));
    if let Err(panic) = result {
        report(
            &fail,
            format!("the mixer thread panicked: {}", panic_message(&*panic)),
        );
    }
}

/// What the mixer thread owns: the mixer, and the master's true-peak and loudness
/// meters, which it runs on every block.
struct Mixing {
    mixer: Mixer,
    metering: MasterMetering,
}

/// Mixes on the media clock, meters the master, and pushes each block into the program
/// mix's audio scope and each drawn bus's and source's block into its scope (pushes
/// neither lock nor allocate, and the list of scopes is adopted lock-free), and into every
/// encoder session the program has handed it, letting go of one that has finished: the
/// master bus's block, or for an output tap's session that tap's block, from its origin
/// on. A rejected block or a poll error is fatal: both mean the mixer and the encoder
/// disagree about the format, which no retry fixes.
fn mixer_loop(
    Mixing {
        mut mixer,
        mut metering,
    }: Mixing,
    clock: &dyn MediaClock,
    out: &MixOut,
    stop: &AtomicBool,
    fail: &Sender<PipelineEvent>,
    collector: &Collector,
) {
    let MixOut {
        inputs,
        format,
        scopes,
        taps,
    } = out;
    let format = *format;
    let scope = scopes.program();
    let mut scope_taps = taps.reader();
    let samples_len = mixer.config().block_frames.saturating_mul(2);
    let mut samples = vec![0.0f32; samples_len];
    // What a silent drawn source's scope gets.
    let silence = vec![0.0f32; samples_len];
    let mut targets: Vec<AudioTap> = Vec::new();
    'mix: while !stop.load(Ordering::Relaxed) {
        match mixer.poll(clock.now(), &mut samples) {
            Ok(Some(block)) => {
                metering.process(&samples);
                scope.push(format.sample_rate(), &samples);
                let (drawn, replaced) = scope_taps.adopt();
                for tap in drawn {
                    let block = match tap.feed {
                        TapFeed::Bus(bus) => mixer.bus_block(bus),
                        TapFeed::Input(input) => mixer.input_block(input),
                        TapFeed::Silence => Some(&silence[..]),
                    };
                    if let Some(block) = block {
                        tap.scope.push(format.sample_rate(), block);
                    }
                }
                if let Some(replaced) = replaced {
                    collector.free(Garbage::Scopes(replaced));
                }
                targets.extend(inputs.try_iter());
                let mut rejected = None;
                // The mix itself does not allocate. `AudioInput::push` takes an owned
                // buffer and keeps it, so each block needs its own per session: this
                // clone is forced by that API. `Full` is counted by the session.
                // `Closed` means the session finished; the outputs on it report an
                // early end themselves.
                push_each(
                    &mut targets,
                    |target| {
                        let (pts, block) = match target.tap {
                            None => (block.pts, samples.as_slice()),
                            Some(tap) => match mixer.output_block(tap) {
                                Some(tapped) => {
                                    target.seen.set(true);
                                    tapped
                                }
                                // Not installed yet, or removed once its output stopped.
                                None => return !target.seen.get(),
                            },
                        };
                        if pts < target.from {
                            return true;
                        }
                        match AudioBuffer::new(format, pts, block.to_vec()) {
                            Ok(buffer) => {
                                !matches!(target.input.push(buffer), Err(PushError::Closed))
                            }
                            Err(err) => {
                                rejected = Some(err);
                                true
                            }
                        }
                    },
                    |target| collector.free(Garbage::Audio(target.input)),
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
    collector.free(Garbage::Scopes(scope_taps.into_held()));
}

/// An output thread's encoder subscriptions, shared with the stop so that, when it
/// stops waiting for a thread blocked in a send, it can drop and count what is queued
/// itself (#385). Each is locked only for a receive, never over a send, so a thread
/// blocked in a send leaves them free.
struct Queues {
    video: Mutex<Subscription>,
    audio: Mutex<Subscription>,
    /// Held by whichever side drops and counts what is queued, until its count is in
    /// `output_drops`; the stop also holds it over its latch. So a count the thread
    /// makes is either wholly in the latch or wholly after it, and then covers only
    /// packets queued after the stop's count. (No packet is counted twice in any case:
    /// each count takes what it counts off the queue.)
    abandon: Mutex<()>,
}

impl Queues {
    /// Drops the packets still queued on both streams, adds how many to `drops`, and
    /// returns that count with the lock still held. A stream already `done` is skipped.
    fn abandon(
        &self,
        video_done: bool,
        audio_done: bool,
        drops: &AtomicU64,
    ) -> (u64, MutexGuard<'_, ()>) {
        let held = lock(&self.abandon);
        let left = discard(&lock(&self.video), video_done)
            .saturating_add(discard(&lock(&self.audio), audio_done));
        drops.fetch_add(left, Ordering::Relaxed);
        (left, held)
    }
}

struct OutputJob {
    sink: Sink,
    queues: Arc<Queues>,
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
/// them in `output_drops` (unless the stop, done waiting, has counted them already), and
/// finishes the target, which past the deadline closes
/// without its goodbye. A failure while stopping is logged, not reported: the output
/// is already on its way to `stopped`. An output leaving encoder sessions that other
/// outputs still use is `cut` instead of waiting for the streams to end: it sends what
/// is queued for it and finishes as soon as a pass finds nothing more.
fn output_loop(job: OutputJob) {
    let OutputJob {
        mut sink,
        queues,
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
            // Bound first, so the lock is not held over the send.
            let received = lock(&queues.video).recv_timeout(Duration::from_millis(2));
            match received {
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
        // Checked again between the two sends, so a target that ignores the deadline
        // keeps the thread past it by one send at most, inside `OUTPUT_GRACE`, and
        // what is left is still counted before the stop reads the final drops.
        if drain.is_some_and(|deadline| Instant::now() >= deadline) {
            abandoned = true;
            break;
        }
        if failed.is_none() && !audio_done {
            let received = lock(&queues.audio).recv_timeout(Duration::from_millis(2));
            match received {
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
        // One lock at a time.
        let video_stats = lock(&queues.video).stats();
        let audio_stats = lock(&queues.audio).stats();
        *lock(&stats.video_subscriber) = video_stats;
        *lock(&stats.audio_subscriber) = audio_stats;
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
        let (left, held) = queues.abandon(video_done, audio_done, &output_drops);
        drop(held);
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
    use super::{
        DrawFn, EncodeRing, Feed, Frames, GpuSources, MixFrame, OpenDraw, Switch, SwitchWhy,
        VideoTap, catch_up, encode_ring_len, lock, open_video, output_size, push_each, stream_info,
        video_config,
    };
    use crate::runtime::captures::rgba_from_surface;
    use crate::snapshot::{CaptureFrame, CaptureImage, Captures, LiveParams, Shot};
    use jstream_media::{
        Codec, ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, Plane,
        Timebase, VideoFrame, VideoFrameInfo,
    };
    use jstream_platform::GpuContext;
    use jstream_platform::wgpu;
    use jstream_platform::{HardwarePreference, RateControl, VideoSurface};
    use jstream_protocol::{
        EncoderProfile, HardwarePreference as ProfileHardware, OutputId,
        RateControl as ProfileRate, VideoCodec, VideoMixId,
    };
    use jstream_sources::CaptureSpec;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    /// Encode targets on a software device: plain textures with a sync that does
    /// nothing, enough for the ring's bookkeeping.
    struct Targets(jstream_render::Compositor);

    #[derive(Debug)]
    struct NoSync;

    impl jstream_platform::FrameSync for NoSync {
        fn stage(&self, _queue: &wgpu::Queue) -> Result<(), jstream_platform::PlatformError> {
            Ok(())
        }
    }

    impl jstream_platform::GpuInterop for Targets {
        fn open_device(
            &self,
        ) -> Result<jstream_platform::GpuContext, jstream_platform::PlatformError> {
            Ok(jstream_platform::GpuContext::new(
                self.0.adapter().clone(),
                self.0.device().clone(),
                self.0.queue().clone(),
                jstream_platform::AdapterId {
                    luid: None,
                    vendor: 0,
                },
                None,
            ))
        }

        fn encode_target(
            &self,
            context: &jstream_platform::GpuContext,
            size: FrameSize,
        ) -> Result<jstream_platform::GpuFrame, jstream_platform::PlatformError> {
            let texture = context.device().create_texture(&wgpu::TextureDescriptor {
                label: Some("jstream-core-test-ring"),
                size: wgpu::Extent3d {
                    width: size.width(),
                    height: size.height(),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let info = VideoFrameInfo {
                size,
                format: PixelFormat::Nv12,
                color: ColorInfo::default(),
                pts: MediaTime::zero(Timebase::MICROS),
            };
            Ok(jstream_platform::GpuFrame::new(
                texture,
                info,
                std::sync::Arc::new(NoSync),
            ))
        }
    }

    /// The ring never hands out a target a queued or encoding frame still holds: with
    /// every target held it gives none (the tick's frame is dropped), and a target comes
    /// back once its frames are let go of. The last frame shown, which the render thread
    /// holds to repeat, does not count as held.
    #[test]
    fn the_encode_ring_skips_targets_a_frame_still_holds() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let compositor =
            match jstream_render::Compositor::open(jstream_render::OpenOptions::headless()) {
                Ok(compositor) => compositor,
                Err(err) => {
                    assert!(
                        std::env::var_os("CI").is_none(),
                        "CI has no wgpu adapter: {err}"
                    );
                    return;
                }
            };
        let gpu = Targets(compositor);
        let context = jstream_platform::GpuInterop::open_device(&gpu).expect("context");
        let size = FrameSize::new(4, 2).expect("size");
        let mut ring = EncodeRing::default();
        let mut queued = Vec::new();
        for _ in 0..encode_ring_len() {
            let frame = ring
                .next(&gpu, Some(&context), size, None)
                .expect("ring")
                .expect("a free target");
            assert!(
                queued
                    .iter()
                    .all(|q: &jstream_platform::GpuFrame| !std::sync::Arc::ptr_eq(
                        q.sync(),
                        frame.sync()
                    )),
                "a held target was handed out again"
            );
            queued.push(frame);
        }
        // The encoder has fallen behind: every target is queued.
        assert!(
            ring.next(&gpu, Some(&context), size, None)
                .expect("ring")
                .is_none()
        );
        // The shown frame alone does not hold its target.
        let shown = queued.pop().expect("one");
        let again = ring
            .next(&gpu, Some(&context), size, Some(&shown))
            .expect("ring")
            .expect("the shown target is free");
        assert!(std::sync::Arc::ptr_eq(again.sync(), shown.sync()));
        drop(again);
        // A frame the encoder read comes back; the others stay held.
        let read = queued.remove(0);
        let read_at = std::sync::Arc::as_ptr(read.sync()).cast::<()>();
        drop(read);
        let freed = ring
            .next(&gpu, Some(&context), size, Some(&shown))
            .expect("ring")
            .expect("the read target is free");
        assert_eq!(std::sync::Arc::as_ptr(freed.sync()).cast::<()>(), read_at);
        drop(freed);
        assert!(
            ring.next(&gpu, Some(&context), size, Some(&shown))
                .expect("ring")
                .is_some(),
            "the shown target is free again"
        );
    }

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
        let size = &Feed {
            mix: None,
            size: FrameSize::new(64, 36).expect("size"),
            divisor: 1,
            owner: None,
        };
        let config = video_config(size, &EncoderProfile::DEFAULT).expect("default");
        assert_eq!(config.rate_control, RateControl::Cbr { kbps: 2500 });
        assert_eq!(config.keyframe_interval, 60);
        assert_eq!(config.hardware, HardwarePreference::Prefer);

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
        profile.video.hardware = ProfileHardware::Avoid;
        let config = video_config(size, &profile).expect("avoid");
        assert_eq!(config.hardware, HardwarePreference::Avoid);
        profile.video.rate_control = ProfileRate::Unknown;
        assert!(video_config(size, &profile).is_err());

        let mut profile = EncoderProfile::DEFAULT;
        assert_eq!(
            video_config(size, &profile).expect("h264").codec,
            Codec::H264
        );
        profile.video.codec = VideoCodec::Unknown;
        assert!(video_config(size, &profile).is_err());
        assert!(stream_info(size, MediaTime::zero(Timebase::MICROS), &profile).is_err());
    }

    /// #433: the program renders and encodes at the canvas, rounded down to even sizes
    /// for NV12, whatever the display's size.
    #[test]
    fn the_output_size_follows_the_canvas() {
        let size = |w, h| FrameSize::new(w, h).expect("size");
        assert_eq!(output_size((1280, 720)), Ok(size(1280, 720)));
        assert_eq!(output_size((1281, 721)), Ok(size(1280, 720)));
        assert_eq!(output_size((1, 1)), Ok(size(2, 2)));
    }

    fn frame(
        format: PixelFormat,
        w: u32,
        h: u32,
        color: ColorInfo,
        planes: Vec<Plane>,
    ) -> VideoSurface {
        let info = VideoFrameInfo {
            size: FrameSize::new(w, h).expect("size"),
            format,
            color,
            pts: MediaTime::zero(Timebase::NANOS),
        };
        VideoSurface::Cpu(VideoFrame::new(info, planes).expect("frame"))
    }

    /// An NV12 capture of 2x2 blocks: white, black, red and a mid grey, BT.709 limited
    /// range (what Windows' display capture delivers), rows padded.
    fn nv12_blocks() -> VideoSurface {
        let luma = [235, 235, 16, 16, 63, 63, 126, 126, 0xEE, 0xEE];
        let chroma = [128, 128, 128, 128, 102, 240, 128, 128, 0xEE, 0xEE];
        let mut y = Vec::new();
        y.extend_from_slice(&luma);
        y.extend_from_slice(&luma);
        frame(
            PixelFormat::Nv12,
            8,
            2,
            ColorInfo::default(),
            vec![Plane::new(y, 10), Plane::new(chroma.to_vec(), 10)],
        )
    }

    /// #433: a capture converts at its own size whatever the output's, and NV12 is
    /// converted with its matrix and range.
    #[test]
    fn captures_convert_to_rgba_at_their_own_size() {
        let mut out = Vec::new();
        let size = rgba_from_surface(&nv12_blocks(), &mut out).expect("NV12");
        assert_eq!(size, FrameSize::new(8, 2).expect("size"));
        assert_eq!(out.len(), 8 * 2 * 4);
        let px = |x: usize| &out[x * 4..x * 4 + 4];
        assert_eq!(px(0), [255, 255, 255, 255]);
        assert_eq!(px(2), [0, 0, 0, 255]);
        let red = px(4);
        assert!(red[0] >= 253 && red[1] <= 2 && red[2] <= 2, "red {red:?}");
        assert_eq!(px(6), [128, 128, 128, 255]);
        assert_eq!(&out[32..], &out[..32], "both rows share the chroma");

        // Full range: luma as is.
        let full = ColorInfo {
            space: ColorSpace::Bt709,
            range: ColorRange::Full,
        };
        let grey = frame(
            PixelFormat::Nv12,
            2,
            2,
            full,
            vec![Plane::new(vec![200; 4], 2), Plane::new(vec![128; 2], 2)],
        );
        rgba_from_surface(&grey, &mut out).expect("NV12 full");
        assert_eq!(&out[..4], [200, 200, 200, 255]);

        // BGRA is swizzled, and its alpha read as opaque (#451); another format is
        // refused and leaves `out` alone.
        let bgra = frame(
            PixelFormat::Bgra8,
            1,
            1,
            ColorInfo::default(),
            vec![Plane::new(vec![1, 2, 3, 4], 4)],
        );
        rgba_from_surface(&bgra, &mut out).expect("BGRA");
        assert_eq!(out, [3, 2, 1, 255]);
        let rgba = frame(
            PixelFormat::Rgba8,
            1,
            1,
            ColorInfo::default(),
            vec![Plane::new(vec![1, 2, 3, 4], 4)],
        );
        rgba_from_surface(&rgba, &mut out).expect("RGBA");
        assert_eq!(out, [1, 2, 3, 255]);
        let i420 = VideoSurface::Cpu(
            VideoFrame::zeroed(VideoFrameInfo {
                size: FrameSize::new(2, 2).expect("size"),
                format: PixelFormat::I420,
                color: ColorInfo::default(),
                pts: MediaTime::zero(Timebase::NANOS),
            })
            .expect("frame"),
        );
        assert_eq!(rgba_from_surface(&i420, &mut out), None);
        assert_eq!(out, [1, 2, 3, 255]);
    }

    /// Frames with no mix and no session yet.
    fn no_frames() -> Frames {
        Frames {
            mixes: Vec::new(),
            shown: Vec::new(),
            targets: Vec::new(),
            encode: Vec::new(),
            program_held: false,
        }
    }

    /// A tap for a session on the CPU path.
    fn cpu_tap(input: jstream_encode::VideoInput) -> VideoTap {
        VideoTap { input, gpu: false }
    }

    /// A mix's size and divisor set the encoder's frame size and period, and its
    /// keyframe interval counts frames at the mix's rate.
    #[test]
    fn a_mix_sets_the_encoders_size_and_rate() {
        let feed = Feed {
            mix: Some(VideoMixId::new("half")),
            size: FrameSize::new(1280, 720).expect("size"),
            divisor: 2,
            owner: None,
        };
        let config = video_config(&feed, &EncoderProfile::DEFAULT).expect("default");
        assert_eq!(config.size, feed.size);
        assert_eq!(config.frame_period, Timebase::new(2, 30).expect("timebase"));
        // 2 s at 15 fps.
        assert_eq!(config.keyframe_interval, 30);
        let slow = Feed {
            divisor: 60,
            ..feed.clone()
        };
        let config = video_config(&slow, &EncoderProfile::DEFAULT).expect("slow");
        assert_eq!(config.keyframe_interval, 1, "at least one frame");
        assert_eq!(
            stream_info(&feed, us(0), &EncoderProfile::DEFAULT)
                .expect("h264")
                .frame_rate,
            15.0
        );
    }

    /// Sessions with the same feed share one drawn mix; a mix whose sessions are gone
    /// stops being drawn; the divisor decides on which base ticks a mix is due.
    #[test]
    fn frames_draw_each_feed_once_on_its_ticks() {
        let size = FrameSize::new(64, 36).expect("size");
        let feed = |mix: Option<&str>, divisor| Feed {
            mix: mix.map(VideoMixId::new),
            size,
            divisor,
            owner: None,
        };
        let mut frames = no_frames();
        let session = open_video(
            &jstream_platform::fake::FakeCodecs,
            None,
            &feed(None, 1),
            &EncoderProfile::DEFAULT,
        )
        .expect("fake session");
        let input = || cpu_tap(session.input());
        frames.add(feed(None, 1), input());
        frames.add(feed(Some("half"), 2), input());
        frames.add(feed(None, 1), input());
        assert_eq!(frames.mixes.len(), 2);
        assert_eq!(frames.targets[0].len(), 2);
        assert_eq!(frames.schedule(0, false), 2);
        assert_eq!(frames.schedule(1, false), 1);
        assert!(frames.mixes[0].due && !frames.mixes[1].due);
        // An overrun that skips tick 2 and lands on 3 still draws the half-rate mix,
        // so a steady overrun landing on odd ticks can't starve it (review N5), and
        // the tick after that is too soon for it. The privacy screen covers every
        // frame due while it is on.
        assert_eq!(frames.schedule(3, false), 2);
        assert_eq!(frames.schedule(4, true), 1);
        assert!(frames.mixes.iter().all(|frame| frame.privacy));
        assert_eq!(frames.schedule(5, false), 2);
        frames.targets[0].clear();
        let mut freed = 0;
        frames.prune(|_| freed += 1);
        assert_eq!(
            freed, 4,
            "the session list, both buffers and the encode targets"
        );
        assert_eq!(frames.mixes.len(), 1);
        assert_eq!(frames.mixes[0].mix, Some(VideoMixId::new("half")));
        assert_eq!(frames.shown.len(), 1);
    }

    /// An output with its own video chain gets its own drawn frame even when another
    /// session shows the same mix at the same size and rate (ADR-0027 §3).
    #[test]
    fn an_owned_feed_is_drawn_on_its_own() {
        let shared = Feed {
            mix: None,
            size: FrameSize::new(64, 36).expect("size"),
            divisor: 1,
            owner: None,
        };
        let owned = Feed {
            owner: Some(OutputId::new("rec")),
            ..shared.clone()
        };
        let mut frames = no_frames();
        let session = open_video(
            &jstream_platform::fake::FakeCodecs,
            None,
            &shared,
            &EncoderProfile::DEFAULT,
        )
        .expect("fake session");
        frames.add(shared.clone(), cpu_tap(session.input()));
        frames.add(owned.clone(), cpu_tap(session.input()));
        frames.add(owned, cpu_tap(session.input()));
        frames.add(shared, cpu_tap(session.input()));
        assert_eq!(frames.mixes.len(), 2);
        assert_eq!(frames.mixes[1].owner, Some(OutputId::new("rec")));
        assert_eq!((frames.targets[0].len(), frames.targets[1].len()), (2, 2));
        assert_eq!(frames.schedule(0, false), 2, "both are drawn");
    }

    /// Under the privacy screen, a tick that could not draw blackens the frames due on
    /// it rather than repeating what they showed, and their GPU sessions miss the frame
    /// rather than get the last encode target again; frames not due keep theirs.
    #[test]
    fn the_privacy_screen_fails_closed() {
        let feed = |divisor| Feed {
            mix: None,
            size: FrameSize::new(2, 1).expect("size"),
            divisor,
            owner: None,
        };
        let mut frames = no_frames();
        let session = open_video(
            &jstream_platform::fake::FakeCodecs,
            None,
            &feed(1),
            &EncoderProfile::DEFAULT,
        )
        .expect("fake session");
        frames.add(feed(1), cpu_tap(session.input()));
        frames.add(feed(2), cpu_tap(session.input()));
        // A session on the GPU path on the first feed too.
        frames.add(
            feed(1),
            VideoTap {
                input: session.input(),
                gpu: true,
            },
        );
        assert_eq!(frames.schedule(0, false), 2);
        for shown in &mut frames.shown {
            *shown = vec![200; 8];
        }
        assert_eq!(frames.schedule(1, true), 1);
        assert_eq!(frames.blacken_due(), 1, "the GPU session misses the frame");
        assert_eq!(frames.shown[0], [0, 0, 0, 255, 0, 0, 0, 255]);
        assert!(
            frames.encode[0].starved,
            "its last encode target isn't repeated"
        );
        assert_eq!(frames.shown[1], [200; 8], "not due, not pushed");
        assert!(!frames.encode[1].starved);
    }

    /// A session on the GPU path keeps the render thread from falling back off the
    /// context, since the CPU path has no encode targets to feed it; CPU sessions don't.
    #[test]
    fn only_a_gpu_session_rules_out_the_fallback() {
        let feed = || Feed {
            mix: None,
            size: FrameSize::new(2, 1).expect("size"),
            divisor: 1,
            owner: None,
        };
        let mut frames = no_frames();
        let session = open_video(
            &jstream_platform::fake::FakeCodecs,
            None,
            &feed(),
            &EncoderProfile::DEFAULT,
        )
        .expect("fake session");
        assert!(!frames.feeds_gpu());
        frames.add(feed(), cpu_tap(session.input()));
        assert!(!frames.feeds_gpu(), "a CPU session can follow a fallback");
        frames.add(
            feed(),
            VideoTap {
                input: session.input(),
                gpu: true,
            },
        );
        assert!(frames.feeds_gpu());
    }

    /// The CPU stand-in draws a covered frame black whatever the capture's size, and
    /// copies or scales the capture into an uncovered one (review B2).
    #[test]
    fn the_cpu_stand_in_draws_covered_frames_black() {
        struct One<'a>(CaptureImage<'a>);
        impl Captures for One<'_> {
            fn frame(&self, _: &CaptureSpec) -> Option<CaptureFrame<'_>> {
                Some(CaptureFrame {
                    version: 1,
                    image: self.0,
                })
            }
        }
        let mut draw = (super::cpu_draw())(None).expect("cpu draw");
        let shot = Shot::default();
        let size = FrameSize::new(2, 1).expect("size");
        let frame = |privacy| MixFrame {
            due: true,
            privacy,
            readback: true,
            ..MixFrame::new(None, size, 1, None)
        };
        let same = [200; 8];
        let other = [100; 16];
        for (own, rgba) in [
            (size, &same[..]),
            (FrameSize::new(2, 2).expect("size"), &other[..]),
        ] {
            let captures = One(CaptureImage::Rgba { size: own, rgba });
            let mut frames = [frame(true), frame(false)];
            draw(
                &shot,
                &LiveParams::default(),
                &captures,
                &mut frames,
                MediaTime::zero(Timebase::NANOS),
            )
            .expect("draw");
            assert_eq!(frames[0].out, [0; 8], "covered, from a {own:?} capture");
            assert_eq!(
                frames[1].out,
                rgba[..8],
                "uncovered, from a {own:?} capture"
            );
        }
    }

    /// A virtual camera holds the program at the program stage's size: its frame is drawn on
    /// every tick with no session on it, shared with an output that shows no mix, and
    /// dropped once neither needs it.
    #[test]
    fn a_camera_holds_the_program_frame() {
        let size = FrameSize::new(64, 36).expect("size");
        let mut frames = no_frames();
        frames.hold_program(true, size);
        frames.hold_program(true, size);
        frames.prune(|_| panic!("held"));
        assert_eq!(frames.program_frame(size), Some(0));
        assert_eq!(frames.mixes.len(), 1);
        assert_eq!(frames.schedule(1, false), 1, "drawn on every tick");

        let program = Feed {
            mix: None,
            size,
            divisor: 1,
            owner: None,
        };
        let session = open_video(
            &jstream_platform::fake::FakeCodecs,
            None,
            &program,
            &EncoderProfile::DEFAULT,
        )
        .expect("fake session");
        frames.add(program, cpu_tap(session.input()));
        assert_eq!(
            frames.mixes.len(),
            1,
            "the session joins the camera's frame"
        );

        frames.targets[0].clear();
        frames.prune(|_| panic!("still held"));
        frames.hold_program(false, size);
        let mut freed = 0;
        frames.prune(|_| freed += 1);
        assert_eq!((freed, frames.mixes.len()), (4, 0));
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

    /// Polls `switch` until it is done, or panics after `by`.
    fn finish(switch: &Switch, by: Duration) -> Result<DrawFn, String> {
        let deadline = Instant::now() + by;
        loop {
            if let Some(done) = switch.poll(Instant::now()) {
                return done;
            }
            assert!(Instant::now() < deadline, "the switch never finished");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn failing() -> SwitchWhy {
        SwitchWhy::Failing {
            cause: "x".into(),
            errors: 10,
        }
    }

    /// A fallback takes the context from the live sources and opens the compositor
    /// without it on a thread of its own; a panic there, or an open past the deadline,
    /// comes back as an error instead of unwinding or blocking the render thread (#577).
    #[test]
    fn a_switch_off_the_gpu_path_reports_a_panic_or_a_hang() {
        let long = Duration::from_secs(30);
        let opens: OpenDraw = Arc::new(|context: Option<&GpuContext>| {
            assert!(context.is_none(), "opened on the context");
            let draw: DrawFn = Box::new(|_, _, _, _, _| Ok(None));
            Ok(draw)
        });
        let released = Arc::new(AtomicBool::new(false));
        let sources = GpuSources::new(Box::new({
            let released = Arc::clone(&released);
            move || released.store(true, Ordering::SeqCst)
        }));
        let switch = Switch::start(&opens, Some(sources), long, failing(), true).expect("start");
        assert!(finish(&switch, long).is_ok(), "opened");
        assert!(
            released.load(Ordering::SeqCst),
            "the sources lost the context"
        );

        let panics: OpenDraw = Arc::new(|_: Option<&GpuContext>| panic!("adapter lost"));
        let switch = Switch::start(&panics, None, long, failing(), true).expect("start");
        let Err(err) = finish(&switch, long) else {
            panic!("a panicking open succeeded");
        };
        assert!(err.contains("panicked: adapter lost"), "{err}");

        let (release, hold) = mpsc::channel::<()>();
        let hold = Mutex::new(hold);
        let hangs: OpenDraw = Arc::new(move |_: Option<&GpuContext>| {
            let _ = lock(&hold).recv();
            Err("released".into())
        });
        let timeout = Duration::from_millis(50);
        let switch = Switch::start(&hangs, None, timeout, failing(), true).expect("start");
        assert!(switch.poll(Instant::now()).is_none(), "still opening");
        let Err(err) = finish(&switch, long) else {
            panic!("a hung open succeeded");
        };
        assert!(err.contains("did not open"), "{err}");
        drop(release);
    }
}
