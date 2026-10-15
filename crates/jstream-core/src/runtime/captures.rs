//! The output pipeline's captures: one per thing the render snapshot captures
//! ([`Shot::captures`]), started and stopped as the program mix changes.
//!
//! The render thread reads each capture's mailbox every tick and keeps its latest frame,
//! a texture on the compositor's device or CPU RGBA8. Starting and stopping a capture
//! are OS calls that can block, so they run on the pipeline's `jstream-capture` thread.
//! The render thread asks it to start what a new snapshot captures and hands it the
//! streams it no longer reads, through channels it never waits on (R-RT-002,
//! R-RT-003). A capture that fails to start, or whose target goes away, keeps showing
//! its last frame and is started again after [`RETRY_MS`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use jstream_media::{FrameSize, MediaTime, PixelFormat, Rounding, Timebase};
use jstream_platform::{
    Capture, CaptureStats, CaptureStream, CaptureTarget, DeviceId, GpuFrame, PlatformError,
    VideoSurface,
};
use jstream_sources::CaptureSpec;

use crate::snapshot::{CaptureFrame, CaptureImage, Captures};

/// How long a capture that failed waits before it is started again, in milliseconds.
pub(crate) const RETRY_MS: i64 = 2_000;

/// The capture backend, and the same backend on the compositor's GPU device when it
/// has a path there ([`Capture::on_context`]).
pub(crate) struct Backends {
    pub(crate) cpu: Arc<dyn Capture>,
    pub(crate) gpu: Option<Arc<dyn Capture>>,
}

/// What the render thread asks the `jstream-capture` thread to do.
enum Request {
    Start(CaptureSpec),
    /// Stop a capture: its stream is dropped there.
    Stop(Box<dyn CaptureStream>),
}

/// A start's outcome, sent back to the render thread.
struct Started {
    spec: CaptureSpec,
    stream: Option<Box<dyn CaptureStream>>,
}

/// Starts the `jstream-capture` thread, and returns the render thread's side of it.
pub(crate) fn spawn(backends: Backends, clock: Timebase) -> Result<RenderCaptures, String> {
    let (requests, rx) = mpsc::channel();
    let (tx, started) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("jstream-capture".into())
        .spawn(move || capture_thread(&backends, &rx, &tx))
        .map_err(|err| format!("starting the jstream-capture thread: {err}"))?;
    let retry = MediaTime::new(RETRY_MS, Timebase::MILLIS)
        .rescale(clock, Rounding::Up)
        .map_err(|err| format!("capture retry interval: {err}"))?;
    Ok(RenderCaptures {
        slots: BTreeMap::new(),
        requests: Some(requests),
        started,
        thread: Some(thread),
        on_gpu: false,
        retry,
        ended: CaptureStats::default(),
        versions: 0,
        gpu_captures: 0,
    })
}

fn capture_thread(backends: &Backends, rx: &Receiver<Request>, tx: &Sender<Started>) {
    // The captures whose last start failed: a retry that fails again logs at `debug`,
    // so a closed window kept on program doesn't warn every retry.
    let mut failing = BTreeSet::new();
    for request in rx {
        match request {
            Request::Start(spec) => {
                let stream = match start(backends, &spec) {
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
                drop(tx.send(Started { spec, stream }));
            }
            Request::Stop(stream) => drop(stream),
        }
    }
}

/// Starts the capture of `spec`: on the GPU path when the backend has one and it
/// starts, else on the CPU path. Fails with the reason when neither starts.
pub(crate) fn start(
    backends: &Backends,
    spec: &CaptureSpec,
) -> Result<Box<dyn CaptureStream>, String> {
    let target = target(backends.cpu.as_ref(), spec)?;
    let gpu_failed = match &backends.gpu {
        Some(gpu) => match gpu.start(&target) {
            Ok(stream) => {
                tracing::info!(capture = ?spec, "capture delivers GPU frames");
                return Ok(stream);
            }
            Err(err) => Some(err.to_string()),
        },
        None => None,
    };
    match (backends.cpu.start(&target), gpu_failed) {
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

/// The render thread's captures: the running streams and the latest frame of each.
pub(crate) struct RenderCaptures {
    slots: BTreeMap<CaptureSpec, Slot>,
    /// `None` once [`stop`](Self::stop) has run.
    requests: Option<Sender<Request>>,
    started: Receiver<Started>,
    thread: Option<JoinHandle<()>>,
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

    /// Starts what `wanted` captures and isn't running, stops what it no longer
    /// captures, takes the streams the `jstream-capture` thread started, and reads every
    /// running capture's mailbox. Never waits.
    pub(crate) fn tick(&mut self, wanted: &BTreeSet<CaptureSpec>, now: MediaTime) {
        let Some(requests) = &self.requests else {
            return;
        };
        let ended = &mut self.ended;
        self.slots.retain(|spec, slot| {
            let keep = wanted.contains(spec);
            if !keep
                && let State::Running { stream, .. } =
                    std::mem::replace(&mut slot.state, State::Starting)
            {
                add(ended, stream.stats());
                let _ = requests.send(Request::Stop(stream));
            }
            keep
        });
        for spec in wanted {
            match self.slots.get_mut(spec) {
                None => {
                    let _ = requests.send(Request::Start(spec.clone()));
                    self.slots.insert(spec.clone(), Slot::new());
                }
                Some(slot) => {
                    if let State::Failed { at } = slot.state
                        && at.checked_add(self.retry).is_ok_and(|due| due <= now)
                    {
                        let _ = requests.send(Request::Start(spec.clone()));
                        slot.state = State::Starting;
                    }
                }
            }
        }
        while let Ok(Started { spec, stream }) = self.started.try_recv() {
            match (self.slots.get_mut(&spec), stream) {
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
                            "capture frame is neither CPU RGBA or BGRA nor an RGBA or BGRA \
                             texture on the compositor's device"
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
    fn frame(&self, spec: &CaptureSpec) -> Option<CaptureFrame<'_>> {
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
        }
    }
}

fn add(total: &mut CaptureStats, stats: CaptureStats) {
    total.frames_captured = total.frames_captured.saturating_add(stats.frames_captured);
    total.frames_skipped = total.frames_skipped.saturating_add(stats.frames_skipped);
}

/// Copies a CPU RGBA8 or BGRA8 capture into `out` as tightly packed RGBA8, reusing its
/// allocation, and returns its size. `None`, leaving `out` as it was, when the frame is
/// not usable.
fn rgba_from_surface(surface: &VideoSurface, out: &mut Vec<u8>) -> Option<FrameSize> {
    let VideoSurface::Cpu(frame) = surface else {
        return None;
    };
    let info = frame.info();
    if !matches!(info.format, PixelFormat::Rgba8 | PixelFormat::Bgra8) {
        return None;
    }
    let width = info.size.width() as usize;
    let height = info.size.height() as usize;
    let len = width.checked_mul(height).and_then(|n| n.checked_mul(4))?;
    let row_len = width * 4;
    for y in 0..height {
        match frame.row(0, y as u32) {
            Some(row) if row.len() >= row_len => {}
            _ => return None,
        }
    }
    out.resize(len, 0);
    for y in 0..height {
        let row = frame.row(0, y as u32)?;
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
    Some(info.size)
}
