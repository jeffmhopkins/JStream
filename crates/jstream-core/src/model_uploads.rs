//! 3D model uploads made off the render thread (ADR-0177).
//!
//! A model's upload copies its meshes and textures, up to ADR-0107's cap, which is far
//! more than a tick's work (R-RT-002). So the program stage's render thread only asks
//! for one here and draws the model from the frame its upload is back: the
//! `jstream-models` thread makes it, on the compositor's device
//! ([`jstream_render::ModelUploader`]). The render thread sends and receives on
//! channels and never waits (R-RT-003). A compositor that is not on a real-time
//! thread (`video.snapshot`'s, a preview stream's) uploads where it draws, so what it
//! draws never depends on when it was asked.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use jstream_media::model::Model;
use jstream_protocol::SourceId;
use jstream_render::{GpuModel, RenderError};

/// A model for the thread to upload.
struct Job {
    source: SourceId,
    model: Arc<Model>,
}

/// An upload the thread finished, or what refused it.
struct Done {
    source: SourceId,
    version: u64,
    result: Result<GpuModel, RenderError>,
}

/// What the thread uploads a model with: a compositor's
/// [`ModelUploader`](jstream_render::ModelUploader).
type Upload = Arc<dyn Fn(&Model) -> Result<GpuModel, RenderError> + Send + Sync>;

/// One compositor's model uploads on the `jstream-models` thread.
pub(crate) struct ModelUploads {
    upload: Upload,
    /// The thread's ends, once a model was asked for. The thread ends when this drops.
    thread: Option<(Sender<Job>, Receiver<Done>)>,
    /// The version of each source's model the thread was asked for and hasn't
    /// answered.
    asked: BTreeMap<SourceId, u64>,
    /// The version of each source's model whose upload was refused, with why: it is
    /// not asked for again.
    refused: BTreeMap<SourceId, (u64, RenderError)>,
}

/// What became of asking for a model's upload.
pub(crate) enum Asked {
    /// The thread has it, or was just given it.
    Coming,
    /// The thread couldn't be started or is gone: upload it here.
    Here,
}

impl ModelUploads {
    /// Uploads made with `upload`, a compositor's. No thread runs until a model is
    /// asked for.
    pub(crate) fn new(
        upload: impl Fn(&Model) -> Result<GpuModel, RenderError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            upload: Arc::new(upload),
            thread: None,
            asked: BTreeMap::new(),
            refused: BTreeMap::new(),
        }
    }

    /// Asks for the upload of `model`, which `source` shows, unless it was asked for
    /// already.
    ///
    /// # Errors
    /// What refused this version's upload when it was made.
    pub(crate) fn ask(
        &mut self,
        source: &SourceId,
        model: &Arc<Model>,
    ) -> Result<Asked, RenderError> {
        let version = model.version();
        if self.asked.get(source) == Some(&version) {
            return Ok(Asked::Coming);
        }
        if let Some((_, error)) = self.refused.get(source).filter(|(v, _)| *v == version) {
            return Err(error.clone());
        }
        let Some(jobs) = self.jobs() else {
            return Ok(Asked::Here);
        };
        let job = Job {
            source: source.clone(),
            model: Arc::clone(model),
        };
        if jobs.send(job).is_err() {
            // The thread is gone, which only a panic in an upload does.
            self.thread = None;
            self.asked.clear();
            return Ok(Asked::Here);
        }
        self.refused.remove(source);
        self.asked.insert(source.clone(), version);
        Ok(Asked::Coming)
    }

    /// Hands `take` each upload the thread finished that is still the one asked for,
    /// and forgets what was asked for the sources `wanted` turns down: their uploads
    /// are dropped when they come.
    pub(crate) fn finished(
        &mut self,
        mut wanted: impl FnMut(&SourceId) -> bool,
        mut take: impl FnMut(SourceId, u64, GpuModel),
    ) {
        if let Some((_, done)) = &self.thread {
            for done in done.try_iter() {
                if self.asked.get(&done.source) != Some(&done.version) {
                    continue;
                }
                self.asked.remove(&done.source);
                match done.result {
                    Ok(model) => take(done.source, done.version, model),
                    Err(error) => {
                        self.refused.insert(done.source, (done.version, error));
                    }
                }
            }
        }
        self.asked.retain(|source, _| wanted(source));
        self.refused.retain(|source, _| wanted(source));
    }

    /// Whether an upload was asked for and isn't back, for tests.
    #[cfg(test)]
    pub(crate) fn busy(&self) -> bool {
        !self.asked.is_empty()
    }

    /// The thread's job queue, starting the thread on first use. `None` when it can't
    /// be started, which is logged.
    fn jobs(&mut self) -> Option<&Sender<Job>> {
        if self.thread.is_none() {
            let (jobs, queued) = mpsc::channel::<Job>();
            let (finished, done) = mpsc::channel();
            let upload = Arc::clone(&self.upload);
            let spawned = thread::Builder::new()
                .name("jstream-models".into())
                .spawn(move || run(&*upload, &queued, &finished));
            match spawned {
                Ok(_) => self.thread = Some((jobs, done)),
                Err(err) => {
                    tracing::error!(error = %err, "starting the jstream-models thread failed");
                    return None;
                }
            }
        }
        self.thread.as_ref().map(|(jobs, _)| jobs)
    }
}

/// The `jstream-models` thread: uploads each model it is given, until the compositor's
/// chains go.
fn run(
    upload: &dyn Fn(&Model) -> Result<GpuModel, RenderError>,
    queued: &Receiver<Job>,
    finished: &Sender<Done>,
) {
    for job in queued {
        let done = Done {
            version: job.model.version(),
            result: upload(&job.model),
            source: job.source,
        };
        if finished.send(done).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use jstream_media::model::{Material, shapes::Shape};

    use super::*;

    /// ADR-0177 decision 3: an upload the thread was refused is reported for that
    /// version from then on and never asked for again; a new version is.
    #[test]
    fn a_refused_upload_is_not_asked_for_again_until_the_model_changes() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let mut uploads = ModelUploads::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            Err(RenderError::Invalid {
                what: "model",
                detail: "refused".into(),
            })
        });
        let source = SourceId::new("cam");
        let model = Arc::new(Shape::Plane.model(Material::default()).expect("a plane"));
        assert!(matches!(uploads.ask(&source, &model), Ok(Asked::Coming)));
        assert!(
            matches!(uploads.ask(&source, &model), Ok(Asked::Coming)),
            "asked once while it is coming"
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while uploads.busy() {
            assert!(Instant::now() < deadline, "the thread never answered");
            std::thread::sleep(Duration::from_millis(1));
            uploads.finished(|_| true, |_, _, _| panic!("a refused upload is no model"));
        }
        for _ in 0..3 {
            assert!(matches!(
                uploads.ask(&source, &model),
                Err(RenderError::Invalid { .. })
            ));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "uploaded once");
        let next = Arc::new(Shape::Plane.model(Material::default()).expect("a plane"));
        assert_ne!(next.version(), model.version());
        assert!(matches!(uploads.ask(&source, &next), Ok(Asked::Coming)));
    }
}
