//! `WindowsPluginHosts`: starts `jstream-plugin-host.exe` processes (ADR-0035). Each run
//! gets a new `PluginBlock`, which the host opens by name, and its stdin and stdout as
//! the control channel. Every host is put in one job object that kills its processes
//! when the engine's handle to it closes, so hosts never outlive the engine. A host
//! starts with a scrubbed environment and inherits only its standard handles.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use jstream_platform::{HostBlock, HostRequest, PlatformError, PluginHost, PluginHosts};
use jstream_plugin_proto::control::{FromHost, PluginDesc, ToHost, read_frame};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};

use crate::PluginBlock;
use crate::com::OsResult;
use crate::vcam::shared::Handle;

/// The host's file name, beside the engine's executable (ARCH › Packaging).
pub const PLUGIN_HOST_EXE: &str = "jstream-plugin-host.exe";

/// How long a scan may take before its host is killed.
const SCAN_TIMEOUT: Duration = Duration::from_secs(20);

/// The environment variables a host keeps; every other one is dropped, so a host never
/// sees the engine's tokens, keys or configuration. These are what Windows programs and
/// plugins (their presets and licences live under the user's folders) expect.
const KEPT_ENV: &[&str] = &[
    "SystemRoot",
    "SystemDrive",
    "windir",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "CommonProgramFiles",
    "CommonProgramFiles(x86)",
    "PATHEXT",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
];

/// Starts plugin hosts on Windows.
pub struct WindowsPluginHosts {
    exe: PathBuf,
    /// Every host's job: kill-on-close, so they end with the engine.
    job: Arc<Handle>,
    /// Numbers the blocks this engine creates.
    next_block: AtomicU64,
}

impl std::fmt::Debug for WindowsPluginHosts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowsPluginHosts")
            .field("exe", &self.exe)
            .finish_non_exhaustive()
    }
}

impl WindowsPluginHosts {
    /// Hosts started from [`PLUGIN_HOST_EXE`] beside this process's executable.
    ///
    /// # Errors
    /// The executable's folder can't be found, or the job object can't be made.
    pub fn new() -> Result<Self, PlatformError> {
        let exe = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join(PLUGIN_HOST_EXE)))
            .ok_or_else(|| PlatformError::Unsupported {
                what: "finding the plugin host beside the engine's executable".into(),
            })?;
        Self::with_exe(exe)
    }

    /// Hosts started from `exe`.
    ///
    /// # Errors
    /// The job object can't be made.
    pub fn with_exe(exe: PathBuf) -> Result<Self, PlatformError> {
        Ok(Self {
            exe,
            job: Arc::new(kill_on_close_job()?),
            next_block: AtomicU64::new(0),
        })
    }

    /// Starts the host with `args`, in the job, with a scrubbed environment.
    fn spawn(&self, args: &[&std::ffi::OsStr], stdin: Stdio) -> Result<Child, PlatformError> {
        let mut command = Command::new(&self.exe);
        command
            .args(args)
            .env_clear()
            .envs(
                KEPT_ENV
                    .iter()
                    .filter_map(|k| std::env::var_os(k).map(|v| (k, v))),
            )
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|e| PlatformError::Os {
            call: "CreateProcessW",
            code: e.raw_os_error().map(i32::cast_unsigned),
            message: format!("starting {}: {e}", self.exe.display()),
        })?;
        if let Err(e) = assign(&self.job, &child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        Ok(child)
    }
}

impl PluginHosts for WindowsPluginHosts {
    fn search_paths(&self) -> Vec<PathBuf> {
        search_paths(|k| std::env::var_os(k))
    }

    fn scan(&self, file: &Path) -> Result<Vec<PluginDesc>, PlatformError> {
        let mut child = self.spawn(&["scan".as_ref(), file.as_os_str()], Stdio::null())?;
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            return Err(host_failed("a scan without its output"));
        };
        let messages = spawn_reader(stdout);
        let deadline = Instant::now() + SCAN_TIMEOUT;
        let result = loop {
            match messages.recv_timeout(Duration::from_millis(50)) {
                Ok(FromHost::Plugins(plugins)) => break Ok(plugins),
                Ok(FromHost::Error(why)) => break Err(host_failed(&why)),
                Ok(_) => {}
                Err(_) if Instant::now() >= deadline => {
                    break Err(host_failed("the scan timed out"));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    break Err(host_failed("the host exited during the scan"));
                }
            }
        };
        let _ = child.kill();
        let _ = child.wait();
        result
    }

    fn start(&self, request: &HostRequest) -> Result<Box<dyn PluginHost>, PlatformError> {
        let name = format!(
            "Local\\JStreamPlugin-{}-{}",
            std::process::id(),
            self.next_block.fetch_add(1, Ordering::Relaxed)
        );
        let block = PluginBlock::create(&name)?;
        block
            .as_ref()
            .init(request.config)
            .map_err(|e| PlatformError::Unsupported {
                what: format!("a plugin block configuration: {e:?}"),
            })?;
        let mut child = self.spawn(
            &[
                "run".as_ref(),
                name.as_ref(),
                request.file.as_os_str(),
                request.plugin_id.as_ref(),
            ],
            Stdio::piped(),
        )?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(host_failed("a host without its control channel"));
        };
        let mut host = WindowsPluginHost {
            block: Arc::new(block),
            child,
            stdin: Some(stdin),
            messages: spawn_reader(stdout),
        };
        if !host.send(&ToHost::Start {
            state: request.state.clone(),
        }) {
            host.kill();
            return Err(host_failed("the host exited before it started"));
        }
        Ok(Box::new(host))
    }
}

/// One running `jstream-plugin-host.exe`.
struct WindowsPluginHost {
    block: Arc<PluginBlock>,
    child: Child,
    stdin: Option<ChildStdin>,
    messages: Receiver<FromHost>,
}

impl PluginHost for WindowsPluginHost {
    fn block(&self) -> HostBlock {
        Arc::clone(&self.block) as HostBlock
    }

    fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    fn try_recv(&mut self) -> Option<FromHost> {
        self.messages.try_recv().ok()
    }

    fn send(&mut self, message: &ToHost) -> bool {
        let Some(stdin) = self.stdin.as_mut() else {
            return false;
        };
        let sent = stdin
            .write_all(&message.encode())
            .and_then(|()| stdin.flush())
            .is_ok();
        if !sent {
            self.stdin = None;
        }
        sent
    }

    fn kill(&mut self) {
        // Closing stdin ends a healthy host; killing ends any.
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for WindowsPluginHost {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Decodes the host's stdout on its own thread. A frame that isn't a message ends it,
/// since the host is then broken; the engine sees the process exit.
fn spawn_reader(mut stdout: impl std::io::Read + Send + 'static) -> Receiver<FromHost> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        while let Ok(Some(payload)) = read_frame(&mut stdout) {
            let Ok(message) = FromHost::decode(&payload) else {
                return;
            };
            if tx.send(message).is_err() {
                return;
            }
        }
    });
    rx
}

fn host_failed(why: &str) -> PlatformError {
    PlatformError::Unsupported {
        what: format!("the CLAP plugin host: {why}"),
    }
}

/// A job that kills every process in it when its last handle closes, and doesn't show
/// the Windows error dialog when one crashes.
fn kill_on_close_job() -> Result<Handle, PlatformError> {
    // SAFETY: plain call; no security attributes, no name.
    let job = Handle(unsafe { CreateJobObjectW(None, None) }.os("CreateJobObjectW")?);
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    // SAFETY: `limits` is the struct this information class takes, and lives for the
    // call; the size passed is its size.
    unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    }
    .os("SetInformationJobObject")?;
    Ok(job)
}

/// Puts `child` in `job`.
fn assign(job: &Handle, child: &Child) -> Result<(), PlatformError> {
    use std::os::windows::io::AsRawHandle;
    let process = HANDLE(child.as_raw_handle());
    // SAFETY: both handles are live: the job is owned by the caller, and the process
    // handle by `child`, which outlives the call.
    unsafe { AssignProcessToJobObject(job.0, process) }.os("AssignProcessToJobObject")
}

/// CLAP's search directories on Windows, in CLAP's order: each directory in
/// `CLAP_PATH`, then `%COMMONPROGRAMFILES%\CLAP` and `%LOCALAPPDATA%\Programs\Common\CLAP`.
/// Only the ones that exist.
fn search_paths(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = var("CLAP_PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(common) = var("COMMONPROGRAMFILES") {
        paths.push(PathBuf::from(common).join("CLAP"));
    }
    if let Some(local) = var("LOCALAPPDATA") {
        paths.push(PathBuf::from(local).join("Programs\\Common\\CLAP"));
    }
    let mut seen = Vec::new();
    paths.retain(|p| {
        let new = p.is_dir() && !seen.contains(p);
        if new {
            seen.push(p.clone());
        }
        new
    });
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_paths_keep_clap_order_and_only_existing_dirs() {
        let root = std::env::temp_dir().join(format!("jstream-clap-{}", std::process::id()));
        let a = root.join("a");
        let common = root.join("common");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(common.join("CLAP")).unwrap();
        let missing = root.join("missing");
        let clap_path = std::env::join_paths([&a, &missing, &a]).unwrap();
        let paths = search_paths(|k| match k {
            "CLAP_PATH" => Some(clap_path.clone()),
            "COMMONPROGRAMFILES" => Some(common.clone().into_os_string()),
            _ => None,
        });
        assert_eq!(paths, vec![a.clone(), common.join("CLAP")]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
