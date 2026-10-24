//! The browser helper's CEF code (Windows, feature `helper`; ADR-0048, ARCH › Browser
//! source): what `jstream-browser-helper.dll` runs when CEF's bootstrap exe calls it,
//! in CEF's browser process and in every one of its sandboxed subprocesses.
//!
//! **In a subprocess** (renderer, GPU, utility) it hands the process to CEF.
//!
//! **In the browser process** it connects to the engine's two pipes (named by
//! `--jstream-pipe`), starts CEF with the sandbox on, off-screen rendering, profiles in
//! memory and the engine's cache and log settings, and runs CEF's message loop on the
//! process's main thread, which is CEF's UI thread. A reader thread takes the engine's
//! messages: pages are opened, restyled and closed from it, which CEF allows on any of
//! the browser process's threads, while a new adapter and shutting down are posted to the
//! UI thread. Each page is a windowless browser with shared textures, painting at its
//! frame rate with a transparent background; its paints are copied into textures it
//! shares with the engine ([`d3d11`]), its audio is sent as interleaved stereo, and its
//! requests go through its [`RequestPolicy`].
//!
//! **Shutting down.** The engine's `Shutdown`, its pipe closing, or a write to it failing
//! closes every page; the loop quits once they have closed, or after [`CLOSE_GRACE`],
//! and CEF shuts down on the UI thread before the process exits.

mod d3d11;
mod handlers;

use std::collections::HashMap;
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use ::cef::{BrowserSettings, CefString, ImplBrowser, ImplBrowserHost, ImplFrame, LogSeverity};

use crate::wire::{self, ToEngine, ToHelper};
use crate::{PageSpec, RequestPolicy, css_script};
use d3d11::{Gpu, PageTextures};

/// How long shutting down waits for open pages to close before CEF's loop is quit
/// anyway.
const CLOSE_GRACE: Duration = Duration::from_secs(10);

/// The switch that names the engine's pipes.
const PIPE_SWITCH: &str = "--jstream-pipe=";

/// The most stereo frames one audio message carries.
const AUDIO_FRAMES: usize = wire::MAX_AUDIO_SAMPLES / 2;

/// Runs the helper's process for CEF's bootstrap exe, which calls it from the DLL's
/// `RunWinMain` export, and returns the process's exit code.
///
/// # Safety
/// `sandbox_info` must be the sandbox information CEF's bootstrap exe passed to
/// `RunWinMain`, unchanged; CEF reads it.
pub unsafe fn run_helper(sandbox_info: *mut c_void) -> i32 {
    // Must come before any other CEF call: it fixes the API version the bindings use.
    let _ = ::cef::api_hash(::cef::sys::CEF_API_VERSION_LAST, 0);
    let args = ::cef::args::Args::new();
    let mut app = handlers::app();
    let code = ::cef::execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        sandbox_info.cast(),
    );
    if code >= 0 {
        // A subprocess, which CEF has run to its end.
        return code;
    }
    match browser_process(&args, &mut app, sandbox_info) {
        Ok(()) => 0,
        Err(()) => 1,
    }
}

/// CEF's browser process: connects to the engine, starts CEF and runs its loop. `Err`
/// when it couldn't start, which it has told the engine if it could.
fn browser_process(
    args: &::cef::args::Args,
    app: &mut ::cef::App,
    sandbox_info: *mut c_void,
) -> Result<(), ()> {
    let name = std::env::args()
        .find_map(|arg| arg.strip_prefix(PIPE_SWITCH).map(str::to_owned))
        .ok_or(())?;
    let mut from_engine = File::open(format!(r"\\.\pipe\{name}-in")).map_err(|_| ())?;
    let to_engine = OpenOptions::new()
        .write(true)
        .open(format!(r"\\.\pipe\{name}-out"))
        .map_err(|_| ())?;
    let host = Arc::new(Host {
        to_engine: Mutex::new(to_engine),
        pages: Mutex::new(HashMap::new()),
        gpu: Mutex::new(None),
        generation: AtomicU64::new(0),
        open: AtomicUsize::new(0),
        started: AtomicBool::new(false),
        stopping: AtomicBool::new(false),
    });
    let Ok(Some(ToHelper::Start {
        cache_root,
        log_file,
    })) = wire::receive::<ToHelper>(&mut from_engine)
    else {
        host.send(&ToEngine::StartFailed {
            reason: "the engine didn't send the start first".into(),
        });
        return Err(());
    };
    let path = |p: &str| -> CefString { p.into() };
    let settings = ::cef::Settings {
        // The sandbox is on (ADR-0021, ADR-0048): CEF's bootstrap exe set it up, and every
        // subprocess starts as the same exe.
        no_sandbox: 0,
        windowless_rendering_enabled: 1,
        root_cache_path: path(&cache_root),
        // No `cache_path`: pages run in CEF's in-memory profile and keep nothing on disk.
        persist_session_cookies: 0,
        log_file: path(log_file.as_deref().unwrap_or_default()),
        log_severity: LogSeverity::WARNING,
        ..Default::default()
    };
    let started = ::cef::initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(app),
        sandbox_info.cast(),
    ) == 1;
    if !started {
        host.send(&ToEngine::StartFailed {
            reason: "cef::initialize failed: libcef.dll and its resources must be next to \
                     the helper"
                .into(),
        });
        return Err(());
    }
    host.started.store(true, Ordering::Release);
    host.send(&ToEngine::Ready);
    let reader = Arc::clone(&host);
    let spawned = std::thread::Builder::new()
        .name("jstream-helper-pipe".into())
        .spawn(move || reader.read(from_engine));
    if spawned.is_err() {
        host.warn("the helper couldn't start its pipe reader");
        host.stop();
    }
    ::cef::run_message_loop();
    // The loop has quit, so no paint can run any more: let go of the device first.
    host.pages().clear();
    *lock(&host.gpu) = None;
    ::cef::shutdown();
    Ok(())
}

/// What the helper's threads and CEF's callbacks share.
pub(crate) struct Host {
    to_engine: Mutex<File>,
    pages: Mutex<HashMap<u32, Arc<PageShared>>>,
    /// The D3D11 device on the engine's adapter; `None` delivers no frames.
    gpu: Mutex<Option<Gpu>>,
    /// Bumped with each device, so pages remake their textures.
    generation: AtomicU64,
    /// Browsers asked for and not yet closed.
    open: AtomicUsize,
    /// Set once CEF has started, so its tasks may be posted.
    started: AtomicBool,
    /// Set when shutting down: the UI thread quits its loop at the last close.
    stopping: AtomicBool,
}

impl Host {
    /// Sends one message to the engine. A failed write means the engine is gone: the
    /// helper shuts down.
    fn send(self: &Arc<Self>, message: &ToEngine) {
        let sent = wire::send(&mut *lock(&self.to_engine), message);
        if sent.is_err() {
            self.stop();
        }
    }

    /// Tells the engine about a problem, for its log.
    pub(crate) fn warn(self: &Arc<Self>, message: &str) {
        self.send(&ToEngine::Warning {
            message: message.to_owned(),
        });
    }

    fn pages(&self) -> MutexGuard<'_, HashMap<u32, Arc<PageShared>>> {
        lock(&self.pages)
    }

    /// The pipe's reader thread: the engine's requests until its pipe closes.
    fn read(self: &Arc<Self>, mut from_engine: File) {
        while let Ok(Some(message)) = wire::receive::<ToHelper>(&mut from_engine) {
            self.handle(message);
        }
        self.stop();
    }

    fn handle(self: &Arc<Self>, message: ToHelper) {
        match message {
            ToHelper::Open { page, spec } => self.open(page, &spec),
            ToHelper::SetCss { page, css } => {
                if let Some(page) = self.pages().get(&page).cloned() {
                    page.set_css(css);
                }
            }
            ToHelper::Close { page } => {
                if let Some(page) = self.pages().remove(&page) {
                    page.close();
                }
            }
            ToHelper::Released { page, slot } => {
                if let Some(page) = self.pages().get(&page).cloned() {
                    page.textures.release(slot);
                }
            }
            ToHelper::Adapter { luid } => {
                let host = Arc::clone(self);
                post(move || host.use_adapter(luid));
            }
            ToHelper::Shutdown => self.stop(),
            ToHelper::Start { .. } => self.warn("the engine sent a second start"),
        }
    }

    /// Makes the device pages copy onto (UI thread).
    fn use_adapter(self: &Arc<Self>, luid: Option<u64>) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let gpu = luid.and_then(|luid| match Gpu::on_adapter(luid, generation) {
            Ok(gpu) => Some(gpu),
            Err(reason) => {
                self.warn(&format!("pages can't deliver frames: {reason}"));
                None
            }
        });
        *lock(&self.gpu) = gpu;
    }

    fn open(self: &Arc<Self>, id: u32, spec: &PageSpec) {
        let page = Arc::new(PageShared {
            id,
            size: (spec.size.width(), spec.size.height()),
            css: Mutex::new(spec.css.clone()),
            browser: Mutex::new(None),
            closing: AtomicBool::new(false),
            textures: PageTextures::default(),
            paint_warned: AtomicBool::new(false),
            channels: AtomicUsize::new(0),
            host: Arc::clone(self),
            policy: RequestPolicy::for_page(spec),
        });
        let window_info = ::cef::WindowInfo {
            windowless_rendering_enabled: 1,
            shared_texture_enabled: 1,
            ..Default::default()
        };
        let settings = BrowserSettings {
            windowless_frame_rate: i32::try_from(spec.fps).unwrap_or(30),
            // Fully transparent (ARGB), so the page's own background decides.
            background_color: 0,
            ..Default::default()
        };
        let mut client = handlers::client(&page);
        let url: CefString = spec.location.to_url().as_str().into();
        self.pages().insert(id, Arc::clone(&page));
        self.open.fetch_add(1, Ordering::AcqRel);
        // Asynchronous: the browser arrives in `OnAfterCreated` on the UI thread.
        let asked = ::cef::browser_host_create_browser(
            Some(&window_info),
            Some(&mut client),
            Some(&url),
            Some(&settings),
            None,
            None,
        ) == 1;
        if asked {
            self.send(&ToEngine::Opened { page: id });
        } else {
            self.open.fetch_sub(1, Ordering::AcqRel);
            self.pages().remove(&id);
            self.send(&ToEngine::OpenFailed {
                page: id,
                reason: "CEF refused to create the browser".into(),
            });
        }
    }

    /// Closes every page and quits the loop once they have closed, or after
    /// [`CLOSE_GRACE`]. From any thread; only the first call does anything.
    fn stop(self: &Arc<Self>) {
        if !self.started.load(Ordering::Acquire) || self.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        let pages: Vec<_> = self.pages().drain().map(|(_, page)| page).collect();
        for page in pages {
            page.close();
        }
        let host = Arc::clone(self);
        post(move || host.quit_if_idle());
        ::cef::post_delayed_task(
            ::cef::ThreadId::UI,
            Some(&mut handlers::task(::cef::quit_message_loop)),
            i64::try_from(CLOSE_GRACE.as_millis()).unwrap_or(i64::MAX),
        );
    }

    /// A browser closed (`OnBeforeClose`, UI thread). The last one quits the loop once
    /// the helper is stopping.
    pub(crate) fn browser_closed(&self) {
        let left = self.open.fetch_sub(1, Ordering::AcqRel).saturating_sub(1);
        if left == 0 && self.stopping.load(Ordering::Acquire) {
            ::cef::quit_message_loop();
        }
    }

    /// Quits the loop now if no browser is open. On the UI thread.
    fn quit_if_idle(&self) {
        if self.open.load(Ordering::Acquire) == 0 {
            ::cef::quit_message_loop();
        }
    }
}

/// Runs `f` on CEF's UI thread.
fn post(f: impl Fn() + Send + Sync + 'static) {
    ::cef::post_task(::cef::ThreadId::UI, Some(&mut handlers::task(f)));
}

/// One page's state, shared by the helper's threads and CEF's callbacks for its browser.
pub(crate) struct PageShared {
    /// The engine's number for the page.
    pub(crate) id: u32,
    /// The view's size in pixels.
    pub(crate) size: (u32, u32),
    css: Mutex<String>,
    browser: Mutex<Option<SendBrowser>>,
    /// Set when the engine closes the page: a browser that arrives later is closed at
    /// once.
    closing: AtomicBool,
    textures: PageTextures,
    /// Whether a failed paint was reported since the last good one.
    paint_warned: AtomicBool,
    /// Channels in the page's current audio stream (`OnAudioStreamStarted`).
    pub(crate) channels: AtomicUsize,
    pub(crate) host: Arc<Host>,
    /// What the page may request; the request handler cancels the rest.
    pub(crate) policy: RequestPolicy,
}

/// A browser, held across threads.
struct SendBrowser(::cef::Browser);

// SAFETY: CEF's browser object is reference counted with atomic counts, and the only
// methods called through it (`host`, `main_frame`, then `close_browser` and
// `execute_java_script`) are documented as callable on any thread of the browser process.
unsafe impl Send for SendBrowser {}

impl PageShared {
    /// The browser arrived (`OnAfterCreated`, UI thread). A page already closed closes it
    /// at once.
    pub(crate) fn created(&self, browser: ::cef::Browser) {
        if self.closing.load(Ordering::Acquire) {
            close(&browser);
            return;
        }
        *lock(&self.browser) = Some(SendBrowser(browser));
        // `closing` may have been set between the check and the store; `close` then
        // found no browser, so close it here.
        if self.closing.load(Ordering::Acquire)
            && let Some(browser) = lock(&self.browser).take()
        {
            close(&browser.0);
        }
    }

    /// The browser is about to be destroyed (`OnBeforeClose`, UI thread).
    pub(crate) fn closed(&self) {
        lock(&self.browser).take();
        self.host.send(&ToEngine::Closed { page: self.id });
        self.host.browser_closed();
    }

    /// The main frame finished loading: applies the custom CSS to the new document.
    pub(crate) fn loaded(&self, frame: &::cef::Frame) {
        let css = lock(&self.css).clone();
        run_script(frame, &css_script(&css));
    }

    /// Whether the engine closed the page and its browser is closing.
    pub(crate) fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }

    /// One accelerated paint of CEF's texture `handle` (UI thread): copied into a shared
    /// texture and announced to the engine. Skipped while the engine holds every texture
    /// or the helper has no device; a failure is reported once until a paint succeeds.
    pub(crate) fn paint(&self, handle: *mut c_void) {
        let painted = {
            let gpu = lock(&self.host.gpu);
            let Some(gpu) = gpu.as_ref() else {
                return;
            };
            self.textures.paint(gpu, handle)
        };
        match painted {
            Ok(Some(painted)) => {
                self.paint_warned.store(false, Ordering::Relaxed);
                self.host.send(&ToEngine::Frame {
                    page: self.id,
                    slot: painted.slot,
                    handle: painted.handle,
                    width: painted.width,
                    height: painted.height,
                });
            }
            Ok(None) => {}
            Err(reason) => {
                if !self.paint_warned.swap(true, Ordering::Relaxed) {
                    self.host.warn(&format!(
                        "a browser page's paint could not be copied: {reason}"
                    ));
                }
            }
        }
    }

    /// One packet of the page's audio, as planes of `frames` frames (CEF's audio
    /// thread): sent to the engine as interleaved stereo, mono doubled.
    pub(crate) fn audio(&self, planes: &[&[f32]], frames: usize) {
        let (left, right) = match planes {
            [mono] => (*mono, *mono),
            [left, right, ..] => (*left, *right),
            [] => return,
        };
        let frames = frames.min(left.len()).min(right.len());
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(AUDIO_FRAMES);
            let mut samples = Vec::with_capacity(n * 2);
            for i in done..done + n {
                samples.push(left[i]);
                samples.push(right[i]);
            }
            self.host.send(&ToEngine::Audio {
                page: self.id,
                samples,
            });
            done += n;
        }
    }

    /// The page's audio stream failed (not while it closes, when that is the stream's
    /// end).
    pub(crate) fn audio_failed(&self, reason: String) {
        self.host.send(&ToEngine::AudioFailed {
            page: self.id,
            reason,
        });
    }

    /// A request was refused: the engine gets the new count, never the URL.
    pub(crate) fn denied(&self) {
        self.host.send(&ToEngine::Denied {
            page: self.id,
            count: self.policy.denied(),
        });
    }

    fn set_css(&self, css: String) {
        let script = css_script(&css);
        *lock(&self.css) = css;
        let frame = lock(&self.browser).as_ref().and_then(|b| b.0.main_frame());
        if let Some(frame) = frame {
            run_script(&frame, &script);
        }
    }

    fn close(&self) {
        self.closing.store(true, Ordering::Release);
        if let Some(browser) = lock(&self.browser).take() {
            close(&browser.0);
        }
    }
}

/// Closes a browser without asking the page (no `beforeunload` prompt).
fn close(browser: &::cef::Browser) {
    if let Some(host) = browser.host() {
        host.close_browser(1);
    }
}

fn run_script(frame: &::cef::Frame, script: &str) {
    frame.execute_java_script(
        Some(&script.into()),
        Some(&"jstream://custom-css".into()),
        0,
    );
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
