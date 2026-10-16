//! The CEF backend (Windows, feature `cef`), from spike S4's result (ARCH › Browser
//! source): `cef` bindings, off-screen rendering with accelerated paint, the audio
//! handler, and CEF's subprocesses in `jstream-browser-helper.exe`.
//!
//! **Threads.** CEF starts on the first [`BrowserBackend::open`], on a `jstream-cef`
//! thread of its own: that thread calls `cef::initialize`, runs CEF's message loop (so it
//! is CEF's UI thread, which delivers paints and lifetime callbacks) and calls
//! `cef::shutdown` when the backend is dropped. Pages are opened and closed from the
//! engine's sources thread, which CEF allows on any browser-process thread. Audio
//! packets arrive on CEF's audio thread.
//!
//! **Shutting down.** Dropping the backend asks the UI thread to quit its loop once every
//! page has closed (or after [`CLOSE_GRACE`]), then waits for the thread: CEF must shut
//! down on the thread that initialized it, after its browsers are gone.
//!
//! **Ctrl-C.** `cef::initialize` installs Chromium's console control handler in the host
//! process, and Windows calls the newest handler first. Chromium's answers Ctrl-C by
//! ending the session and terminating the process with exit code 0, before the host's
//! own handler runs. So once CEF has started, [`CefConfig::on_console_stop`] (when set)
//! is put ahead of it: Ctrl-C and Ctrl-Break call it instead, and the host stops as it
//! chooses (#436).

mod gpu;
mod handlers;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use ::cef::{BrowserSettings, CefString, ImplBrowser, ImplBrowserHost, ImplFrame, LogSeverity};
use jstream_platform::{GpuContext, GpuFrame, MediaClock};

use crate::{BrowserBackend, BrowserError, BrowserPage, PageAudio, PageSpec, css_script};
use gpu::{DeviceSlot, PageFrames};

/// How long shutting down waits for open pages to close before CEF's loop is quit
/// anyway.
const CLOSE_GRACE: Duration = Duration::from_secs(10);

/// The helper exe CEF starts its subprocesses as, next to the host's exe by default.
const HELPER_EXE: &str = "jstream-browser-helper.exe";

/// Where CEF looks and writes.
#[derive(Clone, Debug)]
pub struct CefConfig {
    /// The subprocess exe (`jstream-browser-helper.exe`), which CEF starts for its
    /// renderer, GPU and utility processes.
    pub helper: PathBuf,
    /// CEF's root cache folder. Pages keep no cookies or storage on disk: each runs in
    /// CEF's in-memory profile, and this folder holds only CEF's own state.
    pub cache_root: PathBuf,
    /// CEF's log file, warnings and errors only; `None` leaves CEF's default
    /// (`debug.log` next to the exe).
    pub log_file: Option<PathBuf>,
    /// What Ctrl-C and Ctrl-Break call once CEF has started, in place of Chromium's
    /// console handler, which would end the process at once: a host with a graceful
    /// stop sets it to start that stop. `None` leaves Chromium's handler in charge. It
    /// runs on the thread Windows starts for the console event, so it must only signal.
    pub on_console_stop: Option<fn()>,
}

impl CefConfig {
    /// The helper next to the running exe, and a cache under `state_dir` (the app's
    /// state folder): `state_dir/cef`.
    ///
    /// # Errors
    /// [`BrowserError::Start`] when the running exe's path is unknown.
    pub fn beside_exe(state_dir: &std::path::Path) -> Result<Self, BrowserError> {
        let exe = std::env::current_exe().map_err(|e| start(format!("current_exe: {e}")))?;
        let dir = exe
            .parent()
            .ok_or_else(|| start("the running exe has no folder".into()))?;
        Ok(Self {
            helper: dir.join(HELPER_EXE),
            cache_root: state_dir.join("cef"),
            log_file: None,
            on_console_stop: None,
        })
    }
}

/// Hands control to CEF in one of its subprocesses (renderer, GPU, utility): what
/// `jstream-browser-helper.exe`'s `main` does, and all it does. Returns the exit code
/// CEF gives for the process.
pub fn run_subprocess() -> i32 {
    // Must come before any other CEF call: it fixes the API version the bindings use.
    let _ = ::cef::api_hash(::cef::sys::CEF_API_VERSION_LAST, 0);
    let args = ::cef::args::Args::new();
    ::cef::execute_process(Some(args.as_main_args()), None, std::ptr::null_mut())
}

/// The CEF browser backend. One per process, since CEF starts once.
pub struct CefBackend {
    config: CefConfig,
    clock: Arc<dyn MediaClock>,
    shared: Arc<Shared>,
    /// The `jstream-cef` thread, once CEF has started (or failed to).
    runtime: OnceLock<Result<Mutex<Option<JoinHandle<()>>>, BrowserError>>,
}

/// What the backend, its pages and CEF's callbacks share.
pub(crate) struct Shared {
    /// The engine's device, which pages copy their frames onto.
    pub(crate) device: DeviceSlot,
    /// Browsers asked for and not yet closed.
    open: AtomicUsize,
    /// Set when the backend is dropped: the UI thread quits its loop at the last close.
    stopping: AtomicBool,
}

impl Shared {
    /// A browser closed (`OnBeforeClose`, on the UI thread). The last one quits the loop
    /// once the backend is stopping.
    pub(crate) fn browser_closed(&self) {
        let left = self.open.fetch_sub(1, Ordering::AcqRel).saturating_sub(1);
        if left == 0 && self.stopping.load(Ordering::Acquire) {
            ::cef::quit_message_loop();
        }
    }

    /// Quits the loop now if the backend is stopping and no browser is open. On the UI
    /// thread.
    pub(crate) fn quit_if_idle(&self) {
        if self.open.load(Ordering::Acquire) == 0 {
            ::cef::quit_message_loop();
        }
    }
}

impl CefBackend {
    /// A backend that starts CEF with `config` when the first page opens, and stamps
    /// pages' audio on `clock`.
    pub fn new(config: CefConfig, clock: Arc<dyn MediaClock>) -> Self {
        Self {
            config,
            clock,
            shared: Arc::new(Shared {
                device: DeviceSlot::default(),
                open: AtomicUsize::new(0),
                stopping: AtomicBool::new(false),
            }),
            runtime: OnceLock::new(),
        }
    }

    /// Starts CEF on its thread, once; later calls return the first one's outcome.
    fn runtime(&self) -> Result<(), BrowserError> {
        match self
            .runtime
            .get_or_init(|| self.start().map(|t| Mutex::new(Some(t))))
        {
            Ok(_) => Ok(()),
            Err(e) => Err(e.clone()),
        }
    }

    fn start(&self) -> Result<JoinHandle<()>, BrowserError> {
        if !self.config.helper.is_file() {
            return Err(start(format!(
                "{} is missing; it must be next to the app",
                self.config.helper.display()
            )));
        }
        let (ready_tx, ready_rx) = mpsc::channel();
        let config = self.config.clone();
        let shared = Arc::clone(&self.shared);
        let thread = std::thread::Builder::new()
            .name("jstream-cef".into())
            .spawn(move || run(&config, &shared, &ready_tx))
            .map_err(|e| start(format!("starting the jstream-cef thread: {e}")))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(thread),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(start(
                    "the jstream-cef thread ended while CEF started".into(),
                ))
            }
        }
    }
}

impl std::fmt::Debug for CefBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CefBackend")
            .field("config", &self.config)
            .field("open", &self.shared.open.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// The `jstream-cef` thread: initializes CEF, reports how that went, runs the message
/// loop until the backend stops, and shuts CEF down.
fn run(config: &CefConfig, shared: &Arc<Shared>, ready: &mpsc::Sender<Result<(), BrowserError>>) {
    // Must come before any other CEF call: it fixes the API version the bindings use.
    let _ = ::cef::api_hash(::cef::sys::CEF_API_VERSION_LAST, 0);
    let path = |p: Option<&PathBuf>| -> CefString {
        p.map(|p| p.display().to_string())
            .unwrap_or_default()
            .as_str()
            .into()
    };
    let settings = ::cef::Settings {
        // ADR-0021 asks for the sandbox where supported. On Windows CEF sandboxes only
        // in its bootstrap layout, which this backend doesn't use yet (README › Open
        // items): pages run unsandboxed, as in spike S4.
        no_sandbox: 1,
        windowless_rendering_enabled: 1,
        browser_subprocess_path: path(Some(&config.helper)),
        root_cache_path: path(Some(&config.cache_root)),
        // No `cache_path`: pages run in CEF's in-memory profile and keep nothing on disk.
        persist_session_cookies: 0,
        log_file: path(config.log_file.as_ref()),
        log_severity: LogSeverity::WARNING,
        ..Default::default()
    };
    let args = ::cef::args::Args::new();
    let mut app = handlers::app();
    let ok = ::cef::initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    ) == 1;
    if !ok {
        let _ = ready.send(Err(start(
            "cef::initialize failed: libcef.dll and its resources must be next to the app".into(),
        )));
        return;
    }
    // After `initialize`, which installed Chromium's console handler: ours must be newer.
    if let Some(stop) = config.on_console_stop {
        take_console_stop(stop);
    }
    let _ = ready.send(Ok(()));
    tracing::info!(
        version = %String::from_utf8_lossy(::cef::sys::CEF_VERSION).trim_end_matches('\0'),
        "CEF started"
    );
    ::cef::run_message_loop();
    // The loop has quit, so no paint can run any more: let go of the device first.
    shared.device.set(None);
    ::cef::shutdown();
    tracing::info!("CEF stopped");
}

/// What [`console_handler`] calls: the first started backend's
/// [`CefConfig::on_console_stop`]. A console handler gets no context, so this is the one
/// way to reach it; it is set once, as CEF starts once per process.
static CONSOLE_STOP: OnceLock<fn()> = OnceLock::new();

/// Registers [`console_handler`] calling `stop`, after CEF's own handler so Windows asks
/// ours first. Once per process; a failure is logged and leaves Chromium's handler.
fn take_console_stop(stop: fn()) {
    use windows::Win32::System::Console::SetConsoleCtrlHandler;
    if CONSOLE_STOP.set(stop).is_err() {
        return;
    }
    // SAFETY: `console_handler` is a plain `extern "system"` function that lives for
    // the whole process and only reads `CONSOLE_STOP`, which is already set.
    if let Err(error) = unsafe { SetConsoleCtrlHandler(Some(console_handler), true) } {
        tracing::warn!(%error, "could not put the host's Ctrl-C handler ahead of CEF's");
    }
}

/// The console handler registered after CEF's: Ctrl-C and Ctrl-Break go to the host's
/// stop and stop there (`TRUE`), so Chromium's handler never sees them. Other events
/// (closing the console, log-off, shutdown) go on to the older handlers (`FALSE`).
unsafe extern "system" fn console_handler(ctrl_type: u32) -> windows::core::BOOL {
    use windows::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
    match (ctrl_type, CONSOLE_STOP.get()) {
        (CTRL_C_EVENT | CTRL_BREAK_EVENT, Some(stop)) => {
            stop();
            true.into()
        }
        _ => false.into(),
    }
}

impl BrowserBackend for CefBackend {
    fn open(&self, spec: &PageSpec) -> Result<Box<dyn BrowserPage>, BrowserError> {
        self.runtime()?;
        let page = Arc::new(PageShared {
            size: (spec.size.width(), spec.size.height()),
            css: Mutex::new(spec.css.clone()),
            browser: Mutex::new(None),
            closing: AtomicBool::new(false),
            frames: PageFrames::default(),
            audio: PageAudio::new(),
            channels: AtomicUsize::new(0),
            clock: Arc::clone(&self.clock),
            backend: Arc::clone(&self.shared),
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
        self.shared.open.fetch_add(1, Ordering::AcqRel);
        // Asynchronous: the browser arrives in `OnAfterCreated` on the UI thread.
        let asked = ::cef::browser_host_create_browser(
            Some(&window_info),
            Some(&mut client),
            Some(&url),
            Some(&settings),
            None,
            None,
        ) == 1;
        if !asked {
            self.shared.open.fetch_sub(1, Ordering::AcqRel);
            return Err(BrowserError::Open {
                reason: "CEF refused to create the browser".into(),
            });
        }
        Ok(Box::new(CefPage(page)))
    }

    fn use_device(&self, gpu: Option<&GpuContext>) {
        self.shared.device.set(gpu);
    }
}

impl Drop for CefBackend {
    fn drop(&mut self) {
        let Some(Ok(thread)) = self.runtime.get() else {
            return;
        };
        let Some(thread) = thread.lock().unwrap_or_else(PoisonError::into_inner).take() else {
            return;
        };
        self.shared.stopping.store(true, Ordering::Release);
        let shared = Arc::clone(&self.shared);
        ::cef::post_task(
            ::cef::ThreadId::UI,
            Some(&mut handlers::task(move || shared.quit_if_idle())),
        );
        ::cef::post_delayed_task(
            ::cef::ThreadId::UI,
            Some(&mut handlers::task(::cef::quit_message_loop)),
            i64::try_from(CLOSE_GRACE.as_millis()).unwrap_or(i64::MAX),
        );
        if thread.join().is_err() {
            tracing::error!("the jstream-cef thread panicked");
        }
    }
}

/// One page's state, shared by its [`CefPage`] and CEF's callbacks for its browser.
pub(crate) struct PageShared {
    /// The view's size in pixels.
    pub(crate) size: (u32, u32),
    css: Mutex<String>,
    browser: Mutex<Option<SendBrowser>>,
    /// Set when the page is dropped: a browser that arrives later is closed at once.
    closing: AtomicBool,
    pub(crate) frames: PageFrames,
    pub(crate) audio: Arc<PageAudio>,
    /// Channels in the page's current audio stream (`OnAudioStreamStarted`).
    pub(crate) channels: AtomicUsize,
    pub(crate) clock: Arc<dyn MediaClock>,
    pub(crate) backend: Arc<Shared>,
}

/// A browser, held across threads.
struct SendBrowser(::cef::Browser);

// SAFETY: CEF's browser object is reference counted with atomic counts, and the only
// methods called through it (`host`, `main_frame`, then `close_browser` and
// `execute_java_script`) are documented as callable on any thread of the browser process.
unsafe impl Send for SendBrowser {}

impl PageShared {
    /// The browser arrived (`OnAfterCreated`, UI thread). A page already dropped closes it
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
        self.backend.browser_closed();
    }

    /// The main frame finished loading: applies the custom CSS to the new document.
    pub(crate) fn loaded(&self, frame: &::cef::Frame) {
        let css = lock(&self.css).clone();
        run_script(frame, &css_script(&css));
    }

    /// Whether the page was dropped and its browser is closing.
    pub(crate) fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
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

/// A page the CEF backend opened. Dropping it closes the browser.
struct CefPage(Arc<PageShared>);

impl BrowserPage for CefPage {
    fn frame(&self) -> Option<GpuFrame> {
        self.0.frames.latest()
    }

    fn set_css(&mut self, css: &str) {
        *lock(&self.0.css) = css.to_owned();
        let frame = lock(&self.0.browser)
            .as_ref()
            .and_then(|b| b.0.main_frame());
        if let Some(frame) = frame {
            run_script(&frame, &css_script(css));
        }
    }

    fn audio(&self) -> &Arc<PageAudio> {
        &self.0.audio
    }
}

impl Drop for CefPage {
    fn drop(&mut self) {
        self.0.close();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn start(reason: String) -> BrowserError {
    BrowserError::Start { reason }
}
