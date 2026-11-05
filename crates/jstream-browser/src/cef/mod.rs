//! The CEF backend (Windows and Linux, feature `cef`): pages run in the sandboxed browser
//! helper process, which this backend starts and drives over a private byte stream each
//! way (ADR-0048, ADR-0095, ARCH › Browser source). No CEF code runs in the engine's
//! process.
//!
//! **The helper on Windows.** It starts on the first [`BrowserBackend::open`]:
//! `jstream-browser-helper.exe --jstream-pipe=<name>`, CEF's bootstrap exe, which loads
//! the helper's DLL, runs CEF's browser process with the sandbox on, and connects to the
//! two pipes this backend created. The backend checks that the process on the other end
//! is the one it started. Pages' frames cross as shared textures ([`gpu`]).
//!
//! **The helper on Linux.** `jstream-browser-helper --jstream-frames=<folder>`, a plain
//! executable that runs CEF's browser process with the sandbox on, its standard input one
//! end of a socket pair whose other end this backend holds, so nothing else can be on the
//! line. It paints in software and writes each paint into a slot file in `<folder>`, a
//! private folder this backend makes for it and removes when it stops ([`cpu`]).
//!
//! Either way the backend then sends the cache and log settings, and waits for CEF to
//! start. Pages are then opened, restyled and closed by message ([`Link`]).
//!
//! **A helper that stops** fails its pages (their audio ends; their frames stop), and
//! the next `open` starts a new one. A helper that can't start fails that `open` and
//! every later one with the same error, as CEF failing to start would.
//!
//! **Shutting down.** Dropping the backend asks the helper to close its pages and exit,
//! waits for it (the helper allows its pages [`EXIT_GRACE`] less a margin to close), and
//! kills it if it hasn't exited by then.

#[cfg(target_os = "linux")]
mod cpu;
#[cfg(windows)]
mod gpu;
#[cfg(windows)]
mod job;
#[cfg(windows)]
mod pipe;

use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use jstream_platform::{GpuContext, MediaClock};

use crate::link::Link;
use crate::{BrowserBackend, BrowserError, BrowserPage, PageSpec};
#[cfg(windows)]
use gpu::DeviceSlot;

/// The helper exe, in [`HELPER_DIR`] beside the host's exe, or beside the exe itself.
#[cfg(windows)]
const HELPER_EXE: &str = "jstream-browser-helper.exe";
/// The helper executable, in [`HELPER_DIR`] beside the host's, or beside it.
#[cfg(target_os = "linux")]
const HELPER_EXE: &str = "jstream-browser-helper";

/// The folder beside the host's exe that holds the helper and CEF's runtime in the
/// portable zip (ARCH › Packaging).
const HELPER_DIR: &str = "browser";

/// How long the helper may take to connect to its pipes.
#[cfg(windows)]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long CEF may take to start in the helper; a first start creates its cache.
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the helper may take to exit once asked: its pages get ten seconds to close.
const EXIT_GRACE: Duration = Duration::from_secs(15);

/// Where the helper is and where CEF writes.
#[derive(Clone, Debug)]
pub struct CefConfig {
    /// The helper exe: on Windows `jstream-browser-helper.exe`, CEF's bootstrap exe, with
    /// `jstream-browser-helper.dll` and CEF's runtime files beside it; on Linux
    /// `jstream-browser-helper`, with `libcef.so` and CEF's runtime files beside it.
    pub helper: PathBuf,
    /// CEF's root cache folder. Pages keep no cookies or storage on disk: each runs in
    /// CEF's in-memory profile, and this folder holds only CEF's own state.
    pub cache_root: PathBuf,
    /// CEF's log file, warnings and errors only; `None` leaves CEF's default
    /// (`debug.log` next to the helper exe on Windows, the working folder on Linux).
    pub log_file: Option<PathBuf>,
}

impl CefConfig {
    /// The helper beside the running exe (see [`helper_in`]), and a cache under
    /// `state_dir` (the app's state folder): `state_dir/cef`.
    ///
    /// # Errors
    /// [`BrowserError::Start`] when the running exe's path is unknown.
    pub fn beside_exe(state_dir: &std::path::Path) -> Result<Self, BrowserError> {
        let exe = std::env::current_exe().map_err(|e| start(format!("current_exe: {e}")))?;
        let dir = exe
            .parent()
            .ok_or_else(|| start("the running exe has no folder".into()))?;
        Ok(Self {
            helper: helper_in(dir),
            cache_root: state_dir.join("cef"),
            log_file: None,
        })
    }
}

/// The helper exe for a host in `dir`: in its `browser` folder, as the portable zip lays
/// it out, else beside the host (a build folder the helper was laid out in, as
/// `page_check` does).
fn helper_in(dir: &std::path::Path) -> PathBuf {
    let shipped = dir.join(HELPER_DIR).join(HELPER_EXE);
    if shipped.is_file() {
        shipped
    } else {
        dir.join(HELPER_EXE)
    }
}

/// The CEF backend: one browser helper process at a time, started when the first page
/// opens.
pub struct CefBackend {
    config: CefConfig,
    clock: Arc<dyn MediaClock>,
    /// The device pages' shared textures are opened on (Windows). On Linux frames are in
    /// CPU memory and need none.
    #[cfg(windows)]
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
    /// The folder the helper writes its paints into (Linux), removed once it has
    /// stopped: declared after the fields `stop` uses, so it drops after.
    #[cfg(target_os = "linux")]
    _frames: cpu::FramesDir,
    /// The job the helper and every process it starts run in (Windows), which kills
    /// them all when its handle closes: when this drops after `stop`, and when the
    /// engine's process ends any other way, a crash included, so no helper outlives it
    /// (#1186).
    #[cfg(windows)]
    _job: job::KillOnClose,
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
            #[cfg(windows)]
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
        let launched = self.launch(exe)?;
        let mut child = launched.child;
        let (link, reader) = match Link::connect(
            launched.from_helper,
            launched.to_helper,
            launched.frames,
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
            #[cfg(target_os = "linux")]
            _frames: launched.dir,
            #[cfg(windows)]
            _job: launched.job,
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
        #[cfg(windows)]
        helper.link.use_adapter(self.device.luid());
        tracing::info!(pid = helper.child.id(), "the browser helper started");
        Ok(helper)
    }

    /// Starts the helper process and connects to it (Windows): its two named pipes,
    /// checked to be the helper's, and its shared textures imported onto the device.
    #[cfg(windows)]
    fn launch(&self, exe: &std::path::Path) -> Result<Launched, BrowserError> {
        use std::os::windows::io::AsHandle;
        use std::process::{Command, Stdio};
        let pipes = pipe::Pending::create()?;
        let job = job::KillOnClose::create()?;
        let mut child = Command::new(exe)
            .arg(format!("--jstream-pipe={}", pipes.name()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| start(format!("starting {}: {e}", exe.display())))?;
        // Before CEF starts any subprocess, which only happens once the engine sends
        // the start over the pipe, so every one of them is in the job too.
        if let Err(e) = job.assign(&child) {
            kill(&mut child);
            return Err(e);
        }
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
        Ok(Launched {
            child,
            from_helper: Box::new(from_helper),
            to_helper: Box::new(to_helper),
            frames: Arc::new(gpu::HelperFrames::new(Arc::clone(&self.device), process)),
            job,
        })
    }

    /// Starts the helper process (Linux) on one end of a socket pair, with a fresh
    /// private folder for its paints.
    #[cfg(target_os = "linux")]
    #[expect(clippy::unused_self, reason = "the same shape as the Windows launch")]
    fn launch(&self, exe: &std::path::Path) -> Result<Launched, BrowserError> {
        let dir = cpu::FramesDir::create()?;
        let (child, from_helper, to_helper) = cpu::spawn(exe, &dir)?;
        Ok(Launched {
            child,
            from_helper: Box::new(from_helper),
            to_helper: Box::new(to_helper),
            frames: Arc::new(cpu::CpuFrames::new(dir.path())),
            dir,
        })
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

    #[cfg(windows)]
    fn use_device(&self, gpu: Option<&GpuContext>) {
        let luid = self.device.set(gpu);
        if let State::Running(helper) = &*self.state() {
            helper.link.use_adapter(luid);
        }
    }

    /// Pages' frames on Linux are CPU frames, which need no device.
    #[cfg(target_os = "linux")]
    fn use_device(&self, _gpu: Option<&GpuContext>) {}
}

impl Drop for CefBackend {
    fn drop(&mut self) {
        // Dropping the helper asks it to exit and waits.
        *self.state() = State::Idle;
    }
}

/// A started helper, and the engine's ends of its streams.
struct Launched {
    child: Child,
    from_helper: Box<dyn std::io::Read + Send>,
    to_helper: Box<dyn std::io::Write + Send>,
    frames: Arc<dyn crate::link::Frames>,
    #[cfg(target_os = "linux")]
    dir: cpu::FramesDir,
    #[cfg(windows)]
    job: job::KillOnClose,
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn start(reason: String) -> BrowserError {
    BrowserError::Start { reason }
}

#[cfg(test)]
mod tests {
    use super::{HELPER_DIR, HELPER_EXE, helper_in};

    #[test]
    fn the_helper_is_found_in_the_zips_browser_folder_else_beside_the_host() {
        let dir = std::env::temp_dir().join(format!("jstream-helper-in-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(HELPER_DIR)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(helper_in(&dir), dir.join(HELPER_EXE), "a build folder");
        std::fs::write(dir.join(HELPER_DIR).join(HELPER_EXE), b"")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            helper_in(&dir),
            dir.join(HELPER_DIR).join(HELPER_EXE),
            "the portable zip's layout"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
