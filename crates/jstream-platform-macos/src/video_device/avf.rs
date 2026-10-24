//! The AVFoundation webcam backend: cameras from an `AVCaptureDeviceDiscoverySession`,
//! and an `AVCaptureSession` per open camera whose frames are copied out as NV12.
//!
//! Frames arrive on the stream's own serial dispatch queue, in `Output`'s delegate
//! callback, which catches panics (R-UNSAFE-005) and only touches the stream's `Shared`
//! state. A watcher thread per stream checks every [`WATCH`] that the camera is still
//! connected and the session still running.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use dispatch2::{DispatchQueue, DispatchRetained};
use jstream_platform::mailbox::Mailbox;
use jstream_platform::media::{
    ColorInfo, FrameSize, PixelFormat, Plane, VideoFrame, VideoFrameInfo,
};
use jstream_platform::{
    CaptureDecoder, CaptureStats, CaptureStream, DeviceId, PlatformError, VideoDeviceFormat,
    VideoDeviceInfo, VideoDevices, VideoSurface,
};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_av_foundation::{
    AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceDiscoverySession, AVCaptureDeviceFormat,
    AVCaptureDeviceInput, AVCaptureDevicePosition, AVCaptureDeviceType,
    AVCaptureDeviceTypeBuiltInWideAngleCamera, AVCaptureDeviceTypeContinuityCamera,
    AVCaptureDeviceTypeExternal, AVCaptureDeviceTypeMicrophone, AVCaptureOutput, AVCaptureSession,
    AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaTypeAudio,
    AVMediaTypeVideo,
};
use objc2_core_media::{
    CMSampleBuffer, CMTime, CMTimeFlags, CMVideoFormatDescriptionGetDimensions,
};
use objc2_core_video::{
    CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
    CVPixelBufferIsPlanar, kCVPixelBufferPixelFormatTypeKey,
};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSString};

use super::policy::{self, RawFormat};
use crate::annexb::copy_rows;
use crate::audio::permission::{self, Media, MicStep};
use crate::fourcc::CV_420V;
use crate::pixel_buffer::{ReadLocked, presentation_time};
use crate::{MacosVersion, check_floor};

/// A camera frame, as errors name it.
const FRAME: &str = "a camera frame";

/// How often a stream's watcher checks the camera.
const WATCH: Duration = Duration::from_millis(500);

/// The kind of device the errors name, as on every platform.
const KIND: &str = "video capture device";

/// Webcams through AVFoundation (ADR-0063 §10): built-in, Continuity and external (USB)
/// cameras.
///
/// IDs are the cameras' `uniqueID`s, which stay the same across reboots. Every format is
/// delivered as `VideoSurface::Cpu` NV12 in video range, which AVFoundation converts or
/// decodes (MJPEG) to, so no format needs a `CaptureDecoder`. `pts` is each frame's
/// presentation time on the [`MachClock`](crate::MachClock) time line.
///
/// Listing cameras and their formats needs no permission. Opening one needs the Camera
/// permission: when it isn't determined yet, the first open asks macOS to prompt for it,
/// and every open reports an error naming the System Settings pane until it is granted.
#[derive(Debug)]
pub struct AvFoundationVideoDevices {
    asked_camera: AtomicBool,
}

impl AvFoundationVideoDevices {
    /// The backend, on a release at or above ADR-0063's floor.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] below macOS 14.0.
    pub fn new() -> Result<Self, PlatformError> {
        check_floor(MacosVersion::current())?;
        Ok(Self {
            asked_camera: AtomicBool::new(false),
        })
    }

    /// Checks the Camera permission before a camera opens, asking macOS to prompt the
    /// first time it isn't determined.
    fn check_camera(&self) -> Result<(), PlatformError> {
        let status = permission::authorization_status_for(Media::Camera);
        match permission::mic_step(status, self.asked_camera.load(Ordering::SeqCst)) {
            MicStep::Proceed => Ok(()),
            MicStep::Refuse => Err(permission::denied(Media::Camera, status, false)),
            MicStep::Ask => {
                self.asked_camera.store(true, Ordering::SeqCst);
                permission::request_access_for(Media::Camera);
                Err(permission::denied(Media::Camera, status, true))
            }
        }
    }
}

/// The devices a discovery session finds for `kinds` and the media type.
fn discover(
    kinds: &[&'static AVCaptureDeviceType],
    video: bool,
) -> Retained<NSArray<AVCaptureDevice>> {
    let kinds = NSArray::from_slice(kinds);
    // SAFETY: AVFoundation's exported media-type constants, read only.
    let media = unsafe {
        if video {
            AVMediaTypeVideo
        } else {
            AVMediaTypeAudio
        }
    };
    // SAFETY: a class method taking device types, a media type and a position; it only
    // reads which devices are present.
    let session = unsafe {
        AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
            &kinds,
            media,
            AVCaptureDevicePosition::Unspecified,
        )
    };
    // SAFETY: a getter on the live session.
    unsafe { session.devices() }
}

/// The cameras present now.
fn cameras() -> Retained<NSArray<AVCaptureDevice>> {
    // SAFETY: AVFoundation's exported device-type constants, read only.
    let kinds = unsafe {
        [
            AVCaptureDeviceTypeBuiltInWideAngleCamera,
            AVCaptureDeviceTypeContinuityCamera,
            AVCaptureDeviceTypeExternal,
        ]
    };
    discover(&kinds, true)
}

/// The audio capture devices present now, each as (`uniqueID`, `modelID`).
fn audio_inputs() -> Vec<(String, String)> {
    // SAFETY: as above.
    let kinds = unsafe { [AVCaptureDeviceTypeMicrophone, AVCaptureDeviceTypeExternal] };
    discover(&kinds, false)
        .iter()
        // SAFETY: getters on a live device.
        .map(|d| unsafe { (d.uniqueID().to_string(), d.modelID().to_string()) })
        .collect()
}

/// The camera with `uniqueID` `id`.
fn find(id: &DeviceId) -> Result<Retained<AVCaptureDevice>, PlatformError> {
    cameras()
        .iter()
        // SAFETY: a getter on a live device.
        .find(|d| unsafe { d.uniqueID() }.to_string() == id.as_str())
        .ok_or_else(|| PlatformError::NotFound {
            kind: KIND,
            id: id.as_str().to_owned(),
        })
}

/// A camera's formats, as read, beside the formats themselves.
fn raw_formats(device: &AVCaptureDevice) -> Vec<(RawFormat, Retained<AVCaptureDeviceFormat>)> {
    // SAFETY: a getter on a live device.
    unsafe { device.formats() }
        .iter()
        .map(|format| {
            // SAFETY: getters on a live format; a video device's format description is a
            // video format description, which the dimensions call takes.
            let (dims, ranges) = unsafe {
                (
                    CMVideoFormatDescriptionGetDimensions(&format.formatDescription()),
                    format.videoSupportedFrameRateRanges(),
                )
            };
            let min_durations = ranges
                .iter()
                .map(|r| {
                    // SAFETY: a getter on a live range.
                    let t = unsafe { r.minFrameDuration() };
                    (t.value, t.timescale)
                })
                .collect();
            (
                RawFormat {
                    width: dims.width,
                    height: dims.height,
                    min_durations,
                },
                format,
            )
        })
        .collect()
}

fn ns_error(call: &'static str, error: &NSError) -> PlatformError {
    PlatformError::Os {
        call,
        code: u32::try_from(error.code()).ok(),
        message: error.localizedDescription().to_string(),
    }
}

impl VideoDevices for AvFoundationVideoDevices {
    fn devices(&self) -> Result<Vec<VideoDeviceInfo>, PlatformError> {
        let found = cameras();
        let audio = if found.is_empty() {
            Vec::new()
        } else {
            audio_inputs()
        };
        Ok(found
            .iter()
            .map(|d| {
                // SAFETY: getters on a live device.
                let (id, name, model) = unsafe {
                    (
                        d.uniqueID().to_string(),
                        d.localizedName().to_string(),
                        d.modelID().to_string(),
                    )
                };
                VideoDeviceInfo {
                    audio: policy::paired(&model, &audio),
                    id: DeviceId::new(id),
                    name,
                }
            })
            .collect())
    }

    fn formats(&self, device: &DeviceId) -> Result<Vec<VideoDeviceFormat>, PlatformError> {
        let camera = find(device)?;
        let raw: Vec<RawFormat> = raw_formats(&camera).into_iter().map(|(r, _)| r).collect();
        Ok(policy::formats(&raw))
    }

    /// `decoder` is never needed: no listed format is compressed.
    fn open(
        &self,
        device: &DeviceId,
        format: &VideoDeviceFormat,
        _decoder: Option<Box<dyn CaptureDecoder>>,
    ) -> Result<Box<dyn CaptureStream>, PlatformError> {
        let camera = find(device)?;
        let formats = raw_formats(&camera);
        let raw: Vec<RawFormat> = formats.iter().map(|(r, _)| r.clone()).collect();
        let index = policy::find(&raw, format).ok_or_else(|| PlatformError::Unsupported {
            what: format!("{format:?} on video capture device {:?}", device.as_str()),
        })?;
        self.check_camera()?;
        // SAFETY: a getter on a live device.
        let name = unsafe { camera.localizedName() }.to_string();
        let stream = AvfStream::start(
            camera,
            &formats[index].1,
            format,
            format!("video capture device {name:?}"),
        )?;
        Ok(Box::new(stream))
    }
}

/// What a stream's callback and watcher share with the [`CaptureStream`] that reads it.
#[derive(Debug)]
struct Shared {
    mailbox: Mailbox<VideoSurface>,
    captured: AtomicU64,
    skipped: AtomicU64,
    /// The first failure, after which the stream delivers nothing more.
    failure: OnceLock<PlatformError>,
    /// The size frames are delivered at: the open format's.
    size: FrameSize,
    color: ColorInfo,
    /// The camera, for messages.
    what: String,
}

impl Shared {
    fn fail(&self, e: PlatformError) {
        let _ = self.failure.set(e);
    }

    fn lose(&self, reason: &str) {
        self.fail(PlatformError::DeviceLost {
            what: self.what.clone(),
            reason: reason.to_owned(),
        });
    }
}

/// `Output`'s state.
struct Ivars {
    shared: Arc<Shared>,
    /// A frame the reader never took, reused for the next one. Only the stream's serial
    /// queue locks it, so the lock is never contended.
    spare: Mutex<Option<VideoFrame>>,
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements, and `Output` doesn't implement
    // `Drop`. Its method matches the protocol's selector and types.
    #[unsafe(super(NSObject))]
    #[name = "JStreamCameraOutput"]
    #[ivars = Ivars]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for Output {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn did_output(
            &self,
            _output: &AVCaptureOutput,
            sample: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            let ivars = self.ivars();
            if catch_unwind(AssertUnwindSafe(|| ivars.on_sample(sample))).is_err() {
                ivars.shared.lose("the capture callback panicked");
            }
        }
    }
);

impl Output {
    fn new(shared: Arc<Shared>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars {
            shared,
            spare: Mutex::new(None),
        });
        // SAFETY: `NSObject`'s `init`, once, on the fresh allocation.
        unsafe { msg_send![super(this), init] }
    }
}

impl Ivars {
    fn on_sample(&self, sample: &CMSampleBuffer) {
        if self.shared.failure.get().is_some() {
            return;
        }
        match self.convert(sample) {
            Ok(Some(frame)) => self.publish(frame),
            Ok(None) => {}
            Err(e) => self.shared.fail(e),
        }
    }

    /// The sample's NV12 frame, copied out, or `None` for a sample without one.
    fn convert(&self, sample: &CMSampleBuffer) -> Result<Option<VideoFrame>, PlatformError> {
        // SAFETY: the sample is live for the callback; the image buffer comes back with
        // its own reference.
        let Some(image) = (unsafe { sample.image_buffer() }) else {
            return Ok(None);
        };
        // SAFETY: the sample is live for the callback.
        let pts = presentation_time(unsafe { sample.presentation_time_stamp() }, FRAME)?;
        self.shared.captured.fetch_add(1, Ordering::Relaxed);
        if CVPixelBufferGetPixelFormatType(&image) != CV_420V || !CVPixelBufferIsPlanar(&image) {
            return Err(PlatformError::Unsupported {
                what: "a camera frame that isn't the NV12 the session asked for".into(),
            });
        }
        let size = self.shared.size;
        if CVPixelBufferGetWidth(&image) != size.width() as usize
            || CVPixelBufferGetHeight(&image) != size.height() as usize
        {
            // A frame of another size, as a format change settles: skip it.
            return Ok(None);
        }
        let info = VideoFrameInfo {
            size,
            format: PixelFormat::Nv12,
            color: self.shared.color,
            pts,
        };
        let mut planes = self.planes(info)?;
        let locked = ReadLocked::lock(&image, FRAME)?;
        for (index, plane) in planes.iter_mut().enumerate() {
            let Some(geometry) = info.plane(index)? else {
                break;
            };
            let rows = usize::try_from(geometry.height).unwrap_or(usize::MAX);
            let (src, src_stride) = locked.plane(index, rows)?;
            let stride = plane.stride();
            copy_rows(
                src,
                src_stride,
                plane.data_mut(),
                stride,
                geometry.row_bytes,
                rows,
            )?;
        }
        drop(locked);
        Ok(Some(VideoFrame::new(info, planes)?))
    }

    /// The planes to copy into: the spare frame's, else new ones.
    fn planes(&self, info: VideoFrameInfo) -> Result<Vec<Plane>, PlatformError> {
        let spare = self.spare.lock().ok().and_then(|mut s| s.take());
        Ok(match spare {
            Some(f) if f.info().size == info.size => f.into_planes(),
            _ => VideoFrame::zeroed(info)?.into_planes(),
        })
    }

    fn publish(&self, frame: VideoFrame) {
        if let Some(old) = self.shared.mailbox.put(VideoSurface::Cpu(frame)) {
            self.shared.skipped.fetch_add(1, Ordering::Relaxed);
            if let (VideoSurface::Cpu(old), Ok(mut spare)) = (old, self.spare.lock()) {
                *spare = Some(old);
            }
        }
    }
}

/// The camera and its session, for the watcher thread.
struct Watched {
    camera: Retained<AVCaptureDevice>,
    session: Retained<AVCaptureSession>,
}

// SAFETY: the watcher only reads `isConnected` and `isRunning`, and the objects are
// reference counted atomically. AVFoundation documents both properties as key-value
// observable, but doesn't say they can be read from any thread. That is assumed here,
// and unverified until the hardware run (#609).
unsafe impl Send for Watched {}

/// Checks the camera every [`WATCH`] until told to stop or it is lost.
fn watch(watched: &Watched, shared: &Shared, stop: &mpsc::Receiver<()>) {
    loop {
        match stop.recv_timeout(WATCH) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        // SAFETY: property getters on live objects (see `Watched`).
        let (connected, running) =
            unsafe { (watched.camera.isConnected(), watched.session.isRunning()) };
        if !connected {
            shared.lose("the camera was disconnected");
            return;
        }
        if !running {
            shared.lose("the capture session stopped");
            return;
        }
    }
}

/// A running camera. Dropping it stops the session.
struct AvfStream {
    session: Retained<AVCaptureSession>,
    output: Retained<AVCaptureVideoDataOutput>,
    /// The output's delegate, which it holds weakly, so the stream keeps it alive.
    _delegate: Retained<Output>,
    /// The serial queue the delegate's callbacks run on.
    queue: DispatchRetained<DispatchQueue>,
    shared: Arc<Shared>,
    stop: Sender<()>,
    watcher: Option<JoinHandle<()>>,
}

// SAFETY: the session, output, delegate and queue are touched after `start` only by
// `Drop`, on whichever thread drops this. `stopRunning` and the delegate setter may be
// called from any thread, dispatch queues are thread-safe, and the delegate's state is
// `Shared` (atomics, a lock-free mailbox and a `OnceLock`) and a mutex.
unsafe impl Send for AvfStream {}

impl AvfStream {
    fn start(
        camera: Retained<AVCaptureDevice>,
        active: &AVCaptureDeviceFormat,
        format: &VideoDeviceFormat,
        what: String,
    ) -> Result<Self, PlatformError> {
        let shared = Arc::new(Shared {
            mailbox: Mailbox::new(),
            captured: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            failure: OnceLock::new(),
            size: format.size,
            color: policy::color(format.size.height()),
            what,
        });
        // SAFETY: a class method making an input for a live device.
        let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&camera) }
            .map_err(|e| ns_error("AVCaptureDeviceInput deviceInputWithDevice", &e))?;
        // SAFETY: a fresh session and output, configured once below.
        let (session, output) =
            unsafe { (AVCaptureSession::new(), AVCaptureVideoDataOutput::new()) };
        let delegate = Output::new(Arc::clone(&shared));
        let queue = DispatchQueue::new("org.jstream.camera", None);
        // `kCVPixelBufferPixelFormatTypeKey` is a `CFString`, toll-free bridged to an
        // `NSString`.
        // SAFETY: a Core Video constant, set when the framework loads and never written.
        let key =
            unsafe { &*std::ptr::from_ref(kCVPixelBufferPixelFormatTypeKey).cast::<NSString>() };
        let nv12 = NSNumber::new_u32(CV_420V);
        let value: &AnyObject = nv12.as_ref();
        let settings = NSDictionary::<NSString, AnyObject>::from_slices(&[key], &[value]);
        let sink: &ProtocolObject<dyn AVCaptureVideoDataOutputSampleBufferDelegate> =
            ProtocolObject::from_ref(&*delegate);
        let refused = |what: &str| PlatformError::Os {
            call: "AVCaptureSession",
            code: None,
            message: format!("the capture session refused the camera's {what}"),
        };
        // SAFETY: configuring the fresh session, input and output on this thread before
        // the session starts. The delegate is `delegate` and the queue `queue`, a serial
        // queue, both kept alive by the stream for as long as the output.
        unsafe {
            session.beginConfiguration();
            if !session.canAddInput(&input) {
                session.commitConfiguration();
                return Err(refused("input"));
            }
            session.addInput(&input);
            output.setVideoSettings(Some(&settings));
            output.setAlwaysDiscardsLateVideoFrames(true);
            output.setSampleBufferDelegate_queue(Some(sink), Some(&queue));
            if !session.canAddOutput(&output) {
                session.commitConfiguration();
                return Err(refused("output"));
            }
            session.addOutput(&output);
        }
        let period = CMTime {
            value: i64::from(format.frame_period.num()),
            timescale: i32::try_from(format.frame_period.den()).unwrap_or(i32::MAX),
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        // SAFETY: the device is live; the format is one of its own, and the period is one
        // of that format's frame-rate ranges' (`policy::find`). The lock is held from
        // setting the format until the session runs, so the session keeps it.
        unsafe {
            if let Err(e) = camera.lockForConfiguration() {
                session.commitConfiguration();
                return Err(ns_error("AVCaptureDevice lockForConfiguration", &e));
            }
            camera.setActiveFormat(active);
            camera.setActiveVideoMinFrameDuration(period);
            camera.setActiveVideoMaxFrameDuration(period);
            session.commitConfiguration();
            session.startRunning();
            camera.unlockForConfiguration();
        }
        let (stop, rx) = mpsc::channel();
        let watched = Watched {
            camera,
            session: session.clone(),
        };
        let watch_shared = Arc::clone(&shared);
        let watcher = thread::Builder::new()
            .name("jstream-camera-watch".to_owned())
            .spawn(move || {
                let run = AssertUnwindSafe(|| watch(&watched, &watch_shared, &rx));
                if catch_unwind(run).is_err() {
                    watch_shared.lose("the camera watcher panicked");
                }
            })
            .map_err(|e| PlatformError::Os {
                call: "std::thread::Builder::spawn",
                code: e.raw_os_error().map(i32::cast_unsigned),
                message: e.to_string(),
            });
        let mut this = Self {
            session,
            output,
            _delegate: delegate,
            queue,
            shared,
            stop,
            watcher: None,
        };
        // On failure, dropping `this` stops the session.
        this.watcher = Some(watcher?);
        Ok(this)
    }
}

impl CaptureStream for AvfStream {
    /// Never blocks: a swap of the mailbox's pointer, and a read of the failure cell.
    fn latest(&mut self) -> Result<Option<VideoSurface>, PlatformError> {
        if let Some(e) = self.shared.failure.get() {
            return Err(e.clone());
        }
        Ok(self.shared.mailbox.take())
    }

    fn stats(&self) -> CaptureStats {
        CaptureStats {
            frames_captured: self.shared.captured.load(Ordering::Relaxed),
            frames_skipped: self.shared.skipped.load(Ordering::Relaxed),
            frames_undecodable: 0,
        }
    }
}

impl Drop for AvfStream {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(t) = self.watcher.take() {
            // The watcher catches panics itself, so joining only waits for it to end.
            let _ = t.join();
        }
        // SAFETY: stops the live session (synchronously) and detaches the delegate this
        // value set.
        unsafe {
            self.session.stopRunning();
            self.output.setSampleBufferDelegate_queue(None, None);
        }
        // Wait for any callback already queued, so none runs after the delegate is
        // released.
        self.queue.exec_sync(|| {});
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::VideoDevices;

    use super::AvFoundationVideoDevices;

    #[test]
    fn lists_cameras_and_their_formats_without_asking_for_anything() {
        let backend = AvFoundationVideoDevices::new().expect("backend");
        // A CI Mac has no camera; one that does must list formats for each.
        for camera in backend.devices().expect("devices") {
            let formats = backend.formats(&camera.id).expect("formats");
            assert!(!formats.is_empty(), "{camera:?} has no formats");
        }
    }

    #[test]
    fn an_unknown_camera_is_not_found() {
        let backend = AvFoundationVideoDevices::new().expect("backend");
        let id = jstream_platform::DeviceId::new("no-such-jstream-camera");
        assert!(matches!(
            backend.formats(&id),
            Err(jstream_platform::PlatformError::NotFound { .. })
        ));
    }
}

/// Against a real camera: needs one and the Camera permission for the app running the
/// tests, so it runs by hand on a Mac:
/// `cargo test -p jstream-platform-macos --features hw-tests -- --ignored`.
#[cfg(all(test, feature = "hw-tests"))]
mod hw_tests {
    use std::time::{Duration, Instant};

    use jstream_platform::media::PixelFormat;
    use jstream_platform::{CaptureStream, VideoDevices, VideoSurface};

    use super::AvFoundationVideoDevices;

    #[test]
    #[ignore = "needs a camera and the Camera permission"]
    fn captures_nv12_frames_in_the_first_format_of_the_first_camera() {
        let backend = AvFoundationVideoDevices::new().expect("backend");
        let camera = backend
            .devices()
            .expect("devices")
            .into_iter()
            .next()
            .expect("a camera");
        let format = backend.formats(&camera.id).expect("formats")[0];
        let mut stream: Box<dyn CaptureStream> =
            backend.open(&camera.id, &format, None).expect("open");
        let start = Instant::now();
        let mut frames = 0;
        let mut last = None;
        while start.elapsed() < Duration::from_secs(3) {
            if let Some(VideoSurface::Cpu(frame)) = stream.latest().expect("latest") {
                assert_eq!(frame.info().format, PixelFormat::Nv12);
                assert_eq!(frame.info().size, format.size);
                let pts = frame.info().pts;
                assert!(last.is_none_or(|l| pts > l), "{pts:?} after {last:?}");
                last = Some(pts);
                frames += 1;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(frames > 10, "{frames} frames in 3 s of {format:?}");
        assert!(stream.stats().frames_captured >= frames);
    }
}
