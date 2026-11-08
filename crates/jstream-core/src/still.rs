//! Still images whose decoded pixels are held only while a draw needs them
//! (ADR-0117 §2, #1139).
//!
//! A shot names a [`ShownStill`] in place of a frame
//! ([`Frame::from_still`](crate::snapshot::Frame::from_still)). Once the program
//! stage's compositor has uploaded it, it says so, and the `jstream-stills` thread lets
//! go of the pixels; the texture is drawn from then on. A draw that needs them again (a
//! larger placement, an upload dropped while nothing drew it) finds none and asks for
//! them, and the thread decodes the file again for a later draw ([`Still::frame`]),
//! which keeps drawing the texture it has meanwhile. A still that no mix has drawn for
//! 2 s gets its pixels back, so showing it again draws it at once; one deleted or left
//! for good sooner than that costs no decode.
//!
//! The render thread only reads and sets atomics here, and wakes the thread with
//! [`Thread::unpark`] (R-RT-002, R-RT-003): it never decodes, and never drops the last
//! reference to the pixels, which the thread frees. A compositor that is not on a
//! real-time thread (`video.snapshot`'s, a preview stream's) reads the still's pixels
//! itself, decoding them if no one holds them ([`ShownStill::read`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use arc_swap::ArcSwapOption;
use jstream_media::{FrameSize, VideoFrame};
use jstream_sources::Still;

/// How long the thread waits before it decodes a still again whose file failed to.
const RETRY: Duration = Duration::from_secs(5);

/// How long a still no mix draws waits before its pixels are decoded again, so a
/// source deleted, or a scene left for good, right after it was shown costs no decode.
const UNDRAWN: Duration = Duration::from_secs(2);

/// How often the thread looks again at pixels it let go of that a draw was still
/// reading, to free them once it is done.
const RECHECK: Duration = Duration::from_millis(50);

/// One live source's still, as the shots that show it name it.
pub(crate) struct ShownStill {
    still: Arc<Still>,
    /// Its pixels while they are held. Only the thread and [`Self::new`] write it.
    held: ArcSwapOption<VideoFrame>,
    /// A draw needed the pixels and found none.
    wanted: AtomicBool,
    /// The program stage's compositor has what it needs of them.
    uploaded: AtomicBool,
    /// Whether some mix of the program stage draws it ([`crate::live`]'s `drawn`).
    drawn: Arc<AtomicBool>,
    keeper: Arc<Shared>,
}

impl ShownStill {
    /// The pixels of `still`, as a shot shows them: held while anyone holds the still's
    /// frame (as it is when the source was just built), else made again by the thread
    /// before a draw needs them.
    fn new(still: Arc<Still>, drawn: Arc<AtomicBool>, keeper: Arc<Shared>) -> Self {
        Self {
            held: ArcSwapOption::new(
                still
                    .frame_if_held()
                    .filter(|frame| crate::slot::packed(frame)),
            ),
            still,
            wanted: AtomicBool::new(false),
            uploaded: AtomicBool::new(false),
            drawn,
            keeper,
        }
    }

    /// Its size.
    pub(crate) fn size(&self) -> FrameSize {
        self.still.size()
    }

    /// Its pixels, if they are held now, for the program stage's draw. Lock-free;
    /// dropping it never frees them, which the thread does.
    pub(crate) fn held(&self) -> Option<Arc<VideoFrame>> {
        self.held.load_full()
    }

    /// Its pixels for a compositor that is not on a real-time thread: the held ones, or
    /// the file decoded again here. `None` if the file can't be.
    pub(crate) fn read(&self) -> Option<Arc<VideoFrame>> {
        self.held().or_else(|| {
            self.still
                .frame()
                .inspect_err(
                    |err| tracing::warn!(error = %err, "a still image could not be read again"),
                )
                .ok()
                .filter(|frame| crate::slot::packed(frame))
        })
    }

    /// A draw of the program stage needed its pixels and found none: the thread
    /// decodes them again.
    pub(crate) fn want(&self) {
        if !self.wanted.swap(true, Ordering::AcqRel) {
            self.keeper.wake();
        }
    }

    /// The program stage's compositor has what it needs: the thread lets go of the
    /// pixels, if they are held.
    pub(crate) fn uploaded(&self) {
        if self.held.load().is_some() && !self.uploaded.swap(true, Ordering::AcqRel) {
            self.keeper.wake();
        }
    }
}

impl std::fmt::Debug for ShownStill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShownStill")
            .field("size", &self.size())
            .field("held", &self.held.load().is_some())
            .finish_non_exhaustive()
    }
}

/// The `jstream-stills` thread of one engine's live sources, which holds and lets go
/// of every shown still's pixels. Started by the first still; ended when this drops.
pub(crate) struct Stills {
    shared: Arc<Shared>,
}

struct Shared {
    /// Stills shown since the thread last looked, for it to take on.
    new: Mutex<Vec<Weak<ShownStill>>>,
    /// The thread, once started, to wake: read without a lock.
    thread: OnceLock<Thread>,
    closed: AtomicBool,
}

impl Shared {
    /// Wakes the thread, if it runs. Lock-free.
    fn wake(&self) {
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
    }

    fn lock_new(&self) -> MutexGuard<'_, Vec<Weak<ShownStill>>> {
        self.new.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for Stills {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                new: Mutex::new(Vec::new()),
                thread: OnceLock::new(),
                closed: AtomicBool::new(false),
            }),
        }
    }
}

impl Stills {
    /// `still` as shots show it, its pixels held by the thread from now on. `drawn`
    /// says whether a mix of the program stage draws it.
    pub(crate) fn show(&self, still: Arc<Still>, drawn: Arc<AtomicBool>) -> Arc<ShownStill> {
        let shown = Arc::new(ShownStill::new(still, drawn, Arc::clone(&self.shared)));
        self.shared.lock_new().push(Arc::downgrade(&shown));
        self.start();
        self.shared.wake();
        shown
    }

    /// Tells the thread that whether a still is drawn may have changed: one no mix
    /// draws any more gets its pixels back.
    pub(crate) fn drawn_changed(&self) {
        self.shared.wake();
    }

    /// Starts the thread, once: only the plan shows stills, so no two calls race.
    fn start(&self) {
        if self.shared.thread.get().is_some() {
            return;
        }
        let shared = Arc::clone(&self.shared);
        match thread::Builder::new()
            .name("jstream-stills".into())
            .spawn(move || run(&shared))
        {
            Ok(handle) => {
                let _ = self.shared.thread.set(handle.thread().clone());
            }
            Err(err) => tracing::error!(error = %err, "starting the jstream-stills thread failed"),
        }
    }
}

impl Drop for Stills {
    /// Ends the thread with the engine.
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.wake();
    }
}

/// One still the thread keeps.
struct Kept {
    still: Weak<ShownStill>,
    /// When to try decoding it again, after its file failed to.
    retry: Option<Instant>,
    /// Whether that failure was logged, so a file that stays gone warns once.
    warned: bool,
    /// Since when no mix has drawn it, while its pixels are let go of.
    undrawn: Option<Instant>,
}

impl Kept {
    /// When the thread next has something to do for it unasked.
    fn due(&self) -> Option<Instant> {
        let undrawn = self.undrawn.map(|since| since + UNDRAWN);
        match (self.retry, undrawn) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }
}

/// The `jstream-stills` thread: lets go of the pixels the program stage has uploaded,
/// and decodes again those a draw wants or that no mix has drawn for a while, until
/// the engine goes.
fn run(shared: &Shared) {
    let mut kept: Vec<Kept> = Vec::new();
    // Pixels let go of that a draw may still read, freed here once only this holds them.
    let mut retired: Vec<Arc<VideoFrame>> = Vec::new();
    while !shared.closed.load(Ordering::Acquire) {
        kept.extend(shared.lock_new().drain(..).map(|still| Kept {
            still,
            retry: None,
            warned: false,
            undrawn: None,
        }));
        kept.retain(|kept| kept.still.strong_count() > 0);
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        for kept in &mut kept {
            let Some(still) = kept.still.upgrade() else {
                continue;
            };
            if let Some(frame) = keep(&still, kept, now) {
                retired.push(frame);
            }
            if let Some(due) = kept.due() {
                next = Some(next.map_or(due, |next| next.min(due)));
            }
        }
        retired.retain(|frame| Arc::strong_count(frame) > 1);
        if !retired.is_empty() {
            let recheck = now + RECHECK;
            next = Some(next.map_or(recheck, |next| next.min(recheck)));
        }
        match next {
            Some(at) => thread::park_timeout(at.saturating_duration_since(Instant::now())),
            None => thread::park(),
        }
    }
}

/// Holds or lets go of one still's pixels. Gives back the pixels it let go of.
fn keep(still: &ShownStill, kept: &mut Kept, now: Instant) -> Option<Arc<VideoFrame>> {
    let drawn = still.drawn.load(Ordering::Acquire);
    if still.held.load().is_some() {
        if !drawn {
            // Kept while no mix draws it, and uploaded again when one does.
            still.uploaded.store(false, Ordering::Release);
        } else if still.uploaded.swap(false, Ordering::AcqRel)
            && !still.wanted.swap(false, Ordering::AcqRel)
        {
            return still.held.swap(None);
        }
        still.wanted.store(false, Ordering::Release);
        return None;
    }
    let wanted = still.wanted.load(Ordering::Acquire);
    kept.undrawn = if drawn {
        None
    } else {
        Some(kept.undrawn.unwrap_or(now))
    };
    let rested = kept.undrawn.is_some_and(|since| now >= since + UNDRAWN);
    if !(wanted || rested) || kept.retry.is_some_and(|retry| now < retry) {
        return None;
    }
    kept.undrawn = None;
    still.wanted.store(false, Ordering::Release);
    still.uploaded.store(false, Ordering::Release);
    match still.still.frame() {
        Ok(frame) if crate::slot::packed(&frame) => {
            kept.retry = None;
            kept.warned = false;
            still.held.store(Some(frame));
        }
        Ok(_) => kept.retry = Some(now + RETRY),
        Err(err) => {
            if !std::mem::replace(&mut kept.warned, true) {
                tracing::warn!(error = %err, "a still image could not be read again; it shows what it has");
            }
            kept.retry = Some(now + RETRY);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::Stills;
    use jstream_media::{
        ColorInfo, FrameSize, MediaTime, PixelFormat, Plane, Timebase, VideoFrame, VideoFrameInfo,
    };
    use jstream_sources::Still;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn frame(value: u8) -> VideoFrame {
        let info = VideoFrameInfo {
            size: FrameSize::new(2, 1).expect("size"),
            format: PixelFormat::Rgba8,
            color: ColorInfo::default(),
            pts: MediaTime::zero(Timebase::MILLIS),
        };
        VideoFrame::new(info, vec![Plane::new(vec![value; 8], 8)]).expect("frame")
    }

    /// Waits up to a few seconds for the stills thread to make `done` true.
    fn eventually(done: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        done()
    }

    /// A drawn still lets go of its pixels once the program stage has uploaded it,
    /// decodes them again when a draw wants them, and holds them while no mix draws it.
    #[test]
    fn a_shown_still_holds_its_pixels_only_while_a_draw_needs_them() {
        let loads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&loads);
        let still = Arc::new(Still::new(Arc::new(frame(1)), "a.png".into(), move || {
            counted.fetch_add(1, Ordering::Relaxed);
            Ok(frame(2))
        }));
        let stills = Stills::default();
        let drawn = Arc::new(AtomicBool::new(true));
        let shown = stills.show(Arc::clone(&still), Arc::clone(&drawn));
        let held = shown.held().expect("the frame the build made");
        assert_eq!(held.plane(0).map(|p| p.data()[0]), Some(1));

        // A draw still reads it as it is let go of: freed by the thread after.
        shown.uploaded();
        assert!(
            eventually(|| shown.held().is_none()),
            "let go of once uploaded"
        );
        assert!(still.held(), "the draw reading it holds it");
        drop(held);
        assert!(eventually(|| !still.held()), "freed once the draw is done");
        assert_eq!(loads.load(Ordering::Relaxed), 0);

        shown.want();
        assert!(eventually(|| shown.held().is_some()), "decoded again");
        assert_eq!(loads.load(Ordering::Relaxed), 1);
        assert_eq!(
            shown.held().and_then(|f| f.plane(0).map(|p| p.data()[0])),
            Some(2)
        );

        // Not drawn: an upload doesn't let go of it.
        drawn.store(false, Ordering::Relaxed);
        shown.uploaded();
        stills.drawn_changed();
        std::thread::sleep(Duration::from_millis(50));
        assert!(shown.held().is_some(), "held while no mix draws it");

        // Let go of while drawn, then no longer drawn: decoded again for the next show.
        drawn.store(true, Ordering::Relaxed);
        shown.uploaded();
        assert!(eventually(|| shown.held().is_none()));
        drawn.store(false, Ordering::Relaxed);
        stills.drawn_changed();
        assert!(
            eventually(|| shown.held().is_some()),
            "held again for a show"
        );
        assert_eq!(loads.load(Ordering::Relaxed), 2);
    }
}
