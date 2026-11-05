//! The job object the browser helper runs in (Windows): it kills the helper and every
//! process CEF starts from it once its last handle closes, which the engine's process
//! holds, so no helper outlives the engine however it ends (#1186).

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::Child;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};

use crate::BrowserError;

/// A job that kills every process in it when its handle closes.
#[derive(Debug)]
pub(super) struct KillOnClose(OwnedHandle);

impl KillOnClose {
    /// A new, empty job.
    ///
    /// # Errors
    /// [`BrowserError::Start`] when Windows refuses the job or its limit.
    pub(super) fn create() -> Result<Self, BrowserError> {
        // SAFETY: plain call; no security attributes, no name.
        let raw =
            unsafe { CreateJobObjectW(None, None) }.map_err(|e| failed("CreateJobObjectW", &e))?;
        // SAFETY: `raw` is a job handle just created, owned by nothing else.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(raw.0) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` is the struct this information class takes and lives for the
        // call; the size passed is its size. The job handle is live.
        unsafe {
            SetInformationJobObject(
                job.handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                    .unwrap_or(u32::MAX),
            )
        }
        .map_err(|e| failed("SetInformationJobObject", &e))?;
        Ok(job)
    }

    /// Puts `child` in the job.
    ///
    /// # Errors
    /// [`BrowserError::Start`] when Windows refuses.
    pub(super) fn assign(&self, child: &Child) -> Result<(), BrowserError> {
        // SAFETY: both handles are live: the job is owned by `self`, and the process
        // handle by `child`, which outlives the call.
        unsafe { AssignProcessToJobObject(self.handle(), HANDLE(child.as_raw_handle())) }
            .map_err(|e| failed("AssignProcessToJobObject", &e))
    }

    fn handle(&self) -> HANDLE {
        HANDLE(self.0.as_raw_handle())
    }
}

fn failed(call: &str, error: &windows::core::Error) -> BrowserError {
    BrowserError::Start {
        reason: format!("the browser helper's job: {call} failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::KillOnClose;

    /// #1186: closing the job ends the process in it, as the engine's process ending
    /// does.
    #[test]
    fn closing_the_job_ends_its_process() {
        let job = KillOnClose::create().expect("a job");
        let mut child = Command::new("cmd")
            .args(["/c", "ping -n 30 127.0.0.1 >nul"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("a child");
        job.assign(&child).expect("assigned");
        assert!(
            child.try_wait().expect("a status").is_none(),
            "still running"
        );
        drop(job);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if child.try_wait().expect("a status").is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "the process outlived its job");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
