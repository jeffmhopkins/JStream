//! A video capture device's raw YUV frame bytes as engine frames, shared by every
//! platform crate: Windows' Media Foundation source reader and Linux's V4L2 hand over the
//! same layouts. NV12 and I420 are copied plane by plane, and packed 4:2:2 (YUY2) is
//! converted to NV12 ([`CaptureEncoding::Yuy2`](crate::CaptureEncoding::Yuy2)).
//!
//! The source's rows are `stride` bytes apart, with the chroma planes following the luma
//! plane: NV12's interleaved plane at the same stride, I420's two planes at half of it.
//! Media Foundation and V4L2 (`bytesperline`) both lay them out so. The output frames are
//! tightly packed, with the size, colour and `pts` the caller gives.

use crate::PlatformError;
use crate::media::{PixelFormat, VideoFrame, VideoFrameInfo};

/// The error for a buffer that is shorter than its format and stride need, naming the
/// OS `call` that handed it over.
fn short(call: &'static str, len: usize, needed: usize) -> PlatformError {
    PlatformError::Os {
        call,
        code: None,
        message: format!("a video buffer of {len} bytes, where its format needs {needed}"),
    }
}

/// `rows` rows of `row_bytes` from `src`, the first at `offset` and each `stride` after
/// the last, or an error if `src` is too short.
fn rows<'a>(
    call: &'static str,
    src: &'a [u8],
    offset: usize,
    stride: usize,
    row_bytes: usize,
    rows: u32,
) -> Result<impl Iterator<Item = &'a [u8]>, PlatformError> {
    let rows = rows as usize;
    let needed = match rows {
        0 => offset,
        n => (n - 1)
            .checked_mul(stride)
            .and_then(|b| b.checked_add(offset))
            .and_then(|b| b.checked_add(row_bytes))
            .ok_or_else(|| short(call, src.len(), usize::MAX))?,
    };
    if src.len() < needed || stride < row_bytes {
        return Err(short(call, src.len(), needed));
    }
    Ok((0..rows).map(move |y| &src[offset + y * stride..][..row_bytes]))
}

/// Copies a planar frame (`info.format` is NV12 or I420) out of `src`, whose luma rows are
/// `stride` bytes apart, with the chroma planes after the luma plane (see the module doc).
///
/// # Errors
/// [`PlatformError::Os`] naming `call` when `src` is shorter than the format and stride
/// need, and the frame's own error for a size it can't hold.
pub fn copy_planar(
    call: &'static str,
    src: &[u8],
    stride: usize,
    info: VideoFrameInfo,
) -> Result<VideoFrame, PlatformError> {
    let mut frame = VideoFrame::zeroed(info)?;
    let mut offset = 0;
    for plane in 0..info.format.plane_count() {
        let Some(geometry) = info.plane(plane)? else {
            continue;
        };
        let plane_stride = match (info.format, plane) {
            (PixelFormat::I420, 1 | 2) => stride.div_ceil(2),
            _ => stride,
        };
        for (y, row) in rows(
            call,
            src,
            offset,
            plane_stride,
            geometry.row_bytes,
            geometry.height,
        )?
        .enumerate()
        {
            let dst = frame
                .row_mut(plane, y as u32)
                .ok_or_else(|| short(call, src.len(), offset))?;
            dst.copy_from_slice(row);
        }
        offset += plane_stride * geometry.height as usize;
    }
    Ok(frame)
}

/// Converts packed 4:2:2 (YUY2: `Y0 U Y1 V` per two pixels, rows `stride` bytes apart) to
/// NV12 with `info`'s size, colour and `pts`. Luma is copied; each NV12 chroma sample
/// averages the two rows of its block (an odd last row stands alone), with the horizontal
/// siting YUY2 already has.
///
/// # Errors
/// [`PlatformError::Os`] naming `call` when `src` is shorter than the size and stride
/// need, and the frame's own error for a size it can't hold.
pub fn yuy2_to_nv12(
    call: &'static str,
    src: &[u8],
    stride: usize,
    info: VideoFrameInfo,
) -> Result<VideoFrame, PlatformError> {
    let info = VideoFrameInfo {
        format: PixelFormat::Nv12,
        ..info
    };
    let (w, h) = (info.size.width() as usize, info.size.height());
    // A YUY2 row always holds whole pixel pairs.
    let row_bytes = w.div_ceil(2) * 4;
    let mut frame = VideoFrame::zeroed(info)?;
    let src_rows: Vec<&[u8]> = rows(call, src, 0, stride, row_bytes, h)?.collect();
    for (y, row) in src_rows.iter().enumerate() {
        let luma = frame
            .row_mut(0, y as u32)
            .ok_or_else(|| short(call, src.len(), 0))?;
        for (x, out) in luma.iter_mut().enumerate() {
            *out = row[x * 2];
        }
    }
    for (cy, pair) in src_rows.chunks(2).enumerate() {
        let chroma = frame
            .row_mut(1, cy as u32)
            .ok_or_else(|| short(call, src.len(), 0))?;
        for (cx, uv) in chroma.chunks_exact_mut(2).enumerate() {
            let (u, v) = (cx * 4 + 1, cx * 4 + 3);
            let avg = |i: usize| -> u8 {
                let sum: u16 = pair.iter().map(|r| u16::from(r[i])).sum();
                let n = pair.len() as u16;
                ((sum + n / 2) / n) as u8
            };
            uv[0] = avg(u);
            uv[1] = avg(v);
        }
    }
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use crate::PlatformError;
    use crate::media::{
        ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, Timebase,
        VideoFrameInfo,
    };

    use super::{copy_planar, yuy2_to_nv12};

    const CALL: &str = "test";

    fn info(w: u32, h: u32, format: PixelFormat) -> VideoFrameInfo {
        VideoFrameInfo {
            size: FrameSize::new(w, h).expect("size"),
            format,
            color: ColorInfo {
                space: ColorSpace::Bt601,
                range: ColorRange::Full,
            },
            pts: MediaTime::new(1234, Timebase::new(1, 10_000_000).expect("tb")),
        }
    }

    #[test]
    fn nv12_planes_are_copied_without_row_padding() {
        // 4x2 NV12 with a stride of 6: two luma rows, then one chroma row.
        let src = [
            1, 2, 3, 4, 0, 0, //
            5, 6, 7, 8, 0, 0, //
            10, 20, 30, 40, 0, 0,
        ];
        let frame = copy_planar(CALL, &src, 6, info(4, 2, PixelFormat::Nv12)).expect("copy");
        assert_eq!(frame.row(0, 0), Some(&[1, 2, 3, 4][..]));
        assert_eq!(frame.row(0, 1), Some(&[5, 6, 7, 8][..]));
        assert_eq!(frame.row(1, 0), Some(&[10, 20, 30, 40][..]));
        assert_eq!(frame.info().pts, info(4, 2, PixelFormat::Nv12).pts);
        assert_eq!(frame.info().color.range, ColorRange::Full);
    }

    #[test]
    fn i420_chroma_planes_use_half_the_stride() {
        // 4x2 I420 with a luma stride of 8, so each chroma plane's stride is 4.
        let src = [
            1, 2, 3, 4, 0, 0, 0, 0, //
            5, 6, 7, 8, 0, 0, 0, 0, //
            11, 12, 0, 0, // U
            21, 22, // V (the last row needs no padding)
        ];
        let frame = copy_planar(CALL, &src, 8, info(4, 2, PixelFormat::I420)).expect("copy");
        assert_eq!(frame.row(0, 1), Some(&[5, 6, 7, 8][..]));
        assert_eq!(frame.row(1, 0), Some(&[11, 12][..]));
        assert_eq!(frame.row(2, 0), Some(&[21, 22][..]));
    }

    #[test]
    fn a_short_buffer_is_an_error_naming_the_call_not_a_panic() {
        let src = [0u8; 4 * 2 + 3];
        assert!(matches!(
            copy_planar("VIDIOC_DQBUF", &src, 4, info(4, 2, PixelFormat::Nv12)),
            Err(PlatformError::Os {
                call: "VIDIOC_DQBUF",
                ..
            })
        ));
        assert!(copy_planar(CALL, &[0u8; 12], 3, info(4, 2, PixelFormat::Nv12)).is_err());
        assert!(yuy2_to_nv12(CALL, &[0u8; 15], 8, info(4, 2, PixelFormat::Nv12)).is_err());
    }

    #[test]
    fn yuy2_luma_is_copied_and_chroma_averages_two_rows() {
        // 4x2 YUY2, stride 10: Y0 U Y1 V per pixel pair.
        let src = [
            1, 100, 2, 200, 3, 50, 4, 60, 0, 0, //
            5, 102, 6, 201, 7, 54, 8, 61, 0, 0,
        ];
        let frame = yuy2_to_nv12(CALL, &src, 10, info(4, 2, PixelFormat::Nv12)).expect("convert");
        assert_eq!(frame.info().format, PixelFormat::Nv12);
        assert_eq!(frame.row(0, 0), Some(&[1, 2, 3, 4][..]));
        assert_eq!(frame.row(0, 1), Some(&[5, 6, 7, 8][..]));
        // (100+102)/2, (200+201)/2 rounded, (50+54)/2, (60+61)/2 rounded.
        assert_eq!(frame.row(1, 0), Some(&[101, 201, 52, 61][..]));
    }

    #[test]
    fn yuy2_with_an_odd_height_keeps_its_last_row_chroma() {
        let src = [
            9, 10, 9, 20, //
            9, 30, 9, 40, //
            9, 77, 9, 88,
        ];
        let frame = yuy2_to_nv12(CALL, &src, 4, info(2, 3, PixelFormat::Nv12)).expect("convert");
        assert_eq!(frame.row(1, 0), Some(&[20, 30][..]));
        assert_eq!(frame.row(1, 1), Some(&[77, 88][..]));
        assert_eq!(frame.row(0, 2), Some(&[9, 9][..]));
    }
}
