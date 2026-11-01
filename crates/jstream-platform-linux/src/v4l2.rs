//! Webcams and capture cards through V4L2, the kernel's video capture API (ADR-0060 §9's
//! "plain V4L2 backend"): the [`VideoDevices`](jstream_platform::VideoDevices) backend.
//!
//! There is no library: the device nodes (`/dev/videoN`) are opened and driven with
//! `ioctl`s (`abi`, declared by hand from `<linux/videodev2.h>`, and `device`). A node is
//! listed when it captures video with streaming I/O, through the single-planar API or the
//! multi-planar one (as many SoC cameras and HDMI capture bridges do). Its formats are the
//! sizes and frame intervals the driver enumerates for the pixel formats the backend
//! delivers (`format`): YUYV converted to NV12, NV12 and I420 (also with their planes in
//! separate buffer planes, `NV12M` and `YUV420M`) as they are, and MJPEG handed to the
//! engine's decoder, and 32-bit RGB as it is. An open stream maps four driver buffers and
//! runs its own capture thread (`stream`). Given an audio backend, a device is paired with
//! the microphone on its own hardware (`crate::pairing`).
//!
//! On the engine's GPU context ([`VideoDevices::on_context`](jstream_platform::VideoDevices::on_context)),
//! the backend opens the formats it can import there on the GPU path (`gpu`): the
//! driver's buffers exported as DMA-BUFs and copied into frame textures on the GPU, with
//! no CPU copy. It refuses the others as `Unsupported`, which the engine opens on the CPU
//! path.

#[cfg(any(target_os = "linux", test))]
mod abi;
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
mod device;
#[cfg(any(target_os = "linux", test))]
mod format;
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
mod gpu;
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
mod stream;

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub use backend::V4l2Devices;

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
mod backend {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use std::sync::atomic::Ordering;

    use jstream_platform::{
        CaptureDecoder, CaptureEncoding, CaptureStream, DeviceId, GpuContext, PlatformError,
        VideoDeviceFormat, VideoDeviceInfo, VideoDevices,
    };

    use super::device::Device;
    use super::format::{
        c_name, capture_type, color, formats, fourcc_for, gpu_layout, is_device_path, node_number,
        plane_strides,
    };
    use super::gpu::{GpuDeliver, GpuPath};
    use super::stream::{Capture, Deliver, V4l2Stream};
    use crate::pairing::sound_cards_for;
    use crate::{MonotonicClock, PairedAudio};

    /// The directory of udev's stable links to video nodes, by the device's identity.
    const BY_ID: &str = "/dev/v4l/by-id";
    /// Where sysfs is mounted, for pairing a node with its sound card.
    const SYS: &str = "/sys";

    /// Video capture devices (webcams and capture cards) through V4L2.
    ///
    /// It holds nothing open: each call opens the device nodes it needs, and each stream
    /// keeps its own node open. `Send + Sync`, callable from any thread.
    #[derive(Debug, Default)]
    pub struct V4l2Devices {
        /// The audio backend whose inputs devices are paired with, if any.
        audio: Option<Arc<dyn PairedAudio>>,
        /// The GPU context its streams deliver on, in the backend
        /// [`on_context`](VideoDevices::on_context) returns; `None` on the CPU path.
        gpu: Option<GpuPath>,
    }

    impl V4l2Devices {
        /// The backend, pairing no microphones. Opening nothing yet, it can't fail.
        pub fn new() -> Self {
            Self::default()
        }

        /// The backend, pairing each device with its microphone among `audio`'s inputs
        /// (see [`PairedAudio`]): the backend the engine records audio through, so the
        /// paired ID is one it opens. [`LinuxBackends`](crate::LinuxBackends) passes the
        /// audio backend it chose.
        pub fn with_audio(audio: Arc<dyn PairedAudio>) -> Self {
            Self {
                audio: Some(audio),
                gpu: None,
            }
        }
    }

    fn not_found(id: &DeviceId) -> PlatformError {
        PlatformError::NotFound {
            kind: "video capture device",
            id: id.as_str().to_owned(),
        }
    }

    /// The `/dev/videoN` nodes, in number order.
    fn nodes() -> Vec<PathBuf> {
        let Ok(dir) = std::fs::read_dir("/dev") else {
            return Vec::new();
        };
        let mut found: Vec<(u32, PathBuf)> = dir
            .flatten()
            .filter_map(|e| {
                let n = node_number(e.file_name().to_str()?)?;
                Some((n, e.path()))
            })
            .collect();
        found.sort();
        found.into_iter().map(|(_, p)| p).collect()
    }

    /// The node's stable ID: udev's `/dev/v4l/by-id/` link to it when there is one (it
    /// names the device, so it survives replugging and renumbering), else its path.
    fn stable_id(node: &Path) -> String {
        let link = std::fs::read_dir(BY_ID).ok().and_then(|dir| {
            dir.flatten()
                .map(|e| e.path())
                .find(|p| std::fs::canonicalize(p).is_ok_and(|t| t == node))
        });
        link.as_deref()
            .unwrap_or(node)
            .to_string_lossy()
            .into_owned()
    }

    /// Opens the capture node `id` names.
    fn open_capture(id: &DeviceId) -> Result<Device, PlatformError> {
        if !is_device_path(id.as_str()) {
            return Err(not_found(id));
        }
        let mut device = Device::open(id.as_str())?;
        let ty = capture_type(&device.capability()?).ok_or_else(|| not_found(id))?;
        device.set_type(ty);
        Ok(device)
    }

    impl VideoDevices for V4l2Devices {
        /// IDs are udev's `/dev/v4l/by-id/` links where there is one, else the node's
        /// path. Nodes that can't be opened (another user's, or without permission) are
        /// left out. With an audio backend ([`with_audio`](V4l2Devices::with_audio)), a
        /// device is paired with the backend's input on a sound card of the same
        /// hardware; when the backend can't list its inputs, nothing is paired and the
        /// devices are still listed.
        fn devices(&self) -> Result<Vec<VideoDeviceInfo>, PlatformError> {
            let mut out = Vec::new();
            let mut inputs = None;
            for node in nodes() {
                let Some(path) = node.to_str() else { continue };
                let Ok(device) = Device::open(path) else {
                    continue;
                };
                let Ok(cap) = device.capability() else {
                    continue;
                };
                if capture_type(&cap).is_none() {
                    continue;
                }
                let name = c_name(&cap.card);
                let audio = self.audio.as_ref().and_then(|audio| {
                    let cards = sound_cards_for(Path::new(SYS), node.file_name()?.to_str()?);
                    if cards.is_empty() {
                        return None;
                    }
                    // Read once, and only once a device has a card to pair.
                    inputs
                        .get_or_insert_with(|| audio.card_inputs().unwrap_or_default())
                        .input_for(&cards)
                });
                out.push(VideoDeviceInfo {
                    id: DeviceId::new(stable_id(&node)),
                    name: if name.is_empty() {
                        path.to_owned()
                    } else {
                        name
                    },
                    audio,
                });
            }
            Ok(out)
        }

        fn formats(&self, device: &DeviceId) -> Result<Vec<VideoDeviceFormat>, PlatformError> {
            Ok(formats(&open_capture(device)?.enumerate()))
        }

        fn open(
            &self,
            device: &DeviceId,
            format: &VideoDeviceFormat,
            decoder: Option<Box<dyn CaptureDecoder>>,
        ) -> Result<Box<dyn CaptureStream>, PlatformError> {
            let unsupported = || PlatformError::Unsupported {
                what: format!("{format:?} on video capture device {:?}", device.as_str()),
            };
            if let Some(gpu) = &self.gpu {
                if format.encoding.is_compressed() {
                    return Err(unsupported());
                }
                if gpu.failed.load(Ordering::Acquire) {
                    return Err(PlatformError::Unsupported {
                        what: "the GPU path, which failed earlier on this context".into(),
                    });
                }
            }
            let deliver_decoder = match (format.encoding.is_compressed(), decoder) {
                (true, None) => {
                    return Err(PlatformError::Unsupported {
                        what: format!("{:?} without a decoder", format.encoding),
                    });
                }
                (true, decoder) => decoder,
                (false, _) => None,
            };
            let clock = MonotonicClock::new()?;
            let opened = open_capture(device)?;
            let fourcc = fourcc_for(format, &opened.enumerate()).ok_or_else(unsupported)?;
            let layout = match &self.gpu {
                Some(gpu) => Some(
                    gpu_layout(fourcc)
                        .filter(|&layout| gpu.imports(layout))
                        .ok_or_else(unsupported)?,
                ),
                None => None,
            };
            let (width, height) = (format.size.width(), format.size.height());
            let settled = opened.set_format(fourcc, width, height)?;
            let pix = settled.pix;
            if (pix.pixelformat, pix.width, pix.height) != (fourcc, width, height) {
                return Err(unsupported());
            }
            // A driver without a say over the rate captures at its own.
            opened.set_period(format.frame_period)?;
            let strides = || {
                plane_strides(format.encoding, fourcc, width, &settled.bytesperline)
                    .ok_or_else(unsupported)
            };
            let deliver = match (format.encoding, deliver_decoder) {
                // Made once the buffers exist, below.
                _ if layout.is_some() => None,
                (CaptureEncoding::Mjpeg, Some(decoder)) => Some(Deliver::Decode(decoder)),
                (encoding, _) if !encoding.is_compressed() => Some(Deliver::Raw {
                    encoding,
                    strides: strides()?,
                }),
                _ => return Err(unsupported()),
            };
            let streaming = opened.stream()?;
            let deliver = match (deliver, layout, &self.gpu) {
                (Some(deliver), _, _) => deliver,
                (None, Some(layout), Some(gpu)) => Deliver::Gpu(GpuDeliver::import(
                    gpu,
                    &streaming,
                    layout,
                    (width, height),
                    color(&pix),
                    &strides()?,
                )?),
                _ => return Err(unsupported()),
            };
            let capture = Capture {
                streaming,
                deliver,
                size: format.size,
                color: color(&pix),
            };
            Ok(Box::new(V4l2Stream::start(capture, clock)?))
        }

        /// This backend on `context`'s device, when [`LinuxGpu`](crate::LinuxGpu)
        /// opened it with DMA-BUF import of some format V4L2 buffers come in;
        /// `Ok(None)` when it imports none. It opens a format there when its buffers
        /// import (32-bit RGB, YUYV, NV12 and NV12M), and is `Unsupported` for the others
        /// (I420, MJPEG), and for every
        /// format once a stream's GPU copy failed on that context.
        fn on_context(
            &self,
            context: &GpuContext,
        ) -> Result<Option<Arc<dyn VideoDevices>>, PlatformError> {
            let shared = crate::gpu::Shared::of(context, "V4l2Devices::on_context")?;
            let Some(gpu) = GpuPath::new(shared) else {
                return Ok(None);
            };
            Ok(Some(Arc::new(Self {
                audio: self.audio.clone(),
                gpu: Some(gpu),
            })))
        }
    }
}

/// The `ioctl` path without a camera: no node is opened that could be one.
#[cfg(all(test, target_os = "linux", target_pointer_width = "64"))]
mod tests {
    use jstream_platform::media::{FrameSize, Timebase};
    use jstream_platform::{
        CaptureEncoding, DeviceId, PlatformError, VideoDeviceFormat, VideoDevices,
    };

    use super::V4l2Devices;
    use super::device::Device;

    #[test]
    fn ids_that_arent_video_nodes_are_not_found() {
        let devices = V4l2Devices::new();
        let format = VideoDeviceFormat {
            size: FrameSize::new(640, 480).expect("size"),
            frame_period: Timebase::new(1, 30).expect("period"),
            encoding: CaptureEncoding::Yuy2,
        };
        for id in [
            "/dev/video4096",
            "/dev/null",
            "/etc/passwd",
            "/dev/v4l/by-id/../../null",
        ] {
            let id = DeviceId::new(id);
            assert!(
                matches!(devices.formats(&id), Err(PlatformError::NotFound { .. })),
                "{id:?}"
            );
            assert!(matches!(
                devices.open(&id, &format, None),
                Err(PlatformError::NotFound { .. })
            ));
        }
        // A compressed format without a decoder is refused before anything is opened.
        let mjpeg = VideoDeviceFormat {
            encoding: CaptureEncoding::Mjpeg,
            ..format
        };
        assert!(matches!(
            devices.open(&DeviceId::new("/dev/video4096"), &mjpeg, None),
            Err(PlatformError::Unsupported { .. })
        ));
    }

    /// The GPU path is offered on a context the Linux interop opened with LINEAR
    /// DMA-BUF import of a format V4L2 buffers come in (none on a software adapter), and
    /// a context another platform opened is refused.
    #[test]
    fn the_gpu_path_needs_our_context_with_linear_import() {
        use jstream_platform::{AdapterId, GpuContext, GpuInterop};
        let devices = V4l2Devices::new();
        let ours = match crate::LinuxGpu::with_software().open_device() {
            Ok(context) => context,
            Err(PlatformError::Unsupported { .. }) => return,
            Err(e) => panic!("{e}"),
        };
        let linear = crate::gpu::Shared::of(&ours, "test")
            .expect("ours")
            .linear
            .clone();
        let on_gpu = devices.on_context(&ours).expect("our context");
        assert_eq!(on_gpu.is_some(), !linear.is_empty(), "{linear:?}");
        if let Some(on_gpu) = on_gpu {
            // Compressed formats never open on the GPU path, even before a node opens.
            let format = VideoDeviceFormat {
                size: FrameSize::new(640, 480).expect("size"),
                frame_period: Timebase::new(1, 30).expect("period"),
                encoding: CaptureEncoding::Mjpeg,
            };
            assert!(matches!(
                on_gpu.open(&DeviceId::new("/dev/video4096"), &format, None),
                Err(PlatformError::Unsupported { .. })
            ));
        }
        let foreign = GpuContext::new(
            ours.adapter().clone(),
            ours.device().clone(),
            ours.queue().clone(),
            AdapterId {
                luid: None,
                vendor: 0,
            },
            None,
        );
        assert!(matches!(
            devices.on_context(&foreign),
            Err(PlatformError::InvalidState { .. })
        ));
    }

    /// A character device that isn't V4L2 refuses `VIDIOC_QUERYCAP` with an OS error,
    /// not a panic or a bogus capability.
    #[test]
    fn a_node_that_isnt_v4l2_refuses_the_query() {
        let device = Device::open("/dev/null").expect("open /dev/null");
        assert!(matches!(
            device.capability(),
            Err(PlatformError::Os {
                call: "VIDIOC_QUERYCAP",
                code: Some(_),
                ..
            })
        ));
        assert!(device.pixel_formats().is_empty());
        assert!(device.enumerate().is_empty());
    }
}

/// Platform tests against a real webcam or capture card (R-TEST-004): behind `hw-tests`
/// and `#[ignore]`d, because CI machines have no camera. Run them on a Linux machine with
/// one plugged in (or the `vivid` test driver loaded, `modprobe vivid`):
/// `cargo test -p jstream-platform-linux --features hw-tests -- --ignored v4l2`.
#[cfg(all(
    test,
    target_os = "linux",
    target_pointer_width = "64",
    feature = "hw-tests"
))]
mod device_tests {
    use std::time::{Duration, Instant};

    use jstream_platform::media::PixelFormat;
    use jstream_platform::{
        CaptureEncoding, DeviceId, MediaClock, PlatformError, VideoDeviceFormat, VideoDevices,
        VideoSurface,
    };

    use super::V4l2Devices;
    use crate::MonotonicClock;

    /// The first device with an uncompressed format, and that format at its largest.
    fn first_raw() -> (DeviceId, VideoDeviceFormat) {
        let devices = V4l2Devices::new();
        let list = devices.devices().expect("devices");
        assert!(!list.is_empty(), "no video capture device");
        for d in list {
            let formats = devices.formats(&d.id).expect("formats");
            if let Some(f) = formats
                .iter()
                .filter(|f| !f.encoding.is_compressed())
                .max_by_key(|f| f.size.width() * f.size.height())
            {
                return (d.id, *f);
            }
        }
        panic!("no device with an uncompressed format");
    }

    #[test]
    #[ignore = "needs a V4L2 webcam or capture card"]
    fn devices_list_ids_names_and_formats() {
        let devices = V4l2Devices::new();
        for d in devices.devices().expect("devices") {
            assert!(!d.id.as_str().is_empty() && !d.name.is_empty(), "{d:?}");
            let formats = devices.formats(&d.id).expect("formats");
            assert!(
                !formats.is_empty(),
                "{d:?} has no format the backend delivers"
            );
        }
        assert!(matches!(
            devices.formats(&DeviceId::new("/dev/video999")),
            Err(PlatformError::NotFound { .. })
        ));
        assert!(matches!(
            devices.formats(&DeviceId::new("/etc/passwd")),
            Err(PlatformError::NotFound { .. })
        ));
    }

    #[test]
    #[ignore = "needs a V4L2 webcam or capture card"]
    fn a_raw_format_delivers_recent_frames_of_its_size() {
        let (id, format) = first_raw();
        let clock = MonotonicClock::new().expect("clock");
        let mut stream = V4l2Devices::new().open(&id, &format, None).expect("open");
        let until = Instant::now() + Duration::from_secs(5);
        let mut frames = 0;
        while frames < 10 {
            assert!(Instant::now() < until, "timed out after {frames} frames");
            if let Some(VideoSurface::Cpu(frame)) = stream.latest().expect("latest") {
                let info = frame.info();
                assert_eq!(info.size, format.size);
                let expected = match format.encoding {
                    CaptureEncoding::Yuy2 => PixelFormat::Nv12,
                    CaptureEncoding::Raw(f) => f,
                    other => panic!("{other:?}"),
                };
                assert_eq!(info.format, expected);
                let age = clock.now().ticks() - info.pts.ticks();
                assert!((0..10_000_000).contains(&age), "a frame {age} ticks old");
                frames += 1;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(stream.stats().frames_captured >= 10);
        drop(stream);
        // The device is free again once the stream is dropped.
        drop(V4l2Devices::new().open(&id, &format, None).expect("reopen"));
    }

    /// The GPU path: on each device, the first size of each encoding the backend on the
    /// context opens (RGB, YUYV, NV12) delivers recent RGBA GPU frames of that size, staged on the context's queue, and the device is free
    /// again once the stream drops. Run it with the Vulkan validation layer on
    /// (`VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation`) to check the imports.
    #[test]
    #[ignore = "needs a V4L2 device with a format the GPU path takes, and a Vulkan driver with DMA-BUF import"]
    fn a_format_on_the_gpu_path_delivers_gpu_frames() {
        use jstream_platform::GpuInterop;
        let context = crate::LinuxGpu::new()
            .open_device()
            .expect("a Vulkan device");
        let on_gpu = V4l2Devices::new()
            .on_context(&context)
            .expect("our context")
            .expect("the device imports LINEAR DMA-BUFs");
        let clock = MonotonicClock::new().expect("clock");
        let mut opened = 0;
        for d in V4l2Devices::new().devices().expect("devices") {
            let mut done = Vec::new();
            for format in V4l2Devices::new().formats(&d.id).expect("formats") {
                if format.encoding.is_compressed() || done.contains(&format.encoding) {
                    continue;
                }
                let Ok(mut stream) = on_gpu.open(&d.id, &format, None) else {
                    continue;
                };
                let until = Instant::now() + Duration::from_secs(5);
                let mut frames = 0;
                while frames < 10 {
                    assert!(Instant::now() < until, "timed out after {frames} frames");
                    match stream.latest().expect("latest") {
                        Some(VideoSurface::Gpu(frame)) => {
                            assert_eq!(frame.info().size, format.size);
                            assert_eq!(frame.info().format, PixelFormat::Rgba8);
                            frame.stage(context.queue()).expect("the context's queue");
                            let age = clock.now().ticks() - frame.info().pts.ticks();
                            assert!((0..10_000_000).contains(&age), "a frame {age} ticks old");
                            frames += 1;
                        }
                        Some(other) => panic!("a CPU frame on the GPU path: {other:?}"),
                        None => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
                drop(stream);
                drop(
                    V4l2Devices::new()
                        .open(&d.id, &format, None)
                        .expect("reopen on the CPU path"),
                );
                done.push(format.encoding);
                opened += 1;
            }
        }
        assert!(opened > 0, "no format opened on the GPU path");
    }
}
