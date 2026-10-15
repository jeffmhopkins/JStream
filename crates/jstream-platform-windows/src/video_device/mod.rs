//! Webcams and capture cards through Media Foundation's source reader: the
//! [`VideoDevices`](jstream_platform::VideoDevices) backend, and the MJPEG decode hook
//! (`MfMjpegDecoder`) the engine passes to it.
//!
//! Devices are listed by `source`, their native media types read as formats by `format`,
//! and an open device runs on its own capture thread (`stream`) that delivers frames to
//! the same single-slot mailbox display capture uses. Raw frames are copied or converted
//! to NV12 there (`convert`); MJPEG frames go to the engine's decoder. The paired audio
//! endpoint is found by container ID (`pairing`).

// Platform-independent, so they are tested on every platform.
#[cfg(any(windows, test))]
mod convert;
#[cfg(any(windows, test))]
mod format;
#[cfg(any(windows, test))]
mod pairing;
// The capture mailbox, shared with display capture by path so this module needs no
// change in `capture` (another work item's files). Its own tests run with it.
#[cfg(windows)]
#[allow(
    clippy::duplicate_mod,
    reason = "the capture mailbox, reused without editing `capture`; to become one shared \
              module when that is next changed (#185)"
)]
#[path = "../capture/mailbox.rs"]
mod mailbox;
#[cfg(windows)]
mod mjpeg;
#[cfg(windows)]
mod source;
#[cfg(windows)]
mod stream;

#[cfg(windows)]
pub use backend::MfVideoDevices;
#[cfg(windows)]
pub use mjpeg::MfMjpegDecoder;

#[cfg(windows)]
mod backend {
    use jstream_platform::media::PixelFormat;
    use jstream_platform::{
        CaptureDecoder, CaptureEncoding, CaptureStream, DeviceId, PlatformError, VideoDeviceFormat,
        VideoDeviceInfo, VideoDevices,
    };
    use windows::Win32::Media::MediaFoundation::MF_SOURCE_READER_ALL_STREAMS;

    use super::convert::Layout;
    use super::format::color;
    use super::pairing::paired;
    use super::source::{
        self, MfStarted, VIDEO_STREAM, audio_inputs, container_id, start_error, type_info,
    };
    use super::stream::{Capture, Deliver, MfStream};
    use crate::com::{Com, OsResult};

    /// Video capture devices (webcams and capture cards) through Media Foundation.
    ///
    /// It holds no COM objects, so it is `Send + Sync`. Each call joins the calling
    /// thread to the multithreaded apartment for its duration (as `WasapiAudio` does) and
    /// starts Media Foundation for itself, and each open stream keeps it started, so it can
    /// be called from any thread that is not a single-threaded apartment (not a UI
    /// thread).
    #[derive(Debug, Default)]
    pub struct MfVideoDevices {
        _private: (),
    }

    impl MfVideoDevices {
        /// The backend. Opening nothing yet, it can't fail.
        pub fn new() -> Self {
            Self::default()
        }
    }

    impl VideoDevices for MfVideoDevices {
        /// IDs are the devices' symbolic links (device interface paths), which stay the
        /// same across reboots and replugging into the same port.
        fn devices(&self) -> Result<Vec<VideoDeviceInfo>, PlatformError> {
            let _com = Com::init()?;
            let _mf = MfStarted::start()?;
            let found = source::enumerate()?;
            let inputs = if found.is_empty() {
                Vec::new()
            } else {
                audio_inputs()
            };
            Ok(found
                .into_iter()
                .map(|f| VideoDeviceInfo {
                    audio: paired(container_id(&f.link), &inputs),
                    id: DeviceId::new(f.link),
                    name: f.name,
                })
                .collect())
        }

        fn formats(&self, device: &DeviceId) -> Result<Vec<VideoDeviceFormat>, PlatformError> {
            let _com = Com::init()?;
            let _mf = MfStarted::start()?;
            let opened = source::find(device)?.open()?;
            Ok(opened.natives()?.into_iter().map(|n| n.format).collect())
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
            let decoder = match (format.encoding.is_compressed(), decoder) {
                (true, None) => {
                    return Err(PlatformError::Unsupported {
                        what: format!("{:?} without a decoder", format.encoding),
                    });
                }
                (true, decoder) => decoder,
                (false, _) => None,
            };
            let _com = Com::init()?;
            let mf = MfStarted::start()?;
            let found = source::find(device)?;
            let opened = found.open()?;
            let types = opened.raw_types()?;
            let native = super::format::natives(types.iter().map(|(raw, _)| *raw))
                .into_iter()
                .find(|n| n.format == *format)
                .ok_or_else(unsupported)?;
            let (subtype, media_type) = types
                .into_iter()
                .find(|(raw, _)| raw.index == native.index)
                .map(|(raw, t)| (raw.subtype, t))
                .ok_or_else(unsupported)?;
            let info = type_info(&media_type);
            let width = format.size.width();
            let deliver = match (format.encoding, decoder) {
                (CaptureEncoding::Raw(PixelFormat::Nv12), _) => Deliver::Raw {
                    layout: Layout::Nv12,
                    stride: Layout::Nv12.stride(width, info.stride),
                },
                (CaptureEncoding::Raw(PixelFormat::I420), _) => Deliver::Raw {
                    layout: Layout::I420,
                    stride: Layout::I420.stride(width, info.stride),
                },
                (CaptureEncoding::Yuy2, _) => Deliver::Raw {
                    layout: Layout::Yuy2,
                    stride: Layout::Yuy2.stride(width, info.stride),
                },
                (CaptureEncoding::Mjpeg, Some(decoder)) => Deliver::Decode(decoder),
                _ => return Err(unsupported()),
            };
            // SAFETY: plain COM calls on a live reader and one of its native types. Only
            // the first video stream is read, so others (a photo stream) are deselected.
            unsafe {
                let _ = opened
                    .reader
                    .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
                opened
                    .reader
                    .SetStreamSelection(VIDEO_STREAM, true)
                    .os("IMFSourceReader::SetStreamSelection")?;
                opened
                    .reader
                    .SetCurrentMediaType(VIDEO_STREAM, None, &media_type)
                    .map_err(|e| {
                        start_error("IMFSourceReader::SetCurrentMediaType", &found.link, &e)
                    })?;
            }
            let stream = MfStream::start(Capture {
                opened,
                mf,
                deliver,
                size: format.size,
                subtype,
                color: color(info.matrix, info.range, format.size.height(), false),
                what: format!("video capture device {:?}", found.name),
                id: found.link.clone(),
            })?;
            Ok(Box::new(stream))
        }
    }
}

/// Platform tests against a real webcam or capture card (R-TEST-004): behind `hw-tests`
/// and `#[ignore]`d, because CI machines have no camera. Run them on a Windows machine
/// with one plugged in:
/// `cargo test -p jstream-platform-windows --features hw-tests video_device -- --ignored`.
#[cfg(all(test, windows, feature = "hw-tests"))]
mod device_tests {
    use std::time::{Duration, Instant};

    use jstream_platform::media::PixelFormat;
    use jstream_platform::{
        AudioDirection, AudioIo, CaptureEncoding, MediaClock, PlatformError, VideoDeviceFormat,
        VideoDevices, VideoSurface,
    };

    use super::{MfMjpegDecoder, MfVideoDevices};
    use crate::{QpcClock, WasapiAudio};

    fn first_device_formats() -> (jstream_platform::DeviceId, Vec<VideoDeviceFormat>) {
        let devices = MfVideoDevices::new();
        // A device plugged in just before the test can take a few seconds to enumerate.
        let start = Instant::now();
        let list = loop {
            let list = devices.devices().expect("devices");
            if !list.is_empty() || start.elapsed() > Duration::from_secs(15) {
                break list;
            }
            std::thread::sleep(Duration::from_millis(250));
        };
        let first = list
            .first()
            .expect("a video capture device is plugged in (Media Foundation listed none in 15 s)");
        let formats = devices.formats(&first.id).expect("formats");
        assert!(
            !formats.is_empty(),
            "{} lists no deliverable format",
            first.name
        );
        (first.id.clone(), formats)
    }

    /// Captures `format` for a second and checks the frames.
    fn capture_a_second(format: &VideoDeviceFormat, mjpeg: bool) {
        let devices = MfVideoDevices::new();
        let (id, _) = first_device_formats();
        let decoder: Option<Box<dyn jstream_platform::CaptureDecoder>> =
            mjpeg.then(|| Box::new(MfMjpegDecoder::new().expect("decoder")) as _);
        let mut stream = devices.open(&id, format, decoder).expect("open");
        let clock = QpcClock::new().expect("clock");
        let start = Instant::now();
        let mut frames = 0;
        let mut last_pts = None;
        while start.elapsed() < Duration::from_secs(2) {
            if let Some(surface) = stream.latest().expect("latest") {
                let VideoSurface::Cpu(frame) = surface else {
                    panic!("a CPU frame");
                };
                let info = frame.info();
                assert_eq!(info.size, format.size);
                assert_eq!(info.format, PixelFormat::Nv12);
                let now = clock.now().ticks();
                let pts = info.pts.ticks();
                assert!(pts <= now, "pts {pts} after now {now}");
                assert!(
                    now - pts < 10_000_000,
                    "pts {pts} more than 1 s before {now}"
                );
                if let Some(last) = last_pts {
                    assert!(pts > last, "pts went from {last} to {pts}");
                }
                last_pts = Some(pts);
                frames += 1;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // At least 5 fps for 2 s, allowing for a slow start and dark-scene frame drops.
        assert!(frames >= 10, "{frames} frames in 2 s");
        assert!(stream.stats().frames_captured >= frames);
    }

    #[test]
    #[ignore = "needs a webcam or capture card"]
    fn lists_devices_formats_and_paired_audio() {
        let devices = MfVideoDevices::new();
        let list = devices.devices().expect("devices");
        assert!(!list.is_empty(), "no video capture device found");
        let audio = WasapiAudio::new().devices().expect("audio devices");
        for d in &list {
            assert!(!d.name.is_empty());
            if let Some(paired) = &d.audio {
                assert!(
                    audio
                        .iter()
                        .any(|a| &a.id == paired && a.direction == AudioDirection::Input),
                    "{} pairs with {paired:?}, which is not an audio input",
                    d.name
                );
            }
        }
        let (_, formats) = first_device_formats();
        assert!(formats.iter().all(|f| f.size.width() > 0));
    }

    #[test]
    #[ignore = "needs a webcam or capture card with an uncompressed format"]
    fn captures_an_uncompressed_format() {
        let (_, formats) = first_device_formats();
        let format = formats
            .iter()
            .find(|f| !f.encoding.is_compressed())
            .expect("an uncompressed format (NV12, I420 or YUY2)");
        capture_a_second(format, false);
    }

    #[test]
    #[ignore = "needs a webcam with an MJPEG format"]
    fn captures_mjpeg_through_the_decoder() {
        let (_, formats) = first_device_formats();
        let format = formats
            .iter()
            .filter(|f| f.encoding == CaptureEncoding::Mjpeg)
            .max_by_key(|f| f.size.width() * f.size.height())
            .expect("an MJPEG format");
        capture_a_second(format, true);
    }

    #[test]
    #[ignore = "needs a webcam or capture card"]
    fn a_second_open_is_busy_or_shared() {
        let devices = MfVideoDevices::new();
        let (id, formats) = first_device_formats();
        let format = formats
            .iter()
            .find(|f| !f.encoding.is_compressed())
            .unwrap_or(&formats[0]);
        let decoder = || -> Option<Box<dyn jstream_platform::CaptureDecoder>> {
            format
                .encoding
                .is_compressed()
                .then(|| Box::new(MfMjpegDecoder::new().expect("decoder")) as _)
        };
        let _first = devices.open(&id, format, decoder()).expect("first open");
        match devices.open(&id, format, decoder()) {
            Ok(_) | Err(PlatformError::Busy { .. }) => {}
            Err(e) => panic!("a second open should share the device or be Busy: {e}"),
        }
    }

    #[test]
    #[ignore = "needs a webcam or capture card"]
    fn refuses_unknown_devices_and_formats() {
        let devices = MfVideoDevices::new();
        let unknown = jstream_platform::DeviceId::new("\\\\?\\no-such-device");
        assert!(matches!(
            devices.formats(&unknown),
            Err(PlatformError::NotFound { .. })
        ));
        let (id, formats) = first_device_formats();
        let odd = VideoDeviceFormat {
            size: jstream_platform::media::FrameSize::new(7, 7).expect("size"),
            ..formats[0]
        };
        assert!(matches!(
            devices.open(&id, &odd, None),
            Err(PlatformError::Unsupported { .. })
        ));
        if let Some(mjpeg) = formats.iter().find(|f| f.encoding.is_compressed()) {
            assert!(matches!(
                devices.open(&id, mjpeg, None),
                Err(PlatformError::Unsupported { .. })
            ));
        }
    }

    /// Interactive, so it does nothing unless `JSTREAM_UNPLUG_TEST` is set. Run it on its
    /// own, then unplug the device within 30 s:
    /// `$env:JSTREAM_UNPLUG_TEST=1; cargo test -p jstream-platform-windows --features
    /// hw-tests unplugging -- --ignored`.
    #[test]
    #[ignore = "needs someone to unplug a USB webcam during the test"]
    fn unplugging_ends_the_stream_with_device_lost() {
        if std::env::var_os("JSTREAM_UNPLUG_TEST").is_none() {
            return;
        }
        let devices = MfVideoDevices::new();
        let (id, formats) = first_device_formats();
        let format = formats
            .iter()
            .find(|f| !f.encoding.is_compressed())
            .expect("an uncompressed format");
        let mut stream = devices.open(&id, format, None).expect("open");
        let start = Instant::now();
        loop {
            match stream.latest() {
                Err(PlatformError::DeviceLost { .. }) => break,
                Err(e) => panic!("expected DeviceLost, got {e}"),
                Ok(_) => {}
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "not unplugged in 30 s"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(matches!(
            stream.latest(),
            Err(PlatformError::DeviceLost { .. })
        ));
        assert!(
            devices
                .devices()
                .expect("devices")
                .iter()
                .all(|d| d.id != id)
        );
    }
}
