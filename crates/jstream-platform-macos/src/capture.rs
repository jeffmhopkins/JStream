//! Screen and window capture through ScreenCaptureKit (ADR-0063 §3).
//!
//! Targets will be listed from `SCShareableContent`, with stable IDs, once the Screen
//! Recording permission is granted, so agents and the UI pick them the same way as on
//! Windows. For now, [`ScreenCaptureKitCapture`] checks the macOS floor and reports
//! `Unsupported` for listing and starting.

#[cfg(target_os = "macos")]
pub use backend::ScreenCaptureKitCapture;

#[cfg(target_os = "macos")]
mod backend {
    use jstream_platform::{
        Capture, CaptureStream, CaptureTarget, CaptureTargetInfo, PlatformError,
    };

    use crate::{MacosVersion, check_floor};

    fn not_built(what: &str) -> PlatformError {
        PlatformError::Unsupported {
            what: format!("{what} through ScreenCaptureKit (not built yet, ADR-0063)"),
        }
    }

    /// Video capture through ScreenCaptureKit (ADR-0063 §3). For now listing targets and
    /// starting a capture report `Unsupported`.
    #[derive(Debug)]
    pub struct ScreenCaptureKitCapture {
        _private: (),
    }

    impl ScreenCaptureKitCapture {
        /// The backend, on a release at or above ADR-0063's floor.
        ///
        /// # Errors
        /// [`PlatformError::Unsupported`] below macOS 14.0.
        pub fn new() -> Result<Self, PlatformError> {
            check_floor(MacosVersion::current())?;
            Ok(Self { _private: () })
        }
    }

    impl Capture for ScreenCaptureKitCapture {
        fn targets(&self) -> Result<Vec<CaptureTargetInfo>, PlatformError> {
            Err(not_built("listing displays and windows"))
        }

        fn start(&self, target: &CaptureTarget) -> Result<Box<dyn CaptureStream>, PlatformError> {
            Err(not_built(&format!("capturing {target:?}")))
        }
    }
}
