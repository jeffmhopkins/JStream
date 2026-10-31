//! One open per webcam or capture card, shared by every capture of it: the program
//! stage's and the preview thread's ([`SharedVideoDevices`]).
//!
//! Most video devices open once: a second open of a device already capturing fails, or
//! steals it. The engine wraps its video-device backend in [`SharedVideoDevices`], so an
//! open of a device already open returns another reader of the same stream instead of
//! opening it again, whatever format it asks for. The device closes when its last reader
//! drops, and a lost device is opened afresh by the next open.
//!
//! Each reader gets every frame the device delivers after it joined, once. The newest
//! frame waits in the device's slot until every reader has taken it, or a newer one
//! replaces it; the last reader to take a frame moves it out, the others copy it. A read
//! takes the device's lock with `try_lock` and reads no frame when another reader holds
//! it, so the render thread never waits on the preview thread (R-RT-002); the frame is
//! still there on its next tick. Copies are made outside the lock.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, TryLockError, Weak};

use jstream_platform::{
    CaptureDecoder, CaptureStats, CaptureStream, DeviceId, PlatformError, VideoDeviceFormat,
    VideoDeviceInfo, VideoDevices, VideoSurface,
};

use crate::lock;

/// A video-device backend whose devices each open once, however many captures read them.
pub(crate) struct SharedVideoDevices {
    inner: Arc<dyn VideoDevices>,
    /// The devices open now, by ID; a reader holds its device strongly.
    open: Mutex<BTreeMap<DeviceId, Weak<Device>>>,
}

impl SharedVideoDevices {
    pub(crate) fn new(inner: Arc<dyn VideoDevices>) -> Self {
        Self {
            inner,
            open: Mutex::new(BTreeMap::new()),
        }
    }
}

impl VideoDevices for SharedVideoDevices {
    fn devices(&self) -> Result<Vec<VideoDeviceInfo>, PlatformError> {
        self.inner.devices()
    }

    fn formats(&self, device: &DeviceId) -> Result<Vec<VideoDeviceFormat>, PlatformError> {
        self.inner.formats(device)
    }

    /// Another reader of `device` when it is open and not lost, in the format it was
    /// opened in (`format` and `decoder` are then unused); else opens it.
    fn open(
        &self,
        device: &DeviceId,
        format: &VideoDeviceFormat,
        decoder: Option<Box<dyn CaptureDecoder>>,
    ) -> Result<Box<dyn CaptureStream>, PlatformError> {
        let mut open = lock(&self.open);
        open.retain(|_, weak| weak.strong_count() > 0);
        if let Some(shared) = open.get(device).and_then(Weak::upgrade) {
            let state = lock(&shared.state);
            if state.lost.is_none() {
                if shared.format != *format {
                    tracing::debug!(
                        %device,
                        open = ?shared.format,
                        asked = ?format,
                        "the device is open in another format; sharing it"
                    );
                }
                let seen = state.seq;
                drop(state);
                return Ok(Box::new(Reader {
                    device: shared,
                    seen,
                    stats: CaptureStats::default(),
                }));
            }
        }
        let stream = self.inner.open(device, format, decoder)?;
        let shared = Arc::new(Device {
            format: *format,
            state: Mutex::new(State {
                stream,
                seq: 0,
                frame: None,
                lost: None,
                stats: CaptureStats::default(),
            }),
        });
        open.insert(device.clone(), Arc::downgrade(&shared));
        Ok(Box::new(Reader {
            device: shared,
            seen: 0,
            stats: CaptureStats::default(),
        }))
    }
}

/// One open device and its readers' shared slot.
struct Device {
    format: VideoDeviceFormat,
    state: Mutex<State>,
}

struct State {
    stream: Box<dyn CaptureStream>,
    /// The number of the newest frame the device delivered; 0 before the first.
    seq: u64,
    /// The newest frame, until every reader has taken it.
    frame: Option<Slot>,
    /// What the stream reported when the device was lost: every reader reports it.
    lost: Option<PlatformError>,
    stats: CaptureStats,
}

struct Slot {
    surface: Arc<VideoSurface>,
    /// The readers that haven't taken it yet.
    unread: usize,
}

/// One capture's reader of a shared device.
struct Reader {
    device: Arc<Device>,
    /// The number of the last frame this reader took.
    seen: u64,
    /// The device's counters as of this reader's last read.
    stats: CaptureStats,
}

impl CaptureStream for Reader {
    fn latest(&mut self) -> Result<Option<VideoSurface>, PlatformError> {
        let readers = Arc::strong_count(&self.device);
        let mut state = match self.device.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            // Another reader is in: the frame waits for the next call.
            Err(TryLockError::WouldBlock) => return Ok(None),
        };
        if let Some(lost) = &state.lost {
            return Err(lost.clone());
        }
        match state.stream.latest() {
            Ok(Some(surface)) => {
                state.seq += 1;
                state.frame = Some(Slot {
                    surface: Arc::new(surface),
                    unread: readers,
                });
            }
            Ok(None) => {}
            Err(err) => {
                if matches!(err, PlatformError::DeviceLost { .. }) {
                    state.frame = None;
                    state.lost = Some(err.clone());
                }
                return Err(err);
            }
        }
        state.stats = state.stream.stats();
        self.stats = state.stats;
        if self.seen == state.seq {
            return Ok(None);
        }
        self.seen = state.seq;
        let Some(slot) = state.frame.as_mut() else {
            return Ok(None);
        };
        slot.unread = slot.unread.saturating_sub(1);
        let surface = if slot.unread == 0 {
            state.frame.take().map(|slot| slot.surface)
        } else {
            Some(Arc::clone(&slot.surface))
        };
        drop(state);
        Ok(surface.map(Arc::unwrap_or_clone))
    }

    fn stats(&self) -> CaptureStats {
        self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::SharedVideoDevices;
    use jstream_media::Timebase;
    use jstream_platform::fake::{FAKE_WEBCAM, FakeClock, FakeVideoDevices};
    use jstream_platform::{DeviceId, PlatformError, VideoDevices};
    use std::sync::Arc;

    fn setup() -> (FakeClock, FakeVideoDevices, SharedVideoDevices) {
        let clock = FakeClock::new(Timebase::MICROS);
        let fake = FakeVideoDevices::new(clock.clone());
        let shared = SharedVideoDevices::new(Arc::new(fake.clone()));
        (clock, fake, shared)
    }

    #[test]
    fn two_captures_of_a_device_share_one_open() {
        let (clock, fake, shared) = setup();
        let id = DeviceId::new(FAKE_WEBCAM);
        let format = shared.formats(&id).expect("formats")[0];
        let mut first = shared.open(&id, &format, None).expect("opens");
        let mut second = shared.open(&id, &format, None).expect("shares");
        assert_eq!(fake.open_streams(), 1);

        // Each reader takes each new frame once.
        clock.advance(1);
        assert!(first.latest().expect("reads").is_some());
        assert!(first.latest().expect("reads").is_none());
        assert!(second.latest().expect("reads").is_some());
        assert!(second.latest().expect("reads").is_none());
        clock.advance(1);
        assert!(second.latest().expect("reads").is_some());
        assert!(first.latest().expect("reads").is_some());

        // The device closes with its last reader.
        drop(first);
        assert_eq!(fake.open_streams(), 1);
        clock.advance(1);
        assert!(second.latest().expect("reads").is_some());
        drop(second);
        assert_eq!(fake.open_streams(), 0);
    }

    #[test]
    fn a_lost_device_fails_every_reader_and_opens_afresh() {
        let (clock, fake, shared) = setup();
        let id = DeviceId::new(FAKE_WEBCAM);
        let format = shared.formats(&id).expect("formats")[0];
        let mut first = shared.open(&id, &format, None).expect("opens");
        let mut second = shared.open(&id, &format, None).expect("shares");
        fake.unplug(&id);
        clock.advance(1);
        let lost = |result| matches!(result, Err(PlatformError::DeviceLost { .. }));
        assert!(lost(first.latest()));
        assert!(lost(second.latest()));
        // Lost for good: the next open opens it again, alongside the lost stream.
        assert!(shared.open(&id, &format, None).is_err(), "unplugged");
    }
}
