//! The helper's copy thread (Windows): waits for each paint's GPU copy, off CEF's UI
//! thread, then runs what follows it, in the order the jobs were queued. Every page's
//! paints arrive on CEF's one UI thread, which also paces their frames, so a paint that
//! waited there for its copy held back every page's next frame. A job that only sends a
//! message (a reload's `Reloading`) waits its turn behind the frames queued before it,
//! so the engine still gets them in the order the UI thread made them.

use std::sync::mpsc::{self, Receiver, Sender};

use super::d3d11::{Copy, CopyEvent};

/// What the copy thread does next.
pub(crate) enum Job {
    /// Waits for the copy, then hands it and how the wait went to the closure.
    Wait(Copy, Box<dyn FnOnce(Copy, Result<(), String>) + Send>),
    /// Runs the closure.
    Run(Box<dyn FnOnce() + Send>),
}

impl Job {
    /// Does the job, waiting on `event`; without one (no thread to wait on), a copy is
    /// failed rather than waited for.
    pub(crate) fn run(self, event: Option<&CopyEvent>) {
        match self {
            Self::Wait(copy, then) => {
                let waited = match event {
                    Some(event) => copy.wait(event),
                    None => Err("the helper's copy thread is gone".into()),
                };
                then(copy, waited);
            }
            Self::Run(then) => then(),
        }
    }
}

/// Starts the copy thread. Its jobs end with the returned sender. If it can't start,
/// the sender's jobs run on the sender's thread ([`Job::run`] with no event).
pub(crate) fn start() -> Sender<Job> {
    let (tx, rx) = mpsc::channel();
    let started = std::thread::Builder::new()
        .name("jstream-helper-copies".into())
        .spawn(move || run(&rx));
    if started.is_err() {
        // The receiver went with the closure: every send fails, and runs at once.
        return mpsc::channel().0;
    }
    tx
}

fn run(jobs: &Receiver<Job>) {
    let event = CopyEvent::new().ok();
    for job in jobs {
        job.run(event.as_ref());
    }
}
