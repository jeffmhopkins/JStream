//! A still frame that can be let go of and made again (ADR-0117 §2, #1139).
//!
//! A still image's decoded pixels are needed only until the compositor has them on
//! the GPU, so the host lets go of them then and asks for them again from the file
//! when a draw needs them once more ([`Still::frame`]).

use crate::SourceError;
use jstream_media::{FrameSize, VideoFrame};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

/// What makes a still's frame again: reading and decoding its file once more.
type Load = Box<dyn Fn() -> Result<VideoFrame, SourceError> + Send + Sync>;

/// One still frame, held only while someone holds it.
///
/// Every [`Arc`] that [`frame`](Self::frame) hands out shares one copy, and once all
/// of them are dropped the pixels are freed: the next call makes them again, which
/// reads and decodes the file, so it blocks and never happens on a real-time thread.
pub struct Still {
    size: FrameSize,
    /// The file it is read from, for errors.
    path: String,
    held: Mutex<Held>,
    /// Taken while the file is decoded again.
    decoding: Mutex<()>,
    load: Load,
}

#[derive(Default)]
struct Held {
    /// The frame made when the source was created, until someone takes it.
    first: Option<Arc<VideoFrame>>,
    /// The copy handed out last, while anyone holds it.
    shared: Weak<VideoFrame>,
}

impl Still {
    /// A still that shows `first` and makes it again with `load`, from the file at
    /// `path`.
    pub fn new(
        first: Arc<VideoFrame>,
        path: String,
        load: impl Fn() -> Result<VideoFrame, SourceError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            size: first.info().size,
            path,
            held: Mutex::new(Held {
                first: Some(first),
                shared: Weak::new(),
            }),
            decoding: Mutex::new(()),
            load: Box::new(load),
        }
    }

    /// Its size, which a frame made again keeps.
    #[must_use]
    pub fn size(&self) -> FrameSize {
        self.size
    }

    /// Whether anyone holds its pixels now, so [`frame`](Self::frame) makes no copy.
    #[must_use]
    pub fn held(&self) -> bool {
        let held = self.lock();
        held.first.is_some() || held.shared.strong_count() > 0
    }

    /// Its frame if anyone holds it now, or the one made when the source was created
    /// that no one has taken yet: never read from the file.
    #[must_use]
    pub fn frame_if_held(&self) -> Option<Arc<VideoFrame>> {
        let mut held = self.lock();
        if let Some(first) = held.first.take() {
            held.shared = Arc::downgrade(&first);
            return Some(first);
        }
        held.shared.upgrade()
    }

    /// Its frame: the copy someone holds, or one made again from the file.
    ///
    /// # Errors
    /// What reading or decoding the file gives now: it may have gone, or changed. A
    /// file that decodes at another size fails with [`SourceError::Decode`], since
    /// whatever shows the still is placed at the size it had.
    pub fn frame(&self) -> Result<Arc<VideoFrame>, SourceError> {
        if let Some(frame) = self.frame_if_held() {
            return Ok(frame);
        }
        // One decode at a time, so two callers share one rather than make two copies;
        // under its own lock, so `frame_if_held` and `held` never wait for it.
        let _decoding = self.decoding.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(frame) = self.frame_if_held() {
            return Ok(frame);
        }
        let frame = (self.load)()?;
        if frame.info().size != self.size {
            return Err(SourceError::Decode {
                path: self.path.clone(),
                format: "the image it was",
                reason: format!(
                    "it is {}x{} now, not {}x{} as it was when it was loaded",
                    frame.info().size.width(),
                    frame.info().size.height(),
                    self.size.width(),
                    self.size.height()
                ),
            });
        }
        let frame = Arc::new(frame);
        self.lock().shared = Arc::downgrade(&frame);
        Ok(frame)
    }

    fn lock(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl std::fmt::Debug for Still {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Still")
            .field("size", &self.size)
            .field("held", &self.held())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::Still;
    use crate::color::rgba_frame;
    use jstream_media::{FrameSize, MediaTime, Timebase, VideoFrame};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn frame(width: u32, value: u8) -> VideoFrame {
        let size = FrameSize::new(width, 1).expect("invariant: a size");
        rgba_frame(
            size,
            vec![value; width as usize * 4],
            MediaTime::zero(Timebase::MILLIS),
        )
        .expect("invariant: a frame")
    }

    /// Its pixels live while a frame it handed out does, and are made again after.
    #[test]
    fn a_still_makes_its_frame_again_once_none_is_held() {
        let loads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&loads);
        let still = Still::new(Arc::new(frame(2, 1)), "a.png".into(), move || {
            counted.fetch_add(1, Ordering::Relaxed);
            Ok(frame(2, 9))
        });
        let first = still.frame().expect("the first frame");
        assert_eq!(first.plane(0).map(|p| p.data()[0]), Some(1));
        let again = still.frame().expect("the same frame");
        assert!(Arc::ptr_eq(&first, &again), "one copy, shared");
        assert_eq!(loads.load(Ordering::Relaxed), 0);
        drop((first, again));
        assert!(!still.held(), "nothing holds it now");

        let made = still.frame().expect("made again");
        assert_eq!(made.plane(0).map(|p| p.data()[0]), Some(9));
        assert_eq!(loads.load(Ordering::Relaxed), 1);
        assert!(still.held());
    }

    /// A file that changed size since can't stand in for the still.
    #[test]
    fn a_still_whose_file_changed_size_fails() {
        let still = Still::new(Arc::new(frame(2, 1)), "a.png".into(), || Ok(frame(3, 1)));
        drop(still.frame());
        assert!(still.frame().is_err());
    }
}
