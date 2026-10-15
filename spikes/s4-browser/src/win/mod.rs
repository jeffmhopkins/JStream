//! The Windows run: CEF in off-screen mode with shared textures, each paint opened on
//! wgpu's D3D12 device, copied out of CEF's pool and composited; CEF's audio handler
//! measured alongside.
//!
//! Threads: CEF's UI thread is the main thread (`run_message_loop`), and it delivers the
//! paints, console messages and our tasks. Audio packets arrive on CEF's audio thread.
//! All state sits in [`Shared`] behind mutexes.

mod crash;
mod gpu;
mod handlers;
mod procs;

use std::collections::HashSet;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use cef::{
    AcceleratedPaintInfo, BrowserSettings, CefString, LogSeverity, Settings, ThreadId, WindowInfo,
};

use crate::err::{Context, Error, Result, bail};
pub use crash::stage;
use handlers::Ctx;
use s4_browser::analysis::{ChannelStats, FrameTracker, ProbeTally, decode_counter};
use s4_browser::cli::{Args, Helper};
use s4_browser::json::{self, Obj};
use s4_browser::page;
use s4_browser::stats::Samples;

/// What the audio handler asks CEF for: 48 kHz, 10 ms packets (CEF's default is 1024
/// frames). The engine's mixer runs at 48 kHz (ARCHITECTURE.md › threading).
pub const REQUESTED_AUDIO: (i32, i32) = (48_000, 480);

/// How long after the window ends the loop may keep running before it is stopped
/// regardless (the browser didn't close).
const CLOSE_GRACE_MS: i64 = 15_000;

const WARMUP: u8 = 0;
const MEASURE: u8 = 1;
const DONE: u8 = 2;

/// CEF's subprocesses re-enter `main` with `--type=...` (same-exe layout). Returns their
/// exit code; `None` in the browser process.
pub fn subprocess() -> Option<i32> {
    if !std::env::args_os().any(|a| a.to_string_lossy().starts_with("--type=")) {
        return None;
    }
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let args = cef::args::Args::new();
    Some(cef::execute_process(
        Some(args.as_main_args()),
        None,
        std::ptr::null_mut(),
    ))
}

/// The result: one JSON object and a short text summary.
pub struct Report {
    pub json: String,
    pub text: String,
}

pub fn run(args: &Args) -> Result<Report> {
    crash::install();
    stage(format_args!("start, {args:?}"));
    // Must come before any other CEF call: it fixes the API version the bindings use.
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);

    let gpu = gpu::Gpu::new()?;
    let facts = gpu::adapter_facts(&gpu.dxgi_adapter)?;
    let wgpu_info = gpu.adapter.get_info();
    let selection = gpu.selection.clone();
    stage(format_args!("wgpu device on {} ({selection})", facts.name));
    let compositor = gpu::Compositor::new(gpu, (args.width, args.height));

    let url = match &args.url {
        Some(url) => url.clone(),
        None => write_test_page(args)?,
    };
    let shared = Arc::new(Shared::new(args, compositor));
    let ctx = Ctx(shared.clone());

    let exe = std::env::current_exe().context("current_exe")?;
    let exe_dir = exe.parent().context("exe has no folder")?.to_path_buf();
    let helper = match args.helper {
        Helper::Same => None,
        Helper::Separate => {
            let path = exe_dir.join("s4-browser-helper.exe");
            if !path.is_file() {
                bail!(
                    "{} is missing; build it with `cargo build --release` (both bins)",
                    path.display()
                );
            }
            Some(path)
        }
    };
    let cache = std::env::temp_dir().join("s4-browser-cache");
    let settings = Settings {
        // ADR-0021 wants the sandbox; on Windows it needs CEF's bootstrap layout (README).
        no_sandbox: 1,
        windowless_rendering_enabled: 1,
        browser_subprocess_path: path_string(helper.as_ref()),
        root_cache_path: path_string(Some(&cache)),
        log_file: args.cef_log.as_str().into(),
        log_severity: LogSeverity::WARNING,
        ..Default::default()
    };
    let main_args = cef::args::Args::new();
    let mut app = handlers::S4App::new();
    stage("initializing CEF");
    let init_started = Instant::now();
    if cef::initialize(
        Some(main_args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    ) != 1
    {
        bail!(
            "cef::initialize failed; see {} (is libcef.dll and its resources next to the exe?)",
            args.cef_log
        );
    }
    let init_ms = init_started.elapsed().as_secs_f64() * 1e3;
    stage(format_args!(
        "CEF initialized in {init_ms:.0} ms; creating the browser"
    ));

    let window_info = WindowInfo {
        windowless_rendering_enabled: 1,
        shared_texture_enabled: 1,
        ..Default::default()
    };
    let browser_settings = BrowserSettings {
        windowless_frame_rate: args.fps as i32,
        // Fully transparent (ARGB), so the page's own background decides.
        background_color: 0,
        ..Default::default()
    };
    let mut client = handlers::client(&ctx);
    let browser = cef::browser_host_create_browser_sync(
        Some(&window_info),
        Some(&mut client),
        Some(&url.as_str().into()),
        Some(&browser_settings),
        None,
        None,
    )
    .context("browser_host_create_browser_sync returned no browser")?;
    stage(format_args!("browser created, loading {url}"));

    let warmup_ms = (args.warmup * 1e3) as i64;
    let end_ms = warmup_ms + (args.seconds * 1e3) as i64;
    cef::post_delayed_task(
        ThreadId::UI,
        Some(&mut handlers::StartWindow::new(ctx.clone())),
        warmup_ms,
    );
    cef::post_delayed_task(
        ThreadId::UI,
        Some(&mut handlers::EndWindow::new(ctx.clone(), browser.clone())),
        end_ms,
    );
    cef::post_delayed_task(
        ThreadId::UI,
        Some(&mut handlers::QuitLoop::new()),
        end_ms + CLOSE_GRACE_MS,
    );
    cef::run_message_loop();
    stage("message loop ended");
    let closed = shared.closed.load(Ordering::Acquire) == 1;
    drop(browser);
    drop(client);

    // Release the GPU objects before CEF goes, so nothing can touch a CEF texture after.
    let compositor = lock(&shared.video).compositor.take();
    drop(compositor);
    let report = shared.report(ReportContext {
        args,
        facts: facts.json(),
        wgpu: Obj::new()
            .str("name", &wgpu_info.name)
            .str("driver", &wgpu_info.driver)
            .str("driver_info", &wgpu_info.driver_info)
            .str("selection", &selection)
            .build(),
        helper: helper.as_ref().map(|p| p.display().to_string()),
        init_ms,
        closed,
        runtime: runtime_json(&exe_dir),
    });
    stage("shutting CEF down");
    cef::shutdown();
    stage("done");
    Ok(report)
}

fn path_string(path: Option<&PathBuf>) -> CefString {
    path.map(|p| p.display().to_string())
        .unwrap_or_default()
        .as_str()
        .into()
}

fn write_test_page(args: &Args) -> Result<String> {
    let path = std::env::temp_dir().join(format!("s4-page-{}.html", std::process::id()));
    std::fs::write(&path, page::html(args.width, args.height))
        .with_context(|| format!("write {}", path.display()))?;
    Ok(format!(
        "file:///{}",
        path.display().to_string().replace('\\', "/")
    ))
}

/// The CEF runtime CEF's build script copied next to the exe: its size (ADR-0021 puts it at
/// 150–250 MB) and its top-level files, so the notices it ships with can be checked.
fn runtime_json(dir: &std::path::Path) -> String {
    fn size_of_tree(path: &std::path::Path) -> u64 {
        let Ok(meta) = std::fs::metadata(path) else {
            return 0;
        };
        if meta.is_file() {
            return meta.len();
        }
        std::fs::read_dir(path)
            .map(|entries| entries.flatten().map(|e| size_of_tree(&e.path())).sum())
            .unwrap_or(0)
    }
    // Build products of this crate and cargo's own folders aren't part of the runtime.
    let ours = |name: &str| {
        name.starts_with("s4-browser")
            || name.contains("s4_browser")
            || matches!(
                name,
                "build" | "deps" | "examples" | "incremental" | ".fingerprint"
            )
            || name.starts_with('.')
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return "null".into();
    };
    let mut files: Vec<(String, u64)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (!ours(&name)).then(|| (name, size_of_tree(&e.path())))
        })
        .collect();
    files.sort();
    let total: u64 = files.iter().map(|(_, b)| b).sum();
    Obj::new()
        .str("dir", &dir.display().to_string())
        .int("bytes", total)
        .num("mb", total as f64 / (1024.0 * 1024.0))
        .raw(
            "files",
            json::array(
                files
                    .iter()
                    .map(|(name, bytes)| Obj::new().str("name", name).int("bytes", *bytes).build()),
            ),
        )
        .build()
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

/// Everything the callbacks share.
pub struct Shared {
    width: u32,
    height: u32,
    test_page: bool,
    t0: Instant,
    phase: AtomicU8,
    /// 1 once CEF said the browser is closing.
    closed: AtomicU8,
    video: Mutex<Video>,
    audio: Mutex<Audio>,
    console: Mutex<Console>,
    window: Mutex<Window>,
}

struct Video {
    compositor: Option<gpu::Compositor>,
    first_paint_ms: Option<f64>,
    first_error: Option<String>,
    stats: VideoStats,
}

#[derive(Default)]
struct VideoStats {
    accelerated: u64,
    software: u64,
    popup: u64,
    failures: u64,
    last_paint: Option<Instant>,
    last_timestamp_us: Option<u64>,
    /// Wall-clock time between paints.
    intervals: Samples,
    /// Between CEF's own frame timestamps.
    timestamp_intervals: Samples,
    open: Samples,
    copy: Samples,
    composite: Samples,
    callback: Samples,
    handles: HashSet<usize>,
    texture: Option<gpu::TextureDesc>,
    texture_changes: u64,
    /// CEF's own description: color type, coded size.
    reported: Option<(i32, i32, i32)>,
    frames: FrameTracker,
    probes: ProbeTally,
}

#[derive(Default)]
struct Audio {
    /// Sample rate, frames per buffer and channel layout from `OnAudioStreamStarted`.
    started: Option<(i32, i32, i32)>,
    channels: i32,
    starts: u64,
    stops: u64,
    errors: Vec<String>,
    stats: AudioStats,
}

#[derive(Default)]
struct AudioStats {
    packets: u64,
    frames: u64,
    bad_packets: u64,
    packet_frames: Option<(i32, i32)>,
    first: Option<(i64, Instant, u64)>,
    last: Option<(i64, Instant, u64)>,
    last_at: Option<Instant>,
    intervals: Samples,
    /// `pts` minus the wall clock when the packet arrived.
    pts_lead: Samples,
    channels: Vec<ChannelStats>,
}

#[derive(Default)]
struct Console {
    report: Option<String>,
    other: Vec<String>,
    count: u64,
}

#[derive(Default)]
struct Window {
    start: Option<Instant>,
    end: Option<Instant>,
    procs_start: Vec<procs::Sample>,
    procs_end: Vec<procs::Sample>,
}

struct ReportContext<'a> {
    args: &'a Args,
    facts: String,
    wgpu: String,
    helper: Option<String>,
    init_ms: f64,
    closed: bool,
    runtime: String,
}

impl Shared {
    fn new(args: &Args, compositor: gpu::Compositor) -> Self {
        Self {
            width: args.width,
            height: args.height,
            test_page: args.url.is_none(),
            t0: Instant::now(),
            phase: AtomicU8::new(WARMUP),
            closed: AtomicU8::new(0),
            video: Mutex::new(Video {
                compositor: Some(compositor),
                first_paint_ms: None,
                first_error: None,
                stats: VideoStats::default(),
            }),
            audio: Mutex::default(),
            console: Mutex::default(),
            window: Mutex::default(),
        }
    }

    fn phase(&self) -> u8 {
        self.phase.load(Ordering::Acquire)
    }

    pub fn view_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn start_window(&self) {
        let procs = procs::snapshot();
        lock(&self.video).stats = VideoStats::default();
        lock(&self.audio).stats = AudioStats::default();
        let mut w = lock(&self.window);
        w.procs_start = procs;
        w.start = Some(Instant::now());
        self.phase.store(MEASURE, Ordering::Release);
        stage("measured window starts");
    }

    pub fn end_window(&self) {
        self.phase.store(DONE, Ordering::Release);
        let mut w = lock(&self.window);
        w.end = Some(Instant::now());
        w.procs_end = procs::snapshot();
        stage("measured window ends; closing the browser");
    }

    pub fn on_browser_closed(&self) {
        self.closed.store(1, Ordering::Release);
    }

    pub fn on_software_paint(&self, view: bool) {
        if self.phase() != MEASURE {
            return;
        }
        let mut v = lock(&self.video);
        if view {
            v.stats.software += 1;
        } else {
            v.stats.popup += 1;
        }
    }

    pub fn on_popup_paint(&self) {
        if self.phase() == MEASURE {
            lock(&self.video).stats.popup += 1;
        }
    }

    pub fn on_accelerated_paint(&self, info: Option<&AcceleratedPaintInfo>) {
        let phase = self.phase();
        if phase == DONE {
            return;
        }
        let measuring = phase == MEASURE;
        let now = Instant::now();
        let mut guard = lock(&self.video);
        let v = &mut *guard;
        if v.first_paint_ms.is_none() {
            v.first_paint_ms = Some((now - self.t0).as_secs_f64() * 1e3);
            stage("first accelerated paint");
        }
        let Some(info) = info else {
            v.stats.failures += u64::from(measuring);
            v.first_error
                .get_or_insert_with(|| "OnAcceleratedPaint without info".into());
            return;
        };
        let handle: *mut c_void = info.shared_texture_handle;
        if measuring {
            let s = &mut v.stats;
            s.accelerated += 1;
            if let Some(last) = s.last_paint {
                s.intervals.push((now - last).as_secs_f64() * 1e3);
            }
            if let Some(last) = s.last_timestamp_us {
                s.timestamp_intervals
                    .push(info.extra.timestamp.wrapping_sub(last) as f64 / 1e3);
            }
            s.handles.insert(handle as usize);
            s.reported = Some((
                *info.format.as_ref() as i32,
                info.extra.coded_size.width,
                info.extra.coded_size.height,
            ));
        }
        v.stats.last_paint = Some(now);
        v.stats.last_timestamp_us = Some(info.extra.timestamp);
        let Some(compositor) = v.compositor.as_mut() else {
            return;
        };
        match compositor.paint(handle) {
            Ok(frame) => {
                if !measuring {
                    return;
                }
                let s = &mut v.stats;
                s.open.push(frame.open_ms);
                s.copy.push(frame.copy_ms);
                s.composite.push(frame.composite_ms);
                s.callback.push(ms_since(now));
                if s.texture.is_some_and(|t| t != frame.texture) {
                    s.texture_changes += 1;
                }
                s.texture = Some(frame.texture);
                if self.test_page {
                    s.frames.record(decode_counter(&frame.page_row));
                    s.probes
                        .record(&frame.page_row, &frame.composite_row, gpu::BACKGROUND);
                }
            }
            Err(e) => {
                v.stats.failures += u64::from(measuring);
                if v.first_error.is_none() {
                    let owner = gpu::owner_of(handle);
                    let msg = format!("{e} (CEF's texture opens on: {owner})");
                    stage(format_args!("paint failed: {msg}"));
                    v.first_error = Some(msg);
                }
            }
        }
    }

    pub fn on_audio_started(&self, params: Option<(i32, i32, i32)>, channels: i32) {
        let mut a = lock(&self.audio);
        a.starts += 1;
        a.started = params;
        a.channels = channels;
        stage(format_args!(
            "audio stream started: {params:?} (rate, frames, layout), {channels} channels"
        ));
    }

    pub fn on_audio_stopped(&self) {
        lock(&self.audio).stops += 1;
    }

    pub fn on_audio_error(&self, message: String) {
        let mut a = lock(&self.audio);
        if a.errors.len() < 10 {
            a.errors.push(message);
        }
    }

    pub fn on_audio_packet(&self, data: *mut *const f32, frames: i32, pts: i64) {
        if self.phase() != MEASURE {
            return;
        }
        let now = Instant::now();
        let wall_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(f64::NAN, |d| d.as_secs_f64() * 1e3);
        let mut guard = lock(&self.audio);
        let a = &mut *guard;
        let channels = usize::try_from(a.channels).unwrap_or(0);
        let s = &mut a.stats;
        let Ok(len) = usize::try_from(frames) else {
            s.bad_packets += 1;
            return;
        };
        if data.is_null() || len == 0 || channels == 0 {
            s.bad_packets += 1;
            return;
        }
        s.packets += 1;
        s.packet_frames = Some(match s.packet_frames {
            Some((lo, hi)) => (lo.min(frames), hi.max(frames)),
            None => (frames, frames),
        });
        if let Some(last) = s.last_at {
            s.intervals.push((now - last).as_secs_f64() * 1e3);
        }
        s.last_at = Some(now);
        s.pts_lead.push(pts as f64 - wall_ms);
        s.first.get_or_insert((pts, now, s.frames));
        s.last = Some((pts, now, s.frames));
        s.frames += len as u64;
        if s.channels.len() < channels {
            s.channels.resize(channels, ChannelStats::default());
        }
        for (ch, stats) in s.channels.iter_mut().enumerate().take(channels) {
            // SAFETY: CEF passes `channels` (from OnAudioStreamStarted) planar buffers in
            // `data`, valid for the duration of this callback.
            let plane = unsafe { *data.add(ch) };
            if plane.is_null() {
                continue;
            }
            // SAFETY: each plane holds `frames` f32 samples, valid during the callback.
            let samples = unsafe { std::slice::from_raw_parts(plane, len) };
            stats.record(samples);
        }
    }

    pub fn on_console(&self, message: String) {
        let mut c = lock(&self.console);
        c.count += 1;
        if let Some(report) = message.strip_prefix(page::REPORT_PREFIX) {
            c.report = Some(report.to_owned());
        } else if c.other.len() < 10 {
            c.other.push(message);
        }
    }

    fn report(&self, ctx: ReportContext<'_>) -> Report {
        let args = ctx.args;
        let w = lock(&self.window);
        let seconds = match (w.start, w.end) {
            (Some(s), Some(e)) => (e - s).as_secs_f64(),
            _ => f64::NAN,
        };
        let mut warnings = Vec::new();
        if !ctx.closed {
            warnings.push(format!(
                "the browser did not close within {} s of the window's end",
                CLOSE_GRACE_MS / 1000
            ));
        }
        if w.start.is_none() {
            warnings.push("the measured window never started".to_owned());
        }

        let v = lock(&self.video);
        let s = &v.stats;
        let period = 1e3 / f64::from(args.fps);
        let video = Obj::new()
            .num("first_paint_ms", v.first_paint_ms.unwrap_or(f64::NAN))
            .int("accelerated_paints", s.accelerated)
            .int("software_paints", s.software)
            .int("popup_paints", s.popup)
            .int("failed_paints", s.failures)
            .opt_str("first_error", v.first_error.as_deref())
            .num("paint_fps", s.accelerated as f64 / seconds)
            .raw("paint_interval_ms", s.intervals.summary().json())
            .int(
                "intervals_over_1_5_periods",
                s.intervals.count_above(period * 1.5) as u64,
            )
            .raw(
                "cef_timestamp_interval_ms",
                s.timestamp_intervals.summary().json(),
            )
            .raw("open_ms", s.open.summary().json())
            .raw("copy_and_wait_ms", s.copy.summary().json())
            .raw("composite_and_probe_ms", s.composite.summary().json())
            .raw("callback_ms", s.callback.summary().json())
            .int("distinct_handles", s.handles.len() as u64)
            .raw(
                "texture",
                s.texture.map_or_else(|| "null".into(), |t| t.json()),
            )
            .int("texture_changes", s.texture_changes)
            .raw(
                "cef_reported",
                s.reported.map_or_else(
                    || "null".into(),
                    |(format, w, h)| {
                        Obj::new()
                            .int("color_type", format)
                            .int("coded_width", w)
                            .int("coded_height", h)
                            .build()
                    },
                ),
            )
            .raw(
                "page_frames",
                if self.test_page {
                    s.frames.json()
                } else {
                    "null".into()
                },
            )
            .raw(
                "probes",
                if self.test_page {
                    s.probes.json()
                } else {
                    "null".into()
                },
            )
            .build();
        if s.software > 0 {
            warnings.push(format!(
                "{} software (OnPaint) paints: CEF fell back to the CPU path",
                s.software
            ));
        }

        let a = lock(&self.audio);
        let st = &a.stats;
        let rate = a.started.map_or(f64::NAN, |(r, _, _)| f64::from(r));
        let (by_pts, by_wall) = match (st.first, st.last) {
            (Some((p0, t0, f0)), Some((p1, t1, f1))) if f1 > f0 => {
                let frames = (f1 - f0) as f64;
                (
                    frames / ((p1 - p0) as f64 / 1e3),
                    frames / (t1 - t0).as_secs_f64(),
                )
            }
            _ => (f64::NAN, f64::NAN),
        };
        let channels: Vec<String> = st.channels.iter().map(|c| c.json(rate)).collect();
        let audio = Obj::new()
            .raw(
                "requested",
                Obj::new()
                    .int("sample_rate", REQUESTED_AUDIO.0)
                    .int("frames_per_buffer", REQUESTED_AUDIO.1)
                    .build(),
            )
            .raw(
                "started",
                a.started.map_or_else(
                    || "null".into(),
                    |(rate, frames, layout)| {
                        Obj::new()
                            .int("sample_rate", rate)
                            .int("frames_per_buffer", frames)
                            .int("channel_layout", layout)
                            .int("channels", a.channels)
                            .build()
                    },
                ),
            )
            .int("stream_starts", a.starts)
            .int("stream_stops", a.stops)
            .raw(
                "errors",
                json::array(a.errors.iter().map(|e| json::string(e))),
            )
            .int("packets", st.packets)
            .int("bad_packets", st.bad_packets)
            .int("frames", st.frames)
            .raw(
                "packet_frames_min_max",
                st.packet_frames
                    .map_or_else(|| "null".into(), |(lo, hi)| format!("[{lo},{hi}]")),
            )
            .raw("packet_interval_ms", st.intervals.summary().json())
            .raw("pts_minus_wall_ms", st.pts_lead.summary().json())
            .num("rate_by_pts", by_pts)
            .num("rate_by_wall_clock", by_wall)
            .raw("channels", json::array(channels))
            .raw(
                "expected",
                if self.test_page {
                    Obj::new()
                        .num("left_hz", page::TONE_LEFT_HZ)
                        .num("right_hz", page::TONE_RIGHT_HZ)
                        .num("peak_dbfs", s4_browser::analysis::dbfs(page::TONE_GAIN))
                        .build()
                } else {
                    "null".into()
                },
            )
            .build();
        if a.starts == 0 {
            warnings.push("CEF never started an audio stream".to_owned());
        }

        let c = lock(&self.console);
        let page_json = Obj::new()
            .str(
                "url_kind",
                if self.test_page { "test page" } else { "--url" },
            )
            // Only the test page's line is known to be JSON; another page's stays text.
            .raw(
                "report",
                match &c.report {
                    Some(r) if self.test_page => r.clone(),
                    Some(r) => json::string(r),
                    None => "null".into(),
                },
            )
            .int("console_messages", c.count)
            .raw(
                "other_console",
                json::array(c.other.iter().map(|m| json::string(m))),
            )
            .build();

        let processes = procs::json(&w.procs_start, &w.procs_end, seconds);
        let json = Obj::new()
            .str("spike", "S4")
            .str(
                "cef_version",
                String::from_utf8_lossy(cef::sys::CEF_VERSION).trim_end_matches('\0'),
            )
            .str("helper", args.helper.name())
            .opt_str("helper_path", ctx.helper.as_deref())
            .raw("view_size", format!("[{},{}]", args.width, args.height))
            .int("target_fps", args.fps)
            .num("warmup_s", args.warmup)
            .num("seconds_requested", args.seconds)
            .num("seconds_measured", seconds)
            .num("cef_initialize_ms", ctx.init_ms)
            .raw("adapter", ctx.facts)
            .raw("wgpu", ctx.wgpu)
            .raw("dxgi_adapters", gpu::adapters_json())
            .raw("video", video)
            .raw("audio", audio)
            .raw("page", page_json)
            .raw("processes", processes)
            .raw("runtime", ctx.runtime)
            .raw(
                "warnings",
                json::array(warnings.iter().map(|w| json::string(w))),
            )
            .build();

        let mut text = String::new();
        let mut line = |l: String| {
            text.push_str(&l);
            text.push('\n');
        };
        line(format!(
            "video: {} accelerated paints in {seconds:.2} s = {:.2} fps (target {}), {} software, {} failed",
            s.accelerated,
            s.accelerated as f64 / seconds,
            args.fps,
            s.software,
            s.failures
        ));
        line(format!("  interval   {}", s.intervals.summary().text()));
        line(format!("  open       {}", s.open.summary().text()));
        line(format!("  copy+wait  {}", s.copy.summary().text()));
        line(format!("  callback   {}", s.callback.summary().text()));
        if self.test_page {
            let f = &s.frames;
            let p = &s.probes;
            line(format!(
                "  page frames: {} new, {} repeated, {} skipped (max run {}), {} unreadable",
                f.new_frames, f.repeats, f.skipped, f.max_skip, f.undecodable
            ));
            line(format!(
                "  transparency: page clear {}/{}, half alpha {}/{}, composite clear {}/{}, blend {}/{}",
                p.page_clear_ok,
                p.frames,
                p.page_half_alpha_ok,
                p.frames,
                p.composite_clear_ok,
                p.frames,
                p.composite_blend_ok,
                p.frames
            ));
        }
        line(format!(
            "audio: {} packets, {} frames, started {:?}, {} errors",
            st.packets,
            st.frames,
            a.started,
            a.errors.len()
        ));
        for (i, ch) in st.channels.iter().enumerate() {
            line(format!(
                "  channel {i}: {:.1} Hz, peak {:.1} dBFS",
                ch.frequency(rate),
                s4_browser::analysis::dbfs(f64::from(ch.peak))
            ));
        }
        if let Some(e) = &v.first_error {
            line(format!("first error: {e}"));
        }
        for w in &warnings {
            line(format!("warning: {w}"));
        }
        Report { json, text }
    }
}

impl From<Error> for Report {
    fn from(e: Error) -> Self {
        Report {
            json: Obj::new()
                .str("spike", "S4")
                .str("error", &e.to_string())
                .build(),
            text: format!("error: {e}"),
        }
    }
}
