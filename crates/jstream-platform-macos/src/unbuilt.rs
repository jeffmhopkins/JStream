//! A stand-in GPU interop with no device, for a host that wants the engine on a device of
//! its own and the CPU frame paths. The daemon and the desktop app use `MetalGpu`
//! (ADR-0063 §8).

use jstream_platform::media::FrameSize;
use jstream_platform::{GpuContext, GpuFrame, GpuInterop, PlatformError};

/// GPU interop with no device: `open_device` is `Unsupported`, so the engine opens a
/// device of its own and takes the CPU frame paths.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoGpuInterop;

fn no_interop() -> PlatformError {
    PlatformError::Unsupported {
        what: "GPU interop (this host uses none)".into(),
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
    use jstream_platform::{GpuInterop, PlatformError};

    use super::NoGpuInterop;

    #[test]
    fn gpu_interop_falls_back() {
        assert!(matches!(
            NoGpuInterop.open_device(),
            Err(PlatformError::Unsupported { .. })
        ));
    }
}
