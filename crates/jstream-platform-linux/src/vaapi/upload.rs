//! Writing CPU frames into a mapped VA image as NV12, the encoder's input.
//!
//! NV12 frames are copied row by row. RGBA (the compositor's output, which the engine
//! hands the encoder) and BGRA are converted on the way, straight into the image:
//! BT.709 limited range with left-sited chroma, the same fixed-point arithmetic as the
//! Windows encoder's CPU path (`jstream-platform-windows`' `capture::convert`), so both
//! platforms encode the same pixels.
//!
//! Platform-independent, so it is tested everywhere.

use jstream_platform::PlatformError;
use jstream_platform::media::{PixelFormat, VideoFrame};

/// BT.709 limited-range coefficients in 16.16 fixed point, as on Windows: `Kr`, `Kg`,
/// `Kb` scaled by 219/255 for luma, and the colour-difference weights by 224/255 for
/// chroma. Each chroma row sums to zero, so grey gets exactly 128.
const Y_R: i32 = 11_966;
const Y_G: i32 = 40_254;
const Y_B: i32 = 4_064;
const U_R: i32 = -6_596;
const U_G: i32 = -22_188;
const U_B: i32 = 28_784;
const V_R: i32 = 28_784;
const V_G: i32 = -26_145;
const V_B: i32 = -2_639;

/// An NV12 image in mapped memory: the luma and interleaved chroma planes at their
/// offsets, rows `pitch` bytes apart, as `vaDeriveImage` or `vaCreateImage` lays it out.
#[derive(Debug)]
pub(crate) struct Nv12Image<'a> {
    data: &'a mut [u8],
    offsets: [usize; 2],
    pitches: [usize; 2],
    width: usize,
    height: usize,
}

impl<'a> Nv12Image<'a> {
    /// Wraps `data` as a `width` x `height` NV12 image (both even).
    ///
    /// # Errors
    /// [`PlatformError::InvalidState`] when a plane doesn't fit `data` or a pitch is
    /// narrower than the image.
    pub(crate) fn new(
        data: &'a mut [u8],
        offsets: [usize; 2],
        pitches: [usize; 2],
        width: usize,
        height: usize,
    ) -> Result<Self, PlatformError> {
        let fits = |plane: usize, rows: usize| {
            let (offset, pitch) = (offsets[plane], pitches[plane]);
            pitch >= width
                && rows
                    .checked_sub(1)
                    .and_then(|r| r.checked_mul(pitch))
                    .and_then(|b| b.checked_add(offset))
                    .and_then(|b| b.checked_add(width))
                    .is_some_and(|end| end <= data.len())
        };
        if !fits(0, height) || !fits(1, height / 2) {
            return Err(PlatformError::InvalidState {
                what: "VA-API NV12 upload",
                state: "the image's planes don't fit its buffer",
            });
        }
        Ok(Self {
            data,
            offsets,
            pitches,
            width,
            height,
        })
    }

    /// Row `y` of `plane` (0 luma, 1 chroma), `width` bytes. The constructor checked
    /// that it is in bounds.
    fn row(&mut self, plane: usize, y: usize) -> &mut [u8] {
        let start = self.offsets[plane] + y * self.pitches[plane];
        &mut self.data[start..start + self.width]
    }
}

/// Where red, green and blue sit in a packed 8-bit pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RgbOrder {
    Rgba,
    Bgra,
}

/// Writes `frame` into `dst` as NV12: copied when it is NV12, converted when it is RGBA
/// or BGRA. The frame must be the image's size.
///
/// # Errors
/// [`PlatformError::Unsupported`] for another pixel format, and
/// [`PlatformError::InvalidState`] when the frame's size isn't the image's.
pub(crate) fn write_nv12(frame: &VideoFrame, dst: &mut Nv12Image<'_>) -> Result<(), PlatformError> {
    let info = frame.info();
    if info.size.width() as usize != dst.width || info.size.height() as usize != dst.height {
        return Err(PlatformError::InvalidState {
            what: "VA-API NV12 upload",
            state: "the frame isn't the encoder's size",
        });
    }
    let order = match info.format {
        PixelFormat::Nv12 => {
            for (plane, rows) in [(0, dst.height), (1, dst.height / 2)] {
                for y in 0..rows {
                    let w = dst.width;
                    // `VideoFrame` checks its NV12 planes hold `height` and `height / 2`
                    // rows of at least `width` bytes.
                    if let Some(src) = u32::try_from(y).ok().and_then(|y| frame.row(plane, y)) {
                        dst.row(plane, y).copy_from_slice(&src[..w]);
                    }
                }
            }
            return Ok(());
        }
        PixelFormat::Rgba8 => RgbOrder::Rgba,
        PixelFormat::Bgra8 => RgbOrder::Bgra,
        other => {
            return Err(PlatformError::Unsupported {
                what: format!("uploading {other:?} as NV12"),
            });
        }
    };
    // One plane: `VideoFrame` checks the plane count and size of a packed format.
    let Some(plane) = frame.plane(0) else {
        return Err(PlatformError::InvalidState {
            what: "VA-API NV12 upload",
            state: "the RGB frame has no plane",
        });
    };
    rgb_to_nv12(plane.data(), plane.stride(), order, dst)
}

/// Converts a packed 8-bit RGB image, rows `stride` bytes apart, into `dst`. Chroma is
/// left-sited: each sample filters its block's two rows with a `[1 2 1]` tap centred on
/// the block's left column (the image's edge repeats column 0), and averages the rows.
fn rgb_to_nv12(
    src: &[u8],
    stride: usize,
    order: RgbOrder,
    dst: &mut Nv12Image<'_>,
) -> Result<(), PlatformError> {
    let [ri, gi, bi] = match order {
        RgbOrder::Rgba => [0, 1, 2],
        RgbOrder::Bgra => [2, 1, 0],
    };
    let (w, h) = (dst.width, dst.height);
    let row_bytes = w * 4;
    let needed = h
        .checked_sub(1)
        .and_then(|r| r.checked_mul(stride))
        .and_then(|b| b.checked_add(row_bytes));
    if stride < row_bytes || needed.is_none_or(|n| src.len() < n) {
        return Err(PlatformError::InvalidState {
            what: "RGB to NV12",
            state: "the source is smaller than the frame",
        });
    }
    let src_row = |y: usize| &src[y * stride..y * stride + row_bytes];
    for y in 0..h {
        let s = src_row(y);
        for (d, p) in dst.row(0, y).iter_mut().zip(s.chunks_exact(4)) {
            let (r, g, b) = (i32::from(p[ri]), i32::from(p[gi]), i32::from(p[bi]));
            let v = 16 + ((Y_R * r + Y_G * g + Y_B * b + (1 << 15)) >> 16);
            *d = v.clamp(16, 235) as u8;
        }
    }
    for y in 0..h / 2 {
        let (top, bottom) = (src_row(2 * y), src_row(2 * y + 1));
        for (x, d) in dst.row(1, y).chunks_exact_mut(2).enumerate() {
            // Byte offsets of the pixels left of, on and right of the sample's column
            // (2x); the left one repeats column 0. 2x + 1 < w because w is even.
            let centre = 8 * x;
            let left = centre.saturating_sub(4);
            let right = centre + 4;
            // The [1 2 1] sums of both rows weigh 8 in all: three more bits of shift.
            let sum = |c: usize| {
                let row = |r: &[u8]| {
                    i32::from(r[left + c]) + 2 * i32::from(r[centre + c]) + i32::from(r[right + c])
                };
                row(top) + row(bottom)
            };
            let (sr, sg, sb) = (sum(ri), sum(gi), sum(bi));
            let u = 128 + ((U_R * sr + U_G * sg + U_B * sb + (1 << 18)) >> 19);
            let v = 128 + ((V_R * sr + V_G * sg + V_B * sb + (1 << 18)) >> 19);
            d[0] = u.clamp(16, 240) as u8;
            d[1] = v.clamp(16, 240) as u8;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use jstream_platform::PlatformError;
    use jstream_platform::media::{
        ColorInfo, FrameSize, MediaTime, PixelFormat, Plane, Timebase, VideoFrame, VideoFrameInfo,
    };

    use super::{Nv12Image, write_nv12};

    fn info(format: PixelFormat, w: u32, h: u32) -> VideoFrameInfo {
        VideoFrameInfo {
            size: FrameSize::new(w, h).expect("size"),
            format,
            color: ColorInfo::default(),
            pts: MediaTime::zero(Timebase::NANOS),
        }
    }

    /// Writes `frame` into a `w` x `h` image with padded pitches and a gap between the
    /// planes (as drivers lay them out), and returns the tight NV12 bytes.
    fn upload(frame: &VideoFrame) -> Vec<u8> {
        let (w, h) = (
            frame.info().size.width() as usize,
            frame.info().size.height() as usize,
        );
        let pitch = w + 16;
        let uv_offset = pitch * (h + 2);
        let mut data = vec![0xEE; uv_offset + pitch * h / 2];
        let mut img =
            Nv12Image::new(&mut data, [0, uv_offset], [pitch, pitch], w, h).expect("image");
        write_nv12(frame, &mut img).expect("upload");
        let mut tight = Vec::new();
        for y in 0..h {
            tight.extend_from_slice(&data[y * pitch..y * pitch + w]);
        }
        for y in 0..h / 2 {
            let s = uv_offset + y * pitch;
            tight.extend_from_slice(&data[s..s + w]);
        }
        tight
    }

    #[test]
    fn copies_nv12_rows_into_the_images_layout() {
        let stride = 8;
        let plane = |p: u8, rows: usize| {
            let mut data = vec![0xEE; stride * rows];
            for y in 0..rows {
                data[y * stride..y * stride + 4].fill(p * 100 + y as u8);
            }
            Plane::new(data, stride)
        };
        let frame = VideoFrame::new(
            info(PixelFormat::Nv12, 4, 4),
            vec![plane(0, 4), plane(1, 2)],
        )
        .expect("frame");
        assert_eq!(
            upload(&frame),
            [
                0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, // Y
                100, 100, 100, 100, 101, 101, 101, 101, // UV
            ]
        );
    }

    fn rgb(
        format: PixelFormat,
        w: u32,
        h: u32,
        stride: usize,
        px: impl Fn(u32, u32) -> [u8; 3],
    ) -> VideoFrame {
        let mut data = vec![0xEE; stride * h as usize];
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = px(x, y);
                let o = y as usize * stride + x as usize * 4;
                let bytes = match format {
                    PixelFormat::Bgra8 => [b, g, r, 255],
                    _ => [r, g, b, 255],
                };
                data[o..o + 4].copy_from_slice(&bytes);
            }
        }
        VideoFrame::new(info(format, w, h), vec![Plane::new(data, stride)]).expect("frame")
    }

    /// The values the Windows encoder's converter produces (its #424 tests), so both
    /// platforms encode the same pixels.
    #[test]
    fn converts_rgba_and_bgra_like_windows() {
        for (value, luma) in [(0, 16), (255, 235), (128, 126)] {
            let out = upload(&rgb(PixelFormat::Rgba8, 4, 2, 16, |_, _| [value; 3]));
            assert_eq!(&out[..8], &[luma; 8], "luma of {value}");
            assert_eq!(&out[8..], &[128; 4], "chroma of {value}");
        }
        let out = upload(&rgb(PixelFormat::Rgba8, 2, 2, 8, |_, _| [255, 0, 0]));
        assert_eq!(out, [63, 63, 63, 63, 102, 240]);
        let px = |x: u32, y: u32| [(x * 40) as u8, (y * 60) as u8, (x * y * 10) as u8];
        let tight = upload(&rgb(PixelFormat::Rgba8, 6, 4, 24, px));
        assert_eq!(upload(&rgb(PixelFormat::Rgba8, 6, 4, 40, px)), tight);
        assert_eq!(upload(&rgb(PixelFormat::Bgra8, 6, 4, 32, px)), tight);
    }

    #[test]
    fn refuses_what_it_cant_upload() {
        let mut data = vec![0; 64];
        // Planes beyond the buffer, or a pitch narrower than the image.
        assert!(Nv12Image::new(&mut data, [0, 60], [4, 4], 4, 4).is_err());
        assert!(Nv12Image::new(&mut data, [0, 16], [2, 4], 4, 4).is_err());
        let mut img = Nv12Image::new(&mut data, [0, 16], [4, 4], 4, 4).expect("image");
        let i420 = VideoFrame::zeroed(info(PixelFormat::I420, 4, 4)).expect("frame");
        assert!(matches!(
            write_nv12(&i420, &mut img),
            Err(PlatformError::Unsupported { .. })
        ));
        let small = VideoFrame::zeroed(info(PixelFormat::Nv12, 2, 2)).expect("frame");
        assert!(matches!(
            write_nv12(&small, &mut img),
            Err(PlatformError::InvalidState { .. })
        ));
    }
}
