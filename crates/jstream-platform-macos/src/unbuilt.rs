//! Stand-ins for the backends macOS doesn't have yet (ADR-0063), so a host can build a
//! complete `Backends` from what exists: AVFoundation webcams (§3) and the IOSurface and
//! Metal interop (§8) follow as later parts.

use jstream_platform::media::FrameSize;
use jstream_platform::{
    CaptureDecoder, CaptureStream, DeviceId, GpuContext, GpuFrame, GpuInterop, PlatformError,
    VideoDeviceFormat, VideoDeviceInfo, VideoDevices,
};

/// Webcams and capture cards until AVFoundation capture lands: no devices, so a video
/// device source finds none to open.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoVideoDevices;

fn no_device(device: &DeviceId) -> PlatformError {
    PlatformError::NotFound {
        kind: "video device",
        id: device.as_str().to_owned(),
    }
}

impl VideoDevices for NoVideoDevices {
    fn devices(&self) -> Result<Vec<VideoDeviceInfo>, PlatformError> {
        Ok(Vec::new())
    }

    fn formats(&self, device: &DeviceId) -> Result<Vec<VideoDeviceFormat>, PlatformError> {
        Err(no_device(device))
    }

    fn open(
        &self,
        device: &DeviceId,
        _format: &VideoDeviceFormat,
        _decoder: Option<Box<dyn CaptureDecoder>>,
    ) -> Result<Box<dyn CaptureStream>, PlatformError> {
        Err(no_device(device))
    }
}

/// GPU interop until the Metal spike lands: `open_device` is `Unsupported`, so the engine
/// opens a device of its own and takes the CPU frame paths.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoGpuInterop;

fn no_interop() -> PlatformError {
    PlatformError::Unsupported {
        what: "GPU interop on macOS (IOSurface and Metal, not built yet)".into(),
    }
}

impl GpuInterop for NoGpuInterop {
    fn open_device(&self) -> Result<GpuContext, PlatformError> {
        Err(no_interop())
    }

    fn encode_target(
        &self,
        _context: &GpuContext,
        _size: FrameSize,
    ) -> Result<GpuFrame, PlatformError> {
        Err(no_interop())
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::{DeviceId, GpuInterop, PlatformError, VideoDevices};

    use super::{NoGpuInterop, NoVideoDevices};

    #[test]
    fn there_are_no_webcams_yet() {
        assert_eq!(NoVideoDevices.devices().expect("a list").len(), 0);
        let id = DeviceId::new("camera:1");
        assert!(matches!(
            NoVideoDevices.formats(&id),
            Err(PlatformError::NotFound { .. })
        ));
    }

    #[test]
    fn gpu_interop_falls_back() {
        assert!(matches!(
            NoGpuInterop.open_device(),
            Err(PlatformError::Unsupported { .. })
        ));
    }
}
