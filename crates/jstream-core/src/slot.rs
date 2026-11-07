//! Frame slots (ADR-0117 §2 and §3): a single-frame "latest frame" mailbox a live
//! source's producer writes into and every compositor reads at its tick, so a new
//! frame of the same size reaches the picture without a re-plan.
//!
//! A shot names a slot in place of a frame ([`Frame::from_slot`](crate::snapshot::Frame::from_slot)),
//! and each draw uploads whatever the slot holds then, by the slot frame's own version.
//! Only a new size needs a plan, since where the frame is placed depends on it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use jstream_media::{FrameSize, PixelFormat, VideoFrame};

/// The versions slot frames are uploaded under, apart from the ones plans number
/// frames with (counted from 1 per engine), so a cache never takes one for the other.
static VERSIONS: AtomicU64 = AtomicU64::new(1 << 62);

fn next_version() -> u64 {
    VERSIONS.fetch_add(1, Ordering::Relaxed)
}

/// The latest frame of one live source, packed `Rgba8`.
pub(crate) struct FrameSlot {
    latest: Mutex<Latest>,
}

struct Latest {
    frame: Arc<VideoFrame>,
    version: u64,
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
                latest: Mutex::new(Latest {
                    frame,
                    version: next_version(),
                }),
            })
        })
    }

    /// Hands `frame` to every compositor's next draw, if it is the slot's size and
    /// packed. Never blocks for longer than a swap.
    pub(crate) fn store(&self, frame: Arc<VideoFrame>) -> Stored {
        let mut latest = self.lock();
        if Arc::ptr_eq(&latest.frame, &frame) {
            return Stored::Same;
        }
        if frame.info().size != latest.frame.info().size || !packed(&frame) {
            return Stored::Refused;
        }
        *latest = Latest {
            frame,
            version: next_version(),
        };
        Stored::New
    }

    /// Holds `frame` whatever its size, for a plan, which places it at that size.
    /// `false` when it isn't packed `Rgba8`.
    pub(crate) fn replace(&self, frame: Arc<VideoFrame>) -> bool {
        if !packed(&frame) {
            return false;
        }
        let mut latest = self.lock();
        if !Arc::ptr_eq(&latest.frame, &frame) {
            *latest = Latest {
                frame,
                version: next_version(),
            };
        }
        true
    }

    /// The frame the slot holds now, and its version.
    pub(crate) fn latest(&self) -> (Arc<VideoFrame>, u64) {
        let latest = self.lock();
        (Arc::clone(&latest.frame), latest.version)
    }

    /// The size of the frame it holds.
    pub(crate) fn size(&self) -> FrameSize {
        self.lock().frame.info().size
    }

    fn lock(&self) -> MutexGuard<'_, Latest> {
        self.latest.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl std::fmt::Debug for FrameSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (frame, version) = self.latest();
        f.debug_struct("FrameSlot")
            .field("size", &frame.info().size)
            .field("version", &version)
            .finish()
    }
}

/// Whether `frame` is `Rgba8` with rows packed one after another, as an upload reads it.
fn packed(frame: &VideoFrame) -> bool {
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
        let (_, v1) = slot.latest();
        assert_eq!(slot.store(Arc::clone(&first)), Stored::Same);
        let second = frame(2, 1, 2);
        assert_eq!(slot.store(Arc::clone(&second)), Stored::New);
        let (held, v2) = slot.latest();
        assert!(Arc::ptr_eq(&held, &second) && v2 > v1);
        let wider = frame(3, 1, 3);
        assert_eq!(slot.store(Arc::clone(&wider)), Stored::Refused);
        assert_eq!(slot.size(), FrameSize::new(2, 1).expect("size"));
        assert!(slot.replace(Arc::clone(&wider)));
        let (held, v3) = slot.latest();
        assert!(Arc::ptr_eq(&held, &wider) && v3 > v2);
        assert_eq!(super::rgba(&held), &[3; 12]);
    }
}
