//! A video capture device's raw YUV frame bytes as engine frames, shared by every
//! platform crate: Windows' Media Foundation source reader and Linux's V4L2 hand over the
//! same layouts. NV12 and I420 are copied plane by plane, and packed 4:2:2 (YUY2) is
//! converted to NV12 ([`CaptureEncoding::Yuy2`](crate::CaptureEncoding::Yuy2)).
//!
//! The source's rows are `stride` bytes apart, with the chroma planes following the luma
//! plane: NV12's interleaved plane at the same stride, I420's two planes at half of it.
//! Media Foundation and V4L2 (`bytesperline`) both lay them out so. The output frames are
//! tightly packed, with the size, colour and `pts` the caller gives. A copy writes each
//! byte once, and [`copy_planar_into`] writes into the buffers of a frame the caller is
//! done with.
//!
//! A multi-planar source (V4L2's `NV12M` and `YUV420M`) has each plane in its own
//! buffer, with its own stride: [`copy_planes`] takes them one by one.

use crate::PlatformError;
use crate::media::{PixelFormat, Plane, PlaneGeometry, VideoFrame, VideoFrameInfo};

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
/// A one-plane format (`Bgra8`, `Rgba8`, as V4L2's 32-bit RGB formats deliver it) is its
/// rows `stride` bytes apart, copied as they are.
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
    copy_planar_into(call, src, stride, info, None)
}

/// [`copy_planar`] into the buffers of `spare`, a frame the caller is done with
/// ([`CaptureStream::recycle`](crate::CaptureStream::recycle)), when it has any: a buffer
/// that holds the plane is filled again, so a capture at a steady size allocates no
/// plane after its first few frames. `spare`'s size and format don't matter.
///
/// # Errors
/// As for [`copy_planar`].
pub fn copy_planar_into(
    call: &'static str,
    src: &[u8],
    stride: usize,
    info: VideoFrameInfo,
    spare: Option<VideoFrame>,
) -> Result<VideoFrame, PlatformError> {
    let count = info.format.plane_count();
    let mut spare = spare.map(VideoFrame::into_planes).unwrap_or_default();
    // Taken from the end, so each plane gets the buffer it had last time.
    spare.truncate(count);
    spare.resize_with(count, || Plane::new(Vec::new(), 0));
    spare.reverse();
    let mut planes = Vec::with_capacity(count);
    let mut offset = 0;
    for plane in 0..count {
        let buffer = spare.pop().map(Plane::into_data).unwrap_or_default();
        let Some(geometry) = info.plane(plane)? else {
            continue;
        };
        let plane_stride = match (info.format, plane) {
            (PixelFormat::I420, 1 | 2) => stride.div_ceil(2),
            _ => stride,
        };
        planes.push(packed(call, src, offset, plane_stride, geometry, buffer)?);
        offset += plane_stride * geometry.height as usize;
    }
    Ok(VideoFrame::new(info, planes)?)
}

/// One plane's rows copied out of `src` (the first at `offset`, each `stride` after the
/// last) into a tightly packed [`Plane`] that reuses `buffer`'s memory. Each byte is
/// written once: the plane is not zero-filled first, and rows with no padding between
/// them are copied in one piece, so a webcam's 1080p frame costs one pass over its bytes
/// on the capture thread.
fn packed(
    call: &'static str,
    src: &[u8],
    offset: usize,
    stride: usize,
    geometry: PlaneGeometry,
    mut buffer: Vec<u8>,
) -> Result<Plane, PlatformError> {
    let row_bytes = geometry.row_bytes;
    let rows = rows(call, src, offset, stride, row_bytes, geometry.height)?;
    let len = row_bytes * geometry.height as usize;
    buffer.clear();
    buffer.reserve_exact(len);
    if stride == row_bytes {
        // `rows` checked that `src` holds every row.
        buffer.extend_from_slice(&src[offset..offset + len]);
    } else {
        for row in rows {
            buffer.extend_from_slice(row);
        }
    }
    Ok(Plane::new(buffer, row_bytes))
}

/// Copies a planar frame (`info.format` is NV12 or I420) whose planes are each in their
/// own buffer, `planes[i]` holding plane `i` with its rows `stride` bytes apart.
///
/// # Errors
/// [`PlatformError::Os`] naming `call` when a plane is missing or shorter than the format
/// and its stride need, and the frame's own error for a size it can't hold.
pub fn copy_planes(
    call: &'static str,
    planes: &[(&[u8], usize)],
    info: VideoFrameInfo,
) -> Result<VideoFrame, PlatformError> {
    let count = info.format.plane_count();
    if planes.len() < count {
        return Err(PlatformError::Os {
            call,
            code: None,
            message: format!(
                "a video buffer with {} planes, where its format needs {count}",
                planes.len()
            ),
        });
    }
    let mut copied = Vec::with_capacity(count);
    for (plane, &(src, stride)) in planes.iter().enumerate().take(count) {
        let Some(geometry) = info.plane(plane)? else {
            continue;
        };
        copied.push(packed(call, src, 0, stride, geometry, Vec::new())?);
    }
    Ok(VideoFrame::new(info, copied)?)
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

    use super::{copy_planar, copy_planar_into, copy_planes, yuy2_to_nv12};

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

    /// A one-plane RGB frame is copied row by row, its padding left behind.
    #[test]
    fn a_packed_rgb_frame_is_copied_without_row_padding() {
        // 2x2 BGRA with a stride of 12.
        let src = [
            1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, //
            9, 10, 11, 12, 13, 14, 15, 16,
        ];
        let frame = copy_planar(CALL, &src, 12, info(2, 2, PixelFormat::Bgra8)).expect("copy");
        assert_eq!(frame.row(0, 0), Some(&[1, 2, 3, 4, 5, 6, 7, 8][..]));
        assert_eq!(frame.row(0, 1), Some(&[9, 10, 11, 12, 13, 14, 15, 16][..]));
        assert!(copy_planar(CALL, &src[..19], 12, info(2, 2, PixelFormat::Bgra8)).is_err());
    }

    /// A frame handed back is filled again: each plane keeps its buffer, whatever the
    /// spare's size was, as long as the buffer holds the plane.
    #[test]
    fn a_spare_frames_buffers_are_filled_again() {
        let src: Vec<u8> = (0..4 * 3).collect();
        let nv12 = info(4, 2, PixelFormat::Nv12);
        let first = copy_planar_into(CALL, &src, 4, nv12, None).expect("copy");
        let held: Vec<*const u8> = first.planes().iter().map(|p| p.data().as_ptr()).collect();
        let next: Vec<u8> = (100..100 + 4 * 3).collect();
        let second = copy_planar_into(CALL, &next, 4, nv12, Some(first)).expect("copy");
        let now: Vec<*const u8> = second.planes().iter().map(|p| p.data().as_ptr()).collect();
        assert_eq!(now, held, "no new buffer");
        assert_eq!(second.row(0, 1), Some(&[104, 105, 106, 107][..]));
        assert_eq!(second.row(1, 0), Some(&[108, 109, 110, 111][..]));
        // A spare of another format and size is still used, and the result is right.
        let small = [1, 2, 0, 3, 4, 0, 9, 9, 0];
        let third = copy_planar_into(CALL, &small, 3, info(2, 2, PixelFormat::Nv12), Some(second))
            .expect("copy");
        assert_eq!(third.row(0, 1), Some(&[3, 4][..]));
        assert_eq!(third.row(1, 0), Some(&[9, 9][..]));
        let one = copy_planar_into(
            CALL,
            &[7; 16],
            8,
            info(2, 2, PixelFormat::Bgra8),
            Some(third),
        )
        .expect("copy");
        assert_eq!(one.planes().len(), 1);
        assert_eq!(one.row(0, 1), Some(&[7; 8][..]));
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
    fn separate_planes_are_copied_with_their_own_strides() {
        // 4x2 NV12M: luma at stride 6, chroma in its own buffer at stride 8.
        let luma = [
            1, 2, 3, 4, 0, 0, //
            5, 6, 7, 8,
        ];
        let chroma = [10, 20, 30, 40, 0, 0, 0, 0];
        let frame = copy_planes(
            CALL,
            &[(&luma, 6), (&chroma, 8)],
            info(4, 2, PixelFormat::Nv12),
        )
        .expect("copy");
        assert_eq!(frame.row(0, 1), Some(&[5, 6, 7, 8][..]));
        assert_eq!(frame.row(1, 0), Some(&[10, 20, 30, 40][..]));
        // 4x2 YUV420M: three buffers.
        let frame = copy_planes(
            CALL,
            &[(&luma, 6), (&[11, 12][..], 2), (&[21, 22, 0][..], 3)],
            info(4, 2, PixelFormat::I420),
        )
        .expect("copy");
        assert_eq!(frame.row(1, 0), Some(&[11, 12][..]));
        assert_eq!(frame.row(2, 0), Some(&[21, 22][..]));
        // A missing or short plane is an error naming the call.
        for planes in [
            &[(&luma[..], 6)][..],
            &[(&luma[..], 6), (&chroma[..3], 8)][..],
        ] {
            assert!(matches!(
                copy_planes("VIDIOC_DQBUF", planes, info(4, 2, PixelFormat::Nv12)),
                Err(PlatformError::Os {
                    call: "VIDIOC_DQBUF",
                    ..
                })
            ));
        }
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
