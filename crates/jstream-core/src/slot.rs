//! Frame slots (ADR-0117 §2 and §3): a single-frame "latest frame" mailbox a live
//! source's producer writes into and every compositor reads at its tick, so a new
//! frame of the same size reaches the picture without a re-plan.
//!
//! A shot names a slot in place of a frame ([`Frame::from_slot`](crate::snapshot::Frame::from_slot)),
//! and each draw uploads whatever the slot holds then, by the slot frame's own version.
//! Only a new size needs a plan, since where the frame is placed depends on it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use arc_swap::ArcSwap;
use jstream_media::{FrameSize, PixelFormat, VideoFrame};

/// The versions slot frames are uploaded under, apart from the ones plans number
/// frames with (counted from 1 per engine), so a cache never takes one for the other.
static VERSIONS: AtomicU64 = AtomicU64::new(1 << 62);

fn next_version() -> u64 {
    VERSIONS.fetch_add(1, Ordering::Relaxed)
}

/// The latest frame of one live source, packed `Rgba8`.
///
/// A draw reads it without a lock (R-RT-003): the frame is behind an [`ArcSwap`], and
/// a draw holds what it read as a [`Latest`] for as long as it uses it. The writers
/// (the plan and the sources thread) take `retired`, a lock no draw takes, keep each
/// frame they replace there until no draw holds it, and free it themselves, so the
/// render thread never drops the last reference to a frame.
pub(crate) struct FrameSlot {
    latest: ArcSwap<Latest>,
    /// The frames replaced while a draw may still hold them, freed by a later write
    /// once only this list does.
    retired: Mutex<Vec<Arc<Latest>>>,
}

/// One frame a slot held, and the version it is uploaded under.
#[derive(Debug)]
pub(crate) struct Latest {
    frame: Arc<VideoFrame>,
    version: u64,
}

impl Latest {
    /// The frame.
    pub(crate) fn frame(&self) -> &Arc<VideoFrame> {
        &self.frame
    }

    /// The version it is uploaded under: a new one for each frame the slot takes.
    pub(crate) fn version(&self) -> u64 {
        self.version
    }

    /// Its packed bytes, `width * height * 4` of them.
    pub(crate) fn rgba(&self) -> &[u8] {
        rgba(&self.frame)
    }
}

/// What [`FrameSlot::store`] did with a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stored {
    /// The slot holds it now.
    New,
    /// The slot held it already.
    Same,
    /// Not stored: it is another size, or not packed `Rgba8`, so it needs a plan.
    Refused,
}

impl FrameSlot {
    /// A slot holding `frame`; `None` when it isn't packed `Rgba8`, which only a plan
    /// can repack.
    pub(crate) fn new(frame: Arc<VideoFrame>) -> Option<Arc<Self>> {
        packed(&frame).then(|| {
            Arc::new(Self {
                latest: ArcSwap::from_pointee(Latest {
                    frame,
                    version: next_version(),
                }),
                retired: Mutex::new(Vec::new()),
            })
        })
    }

    /// Hands `frame` to every compositor's next draw, if it is the slot's size and
    /// packed. Never blocks a draw.
    #[cfg(test)]
    pub(crate) fn store(&self, frame: Arc<VideoFrame>) -> Stored {
        self.store_latest(|| frame)
    }

    /// [`Self::store`] of the frame `latest` gives, read while no other write runs: the
    /// plan and the sources thread both store a painter's frame, and whichever writes
    /// last reads the newest, so the slot never goes back to an older one.
    pub(crate) fn store_latest(&self, latest: impl FnOnce() -> Arc<VideoFrame>) -> Stored {
        let mut retired = self.lock();
        let frame = latest();
        let held = self.latest.load_full();
        if Arc::ptr_eq(&held.frame, &frame) {
            return Stored::Same;
        }
        if frame.info().size != held.frame.info().size || !packed(&frame) {
            return Stored::Refused;
        }
        drop(held);
        let freed = self.swap(&mut retired, frame);
        drop(retired);
        drop(freed);
        Stored::New
    }

    /// Holds `frame` whatever its size, for a plan, which places it at that size.
    /// `false` when it isn't packed `Rgba8`.
    pub(crate) fn replace(&self, frame: Arc<VideoFrame>) -> bool {
        if !packed(&frame) {
            return false;
        }
        let mut retired = self.lock();
        if !Arc::ptr_eq(&self.latest.load().frame, &frame) {
            let freed = self.swap(&mut retired, frame);
            drop(retired);
            drop(freed);
        }
        true
    }

    /// Puts `frame` in the slot and retires the one it held; gives back the retired
    /// frames no draw holds any more, for the caller to free once it lets go of the lock.
    fn swap(&self, retired: &mut Vec<Arc<Latest>>, frame: Arc<VideoFrame>) -> Vec<Arc<Latest>> {
        let old = self.latest.swap(Arc::new(Latest {
            frame,
            version: next_version(),
        }));
        retired.push(old);
        // A retired frame is out of the slot, so no draw can take it again: once only
        // this list holds it, it stays that way.
        let (freed, held) = retired
            .drain(..)
            .partition(|latest| Arc::strong_count(latest) == 1);
        *retired = held;
        freed
    }

    /// The frame the slot holds now, and its version. Lock-free. A draw keeps it while
    /// it reads the frame; dropping it never frees the frame, which the slot's writers
    /// do.
    pub(crate) fn latest(&self) -> Arc<Latest> {
        self.latest.load_full()
    }

    /// The size of the frame it holds.
    pub(crate) fn size(&self) -> FrameSize {
        self.latest.load().frame.info().size
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Arc<Latest>>> {
        self.retired.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl std::fmt::Debug for FrameSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let latest = self.latest();
        f.debug_struct("FrameSlot")
            .field("size", &latest.frame.info().size)
            .field("version", &latest.version)
            .finish()
    }
}

/// Whether `frame` is `Rgba8` with rows packed one after another, as an upload reads it.
pub(crate) fn packed(frame: &VideoFrame) -> bool {
    let info = frame.info();
    let row = info.size.width() as usize * 4;
    let len = row * info.size.height() as usize;
    info.format == PixelFormat::Rgba8
        && frame
            .plane(0)
            .is_some_and(|plane| plane.stride() == row && plane.data().len() >= len)
}

/// The packed bytes of `frame`, as [`FrameSlot`] keeps it: `width * height * 4`.
pub(crate) fn rgba(frame: &VideoFrame) -> &[u8] {
    let size = frame.info().size;
    let len = size.width() as usize * size.height() as usize * 4;
    frame
        .plane(0)
        .and_then(|plane| plane.data().get(..len))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{FrameSlot, Stored};
    use jstream_media::{
        ColorInfo, FrameSize, MediaTime, PixelFormat, Plane, Timebase, VideoFrame, VideoFrameInfo,
    };
    use std::sync::Arc;

    fn frame(width: u32, height: u32, value: u8) -> Arc<VideoFrame> {
        let size = FrameSize::new(width, height).expect("size");
        let info = VideoFrameInfo {
            size,
            format: PixelFormat::Rgba8,
            color: ColorInfo::default(),
            pts: MediaTime::zero(Timebase::MILLIS),
        };
        let plane = Plane::new(
            vec![value; (width * height * 4) as usize],
            width as usize * 4,
        );
        Arc::new(VideoFrame::new(info, vec![plane]).expect("frame"))
    }

    /// A frame of the slot's size replaces the one it holds under a new version; one of
    /// another size waits for a plan, which may replace it.
    #[test]
    fn a_slot_takes_frames_of_its_size_and_a_plan_takes_any() {
        let first = frame(2, 1, 1);
        let slot = FrameSlot::new(Arc::clone(&first)).expect("packed");
        let v1 = slot.latest().version();
        assert_eq!(slot.store(Arc::clone(&first)), Stored::Same);
        let second = frame(2, 1, 2);
        assert_eq!(slot.store(Arc::clone(&second)), Stored::New);
        let held = slot.latest();
        let v2 = held.version();
        assert!(Arc::ptr_eq(held.frame(), &second) && v2 > v1);
        let wider = frame(3, 1, 3);
        assert_eq!(slot.store(Arc::clone(&wider)), Stored::Refused);
        assert_eq!(slot.size(), FrameSize::new(2, 1).expect("size"));
        assert!(slot.replace(Arc::clone(&wider)));
        let held = slot.latest();
        assert!(Arc::ptr_eq(held.frame(), &wider) && held.version() > v2);
        assert_eq!(held.rgba(), &[3; 12]);
    }

    /// A frame a draw still holds when the slot takes another is kept, not freed by the
    /// draw letting go of it: the next write frees it (R-RT-003).
    #[test]
    fn a_replaced_frame_a_draw_holds_is_freed_by_a_later_write() {
        let first = frame(1, 1, 1);
        let slot = FrameSlot::new(Arc::clone(&first)).expect("packed");
        let drawing = slot.latest();
        assert_eq!(slot.store(frame(1, 1, 2)), Stored::New);
        drop(drawing);
        assert_eq!(
            Arc::strong_count(&first),
            2,
            "the slot still keeps it, retired"
        );
        assert_eq!(slot.store(frame(1, 1, 3)), Stored::New);
        assert_eq!(Arc::strong_count(&first), 1, "freed by the write");
    }
}
