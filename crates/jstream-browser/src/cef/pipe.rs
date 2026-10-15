//! The helper's two named pipes, one each way: created here before the helper starts,
//! each with a single instance (`FILE_FLAG_FIRST_PIPE_INSTANCE`, so nothing else can
//! already own the name) that refuses remote clients. Once the helper has connected to
//! both, the process on the other end of each is checked to be the helper this backend
//! started; anything else fails the start before a byte is sent.

use std::fs::{File, OpenOptions};
use std::hash::{BuildHasher, RandomState};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::Child;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use windows::Win32::Foundation::{ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAGS_AND_ATTRIBUTES, PIPE_ACCESS_INBOUND,
    PIPE_ACCESS_OUTBOUND,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::core::HSTRING;

use crate::BrowserError;

/// Each pipe's buffer, either way: a frame message is small, an audio packet 8 KiB at
/// most in practice.
const BUFFER: u32 = 64 * 1024;

/// The two pipes, created and waiting for the helper.
pub(super) struct Pending {
    name: String,
    /// The engine writes, the helper reads.
    to_helper: OwnedHandle,
    /// The helper writes, the engine reads.
    from_helper: OwnedHandle,
}

impl Pending {
    /// Creates both pipes under a fresh, unguessable name.
    pub(super) fn create() -> Result<Self, BrowserError> {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let name = format!(
            "jstream-browser-{}-{:016x}",
            std::process::id(),
            RandomState::new().hash_one(nanos)
        );
        Ok(Self {
            to_helper: create(&path(&name, "in"), PIPE_ACCESS_OUTBOUND)?,
            from_helper: create(&path(&name, "out"), PIPE_ACCESS_INBOUND)?,
            name,
        })
    }

    /// The name the helper is given (`--jstream-pipe`); its pipes are `\\.\pipe\<name>-in`
    /// (to the helper) and `-out` (from it).
    pub(super) fn name(&self) -> &str {
        &self.name
    }

    /// Waits up to `timeout` for `child` to connect to both pipes, and returns them as
    /// (from the helper, to the helper).
    ///
    /// # Errors
    /// [`BrowserError::Start`] when the helper exits or doesn't connect in time, or
    /// another process connected.
    pub(super) fn connect(
        self,
        child: &mut Child,
        timeout: Duration,
    ) -> Result<(File, File), BrowserError> {
        let Self {
            name,
            to_helper,
            from_helper,
        } = self;
        let (tx, rx) = mpsc::channel();
        let waiter = std::thread::Builder::new()
            .name("jstream-browser-connect".into())
            .spawn(move || {
                let connected = wait_for_client(&to_helper).and(wait_for_client(&from_helper));
                let _ = tx.send(connected.map(|()| (to_helper, from_helper)));
            })
            .map_err(|e| start(format!("starting the pipe's waiter: {e}")))?;
        let deadline = Instant::now() + timeout;
        let outcome = loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(result) => break result,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err("the pipe's waiter stopped".into());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if let Ok(Some(status)) = child.try_wait() {
                break Err(format!(
                    "the browser helper exited ({status}) before connecting"
                ));
            }
            if Instant::now() >= deadline {
                break Err(format!(
                    "the browser helper didn't connect within {} s",
                    timeout.as_secs()
                ));
            }
        };
        let (to_helper, from_helper) = match outcome {
            Ok(pipes) => pipes,
            Err(reason) => {
                // The waiter may still be blocked in `ConnectNamedPipe`: connecting to
                // each pipe here ends that wait.
                let _ = OpenOptions::new().write(true).open(path(&name, "in"));
                let _ = OpenOptions::new().read(true).open(path(&name, "out"));
                let _ = rx.recv_timeout(Duration::from_secs(1));
                let _ = waiter.join();
                return Err(start(reason));
            }
        };
        let _ = waiter.join();
        for pipe in [&to_helper, &from_helper] {
            let client = client_pid(pipe)?;
            if client != child.id() {
                return Err(start(format!(
                    "process {client}, not the browser helper ({}), connected to its pipe",
                    child.id()
                )));
            }
        }
        Ok((File::from(from_helper), File::from(to_helper)))
    }
}

fn path(name: &str, end: &str) -> String {
    format!(r"\\.\pipe\{name}-{end}")
}

/// One single-instance, byte-mode, blocking pipe of `direction`.
fn create(path: &str, direction: FILE_FLAGS_AND_ATTRIBUTES) -> Result<OwnedHandle, BrowserError> {
    // SAFETY: the name is a valid NUL-terminated wide string for the call; no security
    // attributes (the default DACL: the owner and administrators, plus read for
    // everyone, which the single instance and the client check above cover).
    let handle = unsafe {
        CreateNamedPipeW(
            &HSTRING::from(path),
            direction | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            BUFFER,
            BUFFER,
            0,
            None,
        )
    };
    if handle.is_invalid() {
        return Err(start(format!(
            "creating the browser helper's pipe: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: a fresh handle this process owns and nothing else holds.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
}

/// Blocks until a client connects to `pipe`; one that connected before the call counts.
fn wait_for_client(pipe: &OwnedHandle) -> Result<(), String> {
    // SAFETY: `pipe` is a live pipe server handle opened for blocking (not overlapped)
    // I/O, so no OVERLAPPED is needed.
    match unsafe { ConnectNamedPipe(HANDLE(pipe.as_raw_handle()), None) } {
        Ok(()) => Ok(()),
        Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => Ok(()),
        Err(e) => Err(format!("waiting for the browser helper's connection: {e}")),
    }
}

/// The process id of the client connected to `pipe`.
fn client_pid(pipe: &OwnedHandle) -> Result<u32, BrowserError> {
    let mut pid = 0;
    // SAFETY: `pipe` is a live, connected pipe server handle; `pid` is a valid
    // out-pointer.
    unsafe { GetNamedPipeClientProcessId(HANDLE(pipe.as_raw_handle()), &mut pid) }
        .map_err(|e| start(format!("checking the browser helper's pipe: {e}")))?;
    Ok(pid)
}

fn start(reason: String) -> BrowserError {
    BrowserError::Start { reason }
}
