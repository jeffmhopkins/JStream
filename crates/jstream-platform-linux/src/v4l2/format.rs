//! What a V4L2 device reports, in `jstream-platform`'s terms: which pixel formats the
//! backend delivers, the formats list built from the frame sizes and intervals the driver
//! enumerates, the colour of raw frames, a buffer's time on the media clock, and which
//! device IDs name a capture node. Platform-independent, so it is tested everywhere.

use jstream_platform::media::{
    ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, Timebase, VideoFrame,
    VideoFrameInfo,
};
use jstream_platform::{CaptureEncoding, PlatformError, VideoDeviceFormat, yuv};

use super::abi::{
    BUF_FLAG_TIMESTAMP_MASK, BUF_FLAG_TIMESTAMP_MONOTONIC, BUF_TYPE_VIDEO_CAPTURE,
    BUF_TYPE_VIDEO_CAPTURE_MPLANE, CAP_DEVICE_CAPS, CAP_STREAMING, CAP_VIDEO_CAPTURE,
    CAP_VIDEO_CAPTURE_MPLANE, COLORSPACE_BT2020, COLORSPACE_JPEG, COLORSPACE_REC709,
    COLORSPACE_SMPTE170M, COLORSPACE_SRGB, Capability, Fract, FrmSizeStepwise, PIX_FMT_ABGR32,
    PIX_FMT_JPEG, PIX_FMT_MJPEG, PIX_FMT_NV12, PIX_FMT_NV12M, PIX_FMT_RGBA32, PIX_FMT_RGBX32,
    PIX_FMT_XBGR32, PIX_FMT_YUV420, PIX_FMT_YUV420M, PIX_FMT_YUYV, PixFormat,
    QUANTIZATION_FULL_RANGE, QUANTIZATION_LIM_RANGE, Timeval, YCBCR_ENC_601, YCBCR_ENC_709,
    YCBCR_ENC_BT2020,
};
use crate::hns_from_timespec;

/// The call a short or corrupt buffer is reported against.
pub(crate) const DQBUF: &str = "VIDIOC_DQBUF";

/// How the backend delivers a V4L2 pixel format, or `None` for one it doesn't: packed
/// 4:2:2 (`YUYV`) converted to NV12, NV12 and I420 (`YU12`) as they are, 32-bit RGB with
/// the alpha or padding byte last (`AR24` and `XR24` as BGRA, `AB24` and `XB24` as
/// RGBA, as capture cards and `vivid` offer them) as they are, and MJPEG (`MJPG`, or
/// `JPEG` as some webcams name it) to the engine's decoder. The multi-planar API's
/// `NV12M` and `YUV420M` (each plane in its own buffer plane) are delivered as NV12 and
/// I420. A capture's alpha means nothing to the engine, so padding is delivered as it
/// is.
pub(crate) fn encoding(fourcc: u32) -> Option<CaptureEncoding> {
    match fourcc {
        PIX_FMT_YUYV => Some(CaptureEncoding::Yuy2),
        PIX_FMT_ABGR32 | PIX_FMT_XBGR32 => Some(CaptureEncoding::Raw(PixelFormat::Bgra8)),
        PIX_FMT_RGBA32 | PIX_FMT_RGBX32 => Some(CaptureEncoding::Raw(PixelFormat::Rgba8)),
        PIX_FMT_MJPEG | PIX_FMT_JPEG => Some(CaptureEncoding::Mjpeg),
        PIX_FMT_NV12 | PIX_FMT_NV12M => Some(CaptureEncoding::Raw(PixelFormat::Nv12)),
        PIX_FMT_YUV420 | PIX_FMT_YUV420M => Some(CaptureEncoding::Raw(PixelFormat::I420)),
        _ => None,
    }
}

/// How the GPU path (`V4l2Devices::on_context`) reads a raw format's buffers: which
/// buffer planes are imported as which texels, and the pass that makes them the RGBA
/// frame render samples. `None` for a format it doesn't take, which opens on the CPU
/// path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GpuLayout {
    /// One plane of 4-byte pixels, B, G, R, A (or padding) in memory, copied as they are
    /// with the alpha made opaque.
    Bgra,
    /// One plane of 4-byte pixels, R, G, B, A (or padding) in memory, likewise.
    Rgba,
}

/// The [`GpuLayout`] of a V4L2 pixel format.
pub(crate) fn gpu_layout(fourcc: u32) -> Option<GpuLayout> {
    match fourcc {
        PIX_FMT_ABGR32 | PIX_FMT_XBGR32 => Some(GpuLayout::Bgra),
        PIX_FMT_RGBA32 | PIX_FMT_RGBX32 => Some(GpuLayout::Rgba),
        _ => None,
    }
}

/// How many buffer planes a pixel format's frames come in: one for every format but
/// `NV12M` (two) and `YUV420M` (three).
pub(crate) fn buffer_planes(fourcc: u32) -> usize {
    match fourcc {
        PIX_FMT_NV12M => 2,
        PIX_FMT_YUV420M => 3,
        _ => 1,
    }
}

/// The row stride of each buffer plane of a raw frame, from the driver's `bytesperline`
/// for each (`bytesperline.len()` is the format's [`buffer_planes`]). A plane the driver
/// gives no stride for, or too small a one, gets the least its rows need: the first
/// plane [`min_stride`], NV12's chroma plane the luma stride, I420's chroma planes half
/// of it. `None` when the driver's plane count isn't the format's.
pub(crate) fn plane_strides(
    encoding: CaptureEncoding,
    fourcc: u32,
    width: u32,
    bytesperline: &[u32],
) -> Option<Vec<usize>> {
    if bytesperline.len() != buffer_planes(fourcc) {
        return None;
    }
    let luma = (bytesperline[0] as usize).max(min_stride(encoding, width));
    let chroma_min = match encoding {
        CaptureEncoding::Raw(PixelFormat::I420) => width.div_ceil(2) as usize,
        _ => width.div_ceil(2) as usize * 2,
    };
    let derived = match encoding {
        CaptureEncoding::Raw(PixelFormat::I420) => luma.div_ceil(2),
        _ => luma,
    };
    Some(
        bytesperline
            .iter()
            .enumerate()
            .map(|(i, &b)| match (i, b) {
                (0, _) => luma,
                (_, 0) => derived,
                (_, b) => (b as usize).max(chroma_min),
            })
            .collect(),
    )
}

/// The common sizes offered from a stepwise or continuous size range, largest first.
const COMMON_SIZES: [(u32, u32); 9] = [
    (3840, 2160),
    (2560, 1440),
    (1920, 1080),
    (1280, 720),
    (960, 540),
    (800, 600),
    (640, 480),
    (640, 360),
    (320, 240),
];

/// The sizes listed for a stepwise (or continuous) range: its largest size, then the
/// common sizes within it that its steps reach. A driver with a range rather than a list
/// (a capture card, a virtual device) gets a short list of useful sizes, not every step.
pub(crate) fn stepwise_sizes(range: &FrmSizeStepwise) -> Vec<(u32, u32)> {
    let fits = |v: u32, min: u32, max: u32, step: u32| {
        (min..=max).contains(&v) && (v - min).is_multiple_of(step.max(1))
    };
    let mut out = Vec::new();
    if range.max_width > 0 && range.max_height > 0 {
        out.push((range.max_width, range.max_height));
    }
    for (w, h) in COMMON_SIZES {
        if fits(w, range.min_width, range.max_width, range.step_width)
            && fits(h, range.min_height, range.max_height, range.step_height)
            && !out.contains(&(w, h))
        {
            out.push((w, h));
        }
    }
    out
}

/// A frame interval (seconds per frame) as a frame period time base, or `None` for a
/// zero part.
pub(crate) fn period(interval: Fract) -> Option<Timebase> {
    if interval.numerator == 0 || interval.denominator == 0 {
        return None;
    }
    Timebase::new(interval.numerator, interval.denominator).ok()
}

/// One pixel format's sizes, each with its frame intervals, as the driver enumerated
/// them.
pub(crate) struct Enumerated {
    pub(crate) fourcc: u32,
    pub(crate) sizes: Vec<((u32, u32), Vec<Fract>)>,
}

/// The formats the backend lists: every size and interval of each pixel format it
/// delivers, in the driver's order, without repeats. A format whose size or interval
/// can't be represented is left out.
pub(crate) fn formats(enumerated: &[Enumerated]) -> Vec<VideoDeviceFormat> {
    let mut out: Vec<VideoDeviceFormat> = Vec::new();
    for e in enumerated {
        let Some(encoding) = encoding(e.fourcc) else {
            continue;
        };
        for ((w, h), intervals) in &e.sizes {
            let Ok(size) = FrameSize::new(*w, *h) else {
                continue;
            };
            for frame_period in intervals.iter().copied().filter_map(period) {
                let format = VideoDeviceFormat {
                    size,
                    frame_period,
                    encoding,
                };
                if !out.contains(&format) {
                    out.push(format);
                }
            }
        }
    }
    out
}

/// The FourCC to ask the driver for to capture `format`: the first pixel format it
/// enumerates whose own sizes and intervals include `format`, or `None` when none does.
/// Two FourCCs can deliver the same encoding (`MJPG` and `JPEG`, or `NV12` and `NM12`)
/// with different sizes, so the one asked for must be one that lists the size.
pub(crate) fn fourcc_for(format: &VideoDeviceFormat, enumerated: &[Enumerated]) -> Option<u32> {
    enumerated
        .iter()
        .find(|e| formats(std::slice::from_ref(e)).contains(format))
        .map(|e| e.fourcc)
}

/// The colour of a raw format's frames, from what the driver says. The encoding falls
/// back on the colour space's default (BT.601 for SMPTE 170M, JPEG and sRGB, as V4L2
/// defines them), then on height: BT.601 below 720 lines and BT.709 from there, as the
/// Windows backend does. The range is limited unless the driver says full, or the colour
/// space is JPEG's.
pub(crate) fn color(pix: &PixFormat) -> ColorInfo {
    let space = match (pix.ycbcr_enc, pix.colorspace) {
        (YCBCR_ENC_601, _) => ColorSpace::Bt601,
        (YCBCR_ENC_709, _) => ColorSpace::Bt709,
        (YCBCR_ENC_BT2020, _) => ColorSpace::Bt2020,
        (_, COLORSPACE_SMPTE170M | COLORSPACE_JPEG | COLORSPACE_SRGB) => ColorSpace::Bt601,
        (_, COLORSPACE_REC709) => ColorSpace::Bt709,
        (_, COLORSPACE_BT2020) => ColorSpace::Bt2020,
        _ if pix.height < 720 => ColorSpace::Bt601,
        _ => ColorSpace::Bt709,
    };
    let range = match (pix.quantization, pix.colorspace) {
        (QUANTIZATION_FULL_RANGE, _) => ColorRange::Full,
        (QUANTIZATION_LIM_RANGE, _) => ColorRange::Limited,
        (_, COLORSPACE_JPEG) => ColorRange::Full,
        _ => ColorRange::Limited,
    };
    ColorInfo { space, range }
}

/// The fewest bytes in a row of a raw encoding's first plane.
pub(crate) fn min_stride(encoding: CaptureEncoding, width: u32) -> usize {
    match encoding {
        CaptureEncoding::Yuy2 => width.div_ceil(2) as usize * 4,
        CaptureEncoding::Raw(PixelFormat::Bgra8 | PixelFormat::Rgba8) => width as usize * 4,
        _ => width as usize,
    }
}

/// The fewest bytes a buffer plane of `height` rows, `stride` bytes apart, each
/// `row_bytes` long, holds: the last row needn't be padded. `None` on overflow.
pub(crate) fn plane_bytes(stride: usize, row_bytes: usize, height: u32) -> Option<usize> {
    match height {
        0 => Some(0),
        h => stride.checked_mul(h as usize - 1)?.checked_add(row_bytes),
    }
}

/// A raw frame's buffer planes (each plane's rows `strides[i]` bytes apart, from
/// [`plane_strides`]) as an engine frame with `info`'s size, colour and `pts`. One buffer
/// plane holds the whole frame, its chroma after its luma; several hold one image plane
/// each.
///
/// # Errors
/// [`PlatformError::Os`] for a buffer shorter than the format needs or without as many
/// planes as strides, and [`PlatformError::Unsupported`] for a compressed encoding.
pub(crate) fn raw_frame(
    encoding: CaptureEncoding,
    planes: &[&[u8]],
    strides: &[usize],
    info: VideoFrameInfo,
) -> Result<VideoFrame, PlatformError> {
    if planes.len() < strides.len() || strides.is_empty() {
        return Err(PlatformError::Os {
            call: DQBUF,
            code: None,
            message: format!(
                "a video buffer with {} planes, where its format has {}",
                planes.len(),
                strides.len()
            ),
        });
    }
    match (encoding, strides) {
        (CaptureEncoding::Yuy2, [stride]) => yuv::yuy2_to_nv12(DQBUF, planes[0], *stride, info),
        (
            CaptureEncoding::Raw(
                format @ (PixelFormat::Nv12
                | PixelFormat::I420
                | PixelFormat::Bgra8
                | PixelFormat::Rgba8),
            ),
            [stride],
        ) => yuv::copy_planar(DQBUF, planes[0], *stride, VideoFrameInfo { format, ..info }),
        (CaptureEncoding::Raw(format @ (PixelFormat::Nv12 | PixelFormat::I420)), _) => {
            let separate: Vec<(&[u8], usize)> = planes
                .iter()
                .copied()
                .zip(strides.iter().copied())
                .collect();
            yuv::copy_planes(DQBUF, &separate, VideoFrameInfo { format, ..info })
        }
        (other, _) => Err(PlatformError::Unsupported {
            what: format!("delivering {other:?} frames from V4L2 as they are"),
        }),
    }
}

/// A buffer's time on the media clock (100 ns ticks): its `CLOCK_MONOTONIC` timestamp,
/// when the driver says it is one and it is within a second before `now` and no later
/// than 100 ms after, else `now`. UVC stamps a frame when its first bytes arrive, so the
/// time is the capture's start, slightly before the frame is complete.
pub(crate) fn pts(flags: u32, timestamp: Timeval, now: MediaTime) -> MediaTime {
    let now_ticks = now.ticks();
    let stamped = (flags & BUF_FLAG_TIMESTAMP_MASK == BUF_FLAG_TIMESTAMP_MONOTONIC)
        .then(|| {
            let nanos = timestamp.tv_usec.checked_mul(1000)?;
            hns_from_timespec(timestamp.tv_sec, nanos)
        })
        .flatten()
        .filter(|&t| t <= now_ticks + 1_000_000 && now_ticks - t <= 10_000_000);
    stamped.map_or(now, |t| MediaTime::new(t, now.timebase()))
}

/// The buffer type a node captures video through, with streaming I/O: the single-planar
/// API's when it has it, else the multi-planar one's, else `None`. A UVC webcam has a
/// second node for its metadata, which captures neither.
pub(crate) fn capture_type(cap: &Capability) -> Option<u32> {
    let caps = if cap.capabilities & CAP_DEVICE_CAPS != 0 {
        cap.device_caps
    } else {
        cap.capabilities
    };
    if caps & CAP_STREAMING == 0 {
        None
    } else if caps & CAP_VIDEO_CAPTURE != 0 {
        Some(BUF_TYPE_VIDEO_CAPTURE)
    } else if caps & CAP_VIDEO_CAPTURE_MPLANE != 0 {
        Some(BUF_TYPE_VIDEO_CAPTURE_MPLANE)
    } else {
        None
    }
}

/// A NUL-terminated name from a fixed-size field, lossily as UTF-8.
pub(crate) fn c_name(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).trim().to_owned()
}

/// Whether `id` names a video device node: `/dev/videoN`, or a udev link to one under
/// `/dev/v4l/by-id/` or `/dev/v4l/by-path/`. A device ID can come from a saved
/// collection, so nothing else is opened.
pub(crate) fn is_device_path(id: &str) -> bool {
    if let Some(n) = id.strip_prefix("/dev/video") {
        return !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
    }
    ["/dev/v4l/by-id/", "/dev/v4l/by-path/"].iter().any(|dir| {
        id.strip_prefix(dir).is_some_and(|name| {
            !name.is_empty() && !name.contains('/') && name != ".." && name != "."
        })
    })
}

/// The number of a `videoN` node name, for ordering.
pub(crate) fn node_number(name: &str) -> Option<u32> {
    name.strip_prefix("video")?.parse().ok()
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{ColorRange, ColorSpace, MediaTime, PixelFormat, Timebase};
    use jstream_platform::{CaptureEncoding, PlatformError};

    use super::super::abi::{
        BUF_FLAG_TIMESTAMP_MONOTONIC, BUF_TYPE_VIDEO_CAPTURE, BUF_TYPE_VIDEO_CAPTURE_MPLANE,
        CAP_DEVICE_CAPS, CAP_STREAMING, CAP_VIDEO_CAPTURE, CAP_VIDEO_CAPTURE_MPLANE, Capability,
        Fract, FrmSizeStepwise, PIX_FMT_ABGR32, PIX_FMT_JPEG, PIX_FMT_MJPEG, PIX_FMT_NV12,
        PIX_FMT_NV12M, PIX_FMT_RGBA32, PIX_FMT_RGBX32, PIX_FMT_XBGR32, PIX_FMT_YUV420,
        PIX_FMT_YUV420M, PIX_FMT_YUYV, PixFormat, Timeval, fourcc,
    };
    use super::{
        Enumerated, GpuLayout, buffer_planes, c_name, capture_type, color, encoding, formats,
        fourcc_for, gpu_layout, is_device_path, min_stride, node_number, period, plane_bytes,
        plane_strides, pts, raw_frame, stepwise_sizes,
    };
    use crate::timebase;

    fn fract(n: u32, d: u32) -> Fract {
        Fract {
            numerator: n,
            denominator: d,
        }
    }

    #[test]
    fn delivered_encodings_come_from_their_fourccs() {
        assert_eq!(encoding(PIX_FMT_YUYV), Some(CaptureEncoding::Yuy2));
        assert_eq!(encoding(PIX_FMT_MJPEG), Some(CaptureEncoding::Mjpeg));
        assert_eq!(encoding(PIX_FMT_JPEG), Some(CaptureEncoding::Mjpeg));
        assert_eq!(
            encoding(PIX_FMT_NV12),
            Some(CaptureEncoding::Raw(PixelFormat::Nv12))
        );
        assert_eq!(
            encoding(PIX_FMT_YUV420),
            Some(CaptureEncoding::Raw(PixelFormat::I420))
        );
        assert_eq!(
            encoding(PIX_FMT_NV12M),
            Some(CaptureEncoding::Raw(PixelFormat::Nv12))
        );
        assert_eq!(
            encoding(PIX_FMT_YUV420M),
            Some(CaptureEncoding::Raw(PixelFormat::I420))
        );
        for (rgb, format) in [
            (PIX_FMT_ABGR32, PixelFormat::Bgra8),
            (PIX_FMT_XBGR32, PixelFormat::Bgra8),
            (PIX_FMT_RGBA32, PixelFormat::Rgba8),
            (PIX_FMT_RGBX32, PixelFormat::Rgba8),
        ] {
            assert_eq!(encoding(rgb), Some(CaptureEncoding::Raw(format)));
        }
        assert_eq!(encoding(fourcc(b"H264")), None);
        assert_eq!(encoding(fourcc(b"RGB3")), None);
        // Alpha first (`BA24`, `RA24`) isn't delivered.
        assert_eq!(encoding(fourcc(b"BA24")), None);
    }

    /// The GPU path takes the 32-bit RGB formats, in their byte order; the others open on
    /// the CPU path.
    #[test]
    fn the_gpu_path_takes_the_rgb_formats() {
        assert_eq!(gpu_layout(PIX_FMT_ABGR32), Some(GpuLayout::Bgra));
        assert_eq!(gpu_layout(PIX_FMT_XBGR32), Some(GpuLayout::Bgra));
        assert_eq!(gpu_layout(PIX_FMT_RGBA32), Some(GpuLayout::Rgba));
        assert_eq!(gpu_layout(PIX_FMT_RGBX32), Some(GpuLayout::Rgba));
        for cpu in [PIX_FMT_MJPEG, PIX_FMT_YUV420, PIX_FMT_YUV420M] {
            assert_eq!(gpu_layout(cpu), None);
        }
        assert_eq!(plane_bytes(16, 12, 3), Some(44), "the last row unpadded");
        assert_eq!(plane_bytes(16, 12, 0), Some(0));
        assert_eq!(plane_bytes(usize::MAX, 1, 3), None);
    }

    /// A 32-bit RGB frame is copied at its stride as BGRA or RGBA, and needs four bytes
    /// a pixel.
    #[test]
    fn rgb_frames_are_copied_at_their_stride() {
        let bgra = CaptureEncoding::Raw(PixelFormat::Bgra8);
        assert_eq!(min_stride(bgra, 3), 12);
        assert_eq!(
            plane_strides(bgra, PIX_FMT_XBGR32, 2, &[0]),
            Some(vec![8]),
            "no stride from the driver"
        );
        let info = jstream_platform::media::VideoFrameInfo {
            size: jstream_platform::media::FrameSize::new(2, 2).expect("size"),
            format: PixelFormat::Nv12,
            color: jstream_platform::media::ColorInfo::default(),
            pts: MediaTime::new(0, timebase()),
        };
        let bytes: Vec<u8> = (0..20).collect();
        let frame = raw_frame(bgra, &[&bytes], &[12], info).expect("a frame");
        assert_eq!(frame.info().format, PixelFormat::Bgra8);
        assert_eq!(frame.row(0, 1), Some(&bytes[12..20]));
        assert!(
            raw_frame(bgra, &[&bytes[..19]], &[12], info).is_err(),
            "short"
        );
    }

    #[test]
    fn a_stepwise_range_lists_its_largest_then_reachable_common_sizes() {
        let range = FrmSizeStepwise {
            min_width: 320,
            max_width: 1920,
            step_width: 16,
            min_height: 240,
            max_height: 1080,
            step_height: 8,
        };
        // 1080 - 240 = 840 is a multiple of 8, 540 - 240 = 300 isn't; 360 - 240 = 120 is.
        assert_eq!(
            stepwise_sizes(&range),
            [
                (1920, 1080),
                (1280, 720),
                (800, 600),
                (640, 480),
                (640, 360),
                (320, 240)
            ]
        );
        // A continuous range (step 1) reaches every common size inside it.
        let continuous = FrmSizeStepwise {
            min_width: 2,
            max_width: 1280,
            step_width: 1,
            min_height: 2,
            max_height: 720,
            step_height: 1,
        };
        assert_eq!(
            stepwise_sizes(&continuous)[..3],
            [(1280, 720), (960, 540), (800, 600)]
        );
        assert!(stepwise_sizes(&FrmSizeStepwise::default()).is_empty());
    }

    #[test]
    fn formats_follow_the_driver_without_repeats_or_unknowns() {
        let list = formats(&[
            Enumerated {
                fourcc: PIX_FMT_MJPEG,
                sizes: vec![
                    ((1920, 1080), vec![fract(1, 30), fract(1, 60)]),
                    ((1280, 720), vec![fract(1, 30), fract(0, 30)]),
                ],
            },
            Enumerated {
                fourcc: fourcc(b"H264"),
                sizes: vec![((1920, 1080), vec![fract(1, 30)])],
            },
            // `JPEG` delivers the same as `MJPG`, so it adds nothing new.
            Enumerated {
                fourcc: PIX_FMT_JPEG,
                sizes: vec![((1920, 1080), vec![fract(1, 30)])],
            },
            Enumerated {
                fourcc: PIX_FMT_YUYV,
                sizes: vec![
                    ((640, 480), vec![fract(1001, 30000)]),
                    ((0, 480), vec![fract(1, 30)]),
                ],
            },
        ]);
        let rows: Vec<_> = list
            .iter()
            .map(|f| (f.size.width(), f.size.height(), f.frame_period, f.encoding))
            .collect();
        let tb = |n, d| Timebase::new(n, d).expect("timebase");
        assert_eq!(
            rows,
            [
                (1920, 1080, tb(1, 30), CaptureEncoding::Mjpeg),
                (1920, 1080, tb(1, 60), CaptureEncoding::Mjpeg),
                (1280, 720, tb(1, 30), CaptureEncoding::Mjpeg),
                (640, 480, tb(1001, 30000), CaptureEncoding::Yuy2),
            ]
        );
        assert_eq!(period(fract(1, 0)), None);
    }

    /// The FourCC asked for is one whose own sizes list the format: here 1920x1080 at
    /// 1/30 is offered by `MJPG` and `JPEG` both, and 1280x720 by `JPEG` only. A format
    /// no FourCC lists has none.
    #[test]
    fn the_fourcc_asked_for_lists_the_formats_size() {
        let enumerated = [
            Enumerated {
                fourcc: PIX_FMT_YUYV,
                sizes: vec![((1280, 720), vec![fract(1, 30)])],
            },
            Enumerated {
                fourcc: PIX_FMT_MJPEG,
                sizes: vec![((1920, 1080), vec![fract(1, 30)])],
            },
            Enumerated {
                fourcc: PIX_FMT_JPEG,
                sizes: vec![
                    ((1920, 1080), vec![fract(1, 30)]),
                    ((1280, 720), vec![fract(1, 30)]),
                ],
            },
        ];
        let list = formats(&enumerated);
        let find = |w, h, encoding| {
            list.iter()
                .find(|f| (f.size.width(), f.size.height(), f.encoding) == (w, h, encoding))
                .expect("listed")
        };
        assert_eq!(
            fourcc_for(find(1920, 1080, CaptureEncoding::Mjpeg), &enumerated),
            Some(PIX_FMT_MJPEG)
        );
        assert_eq!(
            fourcc_for(find(1280, 720, CaptureEncoding::Mjpeg), &enumerated),
            Some(PIX_FMT_JPEG)
        );
        assert_eq!(
            fourcc_for(find(1280, 720, CaptureEncoding::Yuy2), &enumerated),
            Some(PIX_FMT_YUYV)
        );
        let unlisted = jstream_platform::VideoDeviceFormat {
            frame_period: Timebase::new(1, 60).expect("timebase"),
            ..*find(1280, 720, CaptureEncoding::Yuy2)
        };
        assert_eq!(fourcc_for(&unlisted, &enumerated), None);
    }

    #[test]
    fn colour_comes_from_the_driver_then_the_colour_space_then_the_height() {
        let pix = |colorspace, ycbcr_enc, quantization, height| PixFormat {
            height,
            colorspace,
            ycbcr_enc,
            quantization,
            ..PixFormat::default()
        };
        let c = color(&pix(3, 1, 1, 1080));
        assert_eq!((c.space, c.range), (ColorSpace::Bt601, ColorRange::Full));
        let c = color(&pix(3, 0, 0, 480));
        assert_eq!((c.space, c.range), (ColorSpace::Bt709, ColorRange::Limited));
        let c = color(&pix(8, 0, 0, 1080));
        assert_eq!((c.space, c.range), (ColorSpace::Bt601, ColorRange::Limited));
        let c = color(&pix(7, 0, 0, 1080));
        assert_eq!((c.space, c.range), (ColorSpace::Bt601, ColorRange::Full));
        let c = color(&pix(0, 0, 0, 480));
        assert_eq!(c.space, ColorSpace::Bt601);
        let c = color(&pix(0, 0, 2, 720));
        assert_eq!((c.space, c.range), (ColorSpace::Bt709, ColorRange::Limited));
    }

    #[test]
    fn raw_frames_become_engine_frames_and_compressed_ones_dont() {
        let info = jstream_platform::media::VideoFrameInfo {
            size: jstream_platform::media::FrameSize::new(4, 2).expect("size"),
            format: PixelFormat::Nv12,
            color: color(&PixFormat::default()),
            pts: MediaTime::new(5, timebase()),
        };
        assert_eq!(min_stride(CaptureEncoding::Yuy2, 5), 12);
        assert_eq!(min_stride(CaptureEncoding::Raw(PixelFormat::Nv12), 5), 5);
        let yuy2 = raw_frame(CaptureEncoding::Yuy2, &[&[9u8; 16]], &[8], info).expect("yuy2");
        assert_eq!(yuy2.info().format, PixelFormat::Nv12);
        let i420 = raw_frame(
            CaptureEncoding::Raw(PixelFormat::I420),
            &[&[1u8; 12]],
            &[4],
            info,
        )
        .expect("i420");
        assert_eq!(i420.info().format, PixelFormat::I420);
        let nv12 = CaptureEncoding::Raw(PixelFormat::Nv12);
        assert!(matches!(
            raw_frame(nv12, &[&[0u8; 11]], &[4], info),
            Err(PlatformError::Os {
                call: "VIDIOC_DQBUF",
                ..
            })
        ));
        assert!(matches!(
            raw_frame(CaptureEncoding::Mjpeg, &[&[0u8; 12]], &[4], info),
            Err(PlatformError::Unsupported { .. })
        ));
        // NV12M: luma and chroma each in their own buffer plane.
        let (luma, chroma) = ([7u8; 8], [3u8; 4]);
        let nv12m = raw_frame(nv12, &[&luma, &chroma], &[4, 4], info).expect("nv12m");
        assert_eq!(nv12m.row(1, 0), Some(&[3, 3, 3, 3][..]));
        assert_eq!(nv12m.row(0, 1), Some(&[7, 7, 7, 7][..]));
        // A buffer with fewer planes than its format is an error, not a panic.
        assert!(matches!(
            raw_frame(nv12, &[&luma], &[4, 4], info),
            Err(PlatformError::Os {
                call: "VIDIOC_DQBUF",
                ..
            })
        ));
        assert!(raw_frame(nv12, &[], &[], info).is_err());
    }

    #[test]
    fn plane_strides_come_from_the_driver_or_the_least_the_rows_need() {
        let nv12 = CaptureEncoding::Raw(PixelFormat::Nv12);
        let i420 = CaptureEncoding::Raw(PixelFormat::I420);
        assert_eq!(
            (buffer_planes(PIX_FMT_NV12M), buffer_planes(PIX_FMT_YUV420M)),
            (2, 3)
        );
        assert_eq!(buffer_planes(PIX_FMT_NV12), 1);
        // One plane: its stride, or the least a row needs.
        assert_eq!(
            plane_strides(CaptureEncoding::Yuy2, PIX_FMT_YUYV, 5, &[0]),
            Some(vec![12])
        );
        assert_eq!(
            plane_strides(nv12, PIX_FMT_NV12, 640, &[704]),
            Some(vec![704])
        );
        // NV12M: the chroma stride as given, or the luma's when missing; never less than
        // a chroma row.
        assert_eq!(
            plane_strides(nv12, PIX_FMT_NV12M, 640, &[704, 768]),
            Some(vec![704, 768])
        );
        assert_eq!(
            plane_strides(nv12, PIX_FMT_NV12M, 640, &[704, 0]),
            Some(vec![704, 704])
        );
        assert_eq!(
            plane_strides(nv12, PIX_FMT_NV12M, 641, &[0, 2]),
            Some(vec![641, 642])
        );
        // YUV420M: chroma at half the luma stride when missing.
        assert_eq!(
            plane_strides(i420, PIX_FMT_YUV420M, 640, &[704, 0, 0]),
            Some(vec![704, 352, 352])
        );
        // The driver's plane count must be the format's.
        assert_eq!(plane_strides(nv12, PIX_FMT_NV12M, 640, &[704]), None);
        assert_eq!(plane_strides(nv12, PIX_FMT_NV12, 640, &[704, 704]), None);
    }

    #[test]
    fn buffer_times_are_used_when_monotonic_and_recent() {
        let now = MediaTime::new(50_000_000, timebase()); // 5 s
        let at = |sec, usec| Timeval {
            tv_sec: sec,
            tv_usec: usec,
        };
        let mono = BUF_FLAG_TIMESTAMP_MONOTONIC;
        assert_eq!(pts(mono, at(4, 900_000), now).ticks(), 49_000_000);
        // Unknown clock, too old, too far ahead, nonsense: `now`.
        assert_eq!(pts(0, at(4, 900_000), now), now);
        assert_eq!(pts(mono, at(3, 0), now), now);
        assert_eq!(pts(mono, at(5, 200_000), now), now);
        assert_eq!(pts(mono, at(4, -1), now), now);
        assert_eq!(pts(mono, at(5, 50_000), now).ticks(), 50_500_000);
    }

    #[test]
    fn capture_nodes_have_capture_and_streaming() {
        let cap = |capabilities, device_caps| Capability {
            driver: [0; 16],
            card: [0; 32],
            bus_info: [0; 32],
            version: 0,
            capabilities,
            device_caps,
            reserved: [0; 3],
        };
        let both = CAP_VIDEO_CAPTURE | CAP_STREAMING;
        let single = Some(BUF_TYPE_VIDEO_CAPTURE);
        assert_eq!(capture_type(&cap(both, 0)), single);
        assert_eq!(capture_type(&cap(both | CAP_DEVICE_CAPS, both)), single);
        // A UVC metadata node: the device can capture, this node can't.
        assert_eq!(
            capture_type(&cap(both | CAP_DEVICE_CAPS, CAP_STREAMING)),
            None
        );
        assert_eq!(capture_type(&cap(CAP_VIDEO_CAPTURE, 0)), None);
        // Multi-planar only, and both (the single-planar API is preferred).
        let mplane = CAP_VIDEO_CAPTURE_MPLANE | CAP_STREAMING;
        assert_eq!(
            capture_type(&cap(mplane | CAP_DEVICE_CAPS, mplane)),
            Some(BUF_TYPE_VIDEO_CAPTURE_MPLANE)
        );
        assert_eq!(capture_type(&cap(mplane | both, 0)), single);
        assert_eq!(capture_type(&cap(CAP_VIDEO_CAPTURE_MPLANE, 0)), None);
    }

    #[test]
    fn names_and_ids() {
        let mut card = [0u8; 32];
        card[..19].copy_from_slice(b"HD Pro Webcam C920 ");
        assert_eq!(c_name(&card), "HD Pro Webcam C920");
        assert_eq!(c_name(b"no terminator"), "no terminator");
        assert!(is_device_path("/dev/video0"));
        assert!(is_device_path("/dev/video12"));
        assert!(is_device_path(
            "/dev/v4l/by-id/usb-046d_HD_Pro_Webcam_C920_AB12-video-index0"
        ));
        assert!(is_device_path(
            "/dev/v4l/by-path/pci-0000:00:14.0-usb-0:1:1.0-video-index0"
        ));
        for bad in [
            "/dev/video",
            "/dev/video0x",
            "/dev/sda",
            "/dev/v4l/by-id/",
            "/dev/v4l/by-id/..",
            "/dev/v4l/by-id/../../sda",
            "/etc/passwd",
            "video0",
        ] {
            assert!(!is_device_path(bad), "{bad}");
        }
        assert_eq!(node_number("video3"), Some(3));
        assert_eq!(node_number("vbi0"), None);
    }
}
