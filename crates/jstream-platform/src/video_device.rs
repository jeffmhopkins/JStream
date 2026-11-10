//! Video capture devices: webcams and capture cards (docs/adr/0140-source-types-and-decoding-scope-v3.md).
//!
//! A device offers a list of formats (size, frame rate and how its frames are encoded).
//! Opening one in a format starts a stream with the same "latest frame" mailbox as
//! display and window capture: the backend's capture thread writes, and the render thread
//! takes through [`CaptureStream`]. Compressed formats (MJPEG) are decoded on the decoder
//! side, through a [`CaptureDecoder`] the engine passes in; the device backend never
//! decodes them itself.

use std::sync::Arc;

use jstream_media::{FrameSize, MediaTime, PixelFormat, Timebase};

use crate::{CaptureStream, DeviceId, GpuContext, PlatformError, VideoSurface};

/// A video capture device as [`VideoDevices::devices`] lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoDeviceInfo {
    /// The device's ID, to pass to [`VideoDevices::formats`] and [`VideoDevices::open`].
    pub id: DeviceId,
    /// A human-readable name.
    pub name: String,
    /// The audio input endpoint that belongs to the same hardware (a webcam's microphone,
    /// a capture card's HDMI audio), when the backend can pair them. It is an
    /// [`AudioIo`](crate::AudioIo) input device: open it with
    /// [`AudioInputTarget::Input`](crate::AudioInputTarget::Input).
    pub audio: Option<DeviceId>,
}

/// How a device encodes the frames of one format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CaptureEncoding {
    /// Uncompressed frames in an engine pixel format, delivered as they are.
    Raw(PixelFormat),
    /// Uncompressed packed 4:2:2 (YUY2, also called YUYV), which the engine has no pixel
    /// format for. The backend converts each frame to [`PixelFormat::Nv12`] before
    /// delivering it.
    Yuy2,
    /// Motion JPEG: each frame is one JPEG image. It is a video codec
    /// (docs/adr/0140-source-types-and-decoding-scope-v3.md), so the stream hands each frame to
    /// a [`CaptureDecoder`].
    Mjpeg,
}

impl CaptureEncoding {
    /// Whether frames need a [`CaptureDecoder`] before they can be delivered.
    pub fn is_compressed(self) -> bool {
        matches!(self, Self::Mjpeg)
    }

    /// The pixel format the stream delivers, or `None` for a compressed encoding, whose
    /// [`CaptureDecoder`] decides.
    pub fn delivered_format(self) -> Option<PixelFormat> {
        match self {
            Self::Raw(format) => Some(format),
            Self::Yuy2 => Some(PixelFormat::Nv12),
            Self::Mjpeg => None,
        }
    }
}

/// One format a device can capture in, as [`VideoDevices::formats`] lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VideoDeviceFormat {
    /// The frame size.
    pub size: FrameSize,
    /// The duration of one frame, as a time base: `1/30` for 30 fps, `1001/30000` for
    /// 29.97 fps (as in [`VideoEncoderConfig`](crate::VideoEncoderConfig)).
    pub frame_period: Timebase,
    /// How the device encodes the frames.
    pub encoding: CaptureEncoding,
}

/// A camera control: an image or lens setting a device lets the application change
/// while it captures (#1196). The names follow the OS APIs' common set: Windows'
/// `IAMVideoProcAmp` and `IAMCameraControl`, and V4L2's user and camera control classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum CameraControlId {
    /// Image brightness.
    Brightness,
    /// Image contrast.
    Contrast,
    /// Colour saturation.
    Saturation,
    /// Hue.
    Hue,
    /// Sharpness.
    Sharpness,
    /// Gamma.
    Gamma,
    /// Gain, the sensor's amplification.
    Gain,
    /// White balance, as a colour temperature in kelvin.
    WhiteBalance,
    /// Backlight compensation.
    BacklightCompensation,
    /// The mains frequency the device filters flicker for: 0 off, 1 50 Hz, 2 60 Hz.
    PowerLineFrequency,
    /// Exposure time, in the backend's unit: log2 seconds on Windows (-6 is 1/64 s),
    /// hundreds of microseconds on Linux.
    Exposure,
    /// Lens focus distance.
    Focus,
    /// Zoom.
    Zoom,
    /// Horizontal pan.
    Pan,
    /// Vertical tilt.
    Tilt,
    /// Iris (aperture).
    Iris,
}

impl CameraControlId {
    /// Every control, in the order the backends list them.
    pub const ALL: [Self; 16] = [
        Self::Brightness,
        Self::Contrast,
        Self::Saturation,
        Self::Hue,
        Self::Sharpness,
        Self::Gamma,
        Self::Gain,
        Self::WhiteBalance,
        Self::BacklightCompensation,
        Self::PowerLineFrequency,
        Self::Exposure,
        Self::Focus,
        Self::Zoom,
        Self::Pan,
        Self::Tilt,
        Self::Iris,
    ];

    /// The control's stable name, in snake case (`white_balance`): what a setting stores.
    pub fn name(self) -> &'static str {
        match self {
            Self::Brightness => "brightness",
            Self::Contrast => "contrast",
            Self::Saturation => "saturation",
            Self::Hue => "hue",
            Self::Sharpness => "sharpness",
            Self::Gamma => "gamma",
            Self::Gain => "gain",
            Self::WhiteBalance => "white_balance",
            Self::BacklightCompensation => "backlight_compensation",
            Self::PowerLineFrequency => "power_line_frequency",
            Self::Exposure => "exposure",
            Self::Focus => "focus",
            Self::Zoom => "zoom",
            Self::Pan => "pan",
            Self::Tilt => "tilt",
            Self::Iris => "iris",
        }
    }

    /// The control [`name`](Self::name)d `name`, if any.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }
}

/// A camera control a device offers, as [`VideoDevices::controls`] lists it. Values are
/// in the backend's own units, between `min` and `max` in steps of `step`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CameraControlInfo {
    /// Which control.
    pub id: CameraControlId,
    /// The lowest value.
    pub min: i32,
    /// The highest value.
    pub max: i32,
    /// The step between values; at least 1.
    pub step: i32,
    /// The device's default value.
    pub default: i32,
    /// The value now. While `auto` is `Some(true)` it is the last value read, which the
    /// device may be changing on its own.
    pub value: i32,
    /// Whether the device sets the control itself: `None` when it has no automatic mode,
    /// else whether that mode is on now.
    pub auto: Option<bool>,
}

/// What [`VideoDevices::set_control`] sets a control to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CameraControlValue {
    /// This value, with the device's automatic mode off. Clamped to the control's range
    /// and rounded to its step by the backend.
    Manual(i32),
    /// The device's automatic mode.
    Auto,
}

impl CameraControlInfo {
    /// `value` clamped to the range and rounded to the nearest step from `min`.
    pub fn snap(&self, value: i32) -> i32 {
        let step = i64::from(self.step.max(1));
        let min = i64::from(self.min);
        let max = i64::from(self.max.max(self.min));
        let v = i64::from(value).clamp(min, max);
        let snapped = (min + ((v - min + step / 2) / step) * step).clamp(min, max);
        // In range of i32: clamped between two i32s.
        i32::try_from(snapped).unwrap_or(self.min)
    }
}

/// One compressed frame, as a device delivered it, handed to a [`CaptureDecoder`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompressedFrame<'a> {
    /// The encoding; always a compressed one ([`CaptureEncoding::is_compressed`]).
    pub encoding: CaptureEncoding,
    /// The frame's bytes: for MJPEG, one complete JPEG image.
    pub data: &'a [u8],
    /// The size of the format the stream was opened in.
    pub size: FrameSize,
    /// When the frame was captured, on the [`MediaClock`](crate::MediaClock).
    pub pts: MediaTime,
}

/// Decodes a device's compressed frames: the decoder-side hook a stream in a compressed
/// format calls.
///
/// It runs on the backend's capture thread, once per frame, before the frame enters the
/// mailbox. The engine builds it from the platform's decoders (MJPEG is decoded by the OS
/// or GPU-vendor path, like any video codec, per
/// docs/adr/0140-source-types-and-decoding-scope-v3.md), so the device backend never names a
/// decoder.
pub trait CaptureDecoder: Send {
    /// Decodes one frame. The returned surface keeps the frame's `pts`.
    ///
    /// # Errors
    /// Any [`PlatformError`]. The stream drops that frame and carries on (devices
    /// occasionally send a corrupt JPEG), so an error never ends the stream.
    fn decode(&mut self, frame: &CompressedFrame<'_>) -> Result<VideoSurface, PlatformError>;
}

/// The video-capture-device backend: lists webcams and capture cards, their formats, and
/// opens streams on them (Media Foundation's source reader on Windows).
///
/// Shared (`Arc<dyn VideoDevices>`) and called from the control thread, so it is
/// `Send + Sync`.
pub trait VideoDevices: Send + Sync {
    /// The devices present now.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the OS refuses to enumerate them.
    fn devices(&self) -> Result<Vec<VideoDeviceInfo>, PlatformError>;

    /// The formats `device` can capture in. The backend lists only formats it can
    /// deliver, in the device's own order.
    ///
    /// # Errors
    /// [`PlatformError::NotFound`] when the device is gone, and [`PlatformError::Os`]
    /// when the OS refuses.
    fn formats(&self, device: &DeviceId) -> Result<Vec<VideoDeviceFormat>, PlatformError>;

    /// Starts capturing `device` in `format`, one of those [`formats`](Self::formats)
    /// listed. Frames arrive on the backend's capture thread and are read through the
    /// returned stream, which behaves as a display capture's does: a single-slot mailbox,
    /// `None` when no new frame has arrived, and [`PlatformError::DeviceLost`] once the
    /// device is unplugged. Dropping the stream stops the capture and drops `decoder`.
    ///
    /// `decoder` is required when the format's encoding
    /// [is compressed](CaptureEncoding::is_compressed), and ignored otherwise.
    ///
    /// # Errors
    /// [`PlatformError::NotFound`] when the device is gone,
    /// [`PlatformError::Unsupported`] when the device doesn't offer `format` or a
    /// compressed format comes without a decoder, [`PlatformError::Busy`] when another
    /// application holds the device exclusively, and [`PlatformError::Os`] when the OS
    /// refuses.
    fn open(
        &self,
        device: &DeviceId,
        format: &VideoDeviceFormat,
        decoder: Option<Box<dyn CaptureDecoder>>,
    ) -> Result<Box<dyn CaptureStream>, PlatformError>;

    /// The camera controls `device` offers, with their ranges and current values
    /// (#1196). Empty, the default, for a backend with no controls (macOS for now) and a
    /// device with none (most capture cards). It reads them whether or not the device
    /// is open.
    ///
    /// # Errors
    /// [`PlatformError::NotFound`] when the device is gone, and [`PlatformError::Os`]
    /// when the OS refuses.
    fn controls(&self, device: &DeviceId) -> Result<Vec<CameraControlInfo>, PlatformError> {
        let _ = device;
        Ok(Vec::new())
    }

    /// Sets one of `device`'s camera controls, open or not. On an open device it applies
    /// to the running stream without reopening it, so no frame is lost; on a closed one
    /// it applies for as long as the device keeps it (until it is unplugged, on most).
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when the device doesn't offer `control`, or has no
    /// automatic mode for [`CameraControlValue::Auto`] (the default, for every control),
    /// [`PlatformError::NotFound`] when the device is gone, and [`PlatformError::Os`]
    /// when the OS refuses.
    fn set_control(
        &self,
        device: &DeviceId,
        control: CameraControlId,
        value: CameraControlValue,
    ) -> Result<(), PlatformError> {
        let _ = value;
        Err(PlatformError::Unsupported {
            what: format!("the {} control on {device}", control.name()),
        })
    }

    /// This backend on `context`'s device: its streams deliver `VideoSurface::Gpu`
    /// frames that render samples without a CPU copy (README › GPU frames), as
    /// [`Capture::on_context`](crate::Capture::on_context)'s do. `Ok(None)`, the
    /// default, when the backend has no GPU path; the engine then opens devices on
    /// this backend and takes their CPU frames.
    ///
    /// The backend on the context opens only the formats it delivers there, and is
    /// [`PlatformError::Unsupported`] for the others, so the engine opens those on this
    /// backend. The engine never asks it for a compressed format.
    ///
    /// # Errors
    /// [`PlatformError::InvalidState`] for a context another platform opened.
    fn on_context(
        &self,
        context: &GpuContext,
    ) -> Result<Option<Arc<dyn VideoDevices>>, PlatformError> {
        let _ = context;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use jstream_media::{ColorInfo, FrameSize, MediaTime, PixelFormat, Timebase};

    use super::{CaptureDecoder, CaptureEncoding, CompressedFrame, VideoDeviceFormat};
    use crate::fake::{
        FAKE_CAPTURE_CARD, FAKE_MIC, FAKE_WEBCAM, FakeCaptureDecoder, FakeClock, FakeVideoDevices,
    };
    use crate::{
        AudioInputTarget, AudioIo, CaptureStream, DeviceId, PlatformError, VideoDevices,
        VideoSurface,
    };

    fn assert_send<T: Send + ?Sized>() {}
    fn assert_send_sync<T: Send + Sync + ?Sized>() {}

    #[test]
    fn traits_are_dyn_compatible_with_their_thread_bounds() {
        assert_send_sync::<dyn VideoDevices>();
        assert_send::<dyn CaptureDecoder>();
    }

    fn setup() -> (FakeClock, FakeVideoDevices, Arc<dyn VideoDevices>) {
        let clock = FakeClock::new(Timebase::MILLIS);
        let fake = FakeVideoDevices::new(clock.clone());
        let devices: Arc<dyn VideoDevices> = Arc::new(fake.clone());
        (clock, fake, devices)
    }

    fn format_with(
        devices: &dyn VideoDevices,
        id: &str,
        encoding: CaptureEncoding,
    ) -> VideoDeviceFormat {
        devices
            .formats(&DeviceId::new(id))
            .unwrap()
            .into_iter()
            .find(|f| f.encoding == encoding)
            .unwrap()
    }

    #[test]
    fn encodings_say_what_they_deliver() {
        let nv12 = CaptureEncoding::Raw(PixelFormat::Nv12);
        assert!(!nv12.is_compressed());
        assert_eq!(nv12.delivered_format(), Some(PixelFormat::Nv12));
        assert!(!CaptureEncoding::Yuy2.is_compressed());
        assert_eq!(
            CaptureEncoding::Yuy2.delivered_format(),
            Some(PixelFormat::Nv12)
        );
        assert!(CaptureEncoding::Mjpeg.is_compressed());
        assert_eq!(CaptureEncoding::Mjpeg.delivered_format(), None);
    }

    #[test]
    fn lists_devices_with_their_paired_audio() {
        let (_, _, devices) = setup();
        let list = devices.devices().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, DeviceId::new(FAKE_WEBCAM));
        assert_eq!(list[0].audio, Some(DeviceId::new(FAKE_MIC)));
        assert_eq!(list[1].id, DeviceId::new(FAKE_CAPTURE_CARD));
        assert_eq!(list[1].audio, None);

        // The paired endpoint is one the fake audio backend opens.
        struct Ignore;
        impl crate::AudioInputHandler for Ignore {
            fn on_packet(&mut self, _: &crate::AudioPacket<'_>) {}
            fn on_error(&mut self, _: &PlatformError) {}
        }
        let clock = FakeClock::new(Timebase::MILLIS);
        let audio: Arc<dyn AudioIo> = Arc::new(
            crate::fake::FakeAudioIo::new(clock, 48_000, jstream_media::ChannelLayout::Stereo)
                .unwrap(),
        );
        let paired = list[0].audio.clone().unwrap();
        assert!(
            audio
                .open_input(&AudioInputTarget::Input(paired), Box::new(Ignore))
                .is_ok()
        );
    }

    #[test]
    fn lists_formats_and_refuses_unknown_devices() {
        let (_, _, devices) = setup();
        let formats = devices.formats(&DeviceId::new(FAKE_WEBCAM)).unwrap();
        let encodings: Vec<_> = formats.iter().map(|f| f.encoding).collect();
        assert_eq!(
            encodings,
            [
                CaptureEncoding::Raw(PixelFormat::Nv12),
                CaptureEncoding::Yuy2,
                CaptureEncoding::Mjpeg,
            ]
        );
        assert!(
            formats
                .iter()
                .all(|f| f.frame_period == Timebase::new(1, 30).unwrap())
        );
        assert!(matches!(
            devices.formats(&DeviceId::new("nope")),
            Err(PlatformError::NotFound {
                kind: "video capture device",
                ..
            })
        ));
    }

    #[test]
    fn raw_stream_delivers_the_newest_frame_when_time_moves() {
        let (clock, _, devices) = setup();
        let format = format_with(
            devices.as_ref(),
            FAKE_WEBCAM,
            CaptureEncoding::Raw(PixelFormat::Nv12),
        );
        let mut stream: Box<dyn CaptureStream> = devices
            .open(&DeviceId::new(FAKE_WEBCAM), &format, None)
            .unwrap();
        let first = stream.latest().unwrap().unwrap();
        assert_eq!(first.info().format, PixelFormat::Nv12);
        assert_eq!(first.info().size, format.size);
        assert_eq!(first.info().pts, MediaTime::new(0, Timebase::MILLIS));
        assert!(stream.latest().unwrap().is_none());
        clock.advance(33);
        let next = stream.latest().unwrap().unwrap();
        assert_eq!(next.info().pts, MediaTime::new(33, Timebase::MILLIS));
        assert_eq!(stream.stats().frames_captured, 2);
    }

    #[test]
    fn yuy2_is_delivered_as_nv12() {
        let (_, _, devices) = setup();
        let format = format_with(devices.as_ref(), FAKE_WEBCAM, CaptureEncoding::Yuy2);
        let mut stream = devices
            .open(&DeviceId::new(FAKE_WEBCAM), &format, None)
            .unwrap();
        let frame = stream.latest().unwrap().unwrap();
        assert_eq!(frame.info().format, PixelFormat::Nv12);
    }

    /// A decoder that records what it is given and fails on request.
    struct Recording {
        seen: Arc<Mutex<Vec<(usize, bool, MediaTime)>>>,
        fail_next: Arc<Mutex<bool>>,
        inner: FakeCaptureDecoder,
    }

    impl CaptureDecoder for Recording {
        fn decode(&mut self, frame: &CompressedFrame<'_>) -> Result<VideoSurface, PlatformError> {
            let jpeg = frame.data.starts_with(&[0xFF, 0xD8]) && frame.data.ends_with(&[0xFF, 0xD9]);
            self.seen
                .lock()
                .unwrap()
                .push((frame.data.len(), jpeg, frame.pts));
            if std::mem::take(&mut *self.fail_next.lock().unwrap()) {
                return Err(PlatformError::Os {
                    call: "decode",
                    code: None,
                    message: "corrupt JPEG".into(),
                });
            }
            self.inner.decode(frame)
        }
    }

    #[test]
    fn mjpeg_goes_through_the_decoder_and_a_bad_frame_is_skipped() {
        let (clock, _, devices) = setup();
        let format = format_with(devices.as_ref(), FAKE_WEBCAM, CaptureEncoding::Mjpeg);
        let id = DeviceId::new(FAKE_WEBCAM);
        assert!(matches!(
            devices.open(&id, &format, None),
            Err(PlatformError::Unsupported { .. })
        ));

        let seen = Arc::new(Mutex::new(Vec::new()));
        let fail_next = Arc::new(Mutex::new(false));
        let decoder = Recording {
            seen: Arc::clone(&seen),
            fail_next: Arc::clone(&fail_next),
            inner: FakeCaptureDecoder::new(PixelFormat::Bgra8),
        };
        let mut stream = devices.open(&id, &format, Some(Box::new(decoder))).unwrap();
        let frame = stream.latest().unwrap().unwrap();
        assert_eq!(frame.info().format, PixelFormat::Bgra8);
        assert_eq!(frame.info().size, format.size);
        assert_eq!(frame.info().color, ColorInfo::default());

        clock.advance(33);
        *fail_next.lock().unwrap() = true;
        assert!(stream.latest().unwrap().is_none(), "a bad frame is dropped");
        // The time has not moved again, so the dropped frame is not retried.
        assert!(stream.latest().unwrap().is_none());
        clock.advance(33);
        assert_eq!(
            stream.latest().unwrap().unwrap().info().pts,
            MediaTime::new(66, Timebase::MILLIS)
        );

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert!(seen.iter().all(|&(len, jpeg, _)| len > 4 && jpeg));
        assert_eq!(seen[1].2, MediaTime::new(33, Timebase::MILLIS));
        assert_eq!(stream.stats().frames_captured, 3);
        assert_eq!(stream.stats().frames_undecodable, 1);
    }

    #[test]
    fn open_refuses_unknown_devices_and_formats() {
        let (_, _, devices) = setup();
        let format = format_with(
            devices.as_ref(),
            FAKE_WEBCAM,
            CaptureEncoding::Raw(PixelFormat::Nv12),
        );
        assert!(matches!(
            devices.open(&DeviceId::new("nope"), &format, None),
            Err(PlatformError::NotFound { .. })
        ));
        let odd = VideoDeviceFormat {
            size: FrameSize::new(7, 7).unwrap(),
            ..format
        };
        assert!(matches!(
            devices.open(&DeviceId::new(FAKE_WEBCAM), &odd, None),
            Err(PlatformError::Unsupported { .. })
        ));
        // The capture card has no MJPEG format.
        let mjpeg = format_with(devices.as_ref(), FAKE_WEBCAM, CaptureEncoding::Mjpeg);
        assert!(matches!(
            devices.open(
                &DeviceId::new(FAKE_CAPTURE_CARD),
                &mjpeg,
                Some(Box::new(FakeCaptureDecoder::new(PixelFormat::Nv12)))
            ),
            Err(PlatformError::Unsupported { .. })
        ));
    }

    #[test]
    fn unplugging_loses_open_streams_and_the_device() {
        let (clock, fake, devices) = setup();
        let id = DeviceId::new(FAKE_WEBCAM);
        let format = format_with(
            devices.as_ref(),
            FAKE_WEBCAM,
            CaptureEncoding::Raw(PixelFormat::Nv12),
        );
        let mut stream = devices.open(&id, &format, None).unwrap();
        assert_eq!(fake.open_streams(), 1);
        fake.unplug(&id);
        clock.advance(33);
        assert!(matches!(
            stream.latest(),
            Err(PlatformError::DeviceLost { .. })
        ));
        assert!(matches!(
            stream.latest(),
            Err(PlatformError::DeviceLost { .. })
        ));
        assert!(devices.devices().unwrap().iter().all(|d| d.id != id));
        assert!(matches!(
            devices.open(&id, &format, None),
            Err(PlatformError::NotFound { .. })
        ));
        drop(stream);
        assert_eq!(fake.open_streams(), 0);
    }

    #[test]
    fn a_control_name_round_trips() {
        for id in super::CameraControlId::ALL {
            assert_eq!(super::CameraControlId::from_name(id.name()), Some(id));
        }
        assert_eq!(super::CameraControlId::from_name("shutter"), None);
    }

    #[test]
    fn a_control_value_snaps_to_the_range_and_step() {
        let info = super::CameraControlInfo {
            id: super::CameraControlId::WhiteBalance,
            min: 2800,
            max: 6500,
            step: 10,
            default: 4600,
            value: 4600,
            auto: None,
        };
        assert_eq!(info.snap(4604), 4600);
        assert_eq!(info.snap(4605), 4610);
        assert_eq!(info.snap(100), 2800);
        assert_eq!(info.snap(i32::MAX), 6500);
        let zero_step = super::CameraControlInfo { step: 0, ..info };
        assert_eq!(zero_step.snap(4601), 4601);
    }

    #[test]
    fn the_fake_webcam_lists_and_sets_its_controls() {
        use super::{CameraControlId, CameraControlValue};
        let (_clock, _fake, devices) = setup();
        let webcam = DeviceId::new(FAKE_WEBCAM);
        let listed = devices.controls(&webcam).unwrap();
        let ids: Vec<_> = listed.iter().map(|c| c.id).collect();
        assert_eq!(
            ids,
            [
                CameraControlId::Brightness,
                CameraControlId::WhiteBalance,
                CameraControlId::Exposure
            ]
        );
        let get = |id| {
            devices
                .controls(&webcam)
                .unwrap()
                .into_iter()
                .find(|c| c.id == id)
                .unwrap()
        };

        devices
            .set_control(
                &webcam,
                CameraControlId::WhiteBalance,
                CameraControlValue::Manual(5003),
            )
            .unwrap();
        let wb = get(CameraControlId::WhiteBalance);
        assert_eq!((wb.value, wb.auto), (5000, Some(false)));
        devices
            .set_control(
                &webcam,
                CameraControlId::WhiteBalance,
                CameraControlValue::Auto,
            )
            .unwrap();
        assert_eq!(get(CameraControlId::WhiteBalance).auto, Some(true));

        devices
            .set_control(
                &webcam,
                CameraControlId::Brightness,
                CameraControlValue::Manual(300),
            )
            .unwrap();
        let brightness = get(CameraControlId::Brightness);
        assert_eq!((brightness.value, brightness.auto), (255, None));
        assert!(matches!(
            devices.set_control(
                &webcam,
                CameraControlId::Brightness,
                CameraControlValue::Auto
            ),
            Err(PlatformError::Unsupported { .. })
        ));
        assert!(matches!(
            devices.set_control(
                &webcam,
                CameraControlId::Zoom,
                CameraControlValue::Manual(1)
            ),
            Err(PlatformError::Unsupported { .. })
        ));
    }

    #[test]
    fn a_device_with_no_controls_or_gone_says_so() {
        use super::{CameraControlId, CameraControlValue};
        let (_clock, fake, devices) = setup();
        let card = DeviceId::new(FAKE_CAPTURE_CARD);
        assert!(devices.controls(&card).unwrap().is_empty());
        let webcam = DeviceId::new(FAKE_WEBCAM);
        fake.unplug(&webcam);
        assert!(matches!(
            devices.controls(&webcam),
            Err(PlatformError::NotFound { .. })
        ));
        assert!(matches!(
            devices.set_control(&webcam, CameraControlId::Gain, CameraControlValue::Auto),
            Err(PlatformError::NotFound { .. })
        ));
    }

    #[test]
    fn a_backend_without_controls_lists_none_and_refuses_a_set() {
        use super::{CameraControlId, CameraControlValue, VideoDeviceInfo};
        struct Bare;
        impl VideoDevices for Bare {
            fn devices(&self) -> Result<Vec<VideoDeviceInfo>, PlatformError> {
                Ok(Vec::new())
            }
            fn formats(&self, _: &DeviceId) -> Result<Vec<VideoDeviceFormat>, PlatformError> {
                Ok(Vec::new())
            }
            fn open(
                &self,
                _: &DeviceId,
                _: &VideoDeviceFormat,
                _: Option<Box<dyn CaptureDecoder>>,
            ) -> Result<Box<dyn CaptureStream>, PlatformError> {
                Err(PlatformError::Unsupported {
                    what: "open".into(),
                })
            }
        }
        let id = DeviceId::new("cam");
        assert!(Bare.controls(&id).unwrap().is_empty());
        assert!(matches!(
            Bare.set_control(&id, CameraControlId::Focus, CameraControlValue::Auto),
            Err(PlatformError::Unsupported { .. })
        ));
    }
}
