//! The output pipeline's captures: one per thing the render snapshot captures
//! ([`Shot::captures`]), started and stopped as the program mix changes. A display or
//! window is captured through the capture backend, one capture per source
//! ([`CaptureKey`]), started with the source's [`CaptureOptions`], so two sources showing
//! the same target run two captures of it. A source whose `show_cursor` is off asks for
//! the pointer to be left out ([`CursorCapture::Hidden`]); the engine never draws it. A
//! webcam or capture card is captured through the video-device backend, on the CPU path,
//! in a format [`device_format`] picks, MJPEG decoded by the platform's capture decoder;
//! sources showing the same device share its one capture, since a device opens once.
//!
//! The render thread reads each capture's mailbox every tick and keeps its latest frame,
//! a texture on the compositor's device or CPU RGBA8. Starting and stopping a capture
//! are OS calls that can block, so they run on the pipeline's `jstream-capture` thread.
//! The render thread asks it to start what a new snapshot captures and hands it the
//! streams it no longer reads, through channels it never waits on (R-RT-002,
//! R-RT-003). A capture that fails to start, or whose target goes away, keeps showing
//! its last frame and is started again after [`RETRY_MS`]. When the render thread leaves
//! the GPU path mid-run ([`RenderCaptures::leave_gpu`]), every display and window
//! capture restarts on the CPU path. A webcam or capture card, already there, runs on.
//!
//! Each capture's frame size, which `source.sizes` reports for its sources (ADR-0022
//! §4), goes back to the engine as a [`SizeChange`] on a channel the render thread never
//! waits on, only when it changes: its first frame, a resize, and `None` once it is no
//! longer captured. Nothing is sent, and nothing allocated, while the size holds. A
//! capture restarting on the CPU path keeps the size it had until a frame says otherwise.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use jstream_media::{
    ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, Rounding, Timebase,
};
use jstream_platform::{
    Capture, CaptureEncoding, CaptureOptions, CaptureStats, CaptureStream, CaptureTarget, Codecs,
    CursorCapture, DeviceId, GpuFrame, PlatformError, VideoDeviceFormat, VideoDevices,
    VideoSurface,
};
use jstream_sources::CaptureSpec;

use crate::snapshot::{CaptureFrame, CaptureImage, CaptureKey, Captures};

/// How long a capture that failed waits before it is started again, in milliseconds.
pub(crate) const RETRY_MS: i64 = 2_000;

/// The capture backend, and the same backend on the compositor's GPU device when it
/// has a path there ([`Capture::on_context`]), the video-device backend, and the codecs
/// that make its MJPEG decoders.
pub(crate) struct Backends {
    pub(crate) cpu: Arc<dyn Capture>,
    pub(crate) gpu: Option<Arc<dyn Capture>>,
    pub(crate) devices: Arc<dyn VideoDevices>,
    pub(crate) codecs: Arc<dyn Codecs>,
}

/// The most pixels a webcam or capture card is opened at: 1920x1080. The render thread
/// converts each CPU frame to RGBA, so a 4K device is opened at a smaller size it offers.
const MAX_DEVICE_PIXELS: u64 = 1920 * 1080;

/// What the render thread asks the `jstream-capture` thread to do.
enum Request {
    /// Start a capture, for the render thread's capture path `epoch`.
    Start(CaptureKey, u32),
    /// Stop a capture: its stream is dropped there.
    Stop(Box<dyn CaptureStream>),
    /// Drop the GPU backend: every later start is on the CPU path.
    CpuOnly,
}

/// A start's outcome, sent back to the render thread.
struct Started {
    spec: CaptureKey,
    stream: Option<Box<dyn CaptureStream>>,
    /// The epoch of the start: one from before the render thread left the GPU path is
    /// stopped again.
    epoch: u32,
}

/// A capture's frame size changed: `Some` for its first frame and for each frame of a new
/// size, `None` once it is no longer captured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SizeChange {
    pub(crate) key: CaptureKey,
    pub(crate) size: Option<FrameSize>,
}

/// Starts the `jstream-capture` thread, and returns the render thread's side of it,
/// which sends each capture's size changes on `sizes`.
pub(crate) fn spawn(
    backends: Backends,
    clock: Timebase,
    sizes: Sender<SizeChange>,
) -> Result<RenderCaptures, String> {
    let (requests, rx) = mpsc::channel();
    let (tx, started) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("jstream-capture".into())
        .spawn(move || capture_thread(backends, &rx, &tx))
        .map_err(|err| format!("starting the jstream-capture thread: {err}"))?;
    let retry = MediaTime::new(RETRY_MS, Timebase::MILLIS)
        .rescale(clock, Rounding::Up)
        .map_err(|err| format!("capture retry interval: {err}"))?;
    Ok(RenderCaptures {
        slots: BTreeMap::new(),
        requests: Some(requests),
        started,
        thread: Some(thread),
        sizes: Some(sizes),
        on_gpu: false,
        retry,
        ended: CaptureStats::default(),
        versions: 0,
        gpu_captures: 0,
        epoch: 0,
    })
}

fn capture_thread(mut backends: Backends, rx: &Receiver<Request>, tx: &Sender<Started>) {
    // The captures whose last start failed: a retry that fails again logs at `debug`,
    // so a closed window kept on program doesn't warn every retry.
    let mut failing = BTreeSet::new();
    for request in rx {
        match request {
            Request::Start(spec, epoch) => {
                let stream = match start(&backends, &spec) {
                    Ok(stream) => {
                        failing.remove(&spec);
                        Some(stream)
                    }
                    Err(err) => {
                        if failing.insert(spec.clone()) {
                            tracing::warn!(capture = ?spec, error = %err, "capture did not start");
                        } else {
                            tracing::debug!(capture = ?spec, error = %err, "capture still does not start");
                        }
                        None
                    }
                };
                // An error hands the stream back: the render thread is gone, so it
                // stops here.
                drop(tx.send(Started {
                    spec,
                    stream,
                    epoch,
                }));
            }
            Request::Stop(stream) => drop(stream),
            Request::CpuOnly => backends.gpu = None,
        }
    }
}

/// Starts the capture of `spec`: on the GPU path when the backend has one and it
/// starts, else on the CPU path. Fails with the reason when neither starts. A video
/// device is opened on the CPU path ([`open_device`]).
pub(crate) fn start(
    backends: &Backends,
    spec: &CaptureKey,
) -> Result<Box<dyn CaptureStream>, String> {
    if let CaptureSpec::Device(id) = &spec.spec {
        return open_device(
            backends.devices.as_ref(),
            backends.codecs.as_ref(),
            id.as_deref(),
        );
    }
    let target = target(backends.cpu.as_ref(), &spec.spec)?;
    let options = match &spec.source {
        Some(source) => CaptureOptions::for_source(source.as_str()),
        None => CaptureOptions::default(),
    }
    .with_cursor(cursor(spec));
    let gpu_failed = match &backends.gpu {
        Some(gpu) => match gpu.start_with(&target, &options) {
            Ok(stream) => {
                tracing::info!(capture = ?spec, "capture delivers GPU frames");
                return Ok(stream);
            }
            Err(err) => Some(err.to_string()),
        },
        None => None,
    };
    match (backends.cpu.start_with(&target, &options), gpu_failed) {
        (Ok(stream), Some(gpu)) => {
            tracing::warn!(capture = ?spec, error = %gpu, "GPU capture did not start; capturing CPU frames");
            Ok(stream)
        }
        (Ok(stream), None) => Ok(stream),
        // Neither path started: one error for the caller to log, so a retry that
        // fails again is logged once.
        (Err(cpu), Some(gpu)) => Err(format!("{cpu} (GPU path: {gpu})")),
        (Err(cpu), None) => Err(cpu.to_string()),
    }
}

/// The platform target `spec` names: a display or window by its ID, or the primary
/// display (else the first one listed).
fn target(capture: &dyn Capture, spec: &CaptureSpec) -> Result<CaptureTarget, String> {
    match spec {
        CaptureSpec::Display(Some(id)) => Ok(CaptureTarget::Display(DeviceId::new(id.clone()))),
        CaptureSpec::Window(Some(id)) => Ok(CaptureTarget::Window(DeviceId::new(id.clone()))),
        CaptureSpec::Window(None) => Err("no window is chosen".into()),
        CaptureSpec::Device(_) => Err("a video device is not a capture target".into()),
        CaptureSpec::Display(None) => {
            let targets = capture
                .targets()
                .map_err(|err| format!("listing capture targets: {err}"))?;
            let displays = || {
                targets
                    .iter()
                    .filter(|info| matches!(info.target, CaptureTarget::Display(_)))
            };
            displays()
                .find(|info| info.primary)
                .or_else(|| displays().next())
                .map(|info| info.target.clone())
                .ok_or_else(|| "no display to capture".into())
        }
    }
}

/// Opens the video device `id`, or the first one listed, in the format
/// [`device_format`] picks. An MJPEG format comes with the platform's capture decoder
/// ([`Codecs::capture_decoder`]); without one, only uncompressed formats are chosen.
fn open_device(
    devices: &dyn VideoDevices,
    codecs: &dyn Codecs,
    id: Option<&str>,
) -> Result<Box<dyn CaptureStream>, String> {
    let id = match id {
        Some(id) => DeviceId::new(id),
        None => devices
            .devices()
            .map_err(|err| format!("listing video devices: {err}"))?
            .into_iter()
            .next()
            .map(|info| info.id)
            .ok_or("no webcam or capture card to capture")?,
    };
    let formats = devices
        .formats(&id)
        .map_err(|err| format!("listing {id}'s formats: {err}"))?;
    let offers_mjpeg = formats.iter().any(|f| f.encoding == CaptureEncoding::Mjpeg);
    let mut decoder = None;
    let mut no_decoder = None;
    if offers_mjpeg {
        match codecs.capture_decoder(CaptureEncoding::Mjpeg) {
            Ok(made) => decoder = Some(made),
            Err(err) => no_decoder = Some(err.to_string()),
        }
    }
    let format = device_format(&formats, decoder.is_some()).ok_or_else(|| match &no_decoder {
        Some(err) => {
            format!(
                "{id} offers no uncompressed format the engine reads, and the platform has no MJPEG decoder: {err}"
            )
        }
        None => format!("{id} offers no format the engine reads"),
    })?;
    if let Some(err) = &no_decoder {
        tracing::info!(device = %id, error = %err, "no MJPEG decoder; choosing among the uncompressed formats");
    }
    tracing::info!(
        device = %id,
        size = ?format.size,
        period = ?format.frame_period,
        encoding = ?format.encoding,
        "opening the video device"
    );
    let decoder = if format.encoding.is_compressed() {
        decoder
    } else {
        None
    };
    devices
        .open(&id, &format, decoder)
        .map_err(|err| format!("opening {id}: {err}"))
}

/// The format a webcam or capture card is opened in, from those it lists.
///
/// The candidates are the formats whose frames arrive uncompressed in a format
/// [`rgba_from_surface`] converts (RGBA, BGRA, or NV12, which YUY2 is delivered as), and
/// MJPEG when `mjpeg` says a decoder is there. Of those it picks the largest of at most
/// [`MAX_DEVICE_PIXELS`] (else the smallest), then the fastest, then an uncompressed one
/// (no decoding), then the first listed. `None` when no format is a candidate.
pub(crate) fn device_format(
    formats: &[VideoDeviceFormat],
    mjpeg: bool,
) -> Option<VideoDeviceFormat> {
    let pixels = |f: &VideoDeviceFormat| u64::from(f.size.width()) * u64::from(f.size.height());
    let fits = |pixels: u64| pixels <= MAX_DEVICE_PIXELS;
    let candidate = |f: &&VideoDeviceFormat| {
        matches!(
            f.encoding.delivered_format(),
            Some(PixelFormat::Rgba8 | PixelFormat::Bgra8 | PixelFormat::Nv12)
        ) || (mjpeg && f.encoding == CaptureEncoding::Mjpeg)
    };
    let mut best: Option<&VideoDeviceFormat> = None;
    for format in formats.iter().filter(candidate) {
        let Some(current) = best else {
            best = Some(format);
            continue;
        };
        let (p, c) = (pixels(format), pixels(current));
        // The shorter frame period, num/den, is faster.
        let (a, b) = (format.frame_period, current.frame_period);
        let (fa, fb) = (
            u64::from(a.num()) * u64::from(b.den()),
            u64::from(b.num()) * u64::from(a.den()),
        );
        let better = match (fits(p), fits(c)) {
            (true, false) => true,
            (false, true) => false,
            (true, true) if p != c => p > c,
            (false, false) if p != c => p < c,
            _ if fa != fb => fa < fb,
            _ => current.encoding.is_compressed() && !format.encoding.is_compressed(),
        };
        if better {
            best = Some(format);
        }
    }
    best.copied()
}

/// How `spec`'s capture asks for the pointer: drawn into the frames by the OS when it
/// shows, else left out ([`CursorCapture::Hidden`]). The engine never draws it.
fn cursor(spec: &CaptureKey) -> CursorCapture {
    if spec.show_cursor {
        CursorCapture::Drawn
    } else {
        CursorCapture::Hidden
    }
}

/// The render thread's captures: the running streams and the latest frame of each.
pub(crate) struct RenderCaptures {
    slots: BTreeMap<CaptureKey, Slot>,
    /// `None` once [`stop`](Self::stop) has run.
    requests: Option<Sender<Request>>,
    started: Receiver<Started>,
    thread: Option<JoinHandle<()>>,
    /// Where each capture's size changes go; `None` once [`stop`](Self::stop) has run,
    /// so the engine's side ends.
    sizes: Option<Sender<SizeChange>>,
    /// Whether the compositor is on the platform's GPU context, so GPU frames can be
    /// sampled. Off, a GPU frame is unusable.
    on_gpu: bool,
    /// [`RETRY_MS`] on the media clock.
    retry: MediaTime,
    /// The counters of the streams that have stopped.
    ended: CaptureStats,
    /// Numbers every new frame, across captures.
    versions: u64,
    gpu_captures: u64,
    /// Bumped when the render thread leaves the GPU path, so a start asked for before it
    /// is stopped again when it comes back.
    epoch: u32,
}

impl RenderCaptures {
    /// Asks the `jstream-capture` thread to start `spec`, for `slot`, at the current
    /// epoch.
    fn request_start(&self, spec: &CaptureKey, slot: &mut Slot) {
        if let Some(requests) = &self.requests {
            let _ = requests.send(Request::Start(spec.clone(), self.epoch));
        }
        slot.epoch = self.epoch;
        slot.state = State::Starting;
    }
}

struct Slot {
    state: State,
    /// The latest frame. Kept while the capture restarts, so it goes on showing.
    frame: Option<Latest>,
    /// The version of `frame`.
    version: u64,
    /// The latest CPU frame's pixels, reused from frame to frame.
    rgba: Vec<u8>,
    warned_format: bool,
    /// The epoch of the slot's latest start: an answer to an earlier one is stopped
    /// again. A video device keeps its epoch when the render thread leaves the GPU path,
    /// since it isn't restarted.
    epoch: u32,
    /// The size last sent for it, [`SizeChange`]. Kept when the render thread leaves the
    /// GPU path, so a capture restarting there sends nothing until its size changes.
    reported: Option<FrameSize>,
}

enum State {
    /// Asked to start; the `jstream-capture` thread hasn't answered.
    Starting,
    Running {
        stream: Box<dyn CaptureStream>,
        failing: bool,
    },
    /// Not running since `at`; started again [`RETRY_MS`] later.
    Failed { at: MediaTime },
}

enum Latest {
    Cpu(FrameSize),
    Gpu(GpuFrame),
}

impl RenderCaptures {
    /// Called once the compositor is open: whether it is on the platform's GPU context.
    pub(crate) fn set_on_gpu(&mut self, on_gpu: bool) {
        self.on_gpu = on_gpu;
    }

    /// Moves every capture onto the CPU path, for a compositor that leaves the platform's
    /// GPU context mid-run (#451): lets go of the frames held, tells the
    /// `jstream-capture` thread to drop its GPU backend, and stops and starts again every
    /// display and window capture that is running or starting. Never waits; the streams
    /// and the backend drop on that thread, which ends before the context goes
    /// ([`stop`](Self::stop)). Such a capture keeps no frame until its CPU stream
    /// delivers one. A webcam or capture card is on the CPU path already: it runs on and
    /// keeps its frame, and one still starting takes the answer it is waiting for, since
    /// most devices open only once (#734).
    pub(crate) fn leave_gpu(&mut self) {
        self.on_gpu = false;
        let Some(requests) = self.requests.clone() else {
            self.release_frames();
            return;
        };
        let _ = requests.send(Request::CpuOnly);
        self.epoch = self.epoch.wrapping_add(1);
        let mut slots = std::mem::take(&mut self.slots);
        for (spec, slot) in &mut slots {
            if matches!(spec.spec, CaptureSpec::Device(_)) {
                // Only a GPU frame uses the compositor's device; a device delivers none.
                if matches!(slot.frame, Some(Latest::Gpu(_))) {
                    slot.frame = None;
                }
                continue;
            }
            slot.frame = None;
            match std::mem::replace(&mut slot.state, State::Starting) {
                State::Running { stream, .. } => {
                    add(&mut self.ended, stream.stats());
                    let _ = requests.send(Request::Stop(stream));
                    self.request_start(spec, slot);
                }
                State::Starting => self.request_start(spec, slot),
                // Started again, on the CPU path, when it is due.
                failed @ State::Failed { .. } => slot.state = failed,
            }
        }
        self.slots = slots;
    }

    /// Starts what `wanted` captures and isn't running, stops what it no longer
    /// captures, takes the streams the `jstream-capture` thread started, and reads every
    /// running capture's mailbox. Never waits.
    pub(crate) fn tick(&mut self, wanted: &BTreeSet<CaptureKey>, now: MediaTime) {
        let Some(requests) = &self.requests else {
            return;
        };
        let ended = &mut self.ended;
        let sizes = &self.sizes;
        self.slots.retain(|spec, slot| {
            let keep = wanted.contains(spec);
            if !keep {
                if let State::Running { stream, .. } =
                    std::mem::replace(&mut slot.state, State::Starting)
                {
                    add(ended, stream.stats());
                    let _ = requests.send(Request::Stop(stream));
                }
                if slot.reported.is_some() {
                    send_size(sizes, spec, None);
                }
            }
            keep
        });
        let mut slots = std::mem::take(&mut self.slots);
        for spec in wanted {
            match slots.get_mut(spec) {
                None => {
                    let mut slot = Slot::new();
                    self.request_start(spec, &mut slot);
                    slots.insert(spec.clone(), slot);
                }
                Some(slot) => {
                    if let State::Failed { at } = slot.state
                        && at.checked_add(self.retry).is_ok_and(|due| due <= now)
                    {
                        self.request_start(spec, slot);
                    }
                }
            }
        }
        self.slots = slots;
        while let Ok(Started {
            spec,
            stream,
            epoch,
        }) = self.started.try_recv()
        {
            match (self.slots.get_mut(&spec), stream) {
                // Asked for before the GPU path was left: started again since.
                (Some(slot), Some(stream)) if epoch != slot.epoch => {
                    let _ = requests.send(Request::Stop(stream));
                }
                (Some(slot), None) if epoch != slot.epoch => {}
                (Some(slot), Some(stream)) if matches!(slot.state, State::Starting) => {
                    slot.state = State::Running {
                        stream,
                        failing: false,
                    };
                }
                (Some(slot), None) if matches!(slot.state, State::Starting) => {
                    slot.state = State::Failed { at: now };
                }
                // No longer wanted: stopped again.
                (_, Some(stream)) => {
                    let _ = requests.send(Request::Stop(stream));
                }
                (_, None) => {}
            }
        }
        for (spec, slot) in &mut self.slots {
            let State::Running { stream, failing } = &mut slot.state else {
                continue;
            };
            match stream.latest() {
                Ok(Some(VideoSurface::Gpu(frame)))
                    if self.on_gpu
                        && matches!(
                            frame.info().format,
                            PixelFormat::Rgba8 | PixelFormat::Bgra8
                        ) =>
                {
                    *failing = false;
                    self.versions += 1;
                    self.gpu_captures += 1;
                    slot.version = self.versions;
                    slot.frame = Some(Latest::Gpu(frame));
                }
                Ok(Some(surface)) => {
                    *failing = false;
                    if let Some(size) = rgba_from_surface(&surface, &mut slot.rgba) {
                        self.versions += 1;
                        slot.version = self.versions;
                        slot.frame = Some(Latest::Cpu(size));
                    } else if !slot.warned_format {
                        slot.warned_format = true;
                        tracing::warn!(
                            capture = ?spec,
                            "capture frame is neither a CPU RGBA, BGRA or NV12 frame nor an \
                             RGBA or BGRA texture on the compositor's device"
                        );
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    if !*failing {
                        *failing = true;
                        tracing::warn!(capture = ?spec, error = %err, "capture failed; repeating its last frame");
                    }
                    // A lost target delivers nothing more: stop it and start it again later.
                    if matches!(err, PlatformError::DeviceLost { .. })
                        && let State::Running { stream, .. } =
                            std::mem::replace(&mut slot.state, State::Failed { at: now })
                    {
                        add(&mut self.ended, stream.stats());
                        let _ = requests.send(Request::Stop(stream));
                    }
                }
            }
        }
        self.report_sizes();
    }

    /// Sends the size of each capture whose frame changed size since the last one sent.
    fn report_sizes(&mut self) {
        for (spec, slot) in &mut self.slots {
            let size = match &slot.frame {
                Some(Latest::Cpu(size)) => *size,
                Some(Latest::Gpu(frame)) => frame.info().size,
                // A capture restarting keeps the size it had until a frame says otherwise.
                None => continue,
            };
            if slot.reported != Some(size) {
                slot.reported = Some(size);
                send_size(&self.sizes, spec, Some(size));
            }
        }
    }

    /// The counters of every capture this pipeline ran, added up.
    pub(crate) fn stats(&self) -> CaptureStats {
        let mut total = self.ended;
        for slot in self.slots.values() {
            if let State::Running { stream, .. } = &slot.state {
                add(&mut total, stream.stats());
            }
        }
        total
    }

    /// GPU frames taken, for [`OutputStats::gpu_captures`](super::OutputStats).
    pub(crate) fn gpu_captures(&self) -> u64 {
        self.gpu_captures
    }

    /// Lets go of the frames held, which use the compositor's device: before the
    /// compositor goes.
    pub(crate) fn release_frames(&mut self) {
        for slot in self.slots.values_mut() {
            slot.frame = None;
        }
    }

    /// Stops every capture and waits for the `jstream-capture` thread to drop the
    /// streams and its backends, which use the platform's GPU context: before the
    /// context goes. Called at the end of the render loop, and on drop.
    pub(crate) fn stop(&mut self) {
        let Some(requests) = self.requests.take() else {
            return;
        };
        self.release_frames();
        for (_, slot) in std::mem::take(&mut self.slots) {
            if let State::Running { stream, .. } = slot.state {
                let _ = requests.send(Request::Stop(stream));
            }
        }
        drop(requests);
        // The engine's side forgets every capture's size when this closes.
        self.sizes.take();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("the jstream-capture thread panicked");
        }
        // Started after the last request was read: dropped here, the thread being gone.
        while let Ok(started) = self.started.try_recv() {
            drop(started);
        }
    }
}

impl Drop for RenderCaptures {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Captures for RenderCaptures {
    fn frame(&self, spec: &CaptureKey) -> Option<CaptureFrame<'_>> {
        let slot = self.slots.get(spec)?;
        let image = match slot.frame.as_ref()? {
            Latest::Gpu(frame) => CaptureImage::Gpu(frame),
            Latest::Cpu(size) => CaptureImage::Rgba {
                size: *size,
                rgba: &slot.rgba,
            },
        };
        Some(CaptureFrame {
            version: slot.version,
            image,
        })
    }
}

impl Slot {
    fn new() -> Self {
        Self {
            state: State::Starting,
            frame: None,
            version: 0,
            rgba: Vec::new(),
            warned_format: false,
            epoch: 0,
            reported: None,
        }
    }
}

/// Sends one capture's size change. The engine's side is gone only while the stage is
/// torn down, when there is no one to tell.
fn send_size(sizes: &Option<Sender<SizeChange>>, key: &CaptureKey, size: Option<FrameSize>) {
    if let Some(sizes) = sizes {
        let _ = sizes.send(SizeChange {
            key: key.clone(),
            size,
        });
    }
}

fn add(total: &mut CaptureStats, stats: CaptureStats) {
    total.frames_captured = total.frames_captured.saturating_add(stats.frames_captured);
    total.frames_skipped = total.frames_skipped.saturating_add(stats.frames_skipped);
}

/// Converts a CPU capture into `out` as opaque RGBA8, reusing its allocation, and returns
/// its size. RGBA8 is copied, BGRA8 swizzled and NV12 (what Windows' display capture
/// delivers) converted with its frame's matrix and range. A capture's alpha means
/// nothing, so every pixel's is 255, as the GPU path reads it (#451). `None`, leaving
/// `out` as it was, when the frame is not one of those.
pub(crate) fn rgba_from_surface(surface: &VideoSurface, out: &mut Vec<u8>) -> Option<FrameSize> {
    let VideoSurface::Cpu(frame) = surface else {
        return None;
    };
    let info = frame.info();
    let width = info.size.width() as usize;
    let height = info.size.height();
    let row_len = width.checked_mul(4)?;
    let len = row_len.checked_mul(height as usize)?;
    match info.format {
        PixelFormat::Rgba8 | PixelFormat::Bgra8 => {
            if (0..height).any(|y| frame.row(0, y).is_none_or(|row| row.len() < row_len)) {
                return None;
            }
            out.resize(len, 0);
            for (y, dst) in (0..height).zip(out.chunks_exact_mut(row_len)) {
                let row = &frame.row(0, y)?[..row_len];
                let rgba = info.format == PixelFormat::Rgba8;
                for (d, s) in dst.chunks_exact_mut(4).zip(row.chunks_exact(4)) {
                    let pixel = if rgba {
                        [s[0], s[1], s[2], 255]
                    } else {
                        [s[2], s[1], s[0], 255]
                    };
                    d.copy_from_slice(&pixel);
                }
            }
        }
        PixelFormat::Nv12 => {
            let chroma_len = width.div_ceil(2) * 2;
            let rows_ok = (0..height).all(|y| frame.row(0, y).is_some_and(|r| r.len() >= width))
                && (0..height.div_ceil(2))
                    .all(|y| frame.row(1, y).is_some_and(|r| r.len() >= chroma_len));
            if !rows_ok {
                return None;
            }
            let yuv = Yuv::new(info.color);
            out.resize(len, 0);
            for (y, dst) in (0..height).zip(out.chunks_exact_mut(row_len)) {
                let luma = frame.row(0, y)?;
                let chroma = frame.row(1, y / 2)?;
                for (x, d) in dst.chunks_exact_mut(4).enumerate() {
                    let c = (x / 2) * 2;
                    d.copy_from_slice(&yuv.rgba(luma[x], chroma[c], chroma[c + 1]));
                }
            }
        }
        _ => return None,
    }
    Some(info.size)
}

/// YCbCr to R'G'B' for one matrix and range, in 16.16 fixed point.
struct Yuv {
    /// Luma: offset, then scale.
    y: (i32, i32),
    /// Chroma scale.
    c: i32,
    /// Cr to R, Cb to G, Cr to G, Cb to B.
    cr_r: i32,
    cb_g: i32,
    cr_g: i32,
    cb_b: i32,
}

impl Yuv {
    fn new(color: ColorInfo) -> Self {
        let (kr, kb) = match color.space {
            ColorSpace::Bt601 => (0.299, 0.114),
            ColorSpace::Bt2020 => (0.2627, 0.0593),
            _ => (0.2126, 0.0722),
        };
        let kg = 1.0 - kr - kb;
        let fixed = |v: f64| (v * 65_536.0).round() as i32;
        let (y, c) = match color.range {
            ColorRange::Full => ((0, fixed(1.0)), fixed(1.0)),
            ColorRange::Limited => ((16, fixed(255.0 / 219.0)), fixed(255.0 / 224.0)),
        };
        Self {
            y,
            c,
            cr_r: fixed(2.0 * (1.0 - kr)),
            cb_g: fixed(2.0 * kb * (1.0 - kb) / kg),
            cr_g: fixed(2.0 * kr * (1.0 - kr) / kg),
            cb_b: fixed(2.0 * (1.0 - kb)),
        }
    }

    /// One opaque RGBA8 pixel.
    fn rgba(&self, y: u8, cb: u8, cr: u8) -> [u8; 4] {
        // 16.16 fixed point throughout; each product stays well inside i64.
        let l = i64::from((i32::from(y) - self.y.0) * self.y.1);
        let cb = (i64::from(i32::from(cb) - 128) * i64::from(self.c)) >> 16;
        let cr = (i64::from(i32::from(cr) - 128) * i64::from(self.c)) >> 16;
        let channel = |v: i64| ((v + (1 << 15)) >> 16).clamp(0, 255) as u8;
        [
            channel(l + cr * i64::from(self.cr_r)),
            channel(l - cb * i64::from(self.cb_g) - cr * i64::from(self.cr_g)),
            channel(l + cb * i64::from(self.cb_b)),
            255,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::{Backends, MAX_DEVICE_PIXELS, device_format, rgba_from_surface, spawn, start};
    use crate::snapshot::{CaptureKey, Captures};
    use jstream_media::{FrameSize, PixelFormat, Timebase};
    use jstream_platform::fake::{
        FAKE_CAPTURE_CARD, FAKE_WEBCAM, FakeCapture, FakeClock, FakeCodecs, FakeVideoDevices,
    };
    use jstream_platform::{
        AudioEncoder, AudioEncoderConfig, CaptureEncoding, CodecInfo, Codecs, MediaClock,
        PlatformError, VideoDecoder, VideoDecoderConfig, VideoDeviceFormat, VideoEncoder,
        VideoEncoderConfig,
    };
    use jstream_sources::CaptureSpec;
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn format(width: u32, height: u32, fps: u32, encoding: CaptureEncoding) -> VideoDeviceFormat {
        VideoDeviceFormat {
            size: FrameSize::new(width, height).expect("size"),
            frame_period: Timebase::new(1, fps).expect("period"),
            encoding,
        }
    }

    /// The fake codecs without a capture decoder: the trait's default.
    struct NoMjpeg;

    impl Codecs for NoMjpeg {
        fn list(&self) -> Result<Vec<CodecInfo>, PlatformError> {
            FakeCodecs.list()
        }
        fn video_encoder(
            &self,
            config: &VideoEncoderConfig,
        ) -> Result<Box<dyn VideoEncoder>, PlatformError> {
            FakeCodecs.video_encoder(config)
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

    #[test]
    fn a_device_opens_in_its_largest_readable_format_up_to_1080p() {
        let nv12 = CaptureEncoding::Raw(PixelFormat::Nv12);
        let mjpeg = format(1920, 1080, 30, CaptureEncoding::Mjpeg);
        let mjpeg_hd = format(1280, 720, 30, CaptureEncoding::Mjpeg);
        let small = format(640, 360, 30, CaptureEncoding::Yuy2);
        let hd = format(1280, 720, 30, nv12);
        let hd60 = format(1280, 720, 60, nv12);
        let full = format(1920, 1080, 30, nv12);
        let uhd = format(3840, 2160, 30, nv12);
        let pick = |formats: &[VideoDeviceFormat], decoder| device_format(formats, decoder);
        assert_eq!(pick(&[small, hd, mjpeg], false), Some(hd), "no decoder");
        assert_eq!(
            pick(&[small, hd, mjpeg], true),
            Some(mjpeg),
            "MJPEG is larger"
        );
        assert_eq!(
            pick(&[mjpeg_hd, hd], true),
            Some(hd),
            "uncompressed of the same size and rate"
        );
        assert_eq!(
            pick(&[hd, hd60, small], false),
            Some(hd60),
            "the faster rate"
        );
        assert_eq!(pick(&[hd60, hd], false), Some(hd60), "the first of equals");
        assert_eq!(
            pick(&[uhd, hd, full], false),
            Some(full),
            "4K is over the limit"
        );
        assert_eq!(pick(&[uhd], false), Some(uhd), "the only size there is");
        assert_eq!(pick(&[mjpeg], false), None, "MJPEG only, no decoder");
        assert_eq!(pick(&[mjpeg], true), Some(mjpeg), "MJPEG only, decoded");
        assert_eq!(pick(&[], true), None);
        assert_eq!(MAX_DEVICE_PIXELS, 1920 * 1080);
    }

    fn backends(
        clock: &FakeClock,
        devices: &FakeVideoDevices,
        codecs: Arc<dyn Codecs>,
    ) -> Backends {
        Backends {
            cpu: Arc::new(FakeCapture::new(
                clock.clone(),
                FrameSize::new(64, 36).expect("size"),
            )),
            gpu: None,
            devices: Arc::new(devices.clone()),
            codecs,
        }
    }

    /// The fake webcam offers 640x360 NV12 and YUY2 and 1280x720 MJPEG: with the fake
    /// codecs' decoder it opens in MJPEG, whose decoded NV12 frames the render thread
    /// reads, and without one in NV12. The capture card offers 1280x720 NV12.
    #[test]
    fn a_device_spec_opens_the_device_and_delivers_frames_rgba_reads() {
        let clock = FakeClock::new(Timebase::MICROS);
        let devices = FakeVideoDevices::new(clock.clone());
        let with: Arc<dyn Codecs> = Arc::new(FakeCodecs);
        let without: Arc<dyn Codecs> = Arc::new(NoMjpeg);
        for (codecs, spec, size) in [
            (&with, CaptureSpec::Device(None), (1280, 720)),
            (
                &with,
                CaptureSpec::Device(Some(FAKE_WEBCAM.into())),
                (1280, 720),
            ),
            (
                &without,
                CaptureSpec::Device(Some(FAKE_WEBCAM.into())),
                (640, 360),
            ),
            (
                &with,
                CaptureSpec::Device(Some(FAKE_CAPTURE_CARD.into())),
                (1280, 720),
            ),
        ] {
            let backends = backends(&clock, &devices, Arc::clone(codecs));
            let key = crate::snapshot::CaptureKey::for_source(
                spec.clone(),
                jstream_protocol::SourceId::new("cam"),
            );
            assert_eq!(
                key.source, None,
                "a device's capture is shared, for no source"
            );
            let mut stream = start(&backends, &key).expect("the device opens");
            assert_eq!(devices.open_streams(), 1, "{spec:?}");
            clock.advance(1_000);
            let surface = stream.latest().expect("latest").expect("a frame");
            let mut rgba = Vec::new();
            let got = rgba_from_surface(&surface, &mut rgba).expect("an NV12 CPU frame");
            assert_eq!((got.width(), got.height()), size, "{spec:?}");
            assert_eq!(stream.stats().frames_captured, 1, "{spec:?}");
            drop(stream);
            assert_eq!(devices.open_streams(), 0, "{spec:?}");
        }
        let backends = backends(&clock, &devices, with);
        let nope = crate::snapshot::CaptureKey::for_source(
            CaptureSpec::Device(Some("nope".into())),
            jstream_protocol::SourceId::new("cam"),
        );
        assert!(start(&backends, &nope).is_err());
    }

    /// Leaving the GPU path restarts a display capture on the CPU path, but not a
    /// webcam, which is on it already: a running one keeps its stream and its frame, and
    /// one still starting takes its first answer rather than opening the device a second
    /// time (#734). Each capture's size is sent once: a restart keeps it (B12).
    #[test]
    fn leaving_the_gpu_path_leaves_a_webcam_running() {
        let clock = FakeClock::new(Timebase::MICROS);
        let devices = FakeVideoDevices::new(clock.clone());
        let (size_tx, size_rx) = std::sync::mpsc::channel();
        let mut captures = spawn(
            backends(&clock, &devices, Arc::new(FakeCodecs)),
            Timebase::MICROS,
            size_tx,
        )
        .expect("the capture thread starts");
        let webcam = CaptureKey::for_source(
            CaptureSpec::Device(Some(FAKE_WEBCAM.into())),
            jstream_protocol::SourceId::new("cam"),
        );
        let display = CaptureKey::primary();
        let wanted = BTreeSet::from([webcam.clone(), display.clone()]);
        // Ticks until `done` holds, never seeing the webcam opened twice.
        let tick_until = |captures: &mut super::RenderCaptures,
                          done: &dyn Fn(&super::RenderCaptures) -> bool|
         -> bool {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                clock.advance(1_000);
                captures.tick(&wanted, clock.now());
                assert!(devices.open_streams() <= 1, "the webcam opened twice");
                if done(captures) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            false
        };
        let both = |captures: &super::RenderCaptures| {
            captures.frame(&webcam).is_some() && captures.frame(&display).is_some()
        };
        // The first tick asks for both; the GPU path is left before either answers.
        captures.tick(&wanted, clock.now());
        captures.leave_gpu();
        assert!(tick_until(&mut captures, &both), "both capture");
        assert_eq!(devices.open_streams(), 1);
        let version = captures.frame(&webcam).expect("a frame").version;
        // Left again while both run: the display's frame goes, the webcam's stays.
        captures.leave_gpu();
        assert!(captures.frame(&display).is_none(), "restarted");
        assert_eq!(
            captures.frame(&webcam).map(|frame| frame.version),
            Some(version),
            "the webcam keeps its frame"
        );
        assert!(
            tick_until(&mut captures, &both),
            "the display captures again"
        );
        assert!(
            tick_until(&mut captures, &|captures| captures
                .frame(&webcam)
                .is_some_and(|frame| frame.version > version)),
            "the webcam's stream delivers on"
        );
        assert_eq!(devices.open_streams(), 1, "the same stream");
        let mut sent: Vec<CaptureKey> = size_rx.try_iter().map(|change| change.key).collect();
        sent.sort();
        let mut each = vec![webcam.clone(), display.clone()];
        each.sort();
        assert_eq!(sent, each, "one size each, kept through the restarts");
        captures.stop();
        assert_eq!(devices.open_streams(), 0);
        assert!(
            size_rx.recv().is_err(),
            "stopping closes the channel, sending nothing more"
        );
    }
}
