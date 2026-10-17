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
//! engine's decoder. An open stream maps four driver buffers and runs its own capture
//! thread (`stream`).

#[cfg(any(target_os = "linux", test))]
mod abi;
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
mod device;
#[cfg(any(target_os = "linux", test))]
mod format;
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
mod stream;

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub use backend::V4l2Devices;

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
mod backend {
    use std::path::{Path, PathBuf};

    use jstream_platform::{
        CaptureDecoder, CaptureEncoding, CaptureStream, DeviceId, PlatformError, VideoDeviceFormat,
        VideoDeviceInfo, VideoDevices,
    };

    use super::device::Device;
    use super::format::{
        c_name, capture_type, color, formats, fourcc_for, is_device_path, node_number,
        plane_strides,
    };
    use super::stream::{Capture, Deliver, V4l2Stream};
    use crate::MonotonicClock;

    /// The directory of udev's stable links to video nodes, by the device's identity.
    const BY_ID: &str = "/dev/v4l/by-id";

    /// Video capture devices (webcams and capture cards) through V4L2.
    ///
    /// It holds nothing open: each call opens the device nodes it needs, and each stream
    /// keeps its own node open. `Send + Sync`, callable from any thread.
    #[derive(Debug, Default)]
    pub struct V4l2Devices {
        _private: (),
    }

    impl V4l2Devices {
        /// The backend. Opening nothing yet, it can't fail.
        pub fn new() -> Self {
            Self::default()
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
        /// left out. No microphone is paired yet.
        fn devices(&self) -> Result<Vec<VideoDeviceInfo>, PlatformError> {
            let mut out = Vec::new();
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
                out.push(VideoDeviceInfo {
                    id: DeviceId::new(stable_id(&node)),
                    name: if name.is_empty() {
                        path.to_owned()
                    } else {
                        name
                    },
                    audio: None,
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
            if !formats(&opened.enumerate()).contains(format) {
                return Err(unsupported());
            }
            let fourcc = fourcc_for(format, &opened.pixel_formats()).ok_or_else(unsupported)?;
            let (width, height) = (format.size.width(), format.size.height());
            let settled = opened.set_format(fourcc, width, height)?;
            let pix = settled.pix;
            if (pix.pixelformat, pix.width, pix.height) != (fourcc, width, height) {
                return Err(unsupported());
            }
            // A driver without a say over the rate captures at its own.
            opened.set_period(format.frame_period)?;
            let deliver = match (format.encoding, deliver_decoder) {
                (CaptureEncoding::Mjpeg, Some(decoder)) => Deliver::Decode(decoder),
                (encoding, _) if !encoding.is_compressed() => Deliver::Raw {
                    encoding,
                    strides: plane_strides(encoding, fourcc, width, &settled.bytesperline)
                        .ok_or_else(unsupported)?,
                },
                _ => return Err(unsupported()),
            };
            let capture = Capture {
                streaming: opened.stream()?,
                deliver,
                size: format.size,
                color: color(&pix),
            };
            Ok(Box::new(V4l2Stream::start(capture, clock)?))
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
}
