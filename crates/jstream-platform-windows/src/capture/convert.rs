//! BGRA to NV12 on the CPU, for the capture fallback path (ARCH › GPU pipeline, step 6).
//!
//! WGC delivers 8-bit BGRA. The H.264 encoder's CPU path takes NV12 in BT.709 limited
//! range (`ColorInfo::default()`), so that is what this produces. The samples are treated
//! as gamma-encoded R'G'B', as every capture and encode pipeline does.
//!
//! Chroma is **left-sited**, the H.264 default (`chroma_sample_location_type` 0, which
//! the encoder leaves unsignalled): each chroma sample sits on its block's left column,
//! between the block's two rows. So it filters each of the two rows with a `[1 2 1]` tap
//! centred on that column (the image's edge repeats its first column), and averages the
//! rows. NV12 needs even dimensions, so the caller crops an odd source to the even size
//! below it ([`even_size`]).
//!
//! Platform-independent, so it is tested on every platform.

use jstream_platform::PlatformError;
use jstream_platform::media::{ColorInfo, PixelFormat, VideoFrame};

/// BT.709 limited-range coefficients in 16.16 fixed point: `Kr`, `Kg`, `Kb` scaled by
/// 219/255 for luma, and the colour-difference weights scaled by 224/255 for chroma.
/// Each chroma row sums to zero, so a grey pixel gets exactly 128.
const Y_R: i32 = 11_966;
const Y_G: i32 = 40_254;
const Y_B: i32 = 4_064;
const U_R: i32 = -6_596;
const U_G: i32 = -22_188;
const U_B: i32 = 28_784;
const V_R: i32 = 28_784;
const V_G: i32 = -26_145;
const V_B: i32 = -2_639;

/// The largest even size within `width` x `height` (an odd dimension loses its last
/// column or row), or `None` when either dimension is below 2.
pub(crate) fn even_size(width: u32, height: u32) -> Option<(u32, u32)> {
    let (w, h) = (width & !1, height & !1);
    (w > 0 && h > 0).then_some((w, h))
}

/// Converts the top-left `width` x `height` pixels of a BGRA image into `frame`, whose
/// size gives `width` and `height`. Rows of `src` are `stride` bytes apart; alpha is
/// ignored.
///
/// # Errors
/// [`PlatformError::Unsupported`] when `frame` isn't NV12 in BT.709 limited range or has
/// an odd dimension, and [`PlatformError::InvalidState`] when `src` or `stride` is too
/// small for the frame.
pub(crate) fn bgra_to_nv12(
    src: &[u8],
    stride: usize,
    frame: &mut VideoFrame,
) -> Result<(), PlatformError> {
    let info = *frame.info();
    if info.format != PixelFormat::Nv12 || info.color != ColorInfo::default() {
        return Err(PlatformError::Unsupported {
            what: format!(
                "BGRA to {:?} in {:?} (the converter writes BT.709 limited NV12)",
                info.format, info.color
            ),
        });
    }
    let (w, h) = (info.size.width(), info.size.height());
    if w % 2 != 0 || h % 2 != 0 {
        return Err(PlatformError::Unsupported {
            what: format!("NV12 at an odd size, {w}x{h}"),
        });
    }
    let row_bytes = w as usize * 4;
    let needed = stride
        .checked_mul(h as usize - 1)
        .and_then(|b| b.checked_add(row_bytes));
    if stride < row_bytes || needed.is_none_or(|n| src.len() < n) {
        return Err(PlatformError::InvalidState {
            what: "BGRA to NV12",
            state: "the source is smaller than the frame",
        });
    }
    let src_row = |y: u32| {
        let start = y as usize * stride;
        &src[start..start + row_bytes]
    };
    for y in 0..h {
        let s = src_row(y);
        if let Some(dst) = frame.row_mut(0, y) {
            for (d, p) in dst.iter_mut().zip(s.chunks_exact(4)) {
                let (b, g, r) = (i32::from(p[0]), i32::from(p[1]), i32::from(p[2]));
                let v = 16 + ((Y_R * r + Y_G * g + Y_B * b + (1 << 15)) >> 16);
                *d = v.clamp(16, 235) as u8;
            }
        }
    }
    for y in 0..h / 2 {
        let (top, bottom) = (src_row(2 * y), src_row(2 * y + 1));
        if let Some(dst) = frame.row_mut(1, y) {
            for (x, d) in dst.chunks_exact_mut(2).enumerate() {
                // Byte offsets of the pixels left of, on and right of the sample's column
                // (2x); the left one repeats column 0 at the image's edge. 2x + 1 < w
                // because w is even.
                let centre = 8 * x;
                let left = centre.saturating_sub(4);
                let right = centre + 4;
                // The [1 2 1] sums of both rows: weights total 8, so the result is
                // shifted by three more bits.
                let sum = |c: usize| {
                    let row = |r: &[u8]| {
                        i32::from(r[left + c])
                            + 2 * i32::from(r[centre + c])
                            + i32::from(r[right + c])
                    };
                    row(top) + row(bottom)
                };
                let (sb, sg, sr) = (sum(0), sum(1), sum(2));
                let u = 128 + ((U_R * sr + U_G * sg + U_B * sb + (1 << 18)) >> 19);
                let v = 128 + ((V_R * sr + V_G * sg + V_B * sb + (1 << 18)) >> 19);
                d[0] = u.clamp(16, 240) as u8;
                d[1] = v.clamp(16, 240) as u8;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{
        ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, Timebase, VideoFrame,
        VideoFrameInfo,
    };

    use super::{bgra_to_nv12, even_size};

    fn frame(w: u32, h: u32) -> VideoFrame {
        VideoFrame::zeroed(VideoFrameInfo {
            size: FrameSize::new(w, h).expect("size"),
            format: PixelFormat::Nv12,
            color: ColorInfo::default(),
            pts: MediaTime::zero(Timebase::NANOS),
        })
        .expect("frame")
    }

    /// A `w` x `h` BGRA image of one colour, with `pad` bytes after each row.
    fn solid(w: usize, h: usize, pad: usize, (r, g, b): (u8, u8, u8)) -> Vec<u8> {
        let mut row: Vec<u8> = [b, g, r, 255].repeat(w);
        row.extend(std::iter::repeat_n(0xEE, pad));
        row.repeat(h)
    }

    fn yuv_of(f: &VideoFrame) -> (u8, u8, u8) {
        let y = f.row(0, 0).expect("Y row")[0];
        let uv = f.row(1, 0).expect("UV row");
        (y, uv[0], uv[1])
    }

    #[test]
    fn known_colours_match_bt709_limited() {
        // The reference values of BT.709 limited range for 8-bit R'G'B'.
        for (rgb, yuv) in [
            ((255, 255, 255), (235, 128, 128)),
            ((0, 0, 0), (16, 128, 128)),
            ((128, 128, 128), (126, 128, 128)),
            ((255, 0, 0), (63, 102, 240)),
            ((0, 255, 0), (173, 42, 26)),
            ((0, 0, 255), (32, 240, 118)),
        ] {
            let mut f = frame(4, 2);
            bgra_to_nv12(&solid(4, 2, 0, rgb), 16, &mut f).expect("convert");
            assert_eq!(yuv_of(&f), yuv, "{rgb:?}");
            // Every sample of a solid image is the same.
            assert!(f.row(0, 1).expect("row").iter().all(|&y| y == yuv.0));
            assert!(
                f.row(1, 0)
                    .expect("row")
                    .chunks_exact(2)
                    .all(|c| c == [yuv.1, yuv.2])
            );
        }
    }

    #[test]
    fn chroma_is_left_sited() {
        // White, except column 1, which is red. Left-sited, each chroma sample filters
        // columns 2x-1, 2x and 2x+1 with [1 2 1] (column -1 repeats column 0), so both
        // samples see one quarter red: three-quarter white plus a quarter red. A 2x2 box
        // average would give the left block half red and the right block none.
        let px = |r: u8, g: u8, b: u8| [b, g, r, 255];
        let row = [
            px(255, 255, 255),
            px(255, 0, 0),
            px(255, 255, 255),
            px(255, 255, 255),
        ]
        .concat();
        let src = [row.clone(), row].concat();
        let mut f = frame(4, 2);
        bgra_to_nv12(&src, 16, &mut f).expect("convert");
        assert_eq!(f.row(0, 0).expect("row"), &[235, 63, 235, 235]);
        // (255, 191.25, 191.25) in BT.709 limited range, rounded as the converter does.
        assert_eq!(f.row(1, 0).expect("row"), &[122, 156, 122, 156]);
    }

    #[test]
    fn chroma_averages_the_two_rows() {
        // Top row black, bottom row white: every chroma sample is the grey between them,
        // so neutral.
        let mut src = solid(4, 1, 0, (0, 0, 0));
        src.extend(solid(4, 1, 0, (255, 255, 255)));
        let mut f = frame(4, 2);
        bgra_to_nv12(&src, 16, &mut f).expect("convert");
        assert_eq!(f.row(1, 0).expect("row"), &[128, 128, 128, 128]);
    }

    #[test]
    fn padded_source_rows_are_skipped() {
        let mut f = frame(2, 4);
        let src = solid(2, 4, 24, (0, 0, 255));
        bgra_to_nv12(&src, 32, &mut f).expect("convert");
        for y in 0..4 {
            assert_eq!(f.row(0, y).expect("row"), &[32, 32]);
        }
        for y in 0..2 {
            assert_eq!(f.row(1, y).expect("row"), &[240, 118]);
        }
        // The last row needs no padding.
        bgra_to_nv12(&src[..32 * 3 + 8], 32, &mut f).expect("unpadded last row");
    }

    #[test]
    fn odd_sources_are_cropped_to_even() {
        assert_eq!(even_size(1920, 1080), Some((1920, 1080)));
        assert_eq!(even_size(1919, 1081), Some((1918, 1080)));
        assert_eq!(even_size(3, 5), Some((2, 4)));
        assert_eq!(even_size(1, 100), None);
        assert_eq!(even_size(100, 1), None);
        assert_eq!(even_size(0, 0), None);
        // A 3x3 source converts its top-left 2x2: the last column and row (green) are
        // never read.
        let mut src = Vec::new();
        for y in 0..3 {
            for x in 0..3 {
                let rgb = if x == 2 || y == 2 {
                    (0, 255, 0)
                } else {
                    (255, 255, 255)
                };
                src.extend([rgb.2, rgb.1, rgb.0, 255]);
            }
        }
        let (w, h) = even_size(3, 3).expect("even");
        let mut f = frame(w, h);
        bgra_to_nv12(&src, 12, &mut f).expect("convert");
        assert_eq!(f.row(0, 0).expect("row"), &[235, 235]);
        assert_eq!(f.row(0, 1).expect("row"), &[235, 235]);
        assert_eq!(f.row(1, 0).expect("row"), &[128, 128]);
    }

    #[test]
    fn bad_inputs_are_refused() {
        let mut f = frame(4, 2);
        // Too short, and a stride below the row size.
        assert!(bgra_to_nv12(&[0; 31], 16, &mut f).is_err());
        assert!(bgra_to_nv12(&[0; 64], 12, &mut f).is_err());
        // Another format, another colour space, an odd size.
        let mut i420 = VideoFrame::zeroed(VideoFrameInfo {
            format: PixelFormat::I420,
            ..*f.info()
        })
        .expect("frame");
        assert!(bgra_to_nv12(&[0; 32], 16, &mut i420).is_err());
        let mut full = VideoFrame::zeroed(VideoFrameInfo {
            color: ColorInfo {
                space: ColorSpace::Bt709,
                range: ColorRange::Full,
            },
            ..*f.info()
        })
        .expect("frame");
        assert!(bgra_to_nv12(&[0; 32], 16, &mut full).is_err());
        let mut odd = frame(3, 2);
        assert!(bgra_to_nv12(&[0; 24], 12, &mut odd).is_err());
    }
}
