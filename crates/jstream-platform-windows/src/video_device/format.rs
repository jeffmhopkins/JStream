//! Reading a device's native Media Foundation media types as [`VideoDeviceFormat`]s:
//! which subtypes the backend can deliver, frame rates as frame periods, the colour a
//! type declares, and the list a device shows (in its own order, without repeats).
//!
//! The values come in as plain numbers (a subtype GUID as `u128`, attribute values as
//! integers), so this is platform-independent and tested on every platform.

use jstream_platform::media::{
    ColorInfo, ColorRange, ColorSpace, FrameSize, PixelFormat, Timebase,
};
use jstream_platform::{CaptureEncoding, VideoDeviceFormat};

/// The tail every FourCC-based Media Foundation video subtype shares
/// (`XXXXXXXX-0000-0010-8000-00AA00389B71`, the FourCC in the first field).
const FOURCC_BASE: u128 = 0x0000_0000_0000_0010_8000_00aa_0038_9b71;
const FOURCC_MASK: u128 = 0x0000_0000_ffff_ffff_ffff_ffff_ffff_ffff;

/// The subtype GUID of a FourCC format.
const fn fourcc(code: &[u8; 4]) -> u128 {
    ((u32::from_le_bytes(*code) as u128) << 96) | FOURCC_BASE
}

pub(crate) const NV12: u128 = fourcc(b"NV12");
pub(crate) const YUY2: u128 = fourcc(b"YUY2");
pub(crate) const MJPG: u128 = fourcc(b"MJPG");
pub(crate) const I420: u128 = fourcc(b"I420");
pub(crate) const IYUV: u128 = fourcc(b"IYUV");

/// How the backend delivers frames of `subtype`, or `None` for a subtype it can't (RGB,
/// 10-bit, H.264 from the camera and anything else it has no path for yet).
pub(crate) fn encoding(subtype: u128) -> Option<CaptureEncoding> {
    if subtype & FOURCC_MASK != FOURCC_BASE {
        return None;
    }
    match subtype {
        NV12 => Some(CaptureEncoding::Raw(PixelFormat::Nv12)),
        I420 | IYUV => Some(CaptureEncoding::Raw(PixelFormat::I420)),
        YUY2 => Some(CaptureEncoding::Yuy2),
        MJPG => Some(CaptureEncoding::Mjpeg),
        _ => None,
    }
}

/// The frame period of a Media Foundation frame rate (`MF_MT_FRAME_RATE`, frames per
/// second as `num / den`): `30 / 1` is `1/30`, `30000 / 1001` is `1001/30000`.
pub(crate) fn frame_period(num: u32, den: u32) -> Option<Timebase> {
    Timebase::new(den, num).ok()
}

/// Splits a packed Media Foundation pair (`MF_MT_FRAME_SIZE`, `MF_MT_FRAME_RATE`): the
/// high 32 bits, then the low.
pub(crate) fn unpack(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

/// One of a device's native media types, as the backend lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Native {
    /// The type's index in the source reader's native types, to select it on open.
    pub(crate) index: u32,
    /// What it is.
    pub(crate) format: VideoDeviceFormat,
}

/// A native type as read, before it is known whether the backend can deliver it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RawType {
    pub(crate) index: u32,
    pub(crate) subtype: u128,
    /// `MF_MT_FRAME_SIZE`.
    pub(crate) size: Option<u64>,
    /// `MF_MT_FRAME_RATE`.
    pub(crate) rate: Option<u64>,
}

/// The formats a device lists: each native type the backend can deliver, in the device's
/// order. A type that repeats an earlier one's size, rate and encoding (devices list
/// variants that differ only in attributes the engine doesn't use) is left out, so the
/// first one is the one opened.
pub(crate) fn natives(types: impl IntoIterator<Item = RawType>) -> Vec<Native> {
    let mut out: Vec<Native> = Vec::new();
    for t in types {
        let Some(encoding) = encoding(t.subtype) else {
            continue;
        };
        let Some((w, h)) = t.size.map(unpack) else {
            continue;
        };
        let Ok(size) = FrameSize::new(w, h) else {
            continue;
        };
        let Some(frame_period) = t.rate.map(unpack).and_then(|(n, d)| frame_period(n, d)) else {
            continue;
        };
        let format = VideoDeviceFormat {
            size,
            frame_period,
            encoding,
        };
        if out.iter().all(|n| n.format != format) {
            out.push(Native {
                index: t.index,
                format,
            });
        }
    }
    out
}

/// `MFVideoTransferMatrix` values.
const MATRIX_BT709: u32 = 1;
const MATRIX_BT601: u32 = 2;
const MATRIX_BT2020: u32 = 4;
/// `MFNominalRange` values.
const RANGE_FULL: u32 = 1;
const RANGE_LIMITED: u32 = 2;

/// The colour of frames whose media type declares `matrix` (`MF_MT_YUV_MATRIX`) and
/// `range` (`MF_MT_VIDEO_NOMINAL_RANGE`), when it does.
///
/// Webcams often declare neither. The fallbacks are the usual conventions: BT.601 below
/// 720 lines and BT.709 from there, in limited range; a decoded JPEG (`jpeg`) is BT.601 in
/// full range, as JFIF defines it.
pub(crate) fn color(matrix: Option<u32>, range: Option<u32>, height: u32, jpeg: bool) -> ColorInfo {
    let space = match matrix {
        Some(MATRIX_BT709) => ColorSpace::Bt709,
        Some(MATRIX_BT601) => ColorSpace::Bt601,
        Some(MATRIX_BT2020) => ColorSpace::Bt2020,
        _ if jpeg || height < 720 => ColorSpace::Bt601,
        _ => ColorSpace::Bt709,
    };
    let range = match range {
        Some(RANGE_FULL) => ColorRange::Full,
        Some(RANGE_LIMITED) => ColorRange::Limited,
        _ if jpeg => ColorRange::Full,
        _ => ColorRange::Limited,
    };
    ColorInfo { space, range }
}

#[cfg(test)]
mod tests {
    use jstream_platform::CaptureEncoding;
    use jstream_platform::media::{ColorRange, ColorSpace, FrameSize, PixelFormat, Timebase};

    use super::{MJPG, NV12, RawType, YUY2, color, encoding, frame_period, natives, unpack};

    /// The subtype GUIDs as the `windows` crate spells them, so the FourCC arithmetic is
    /// checked against the SDK's values on every platform.
    #[test]
    fn subtypes_match_the_sdk_guids() {
        assert_eq!(NV12, 0x3231564e_0000_0010_8000_00aa00389b71);
        assert_eq!(YUY2, 0x32595559_0000_0010_8000_00aa00389b71);
        assert_eq!(MJPG, 0x47504a4d_0000_0010_8000_00aa00389b71);
    }

    #[test]
    fn deliverable_subtypes_map_to_encodings() {
        assert_eq!(
            encoding(NV12),
            Some(CaptureEncoding::Raw(PixelFormat::Nv12))
        );
        assert_eq!(encoding(YUY2), Some(CaptureEncoding::Yuy2));
        assert_eq!(encoding(MJPG), Some(CaptureEncoding::Mjpeg));
        assert_eq!(
            encoding(super::IYUV),
            Some(CaptureEncoding::Raw(PixelFormat::I420))
        );
        // RGB32 (a D3DFORMAT-based subtype) and H.264 are not delivered.
        assert_eq!(encoding(0x00000016_0000_0010_8000_00aa00389b71), None);
        assert_eq!(encoding(0x34363248_0000_0010_8000_00aa00389b71), None);
        // A GUID that only shares the FourCC field is not a FourCC subtype.
        assert_eq!(encoding(0x3231564e_0000_0000_0000_000000000000), None);
    }

    #[test]
    fn frame_rates_become_frame_periods() {
        assert_eq!(frame_period(30, 1), Timebase::new(1, 30).ok());
        assert_eq!(frame_period(30000, 1001), Timebase::new(1001, 30000).ok());
        assert_eq!(frame_period(0, 1), None);
        assert_eq!(frame_period(30, 0), None);
        assert_eq!(unpack((1920 << 32) | 1080), (1920, 1080));
    }

    fn raw(index: u32, subtype: u128, w: u64, h: u64, fps: u64) -> RawType {
        RawType {
            index,
            subtype,
            size: Some((w << 32) | h),
            rate: Some((fps << 32) | 1),
        }
    }

    #[test]
    fn natives_keep_the_device_order_and_drop_repeats_and_unknowns() {
        let types = [
            raw(0, MJPG, 1920, 1080, 30),
            raw(1, 0x00000016_0000_0010_8000_00aa00389b71, 1920, 1080, 30), // RGB32
            raw(2, YUY2, 640, 480, 30),
            raw(3, MJPG, 1920, 1080, 30), // a repeat
            raw(4, YUY2, 640, 480, 15),
            RawType {
                rate: None,
                ..raw(5, NV12, 640, 480, 30)
            },
            raw(6, NV12, 0, 480, 30),
        ];
        let list = natives(types);
        let summary: Vec<_> = list
            .iter()
            .map(|n| (n.index, n.format.encoding, n.format.size))
            .collect();
        let size = |w, h| FrameSize::new(w, h).expect("size");
        assert_eq!(
            summary,
            [
                (0, CaptureEncoding::Mjpeg, size(1920, 1080)),
                (2, CaptureEncoding::Yuy2, size(640, 480)),
                (4, CaptureEncoding::Yuy2, size(640, 480)),
            ]
        );
        assert_eq!(
            list[2].format.frame_period,
            Timebase::new(1, 15).expect("tb")
        );
    }

    #[test]
    fn colour_follows_the_type_then_the_conventions() {
        let c = color(Some(1), Some(1), 480, false);
        assert_eq!((c.space, c.range), (ColorSpace::Bt709, ColorRange::Full));
        let c = color(None, None, 480, false);
        assert_eq!((c.space, c.range), (ColorSpace::Bt601, ColorRange::Limited));
        let c = color(None, None, 1080, false);
        assert_eq!((c.space, c.range), (ColorSpace::Bt709, ColorRange::Limited));
        let c = color(None, None, 1080, true);
        assert_eq!((c.space, c.range), (ColorSpace::Bt601, ColorRange::Full));
        let c = color(Some(4), Some(2), 2160, true);
        assert_eq!(
            (c.space, c.range),
            (ColorSpace::Bt2020, ColorRange::Limited)
        );
    }
}
