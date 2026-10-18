//! A backend that isn't there: webcams (V4L2) and media files are not built on Linux yet,
//! and audio needs libpipewire. A host fills those slots of `Backends` with
//! [`Unavailable`], so the engine runs and every call says why it can't.

use std::path::Path;

use jstream_platform::AudioDeviceInfo;
use jstream_platform::media::AudioFormat;
use jstream_platform::{
    AudioInputHandler, AudioInputTarget, AudioIo, AudioOutputHandler, AudioOutputTarget,
    AudioProcess, AudioStream, CaptureDecoder, CaptureStream, DeviceId, MediaDecoder,
    MediaDecoderConfig, MediaDecoders, PlatformError, VideoDeviceFormat, VideoDeviceInfo,
    VideoDevices,
};

/// A backend whose every call fails with the same error: the reason it is missing.
#[derive(Clone, Debug)]
pub struct Unavailable {
    why: PlatformError,
}

impl Unavailable {
    /// A backend that fails with `why`, such as the error libpipewire's loading gave.
    pub fn new(why: PlatformError) -> Self {
        Self { why }
    }

    /// A backend for `what` that isn't built on Linux yet: `Unsupported`, naming it and
    /// ADR-0060.
    pub fn not_built(what: &str) -> Self {
        Self::new(PlatformError::Unsupported {
            what: format!("{what} on Linux (not built yet, ADR-0060)"),
        })
    }

    fn fail<T>(&self) -> Result<T, PlatformError> {
        Err(self.why.clone())
    }
}

impl VideoDevices for Unavailable {
    fn devices(&self) -> Result<Vec<VideoDeviceInfo>, PlatformError> {
        self.fail()
    }

    fn formats(&self, _device: &DeviceId) -> Result<Vec<VideoDeviceFormat>, PlatformError> {
        self.fail()
    }

    fn open(
        &self,
        _device: &DeviceId,
        _format: &VideoDeviceFormat,
        _decoder: Option<Box<dyn CaptureDecoder>>,
    ) -> Result<Box<dyn CaptureStream>, PlatformError> {
        self.fail()
    }
}

impl MediaDecoders for Unavailable {
    fn open(
        &self,
        _path: &Path,
        _config: &MediaDecoderConfig,
    ) -> Result<Box<dyn MediaDecoder>, PlatformError> {
        self.fail()
    }
}

impl AudioIo for Unavailable {
    fn devices(&self) -> Result<Vec<AudioDeviceInfo>, PlatformError> {
        self.fail()
    }

    fn processes(&self) -> Result<Vec<AudioProcess>, PlatformError> {
        self.fail()
    }

    fn input_format(&self, _target: &AudioInputTarget) -> Result<AudioFormat, PlatformError> {
        self.fail()
    }

    fn open_input(
        &self,
        _target: &AudioInputTarget,
        _handler: Box<dyn AudioInputHandler>,
    ) -> Result<Box<dyn AudioStream>, PlatformError> {
        self.fail()
    }

    fn open_output(
        &self,
        _target: &AudioOutputTarget,
        _handler: Box<dyn AudioOutputHandler>,
    ) -> Result<Box<dyn AudioStream>, PlatformError> {
        self.fail()
    }
}

#[cfg(test)]
mod tests {
    use super::Unavailable;
    use jstream_platform::media::PixelFormat;
    use jstream_platform::{
        AudioIo, DeviceId, HardwarePreference, MediaDecoderConfig, MediaDecoders, PlatformError,
        VideoDevices,
    };

    #[test]
    fn every_call_says_why() {
        let webcams = Unavailable::not_built("webcams");
        let err = VideoDevices::devices(&webcams).expect_err("not built");
        assert_eq!(
            err.to_string(),
            "not supported by this backend: webcams on Linux (not built yet, ADR-0060)"
        );
        assert!(webcams.formats(&DeviceId::new("cam")).is_err());
        assert!(
            MediaDecoders::open(
                &Unavailable::not_built("media files"),
                std::path::Path::new("/x.mp4"),
                &MediaDecoderConfig {
                    video_format: PixelFormat::Rgba8,
                    hardware: HardwarePreference::Prefer,
                    audio: false,
                }
            )
            .is_err()
        );
        let missing = PlatformError::MissingComponent {
            component: "PipeWire",
            remedy: "install pipewire".into(),
        };
        let audio = Unavailable::new(missing.clone());
        assert_eq!(audio.processes().expect_err("missing"), missing);
        assert_eq!(AudioIo::devices(&audio).expect_err("missing"), missing);
    }
}
