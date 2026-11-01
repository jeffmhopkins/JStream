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
//! still there on its next tick. Copies are made outside the lock. Each reader counts the
//! device's frames from when it joined, so an output that joins a webcam the preview
//! opened reports only the skips since, and a capture that stops and starts again doesn't
//! count them twice.
//!
//! The program stage's backend on the GPU context is a second view of the same devices
//! ([`VideoDevices::on_context`]): it opens a device that isn't open on the platform's GPU
//! path, and shares one that is, on either path. Only a capture on the context can read
//! GPU frames; the preview's can't. So when a capture through the CPU view joins a device
//! open on the GPU path, the device is moved to the CPU path in place: its GPU stream is
//! closed and the device opened again on the CPU backend in the same format, under the
//! device's lock, so every reader goes on reading the same device and only misses the
//! frames of the move. It stays on the CPU path until it closes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, TryLockError, Weak};

use jstream_platform::{
    CaptureDecoder, CaptureStats, CaptureStream, DeviceId, GpuContext, PlatformError,
    VideoDeviceFormat, VideoDeviceInfo, VideoDevices, VideoSurface,
};

use crate::lock;

/// The devices open now, by ID; a reader holds its device strongly. Shared by the CPU
/// view and the GPU one.
type OpenDevices = Arc<Mutex<BTreeMap<DeviceId, Weak<Device>>>>;

/// A video-device backend whose devices each open once, however many captures read them.
pub(crate) struct SharedVideoDevices {
    /// The platform's backend, on the CPU path.
    inner: Arc<dyn VideoDevices>,
    /// The platform's backend on the GPU context, in the view
    /// [`on_context`](VideoDevices::on_context) returns; `None` in the CPU view.
    gpu: Option<Arc<dyn VideoDevices>>,
    open: OpenDevices,
}

impl SharedVideoDevices {
    pub(crate) fn new(inner: Arc<dyn VideoDevices>) -> Self {
        Self {
            inner,
            gpu: None,
            open: Arc::default(),
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
    /// opened in (`format` and `decoder` are then unused), after moving it to the CPU
    /// path when it is on the GPU path and this is the CPU view; else opens it, on the
    /// GPU path in the GPU view.
    ///
    /// # Errors
    /// The backend's, when it doesn't open, and when a device can't be opened again on
    /// the CPU path; that device is then lost for every reader.
    fn open(
        &self,
        device: &DeviceId,
        format: &VideoDeviceFormat,
        decoder: Option<Box<dyn CaptureDecoder>>,
    ) -> Result<Box<dyn CaptureStream>, PlatformError> {
        let mut open = lock(&self.open);
        open.retain(|_, weak| weak.strong_count() > 0);
        if let Some(shared) = open.get(device).and_then(Weak::upgrade) {
            let mut state = lock(&shared.state);
            if state.lost.is_none() && state.on_gpu && self.gpu.is_none() {
                move_to_cpu(self.inner.as_ref(), device, shared.format, &mut state)?;
            }
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
                // Counted from now: the frames the device skipped before are not this
                // reader's.
                let base = sum(state.carried, state.stream.stats());
                drop(state);
                return Ok(Box::new(Reader {
                    device: shared,
                    seen,
                    base,
                    stats: base,
                }));
            }
        }
        let backend = self.gpu.as_ref().unwrap_or(&self.inner);
        let stream = backend.open(device, format, decoder)?;
        let base = stream.stats();
        let shared = Arc::new(Device {
            format: *format,
            state: Mutex::new(State {
                stream,
                on_gpu: self.gpu.is_some(),
                carried: CaptureStats::default(),
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
            base,
            stats: base,
        }))
    }

    /// The GPU view of these devices: the same open devices, opening on the platform's
    /// backend on `context` ([`VideoDevices::on_context`]). `None` when the platform's
    /// backend has no GPU path.
    fn on_context(
        &self,
        context: &GpuContext,
    ) -> Result<Option<Arc<dyn VideoDevices>>, PlatformError> {
        let Some(gpu) = self.inner.on_context(context)? else {
            return Ok(None);
        };
        Ok(Some(Arc::new(Self {
            inner: Arc::clone(&self.inner),
            gpu: Some(gpu),
            open: Arc::clone(&self.open),
        })))
    }
}

/// Moves a device open on the GPU path to the CPU path, under its lock: closes its GPU
/// stream (the device opens once), then opens it on `cpu` in the same `format`. Its
/// counters carry on from where the GPU stream's ended, and the frame waiting in its
/// slot, a GPU frame, is dropped. A device that doesn't open again is lost, for every
/// reader.
fn move_to_cpu(
    cpu: &dyn VideoDevices,
    device: &DeviceId,
    format: VideoDeviceFormat,
    state: &mut State,
) -> Result<(), PlatformError> {
    tracing::info!(%device, "a capture without the GPU context reads the device; moving it to the CPU path");
    state.carried = state.stats;
    state.frame = None;
    state.on_gpu = false;
    drop(std::mem::replace(&mut state.stream, Box::new(Closed)));
    match cpu.open(device, &format, None) {
        Ok(stream) => {
            state.stream = stream;
            Ok(())
        }
        Err(err) => {
            state.lost = Some(err.clone());
            Err(err)
        }
    }
}

/// The stream of a device between closing on one path and opening on the other: it
/// delivers nothing. Never read, since the device's lock is held throughout.
struct Closed;

impl CaptureStream for Closed {
    fn latest(&mut self) -> Result<Option<VideoSurface>, PlatformError> {
        Ok(None)
    }

    fn stats(&self) -> CaptureStats {
        CaptureStats::default()
    }
}

/// One open device and its readers' shared slot.
struct Device {
    format: VideoDeviceFormat,
    state: Mutex<State>,
}

struct State {
    stream: Box<dyn CaptureStream>,
    /// Whether `stream` was opened on the GPU path.
    on_gpu: bool,
    /// The counters of the streams the device had before `stream`: the GPU stream, when
    /// it moved to the CPU path.
    carried: CaptureStats,
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
    /// The device's counters when this reader joined: [`stats`](CaptureStream::stats)
    /// reports what they counted since.
    base: CaptureStats,
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
        state.stats = sum(state.carried, state.stream.stats());
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
        let (now, base) = (self.stats, self.base);
        CaptureStats {
            frames_captured: now.frames_captured.saturating_sub(base.frames_captured),
            frames_skipped: now.frames_skipped.saturating_sub(base.frames_skipped),
            frames_undecodable: now
                .frames_undecodable
                .saturating_sub(base.frames_undecodable),
        }
    }
}

fn sum(a: CaptureStats, b: CaptureStats) -> CaptureStats {
    CaptureStats {
        frames_captured: a.frames_captured.saturating_add(b.frames_captured),
        frames_skipped: a.frames_skipped.saturating_add(b.frames_skipped),
        frames_undecodable: a.frames_undecodable.saturating_add(b.frames_undecodable),
    }
}

#[cfg(test)]
mod tests {
    use super::SharedVideoDevices;
    use jstream_media::Timebase;
    use jstream_platform::fake::{FAKE_WEBCAM, FakeClock, FakeVideoDevices};
    use jstream_platform::{
        CaptureDecoder, CaptureStats, CaptureStream, DeviceId, PlatformError, VideoDeviceFormat,
        VideoDeviceInfo, VideoDevices,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    /// A reader that joins a device already capturing counts from then on, so an output
    /// that joins the preview's webcam reports only its own frames and drops (R-RT-004).
    /// (The fake skips none; every counter is subtracted alike.)
    #[test]
    fn a_reader_counts_from_when_it_joined() {
        let (clock, _fake, shared) = setup();
        let id = DeviceId::new(FAKE_WEBCAM);
        let format = shared.formats(&id).expect("formats")[0];
        let mut first = shared.open(&id, &format, None).expect("opens");
        for _ in 0..3 {
            clock.advance(1);
            assert!(first.latest().expect("reads").is_some());
        }
        assert_eq!(first.stats().frames_captured, 3);
        let mut joined = shared.open(&id, &format, None).expect("shares");
        assert_eq!(joined.stats(), CaptureStats::default());
        clock.advance(1);
        assert!(joined.latest().expect("reads").is_some());
        assert_eq!(joined.stats().frames_captured, 1);
        assert_eq!(first.stats().frames_captured, 3, "not read since");
    }

    /// A lost device fails every reader, and the next open asks the backend again rather
    /// than sharing the lost stream; here that fails, the device being gone.
    #[test]
    fn a_lost_device_fails_every_reader_and_is_not_shared() {
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
        let lost_again = fake.open_streams();
        assert!(shared.open(&id, &format, None).is_err(), "unplugged");
        assert_eq!(fake.open_streams(), lost_again, "nothing shared or opened");
    }

    /// The fake devices on the GPU path: the same devices (a device open on either path
    /// counts in `open_streams`), counting the opens asked of it.
    struct OnGpu {
        devices: FakeVideoDevices,
        opens: AtomicUsize,
    }

    impl VideoDevices for OnGpu {
        fn devices(&self) -> Result<Vec<VideoDeviceInfo>, PlatformError> {
            self.devices.devices()
        }
        fn formats(&self, device: &DeviceId) -> Result<Vec<VideoDeviceFormat>, PlatformError> {
            self.devices.formats(device)
        }
        fn open(
            &self,
            device: &DeviceId,
            format: &VideoDeviceFormat,
            decoder: Option<Box<dyn CaptureDecoder>>,
        ) -> Result<Box<dyn CaptureStream>, PlatformError> {
            self.opens.fetch_add(1, Ordering::Relaxed);
            self.devices.open(device, format, decoder)
        }
    }

    /// The CPU view `shared` and a GPU view of the same devices, opening on `gpu`.
    fn gpu_view(shared: &SharedVideoDevices, gpu: &Arc<OnGpu>) -> SharedVideoDevices {
        SharedVideoDevices {
            inner: Arc::clone(&shared.inner),
            gpu: Some(Arc::clone(gpu) as Arc<dyn VideoDevices>),
            open: Arc::clone(&shared.open),
        }
    }

    /// The GPU view opens a device on the GPU path, and shares one the CPU view opened.
    /// A capture through the CPU view joining a device on the GPU path moves it to the
    /// CPU path in place: one open at a time, the GPU reader reading on, and its counters
    /// carrying on.
    #[test]
    fn a_cpu_reader_moves_a_device_on_the_gpu_path_to_the_cpu() {
        let (clock, fake, shared) = setup();
        let gpu = Arc::new(OnGpu {
            devices: fake.clone(),
            opens: AtomicUsize::new(0),
        });
        let on_gpu = gpu_view(&shared, &gpu);
        let id = DeviceId::new(FAKE_WEBCAM);
        let format = shared.formats(&id).expect("formats")[0];

        let mut program = on_gpu.open(&id, &format, None).expect("opens on the GPU");
        assert_eq!(gpu.opens.load(Ordering::Relaxed), 1);
        for _ in 0..2 {
            clock.advance(1);
            assert!(program.latest().expect("reads").is_some());
        }
        let mut preview = shared.open(&id, &format, None).expect("moves and shares");
        assert_eq!(fake.open_streams(), 1, "closed on the GPU before reopening");
        assert_eq!(gpu.opens.load(Ordering::Relaxed), 1);
        clock.advance(1);
        assert!(program.latest().expect("reads on").is_some());
        assert!(preview.latest().expect("reads").is_some());
        assert_eq!(
            program.stats().frames_captured,
            3,
            "counted across the move"
        );
        assert_eq!(preview.stats().frames_captured, 1);
        // On the CPU path now: another GPU-view open shares it, opening nothing.
        let third = on_gpu.open(&id, &format, None).expect("shares");
        assert_eq!(gpu.opens.load(Ordering::Relaxed), 1);
        assert_eq!(fake.open_streams(), 1);
        drop((program, preview, third));
        assert_eq!(fake.open_streams(), 0);

        // Opened through the CPU view first, the GPU view shares it as it is.
        let first = shared.open(&id, &format, None).expect("opens");
        let second = on_gpu.open(&id, &format, None).expect("shares");
        assert_eq!(
            gpu.opens.load(Ordering::Relaxed),
            1,
            "not opened on the GPU"
        );
        assert_eq!(fake.open_streams(), 1);
        drop((first, second));
    }

    /// A device that doesn't open again on the CPU path is lost for every reader, and
    /// the join that moved it fails.
    #[test]
    fn a_device_that_does_not_reopen_on_the_cpu_is_lost() {
        let (_clock, fake, shared) = setup();
        let gpu = Arc::new(OnGpu {
            devices: fake.clone(),
            opens: AtomicUsize::new(0),
        });
        let id = DeviceId::new(FAKE_WEBCAM);
        let format = shared.formats(&id).expect("formats")[0];
        let mut program = gpu_view(&shared, &gpu)
            .open(&id, &format, None)
            .expect("opens on the GPU");
        fake.unplug(&id);
        assert!(shared.open(&id, &format, None).is_err());
        assert_eq!(fake.open_streams(), 0, "the GPU stream closed");
        assert!(program.latest().is_err());
    }
}
