//! The CEF backend (Windows, feature `cef`): pages run in the sandboxed browser helper
//! process, which this backend starts and drives over a pair of named pipes (ADR-0048,
//! ARCH › Browser source). No CEF code runs in the engine's process.
//!
//! **The helper.** It starts on the first [`BrowserBackend::open`]:
//! `jstream-browser-helper.exe --jstream-pipe=<name>`, CEF's bootstrap exe, which loads
//! the helper's DLL, runs CEF's browser process with the sandbox on, and connects to the
//! two pipes this backend created. The backend checks that the process on the other end
//! is the one it started, sends the cache and log settings, and waits for CEF to start.
//! Pages are then opened, restyled and closed by message ([`Link`]).
//!
//! **A helper that stops** fails its pages (their audio ends; their frames stop), and
//! the next `open` starts a new one. A helper that can't start fails that `open` and
//! every later one with the same error, as CEF failing to start would.
//!
//! **Shutting down.** Dropping the backend asks the helper to close its pages and exit,
//! waits for it (the helper allows its pages [`EXIT_GRACE`] less a margin to close), and
//! kills it if it hasn't exited by then.

mod gpu;
mod pipe;

use std::os::windows::io::AsHandle;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use jstream_platform::{GpuContext, MediaClock};

use crate::link::Link;
use crate::{BrowserBackend, BrowserError, BrowserPage, PageSpec};
use gpu::{DeviceSlot, HelperFrames};

/// The helper exe, next to the host's exe by default.
const HELPER_EXE: &str = "jstream-browser-helper.exe";

/// How long the helper may take to connect to its pipes.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long CEF may take to start in the helper; a first start creates its cache.
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the helper may take to exit once asked: its pages get ten seconds to close.
const EXIT_GRACE: Duration = Duration::from_secs(15);

/// Where the helper is and where CEF writes.
#[derive(Clone, Debug)]
pub struct CefConfig {
    /// The helper exe (`jstream-browser-helper.exe`): CEF's bootstrap exe, with
    /// `jstream-browser-helper.dll` and CEF's runtime files beside it.
    pub helper: PathBuf,
    /// CEF's root cache folder. Pages keep no cookies or storage on disk: each runs in
    /// CEF's in-memory profile, and this folder holds only CEF's own state.
    pub cache_root: PathBuf,
    /// CEF's log file, warnings and errors only; `None` leaves CEF's default
    /// (`debug.log` next to the helper exe).
    pub log_file: Option<PathBuf>,
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
        })
    }
}

/// The CEF backend: one browser helper process at a time, started when the first page
/// opens.
pub struct CefBackend {
    config: CefConfig,
    clock: Arc<dyn MediaClock>,
    device: Arc<DeviceSlot>,
    helper: Mutex<State>,
}

enum State {
    /// No helper yet.
    Idle,
    Running(Helper),
    /// The helper couldn't start; every later open fails the same way.
    Failed(BrowserError),
}

/// A running helper process and the engine's end of its pipes.
struct Helper {
    link: Arc<Link>,
    child: Child,
    reader: Option<JoinHandle<()>>,
}

impl Helper {
    /// Asks the helper to exit, waits up to [`EXIT_GRACE`], then kills it if it is still
    /// there, and waits for the pipe's reader.
    fn stop(&mut self) {
        self.link.shutdown();
        let deadline = Instant::now() + EXIT_GRACE;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) | Err(_) => {
                    tracing::warn!("the browser helper didn't exit in time; killing it");
                    kill(&mut self.child);
                    break;
                }
            }
        }
        if let Some(reader) = self.reader.take()
            && reader.join().is_err()
        {
            tracing::error!("the browser helper's reader panicked");
        }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        self.stop();
    }
}

impl CefBackend {
    /// A backend that starts the helper described by `config` when the first page opens,
    /// and stamps pages' frames and audio on `clock` as they arrive.
    pub fn new(config: CefConfig, clock: Arc<dyn MediaClock>) -> Self {
        Self {
            config,
            clock,
            device: Arc::new(DeviceSlot::default()),
            helper: Mutex::new(State::Idle),
        }
    }

    /// The running helper's link, starting a helper if there is none or the last one
    /// stopped.
    fn link(&self, state: &mut State) -> Result<Arc<Link>, BrowserError> {
        match state {
            State::Running(helper) if helper.link.is_alive() => {
                return Ok(Arc::clone(&helper.link));
            }
            State::Running(_) => {
                // Dropping it reaps the process and its reader.
                *state = State::Idle;
                tracing::info!("starting a new browser helper");
            }
            State::Failed(e) => return Err(e.clone()),
            State::Idle => {}
        }
        match self.start() {
            Ok(helper) => {
                let link = Arc::clone(&helper.link);
                *state = State::Running(helper);
                Ok(link)
            }
            Err(e) => {
                *state = State::Failed(e.clone());
                Err(e)
            }
        }
    }

    fn start(&self) -> Result<Helper, BrowserError> {
        let exe = &self.config.helper;
        if !exe.is_file() {
            return Err(start(format!(
                "{} is missing; it must be next to the app",
                exe.display()
            )));
        }
        let pipes = pipe::Pending::create()?;
        let mut child = Command::new(exe)
            .arg(format!("--jstream-pipe={}", pipes.name()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| start(format!("starting {}: {e}", exe.display())))?;
        let (from_helper, to_helper) = match pipes.connect(&mut child, CONNECT_TIMEOUT) {
            Ok(pipes) => pipes,
            Err(e) => {
                kill(&mut child);
                return Err(e);
            }
        };
        let process = match child.as_handle().try_clone_to_owned() {
            Ok(process) => process,
            Err(e) => {
                kill(&mut child);
                return Err(start(format!("the browser helper's process handle: {e}")));
            }
        };
        let frames = HelperFrames::new(Arc::clone(&self.device), process);
        let (link, reader) = match Link::connect(
            from_helper,
            to_helper,
            Arc::new(frames),
            Arc::clone(&self.clock),
        ) {
            Ok(link) => link,
            Err(e) => {
                kill(&mut child);
                return Err(e);
            }
        };
        let mut helper = Helper {
            link,
            child,
            reader: Some(reader),
        };
        let path = |p: &PathBuf| p.display().to_string();
        if let Err(e) = helper.link.start(
            path(&self.config.cache_root),
            self.config.log_file.as_ref().map(path),
            START_TIMEOUT,
        ) {
            kill(&mut helper.child);
            return Err(e);
        }
        helper.link.use_adapter(self.device.luid());
        tracing::info!(pid = helper.child.id(), "the browser helper started");
        Ok(helper)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.helper.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl std::fmt::Debug for CefBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = match &*self.state() {
            State::Idle => "idle",
            State::Running(_) => "running",
            State::Failed(_) => "failed",
        };
        f.debug_struct("CefBackend")
            .field("config", &self.config)
            .field("helper", &state)
            .finish_non_exhaustive()
    }
}

impl BrowserBackend for CefBackend {
    fn open(&self, spec: &PageSpec) -> Result<Box<dyn BrowserPage>, BrowserError> {
        let link = self.link(&mut self.state())?;
        Ok(Box::new(link.open(spec)?))
    }

    fn use_device(&self, gpu: Option<&GpuContext>) {
        let luid = self.device.set(gpu);
        if let State::Running(helper) = &*self.state() {
            helper.link.use_adapter(luid);
        }
    }
}

impl Drop for CefBackend {
    fn drop(&mut self) {
        // Dropping the helper asks it to exit and waits.
        *self.state() = State::Idle;
    }
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn start(reason: String) -> BrowserError {
    BrowserError::Start { reason }
}
