//! The platform-independent half of the media-file backend (ADR-0061, ADR-0063 §6): what
//! an `AVAssetReader` is asked for, for one [`MediaDecoderConfig`], and what is done to
//! what it gives back.
//!
//! - Video is read as BGRA or NV12 pixel buffers, the formats AVFoundation decodes to
//!   on every Mac. RGBA, the format the media-file source asks for, is read as BGRA and
//!   its red and blue swapped ([`bgra_to_rgba`]).
//! - Audio is read as interleaved `f32` at the track's own rate: mono stays mono, and
//!   anything wider is mixed down to stereo by AVFoundation ([`audio_layout`]).
//! - AVFoundation picks its decoder itself and doesn't say which, so a configuration
//!   that requires or avoids hardware is refused, as on Windows for one that requires it.

use jstream_platform::media::{ChannelLayout, PixelFormat};
use jstream_platform::{HardwarePreference, MediaDecoderConfig, PlatformError};

use crate::fourcc::{CV_420V, CV_BGRA};

/// What an `AVAssetReaderTrackOutput` is opened with for a file's video.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AvReaderSettings {
    /// The Core Video pixel format the reader is asked for.
    pub pixel_format: u32,
    /// Whether the frames it gives are BGRA to be handed on as RGBA.
    pub bgra_to_rgba: bool,
}

/// The reader settings for `config`.
///
/// # Errors
/// [`PlatformError::Unsupported`] for a pixel format other than RGBA, BGRA or NV12, and
/// for a hardware preference other than `Prefer`.
pub fn av_reader_settings(config: &MediaDecoderConfig) -> Result<AvReaderSettings, PlatformError> {
    if config.hardware != HardwarePreference::Prefer {
        // AVFoundation picks the decoder itself, and doesn't say which.
        return Err(PlatformError::Unsupported {
            what: format!(
                "choosing the decoder for media files on macOS ({:?})",
                config.hardware
            ),
        });
    }
    let (pixel_format, bgra_to_rgba) = match config.video_format {
        PixelFormat::Rgba8 => (CV_BGRA, true),
        PixelFormat::Bgra8 => (CV_BGRA, false),
        PixelFormat::Nv12 => (CV_420V, false),
        other => {
            return Err(PlatformError::Unsupported {
                what: format!("decoding media files to {other:?} on macOS"),
            });
        }
    };
    Ok(AvReaderSettings {
        pixel_format,
        bgra_to_rgba,
    })
}

/// The layout a track of `channels` channels is read in: mono as mono, anything wider
/// mixed down to stereo. `None` for a track that says it has no channels.
pub fn audio_layout(channels: u32) -> Option<ChannelLayout> {
    match channels {
        0 => None,
        1 => Some(ChannelLayout::Mono),
        _ => Some(ChannelLayout::Stereo),
    }
}

/// Swaps the red and blue bytes of every BGRA pixel in `data`, making it RGBA. A trailing
/// partial pixel is left as it is.
pub fn bgra_to_rgba(data: &mut [u8]) {
    for px in data.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
}

/// Whether an `AVError` code means the file's container or codec isn't one AVFoundation
/// plays (`Unsupported`), rather than a failure to read it (`Os`).
pub fn av_error_is_unsupported(code: isize) -> bool {
    matches!(
        code,
        -11828 // AVErrorFileFormatNotRecognized
            | -11829 // AVErrorFileFailedToParse
            | -11833 // AVErrorDecoderNotFound
            | -11838 // AVErrorOperationNotSupportedForAsset
            | -11853 // AVErrorFailedToParse
            | -11864 // AVErrorFormatUnsupported
    )
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{ChannelLayout, PixelFormat};
    use jstream_platform::{HardwarePreference, MediaDecoderConfig, PlatformError};

    use super::{audio_layout, av_error_is_unsupported, av_reader_settings, bgra_to_rgba};
    use crate::fourcc::{CV_420V, CV_BGRA};

    fn config(video_format: PixelFormat, hardware: HardwarePreference) -> MediaDecoderConfig {
        MediaDecoderConfig {
            video_format,
            hardware,
            audio: true,
        }
    }

    #[test]
    fn video_is_read_as_bgra_or_nv12() {
        let s = av_reader_settings(&config(PixelFormat::Rgba8, HardwarePreference::Prefer))
            .expect("rgba");
        assert_eq!((s.pixel_format, s.bgra_to_rgba), (CV_BGRA, true));
        let s = av_reader_settings(&config(PixelFormat::Bgra8, HardwarePreference::Prefer))
            .expect("bgra");
        assert_eq!((s.pixel_format, s.bgra_to_rgba), (CV_BGRA, false));
        let s = av_reader_settings(&config(PixelFormat::Nv12, HardwarePreference::Prefer))
            .expect("nv12");
        assert_eq!((s.pixel_format, s.bgra_to_rgba), (CV_420V, false));
        for (format, hardware) in [
            (PixelFormat::I420, HardwarePreference::Prefer),
            (PixelFormat::P010, HardwarePreference::Prefer),
            (PixelFormat::Rgba8, HardwarePreference::Require),
            (PixelFormat::Rgba8, HardwarePreference::Avoid),
        ] {
            assert!(
                matches!(
                    av_reader_settings(&config(format, hardware)),
                    Err(PlatformError::Unsupported { .. })
                ),
                "{format:?} {hardware:?}"
            );
        }
    }

    #[test]
    fn audio_is_mono_or_stereo() {
        assert_eq!(audio_layout(0), None);
        assert_eq!(audio_layout(1), Some(ChannelLayout::Mono));
        assert_eq!(audio_layout(2), Some(ChannelLayout::Stereo));
        assert_eq!(
            audio_layout(6),
            Some(ChannelLayout::Stereo),
            "5.1 mixed down"
        );
    }

    #[test]
    fn bgra_becomes_rgba() {
        let mut px = vec![1, 2, 3, 4, 5, 6, 7, 8, 9];
        bgra_to_rgba(&mut px);
        assert_eq!(px, [3, 2, 1, 4, 7, 6, 5, 8, 9]);
    }

    #[test]
    fn unplayable_files_are_told_from_failed_reads() {
        assert!(av_error_is_unsupported(-11828));
        assert!(av_error_is_unsupported(-11833));
        assert!(!av_error_is_unsupported(-11800), "AVErrorUnknown");
        assert!(!av_error_is_unsupported(0));
    }
}
