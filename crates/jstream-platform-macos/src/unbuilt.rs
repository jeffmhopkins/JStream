//! The stand-in for the backend macOS doesn't have yet (ADR-0063), so a host can build a
//! complete `Backends` from what exists: the IOSurface and Metal interop (§8) follows as
//! a later part.

use jstream_platform::media::FrameSize;
use jstream_platform::{GpuContext, GpuFrame, GpuInterop, PlatformError};

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
