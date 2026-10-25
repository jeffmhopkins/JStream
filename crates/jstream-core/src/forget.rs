//! Forgetting what the capture backend keeps for a deleted source, such as the ScreenCast
//! portal's restore token (`Capture::forget_source`), on a thread of its own: it can
//! remove a file, which must not happen under the engine's lock or on the render thread.

use std::sync::Arc;
use std::sync::mpsc::{self, Sender};

use jstream_platform::Capture;
use jstream_protocol::SourceId;

/// The `jstream-forget` thread's side, held by the engine. Dropping it ends the thread
/// once it has forgotten what it was sent.
pub(crate) struct ForgetSources {
    tx: Sender<SourceId>,
}

impl ForgetSources {
    /// Starts the thread that forgets sources on `capture`. `None`, after logging, when
    /// the thread can't start: deleted sources then keep their grants.
    pub(crate) fn spawn(capture: Arc<dyn Capture>) -> Option<Self> {
        let (tx, rx) = mpsc::channel::<SourceId>();
        let spawned = std::thread::Builder::new()
            .name("jstream-forget".into())
            .spawn(move || {
                for source in rx {
                    capture.forget_source(source.as_str());
                    tracing::debug!(source = %source.as_str(), "forgot the capture grant of a deleted source");
                }
            });
        match spawned {
            Ok(_) => Some(Self { tx }),
            Err(error) => {
                tracing::warn!(%error, "deleted sources keep their capture grants: no jstream-forget thread");
                None
            }
        }
    }

    /// Asks the thread to forget `source`. Never blocks.
    pub(crate) fn forget(&self, source: SourceId) {
        // The thread only ends when this side is dropped.
        let _ = self.tx.send(source);
    }
}
