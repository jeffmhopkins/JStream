//! The output pipeline's captures: one per thing the render snapshot captures
//! ([`Shot::captures`]), started and stopped as the program mix changes. That is one per
//! source ([`CaptureKey`]), started with the source's [`CaptureOptions`], so two sources
//! showing the same target run two captures of it. A source whose `show_cursor` is off
//! asks for the pointer apart from the picture ([`CursorCapture::Metadata`]) and the
//! engine doesn't draw it.
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

use jstream_media::{
    ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, Rounding, Timebase,
};
use jstream_platform::{
    Capture, CaptureOptions, CaptureStats, CaptureStream, CaptureTarget, CursorCapture, DeviceId,
    GpuFrame, PlatformError, VideoSurface,
};
use jstream_sources::CaptureSpec;

use crate::snapshot::{CaptureFrame, CaptureImage, CaptureKey, Captures};

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
    Start(CaptureKey),
    /// Stop a capture: its stream is dropped there.
    Stop(Box<dyn CaptureStream>),
}

/// A start's outcome, sent back to the render thread.
struct Started {
    spec: CaptureKey,
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
    spec: &CaptureKey,
) -> Result<Box<dyn CaptureStream>, String> {
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

/// How `spec`'s capture asks for the pointer: drawn into the frames when it shows, or
/// else apart from them ([`CursorCapture::Metadata`]), which the engine doesn't draw, so
/// the pointer is left out. A backend that can't send it apart draws it in all the same.
fn cursor(spec: &CaptureKey) -> CursorCapture {
    if spec.show_cursor {
        CursorCapture::Drawn
    } else {
        CursorCapture::Metadata
    }
}

/// The render thread's captures: the running streams and the latest frame of each.
pub(crate) struct RenderCaptures {
    slots: BTreeMap<CaptureKey, Slot>,
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
    pub(crate) fn tick(&mut self, wanted: &BTreeSet<CaptureKey>, now: MediaTime) {
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
        }
    }
}

fn add(total: &mut CaptureStats, stats: CaptureStats) {
    total.frames_captured = total.frames_captured.saturating_add(stats.frames_captured);
    total.frames_skipped = total.frames_skipped.saturating_add(stats.frames_skipped);
}

/// Converts a CPU capture into `out` as RGBA8, reusing its allocation, and returns its
/// size. RGBA8 is copied, BGRA8 swizzled and NV12 (what Windows' display capture
/// delivers) converted with its frame's matrix and range. `None`, leaving `out` as it
/// was, when the frame is not one of those.
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
                if info.format == PixelFormat::Rgba8 {
                    dst.copy_from_slice(row);
                } else {
                    for (d, s) in dst.chunks_exact_mut(4).zip(row.chunks_exact(4)) {
                        d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
                    }
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
