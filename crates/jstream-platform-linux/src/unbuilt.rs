//! The stand-in for the backend Linux doesn't have yet, so a host can build a complete
//! `Backends` from what exists: media-file decoding (ADR-0072) follows as a later part.

use std::path::Path;

use jstream_platform::{MediaDecoder, MediaDecoderConfig, MediaDecoders, PlatformError};

/// Media-file decoders until ADR-0072's are built: every file is `Unsupported`, so a
/// media-file source shows nothing and says why, and the rest of the engine runs.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoMediaDecoders;

impl MediaDecoders for NoMediaDecoders {
    fn open(
        &self,
        _path: &Path,
        _config: &MediaDecoderConfig,
    ) -> Result<Box<dyn MediaDecoder>, PlatformError> {
        Err(PlatformError::Unsupported {
            what: "media files on Linux (ADR-0072's decoders are not built yet)".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use jstream_platform::media::PixelFormat;
    use jstream_platform::{HardwarePreference, MediaDecoderConfig, MediaDecoders, PlatformError};

    use super::NoMediaDecoders;

    #[test]
    fn every_file_is_unsupported() {
        let config = MediaDecoderConfig {
            video_format: PixelFormat::Bgra8,
            hardware: HardwarePreference::Prefer,
            audio: true,
        };
        let result = NoMediaDecoders.open(Path::new("/tmp/clip.mp4"), &config);
        assert!(
            matches!(result, Err(PlatformError::Unsupported { ref what }) if what.contains("ADR-0072")),
            "{:?}",
            result.err()
        );
    }
}
