//! The browser helper's CEF code (Windows and Linux, feature `helper`; ADR-0048,
//! ADR-0095, ARCH › Browser source): what `jstream-browser-helper.dll` runs when CEF's
//! bootstrap exe calls it on Windows, and what the `jstream-browser-helper` executable
//! runs on Linux, in CEF's browser process and in every one of its sandboxed
//! subprocesses.
//!
//! **In a subprocess** (renderer, GPU, utility) it hands the process to CEF.
//!
//! **In the browser process** it connects to the engine: on Windows to its two pipes
//! (named by `--jstream-pipe`), on Linux through the socket that is its standard input,
//! which it then swaps for `/dev/null` so CEF's subprocesses never hold it. It starts CEF with the sandbox on, off-screen rendering, profiles in
//! memory and the engine's cache and log settings, and runs CEF's message loop on the
//! process's main thread, which is CEF's UI thread. A reader thread takes the engine's
//! messages: pages are opened, restyled and closed from it, which CEF allows on any of
//! the browser process's threads, while a new adapter and shutting down are posted to the
//! UI thread. Each page is a windowless browser with shared textures, painting at its
//! frame rate with a transparent background; its paints are copied into textures it
//! shares with the engine on Windows (`d3d11`), and on Linux painted in software and
//! written into slot files in the folder the engine named (`--jstream-frames`,
//! `shm`), its audio is sent as interleaved stereo, and its
//! requests go through its [`RequestPolicy`].
//!
//! **Shutting down.** The engine's `Shutdown`, its pipe closing, or a write to it failing
//! closes every page; the loop quits once they have closed, or after [`CLOSE_GRACE`],
//! and CEF shuts down on the UI thread before the process exits.

#[cfg(windows)]
mod d3d11;
mod handlers;
mod page_api;
#[cfg(target_os = "linux")]
mod shm;

use std::collections::HashMap;
use std::ffi::c_void;
#[cfg(windows)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use ::cef::{BrowserSettings, CefString, ImplBrowser, ImplBrowserHost, ImplFrame, LogSeverity};

use crate::page_api::{ApiHome, PageCache};
use crate::reload_hold::{self, ReloadHold};
use crate::wire::{self, ToEngine, ToHelper};
use crate::{PageData, PageSpec, RequestPolicy, css_script};
#[cfg(windows)]
use d3d11::{Gpu, PageTextures};
#[cfg(target_os = "linux")]
use shm::PageTextures;

/// The helper's end of its connection to the engine, each way.
#[cfg(windows)]
type Stream = std::fs::File;
#[cfg(target_os = "linux")]
type Stream = std::os::unix::net::UnixStream;

/// How long shutting down waits for open pages to close before CEF's loop is quit
/// anyway.
const CLOSE_GRACE: Duration = Duration::from_secs(10);

/// The switch that names the engine's pipes (Windows).
#[cfg(windows)]
const PIPE_SWITCH: &str = "--jstream-pipe=";

/// The switch that names the folder paints go into (Linux).
#[cfg(target_os = "linux")]
const FRAMES_SWITCH: &str = "--jstream-frames=";

/// The most stereo frames one audio message carries.
const AUDIO_FRAMES: usize = wire::MAX_AUDIO_SAMPLES / 2;

/// Runs the helper's process for CEF's bootstrap exe, which calls it from the DLL's
/// `RunWinMain` export, and returns the process's exit code.
///
/// # Safety
/// `sandbox_info` must be the sandbox information CEF's bootstrap exe passed to
/// `RunWinMain`, unchanged; CEF reads it.
#[cfg(windows)]
pub unsafe fn run_helper(sandbox_info: *mut c_void) -> i32 {
    run(sandbox_info)
}

/// Runs the helper's process, from the `jstream-browser-helper` executable's `main`,
/// and returns its exit code. CEF's subprocesses are started as the same executable, so
/// they come here too. Chromium's sandbox needs no information from the caller on
/// Linux: it sets itself up in each subprocess.
#[cfg(target_os = "linux")]
#[must_use]
pub fn run_helper() -> i32 {
    run(std::ptr::null_mut())
}

fn run(sandbox_info: *mut c_void) -> i32 {
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
    let (mut from_engine, to_engine, frames) = connect().ok_or(())?;
    let host = Arc::new(Host {
        to_engine: Mutex::new(to_engine),
        pages: Mutex::new(HashMap::new()),
        #[cfg(windows)]
        gpu: Mutex::new(None),
        #[cfg(windows)]
        generation: AtomicU64::new(0),
        frames,
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
        // The sandbox is on (ADR-0021, ADR-0048, ADR-0095): on Windows CEF's bootstrap exe
        // set it up, on Linux Chromium's own (user namespaces, or the setuid
        // `chrome-sandbox` beside the helper) starts in each subprocess, and every
        // subprocess starts as the same executable.
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
            reason: INITIALIZE_FAILED.into(),
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
    #[cfg(windows)]
    {
        *lock(&host.gpu) = None;
    }
    ::cef::shutdown();
    Ok(())
}

/// What the engine is told when CEF won't start.
#[cfg(windows)]
const INITIALIZE_FAILED: &str = "cef::initialize failed: libcef.dll and its resources must be \
     next to the helper, and no other process may be using the same browser cache folder";
#[cfg(target_os = "linux")]
const INITIALIZE_FAILED: &str = "cef::initialize failed: libcef.so and its resources must be \
     next to the helper, and the sandbox must be able to start: run as a user other than \
     root, with unprivileged user namespaces allowed or chrome-sandbox set up \
     (docs/adr/0095-browser-helper-on-linux.md)";

/// Connects to the engine's two pipes, named by `--jstream-pipe` (Windows). Returns
/// (from the engine, to the engine, no folder).
#[cfg(windows)]
fn connect() -> Option<(Stream, Stream, ())> {
    use std::fs::{File, OpenOptions};
    let name = std::env::args().find_map(|arg| arg.strip_prefix(PIPE_SWITCH).map(str::to_owned))?;
    let from_engine = File::open(format!(r"\\.\pipe\{name}-in")).ok()?;
    let to_engine = OpenOptions::new()
        .write(true)
        .open(format!(r"\\.\pipe\{name}-out"))
        .ok()?;
    Some((from_engine, to_engine, ()))
}

/// Takes the engine's socket, the process's standard input, and puts `/dev/null` in its
/// place, so the subprocesses CEF starts never inherit it and the engine sees the
/// helper's end close when the helper exits (Linux). Returns (from the engine, to the
/// engine, the folder named by `--jstream-frames`).
#[cfg(target_os = "linux")]
fn connect() -> Option<(Stream, Stream, std::path::PathBuf)> {
    use std::os::fd::{AsFd, AsRawFd};
    let frames = std::env::args().find_map(|arg| {
        arg.strip_prefix(FRAMES_SWITCH)
            .map(std::path::PathBuf::from)
    })?;
    // `try_clone_to_owned` duplicates it close-on-exec.
    let socket = std::io::stdin().as_fd().try_clone_to_owned().ok()?;
    let null = std::fs::File::open("/dev/null").ok()?;
    // SAFETY: both are open descriptors this process owns; `dup2` replaces descriptor 0
    // with `/dev/null` atomically, and the socket lives on in its close-on-exec copy.
    if unsafe { libc::dup2(null.as_raw_fd(), 0) } < 0 {
        return None;
    }
    let from_engine = Stream::from(socket);
    let to_engine = from_engine.try_clone().ok()?;
    Some((from_engine, to_engine, frames))
}

/// What the helper's threads and CEF's callbacks share.
pub(crate) struct Host {
    to_engine: Mutex<Stream>,
    pages: Mutex<HashMap<u32, Arc<PageShared>>>,
    /// The D3D11 device on the engine's adapter; `None` delivers no frames (Windows).
    #[cfg(windows)]
    gpu: Mutex<Option<Gpu>>,
    /// Bumped with each device, so pages remake their textures (Windows).
    #[cfg(windows)]
    generation: AtomicU64,
    /// The folder paints are written into (Linux); nothing on Windows.
    #[cfg_attr(windows, expect(dead_code, reason = "Windows needs no folder"))]
    frames: Frames,
    /// Browsers asked for and not yet closed.
    open: AtomicUsize,
    /// Set once CEF has started, so its tasks may be posted.
    started: AtomicBool,
    /// Set when shutting down: the UI thread quits its loop at the last close.
    stopping: AtomicBool,
}

impl Host {
    /// Sends one message to the engine. A failed write means the engine is gone: the
    /// helper shuts down, from a task on the UI thread, so no lock the caller holds is
    /// taken again.
    fn send(self: &Arc<Self>, message: &ToEngine) {
        let sent = wire::send(&mut *lock(&self.to_engine), message);
        // Before CEF has started there is nothing to stop, and no UI thread to post to.
        if sent.is_err() && self.started.load(Ordering::Acquire) {
            // Later, on the UI thread: the sender may hold a page's locks, which
            // stopping takes to close the page.
            let host = Arc::clone(self);
            post(move || host.stop());
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
    fn read(self: &Arc<Self>, mut from_engine: Stream) {
        while let Ok(Some(message)) = wire::receive::<ToHelper>(&mut from_engine) {
            self.handle(message);
        }
        self.stop();
    }

    fn handle(self: &Arc<Self>, message: ToHelper) {
        match message {
            ToHelper::Open { page, spec } => self.open(page, &spec),
            ToHelper::SetCss { page, css } => {
                let page = self.pages().get(&page).cloned();
                if let Some(page) = page {
                    page.set_css(css);
                }
            }
            ToHelper::Reload { page } => {
                let page = self.pages().get(&page).cloned();
                if let Some(page) = page {
                    page.reload();
                }
            }
            ToHelper::PageData { page, data } => {
                let page = self.pages().get(&page).cloned();
                if let Some(page) = page {
                    page.page_data(&data);
                }
            }
            ToHelper::SetHidden { page, hidden } => {
                if let Some(page) = self.pages().get(&page).cloned() {
                    page.set_hidden(hidden);
                }
            }
            ToHelper::Close { page } => {
                let page = self.pages().remove(&page);
                if let Some(page) = page {
                    page.close();
                }
            }
            ToHelper::Released { page, slot } => {
                let page = self.pages().get(&page).cloned();
                if let Some(page) = page {
                    page.textures.release(slot);
                }
            }
            #[cfg(windows)]
            ToHelper::Adapter { luid } => {
                let host = Arc::clone(self);
                post(move || host.use_adapter(luid));
            }
            // Pages paint in software on Linux, on no adapter.
            #[cfg(target_os = "linux")]
            ToHelper::Adapter { .. } => {}
            ToHelper::Shutdown => self.stop(),
            ToHelper::Start { .. } => self.warn("the engine sent a second start"),
        }
    }

    /// Makes the device pages copy onto (UI thread), and asks every page to paint
    /// again. The engine drops a page's frames from its last device (#1183), so a static
    /// page, which paints nothing more on its own, would otherwise show nothing there.
    #[cfg(windows)]
    fn use_adapter(self: &Arc<Self>, luid: Option<u64>) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let gpu = luid.and_then(|luid| match Gpu::on_adapter(luid, generation) {
            Ok(gpu) => Some(gpu),
            Err(reason) => {
                self.warn(&format!("pages can't deliver frames: {reason}"));
                None
            }
        });
        let painting = gpu.is_some();
        *lock(&self.gpu) = gpu;
        if painting {
            let pages: Vec<_> = self.pages().values().cloned().collect();
            for page in pages {
                if let Some(host) = lock(&page.browser).as_ref().and_then(|b| b.0.host()) {
                    host.invalidate(::cef::PaintElementType::VIEW);
                }
            }
        }
    }

    fn open(self: &Arc<Self>, id: u32, spec: &PageSpec) {
        // A grant on a page with no document of its own (a blank page) gets nothing.
        let page_api = spec
            .page_api
            .and_then(|api| Some((api, ApiHome::of(&spec.location)?)));
        let page = Arc::new(PageShared {
            id,
            size: (spec.size.width(), spec.size.height()),
            css: Mutex::new(spec.css.clone()),
            browser: Mutex::new(None),
            closing: AtomicBool::new(false),
            hidden: AtomicBool::new(false),
            textures: PageTextures::default(),
            paint_warned: AtomicBool::new(false),
            // Held from the start, so the first load's white paint is skipped (#1155).
            reloading: Mutex::new(ReloadHold::opening()),
            channels: AtomicUsize::new(0),
            host: Arc::clone(self),
            policy: RequestPolicy::for_page(spec),
            page_api: page_api.as_ref().map(|_| Mutex::new(PageCache::default())),
        });
        let window_info = ::cef::WindowInfo {
            windowless_rendering_enabled: 1,
            // Shared textures on Windows; software paints on Linux (ADR-0095).
            shared_texture_enabled: i32::from(cfg!(windows)),
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
        // A granted page's grant and rule cross to its renderer here, and it gets a
        // request context of its own, so its renderer process is never shared with
        // another source's page (ADR-0102 decision 3). In memory, as every page's is.
        let (mut extra_info, mut context) = match &page_api {
            Some((api, home)) => (
                page_api::extra_info(*api, home),
                ::cef::request_context_create_context(
                    Some(&::cef::RequestContextSettings::default()),
                    None,
                ),
            ),
            None => (None, None),
        };
        self.pages().insert(id, Arc::clone(&page));
        self.open.fetch_add(1, Ordering::AcqRel);
        // Asynchronous: the browser arrives in `OnAfterCreated` on the UI thread.
        let asked = ::cef::browser_host_create_browser(
            Some(&window_info),
            Some(&mut client),
            Some(&url),
            Some(&settings),
            extra_info.as_mut(),
            context.as_mut(),
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
    /// Whether the engine said nothing draws the page (#1071). A browser that arrives
    /// later starts hidden.
    hidden: AtomicBool,
    textures: PageTextures,
    /// Whether a failed paint was reported since the last good one.
    paint_warned: AtomicBool,
    /// Held from the page's opening (#1155) or a reload until the document has loaded and
    /// its first paint was skipped (`reload_hold.rs`): the old document's paints and the
    /// new one's first, which can be opaque white (#1151), are skipped, so the engine
    /// shows neither. A paint holds the lock from its check until its frame is
    /// sent, and a reload holds it until [`ToEngine::Reloading`] is sent, so no old frame
    /// follows that message.
    reloading: Mutex<ReloadHold>,
    /// Channels in the page's current audio stream (`OnAudioStreamStarted`).
    pub(crate) channels: AtomicUsize,
    pub(crate) host: Arc<Host>,
    /// A granted page's merged updates (ADR-0102), sent whole once its browser exists and
    /// then update by update; `None` for a page without a page API.
    page_api: Option<Mutex<PageCache>>,
    /// What the page may request; the request handler cancels the rest.
    pub(crate) policy: RequestPolicy,
}

/// A browser, held across threads.
struct SendBrowser(::cef::Browser);

// SAFETY: CEF's browser object is reference counted with atomic counts, and the only
// methods called through it (`host`, `main_frame`, then `close_browser`,
// `execute_java_script`, `send_process_message` and `reload`) are documented as callable
// on any thread of the browser process. `was_hidden` is called only on CEF's UI thread.
unsafe impl Send for SendBrowser {}

impl PageShared {
    /// The browser arrived (`OnAfterCreated`, UI thread). A page already closed closes it
    /// at once.
    pub(crate) fn created(&self, browser: ::cef::Browser) {
        if self.closing.load(Ordering::Acquire) {
            close(&browser);
            return;
        }
        if self.hidden.load(Ordering::Acquire)
            && let Some(host) = browser.host()
        {
            host.was_hidden(1);
        }
        // Taken before the browser is stored, as `page_data` takes it, so no update is
        // sent twice or missed between the two.
        let cache = self.page_api.as_ref().map(lock);
        if let Some(cache) = &cache {
            send_page_data(&browser, &cache.full());
        }
        *lock(&self.browser) = Some(SendBrowser(browser));
        drop(cache);
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
        #[cfg(target_os = "linux")]
        self.textures.remove(&self.host.frames, self.id);
        self.host.send(&ToEngine::Closed { page: self.id });
        self.host.browser_closed();
    }

    /// The main frame started loading a document (`OnLoadStart`, UI thread): while a
    /// reload holds the page's paints, [`reload_hold::DEADLINE`] from now counts as the
    /// load's end, so a load that never ends still shows (#1151).
    pub(crate) fn load_started(self: &Arc<Self>) {
        let Some(token) = lock(&self.reloading).load_started() else {
            return;
        };
        let page = Arc::clone(self);
        ::cef::post_delayed_task(
            ::cef::ThreadId::UI,
            Some(&mut handlers::task(move || {
                let released = lock(&page.reloading).deadline(token);
                if let Some(released) = released {
                    page.repaint_later(released);
                }
            })),
            i64::try_from(reload_hold::DEADLINE.as_millis()).unwrap_or(i64::MAX),
        );
    }

    /// The main frame ended or failed loading a document: a reload's hold ends if the new
    /// document's first paint was already skipped, or else with that paint (#1151).
    pub(crate) fn load_finished(self: &Arc<Self>) {
        let released = lock(&self.reloading).load_finished();
        if let Some(released) = released {
            self.repaint_later(released);
        }
    }

    /// A hold just ended, and the paints it skipped may have been the page's last, a
    /// static page painting nothing more on its own: asks CEF to paint the whole view
    /// again [`reload_hold::REPAINT_AFTER`] from now, unless a paint went through by then.
    /// Asked for at once, Chromium paints the new document's blank, opaque white.
    fn repaint_later(self: &Arc<Self>, released: reload_hold::Released) {
        let page = Arc::clone(self);
        ::cef::post_delayed_task(
            ::cef::ThreadId::UI,
            Some(&mut handlers::task(move || {
                if !lock(&page.reloading).needs_repaint(released) {
                    return;
                }
                if let Some(host) = lock(&page.browser).as_ref().and_then(|b| b.0.host()) {
                    host.invalidate(::cef::PaintElementType::VIEW);
                }
            })),
            i64::try_from(reload_hold::REPAINT_AFTER.as_millis()).unwrap_or(i64::MAX),
        );
    }

    /// The main frame started loading a document (`OnLoadStart`): sends it everything the
    /// page API cache holds. Chromium may put the new document in a renderer process
    /// that has none of the page's updates (a reload of a `data:` page does), so each
    /// document is sent the whole state again; the renderer merges it, and the shim
    /// reports only what changed. Nothing for a page without a page API.
    pub(crate) fn document_started(&self, frame: &::cef::Frame) {
        let Some(cache) = &self.page_api else {
            return;
        };
        // Held while sending, as `page_data` holds it, so no update overtakes it.
        let cache = lock(cache);
        send_to_frame(frame, &cache.full());
    }

    /// The main frame finished loading: applies the custom CSS to the new document.
    pub(crate) fn loaded(self: &Arc<Self>, frame: &::cef::Frame) {
        self.load_finished();
        let css = lock(&self.css).clone();
        run_script(frame, &css_script(&css));
    }

    /// Whether the engine closed the page and its browser is closing.
    pub(crate) fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }

    /// One accelerated paint of CEF's texture `handle` (UI thread, Windows): copied into
    /// a shared texture and announced to the engine. Skipped while the engine holds every
    /// texture or the helper has no device; a failure is reported once until a paint
    /// succeeds.
    #[cfg(windows)]
    pub(crate) fn paint(self: &Arc<Self>, handle: *mut c_void) {
        let reloading = lock(&self.reloading);
        let Some(reloading) = self.paint_through(reloading) else {
            return;
        };
        let painted = {
            let gpu = lock(&self.host.gpu);
            let Some(gpu) = gpu.as_ref() else {
                return;
            };
            self.textures.paint(gpu, handle)
        };
        self.painted(painted);
        drop(reloading);
    }

    /// One software paint, `bgra` of `width` × `height` premultiplied BGRA8 (UI thread,
    /// Linux): written into a slot file as straight RGBA8 and announced to the engine.
    /// Skipped while the engine holds every slot; a failure is reported once until a
    /// paint succeeds.
    #[cfg(target_os = "linux")]
    pub(crate) fn paint(self: &Arc<Self>, bgra: &[u8], width: u32, height: u32) {
        let reloading = lock(&self.reloading);
        let Some(reloading) = self.paint_through(reloading) else {
            return;
        };
        let painted = self
            .textures
            .paint(&self.host.frames, self.id, bgra, (width, height));
        self.painted(painted);
        drop(reloading);
    }

    /// Whether a paint goes to the engine, the reload hold deciding: `Some` with the lock
    /// still held, so a reload waits for the paint's frame to be sent. A paint that ends
    /// the hold is skipped, and a repaint asked for later.
    fn paint_through<'a>(
        self: &Arc<Self>,
        mut reloading: MutexGuard<'a, ReloadHold>,
    ) -> Option<MutexGuard<'a, ReloadHold>> {
        match reloading.paint() {
            reload_hold::Paint::Send => Some(reloading),
            reload_hold::Paint::Skip => None,
            reload_hold::Paint::SkipLast(released) => {
                drop(reloading);
                self.repaint_later(released);
                None
            }
        }
    }

    fn painted(&self, painted: Result<Option<Painted>, String>) {
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

    /// Takes an update to the page's `window.jstream`: kept, and sent to its renderer
    /// if the browser exists. Nothing for a page without a page API.
    fn page_data(&self, data: &PageData) {
        let Some(cache) = &self.page_api else {
            return;
        };
        let mut cache = lock(cache);
        cache.merge(data);
        if let Some(browser) = lock(&self.browser).as_ref() {
            send_page_data(&browser.0, data);
        }
    }

    fn set_css(&self, css: String) {
        let script = css_script(&css);
        *lock(&self.css) = css;
        let frame = lock(&self.browser).as_ref().and_then(|b| b.0.main_frame());
        if let Some(frame) = frame {
            run_script(&frame, &script);
        }
    }

    /// Loads the page again; its custom CSS goes in again on load, as on the first.
    /// The old document's paints stop first, and the engine is told so
    /// ([`ToEngine::Reloading`]) before any paint of the new one.
    fn reload(&self) {
        let browser = lock(&self.browser);
        if let Some(browser) = browser.as_ref() {
            let mut reloading = lock(&self.reloading);
            reloading.reload();
            self.host.send(&ToEngine::Reloading { page: self.id });
            drop(reloading);
            // The reload's main-frame load, of the page's current URL, is the helper's
            // own (#1069).
            if let Some(frame) = browser.0.main_frame() {
                self.policy
                    .expect_own_load(&CefString::from(&frame.url()).to_string());
            }
            browser.0.reload();
        } else {
            // No browser yet: its first load is the start the engine waits for.
            self.host.send(&ToEngine::Reloading { page: self.id });
        }
    }

    /// Hides or shows the page (`WasHidden`): hidden, it stops painting. On CEF's UI
    /// thread, in order with the browser's arrival, which applies a hide sent before it.
    fn set_hidden(self: &Arc<Self>, hidden: bool) {
        let page = Arc::clone(self);
        post(move || {
            if page.hidden.swap(hidden, Ordering::AcqRel) == hidden {
                return;
            }
            if let Some(host) = lock(&page.browser).as_ref().and_then(|b| b.0.host()) {
                host.was_hidden(i32::from(hidden));
            }
        });
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

/// Sends a page API update to `browser`'s main frame in its renderer, split as
/// [`PageData::split`] does.
fn send_page_data(browser: &::cef::Browser, data: &PageData) {
    if let Some(frame) = browser.main_frame() {
        send_to_frame(&frame, data);
    }
}

/// Sends a page API update to `frame` in its renderer, split as [`PageData::split`]
/// does.
fn send_to_frame(frame: &::cef::Frame, data: &PageData) {
    if data.is_empty() {
        return;
    }
    for part in data.clone().split() {
        if let Some(mut message) = page_api::message(&part) {
            frame.send_process_message(::cef::ProcessId::RENDERER, Some(&mut message));
        }
    }
}

fn run_script(frame: &::cef::Frame, script: &str) {
    frame.execute_java_script(
        Some(&script.into()),
        Some(&"jstream://custom-css".into()),
        0,
    );
}

/// Where a page's paints go: the slot files' folder on Linux, nothing on Windows.
#[cfg(windows)]
type Frames = ();
#[cfg(target_os = "linux")]
type Frames = std::path::PathBuf;

#[cfg(windows)]
use d3d11::Painted;
#[cfg(target_os = "linux")]
use shm::Painted;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
