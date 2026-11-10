//! One open per webcam or capture card, shared by every capture of it: the program
//! stage's and the preview thread's ([`SharedVideoDevices`]).
//!
//! Most video devices open once: a second open of a device already capturing fails, or
//! steals it. The engine wraps its video-device backend in [`SharedVideoDevices`], so an
//! open of a device already open returns another reader of the same stream instead of
//! opening it again (in the format it asks, below). The device closes when its last reader
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
//! ([`VideoDevices::on_context`]): it opens a device that isn't open yet on the
//! platform's GPU path, shares one open on the GPU path, and refuses one open on the CPU
//! path (`Unsupported`), which the caller then shares through the CPU view. Only a capture on the context can read
//! GPU frames; the preview's can't. So when a capture through the CPU view joins a device
//! open on the GPU path, the device is moved to the CPU path in place: its GPU stream is
//! closed and the device opened again on the CPU backend in the same format, under the
//! device's lock, so every reader goes on reading the same device and only misses the
//! frames of the move. It stays on the CPU path until it closes.
//!
//! An open that asks another format than the one a device is open in switches the device
//! to it in place, under its lock, the same way (#1265): its stream is closed and the
//! device opened again in the asked format, on the path the asking view opens on (the
//! CPU path for a compressed format, and for a device on the CPU path, whose CPU-view
//! readers read only CPU frames), so every reader goes on reading the same device in
//! the new format and only misses the frames of the switch. A `webcam` whose format
//! setting changed opens its new format while its old capture still holds a reader, so
//! the change applies at once; of two captures asking different formats of one device,
//! the one that opened last wins. A format the device doesn't list is never switched to,
//! and the device is shared as it is. When the device doesn't open in the new format, it is
//! opened again in the old one and shared in that, and when that fails too it is lost.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, TryLockError, Weak};

use jstream_platform::{
    CameraControlId, CameraControlInfo, CameraControlValue, CaptureDecoder, CaptureStats,
    CaptureStream, DeviceId, GpuContext, PlatformError, VideoDeviceFormat, VideoDeviceInfo,
    VideoDevices, VideoSurface,
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

    /// The backend's: camera controls belong to the device, on whichever path it is
    /// open (#1196).
    fn controls(&self, device: &DeviceId) -> Result<Vec<CameraControlInfo>, PlatformError> {
        self.inner.controls(device)
    }

    /// The backend's, which applies to a running stream without reopening it, so every
    /// reader of a shared device sees it.
    fn set_control(
        &self,
        device: &DeviceId,
        control: CameraControlId,
        value: CameraControlValue,
    ) -> Result<(), PlatformError> {
        self.inner.set_control(device, control, value)
    }

    /// Another reader of `device` when it is open and not lost, after switching it to
    /// `format` when it is open in another (`decoder` is otherwise unused), or moving
    /// it to the CPU path when it is on the GPU path and this is the CPU view; else
    /// opens it, on the GPU path in the GPU view.
    ///
    /// # Errors
    /// The backend's, when it doesn't open; [`PlatformError::DeviceLost`] when a device
    /// can't be opened again on the CPU path, as every reader then reports; and
    /// [`PlatformError::Unsupported`] in the GPU view for a device open on the CPU path,
    /// which the caller then opens through the CPU view.
    fn open(
        &self,
        device: &DeviceId,
        format: &VideoDeviceFormat,
        mut decoder: Option<Box<dyn CaptureDecoder>>,
    ) -> Result<Box<dyn CaptureStream>, PlatformError> {
        let mut open = lock(&self.open);
        open.retain(|_, weak| weak.strong_count() > 0);
        if let Some(shared) = open.get(device).and_then(Weak::upgrade) {
            let mut state = lock(&shared.state);
            // A format the device doesn't list is never switched to: closing the stream
            // for an open that fails would cost every reader frames, or the device.
            let listed = || {
                self.inner
                    .formats(device)
                    .is_ok_and(|formats| formats.contains(format))
            };
            if state.lost.is_none() && state.format != *format && !listed() {
                tracing::warn!(%device, open = ?state.format, asked = ?format, "the device doesn't list the asked format; sharing it in the one it has");
            } else if state.lost.is_none() && state.format != *format {
                // Switched on the CPU path from the CPU view, for a device on the CPU
                // path, which CPU-view readers may hold and which must stay there, and
                // for a compressed format, which only the CPU path decodes.
                let to_cpu = self.gpu.is_none() || !state.on_gpu || format.encoding.is_compressed();
                let backend = match (&self.gpu, to_cpu) {
                    (Some(gpu), false) => gpu.as_ref(),
                    _ => self.inner.as_ref(),
                };
                let to = Target {
                    backend,
                    on_gpu: !to_cpu,
                };
                switch_format(
                    to,
                    self.inner.as_ref(),
                    device,
                    *format,
                    decoder.take(),
                    &mut state,
                )?;
            } else if state.lost.is_none() && state.on_gpu && self.gpu.is_none() {
                let format = state.format;
                move_to_cpu(self.inner.as_ref(), device, format, &mut state)?;
            }
            if state.lost.is_none() && !state.on_gpu && self.gpu.is_some() {
                // Its frames are CPU frames: the caller opens it through the CPU view,
                // and shares it there.
                return Err(PlatformError::Unsupported {
                    what: format!("{device} on the GPU path: it is open on the CPU path"),
                });
            }
            if state.lost.is_none() {
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
            state: Mutex::new(State {
                format: *format,
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
/// slot, a GPU frame, is dropped. A device that doesn't open again is lost
/// ([`PlatformError::DeviceLost`]), for every reader.
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
        // `DeviceLost`, which every reader reports, so each capture stops and is
        // opened afresh later (`CaptureStream::latest`'s contract).
        Err(err) => {
            let lost = PlatformError::DeviceLost {
                what: device.to_string(),
                reason: format!("it did not open again on the CPU path: {err}"),
            };
            state.lost = Some(lost.clone());
            Err(lost)
        }
    }
}

/// Where [`switch_format`] opens a device: on `backend`, which opens on the GPU path
/// when `on_gpu`.
struct Target<'b> {
    backend: &'b dyn VideoDevices,
    on_gpu: bool,
}

/// Switches a device to `format` under its lock (#1265): closes its stream, then opens it
/// on `to` in `format`. When that fails, it opens again in the format it had on `cpu`,
/// and is shared in that (logged); when that fails too, the device is lost
/// ([`PlatformError::DeviceLost`]) for every reader. Its counters carry on, and the
/// frame waiting in its slot, of the old format, is dropped.
fn switch_format(
    to: Target<'_>,
    cpu: &dyn VideoDevices,
    device: &DeviceId,
    format: VideoDeviceFormat,
    decoder: Option<Box<dyn CaptureDecoder>>,
    state: &mut State,
) -> Result<(), PlatformError> {
    tracing::info!(%device, from = ?state.format, to = ?format, "switching the open device to the format a capture asks");
    let old = state.format;
    state.carried = state.stats;
    state.frame = None;
    drop(std::mem::replace(&mut state.stream, Box::new(Closed)));
    let failed = match to.backend.open(device, &format, decoder) {
        Ok(stream) => {
            state.stream = stream;
            state.format = format;
            state.on_gpu = to.on_gpu;
            return Ok(());
        }
        Err(err) => err,
    };
    // The old format, on the CPU path. A compressed one needs a decoder this open
    // hasn't got, so the device is lost instead.
    let restored = if old.encoding.is_compressed() {
        Err(failed.clone())
    } else {
        cpu.open(device, &old, None)
    };
    match restored {
        Ok(stream) => {
            tracing::warn!(%device, asked = ?format, error = %failed, "the device did not open in the asked format; sharing it in the one it had");
            state.stream = stream;
            state.on_gpu = false;
            Ok(())
        }
        Err(err) => {
            let lost = PlatformError::DeviceLost {
                what: device.to_string(),
                reason: format!(
                    "it did not open again in {format:?} ({failed}) nor in {old:?} ({err})"
                ),
            };
            state.lost = Some(lost.clone());
            Err(lost)
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
    state: Mutex<State>,
}

struct State {
    /// The format `stream` was opened in.
    format: VideoDeviceFormat,
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
    use jstream_platform::fake::{FAKE_WEBCAM, FakeCaptureDecoder, FakeClock, FakeVideoDevices};
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

    /// An open in another format switches the device to it in place, for every reader
    /// (#1265): a webcam whose format changed shows it at once, though its old capture
    /// still reads the device.
    #[test]
    fn an_open_in_another_format_switches_the_device_for_every_reader() {
        let (clock, fake, shared) = setup();
        let id = DeviceId::new(FAKE_WEBCAM);
        let formats = shared.formats(&id).expect("formats");
        // 640x360 NV12, then 1280x720 MJPEG with its decoder.
        let (small, large) = (formats[0], formats[2]);
        assert_ne!(small.size, large.size);
        let mut old = shared.open(&id, &small, None).expect("opens");
        clock.advance(1);
        assert!(old.latest().expect("reads").is_some());
        let decoder = Box::new(FakeCaptureDecoder::new(jstream_media::PixelFormat::Bgra8));
        let mut new = shared.open(&id, &large, Some(decoder)).expect("switches");
        assert_eq!(fake.open_streams(), 1, "still one open");
        clock.advance(1);
        for reader in [&mut old, &mut new] {
            let frame = reader.latest().expect("reads").expect("a frame");
            assert_eq!(frame.info().size, large.size);
        }
        assert_eq!(old.stats().frames_captured, 2, "counters carry on");
        // A third open in the format it has now shares it as it is.
        let _third = shared.open(&id, &large, None).expect("shares");
        assert_eq!(fake.open_streams(), 1);
    }

    /// A format the device refuses leaves it open in the one it had, shared.
    #[test]
    fn a_refused_format_keeps_the_device_in_the_one_it_had() {
        let (clock, fake, shared) = setup();
        let id = DeviceId::new(FAKE_WEBCAM);
        let format = shared.formats(&id).expect("formats")[0];
        let mut first = shared.open(&id, &format, None).expect("opens");
        let odd = VideoDeviceFormat {
            size: jstream_media::FrameSize::new(7, 7).expect("size"),
            ..format
        };
        let mut second = shared.open(&id, &odd, None).expect("shares the old format");
        assert_eq!(fake.open_streams(), 1);
        clock.advance(1);
        for reader in [&mut first, &mut second] {
            let frame = reader.latest().expect("reads").expect("a frame");
            assert_eq!(frame.info().size, format.size);
        }
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

    /// A format switch asked from the GPU view of a device on the CPU path switches it on
    /// the CPU path, so the CPU view's readers keep reading CPU frames, and the GPU view's
    /// caller shares it through the CPU view (`Unsupported`), as before.
    #[test]
    fn a_gpu_view_switch_of_a_device_on_the_cpu_path_stays_on_the_cpu() {
        let (clock, fake, shared) = setup();
        let gpu = Arc::new(OnGpu {
            devices: fake.clone(),
            opens: AtomicUsize::new(0),
        });
        let on_gpu = gpu_view(&shared, &gpu);
        let id = DeviceId::new(FAKE_WEBCAM);
        let formats = shared.formats(&id).expect("formats");
        let (nv12, yuy2) = (formats[0], formats[1]);
        let mut preview = shared.open(&id, &nv12, None).expect("opens on the CPU");
        assert!(matches!(
            on_gpu.open(&id, &yuy2, None),
            Err(PlatformError::Unsupported { .. })
        ));
        assert_eq!(
            gpu.opens.load(Ordering::Relaxed),
            0,
            "never opened on the GPU"
        );
        assert_eq!(fake.open_streams(), 1);
        let mut program = shared.open(&id, &yuy2, None).expect("shares, switched");
        assert_eq!(fake.open_streams(), 1);
        clock.advance(1);
        assert!(preview.latest().expect("reads on").is_some());
        assert!(program.latest().expect("reads").is_some());
    }

    /// A format the device doesn't list leaves it as it is, even open in MJPEG, whose
    /// decoder a reopen in it would need.
    #[test]
    fn an_unlisted_format_never_closes_the_device() {
        let (clock, fake, shared) = setup();
        let id = DeviceId::new(FAKE_WEBCAM);
        let mjpeg = shared.formats(&id).expect("formats")[2];
        let decoder = Box::new(FakeCaptureDecoder::new(jstream_media::PixelFormat::Bgra8));
        let mut first = shared.open(&id, &mjpeg, Some(decoder)).expect("opens");
        let odd = VideoDeviceFormat {
            size: jstream_media::FrameSize::new(7, 7).expect("size"),
            ..mjpeg
        };
        let mut second = shared.open(&id, &odd, None).expect("shares as it is");
        assert_eq!(fake.open_streams(), 1);
        clock.advance(1);
        for reader in [&mut first, &mut second] {
            let frame = reader.latest().expect("reads").expect("a frame");
            assert_eq!(frame.info().size, mjpeg.size);
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
        // On the CPU path now: the GPU view refuses it, opening nothing, so the caller
        // shares it through the CPU view.
        let refused = |result: Result<_, PlatformError>| {
            matches!(result, Err(PlatformError::Unsupported { .. }))
        };
        assert!(refused(on_gpu.open(&id, &format, None).map(drop)));
        assert_eq!(gpu.opens.load(Ordering::Relaxed), 1);
        assert_eq!(fake.open_streams(), 1);
        drop((program, preview));
        assert_eq!(fake.open_streams(), 0);

        // Opened through the CPU view first, the GPU view refuses it likewise.
        let first = shared.open(&id, &format, None).expect("opens");
        assert!(refused(on_gpu.open(&id, &format, None).map(drop)));
        assert_eq!(
            gpu.opens.load(Ordering::Relaxed),
            1,
            "not opened on the GPU"
        );
        assert_eq!(fake.open_streams(), 1);
        drop(first);
    }

    /// A device that doesn't open again on the CPU path is `DeviceLost` for every
    /// reader, so each capture stops and opens it afresh later (B1), and the join that
    /// moved it fails the same way.
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
        let lost = |result: Result<_, PlatformError>| {
            matches!(result, Err(PlatformError::DeviceLost { .. }))
        };
        assert!(lost(shared.open(&id, &format, None).map(drop)));
        assert_eq!(fake.open_streams(), 0, "the GPU stream closed");
        assert!(lost(program.latest().map(drop)));
    }
}
