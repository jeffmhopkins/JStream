//! Four-character codes: the `OSType` values Core Video, Core Media, VideoToolbox and
//! AudioToolbox name formats and codecs with, and the `OSStatus` errors many Apple calls
//! return as four printable characters.

use jstream_platform::PlatformError;
use jstream_platform::media::PixelFormat;

/// A four-character code from its characters, big-endian, as Apple's headers write them
/// (`'420v'`).
pub const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

/// The four characters of `code`, if all of them are printable ASCII.
pub fn fourcc_str(code: u32) -> Option<String> {
    let bytes = code.to_be_bytes();
    bytes
        .iter()
        .all(|b| (0x20..0x7f).contains(b))
        .then(|| bytes.iter().map(|&b| char::from(b)).collect())
}

/// A failed Apple call as a [`PlatformError::Os`]. The message shows the status as its four
/// characters when it is a printable code (`'!dat'`), and in decimal otherwise, as Apple's
/// documentation lists them (`-12902`).
pub fn os_status(call: &'static str, status: i32, what: &str) -> PlatformError {
    let code = u32::from_ne_bytes(status.to_ne_bytes());
    let shown = fourcc_str(code).map_or_else(|| status.to_string(), |s| format!("'{s}'"));
    PlatformError::Os {
        call,
        code: Some(code),
        message: format!("{what} (OSStatus {shown})"),
    }
}

/// `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`: NV12, video range.
pub const CV_420V: u32 = fourcc(b"420v");
/// `kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange`: P010, video range.
pub const CV_X420: u32 = fourcc(b"x420");
/// `kCVPixelFormatType_420YpCbCr8Planar`: I420.
pub const CV_Y420: u32 = fourcc(b"y420");
/// `kCVPixelFormatType_32BGRA`.
pub const CV_BGRA: u32 = fourcc(b"BGRA");
/// `kCVPixelFormatType_32RGBA`.
pub const CV_RGBA: u32 = fourcc(b"RGBA");

/// The Core Video pixel format for `format`. The 4:2:0 formats are video range, the range
/// the engine's encoders take. `None` for a format added to the engine after this
/// mapping.
pub const fn cv_pixel_format(format: PixelFormat) -> Option<u32> {
    match format {
        PixelFormat::Nv12 => Some(CV_420V),
        PixelFormat::P010 => Some(CV_X420),
        PixelFormat::I420 => Some(CV_Y420),
        PixelFormat::Bgra8 => Some(CV_BGRA),
        PixelFormat::Rgba8 => Some(CV_RGBA),
        _ => None,
    }
}

/// The engine's pixel format for a Core Video one, or `None` for a format the engine has
/// no equivalent of (full-range 4:2:0 among them).
pub const fn pixel_format_from_cv(code: u32) -> Option<PixelFormat> {
    match code {
        CV_420V => Some(PixelFormat::Nv12),
        CV_X420 => Some(PixelFormat::P010),
        CV_Y420 => Some(PixelFormat::I420),
        CV_BGRA => Some(PixelFormat::Bgra8),
        CV_RGBA => Some(PixelFormat::Rgba8),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::PlatformError;
    use jstream_platform::media::PixelFormat;

    use super::{cv_pixel_format, fourcc, fourcc_str, os_status, pixel_format_from_cv};

    #[test]
    fn codes_match_apple_headers() {
        // Values from CVPixelBuffer.h and CMFormatDescription.h.
        assert_eq!(fourcc(b"420v"), 0x3432_3076);
        assert_eq!(fourcc(b"BGRA"), 0x4247_5241);
        assert_eq!(fourcc(b"avc1"), 0x6176_6331);
        assert_eq!(fourcc_str(0x3432_3076).as_deref(), Some("420v"));
        assert_eq!(fourcc_str(32), None);
    }

    #[test]
    fn statuses_show_their_characters_or_number() {
        // kAudioHardwareUnknownPropertyError is 'who?'.
        let who = i32::from_be_bytes(*b"who?");
        match os_status("AudioObjectGetPropertyData", who, "reading a property") {
            PlatformError::Os {
                call,
                code,
                message,
            } => {
                assert_eq!(call, "AudioObjectGetPropertyData");
                assert_eq!(code, Some(fourcc(b"who?")));
                assert_eq!(message, "reading a property (OSStatus 'who?')");
            }
            other => panic!("unexpected: {other:?}"),
        }
        // kVTVideoEncoderMalfunctionErr is -12911.
        match os_status("VTCompressionSessionEncodeFrame", -12911, "encoding") {
            PlatformError::Os { code, message, .. } => {
                assert_eq!(code, Some(0xFFFF_CD91));
                assert_eq!(message, "encoding (OSStatus -12911)");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn pixel_formats_round_trip() {
        for format in PixelFormat::ALL {
            let code = cv_pixel_format(format).expect("every format maps");
            assert_eq!(pixel_format_from_cv(code), Some(format));
        }
        // Full-range NV12 ('420f') has no engine equivalent.
        assert_eq!(pixel_format_from_cv(fourcc(b"420f")), None);
    }
}
