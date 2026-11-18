//! Decoded NV12 into the frame format a media-file source asks for: NV12 itself, or
//! packed RGBA or BGRA through the stream's colour matrix and range.
//!
//! The matrix is BT.709 or BT.601, as the stream's VUI says, and when it doesn't, BT.709
//! from 720 lines up and BT.601 below, as players guess. Chroma is upsampled by nearest
//! neighbour, which is what a 4:2:0 frame shown at its own size needs.

use jstream_platform::PlatformError;
use jstream_platform::media::{
    ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, Plane, VideoFrame,
    VideoFrameInfo,
};

/// How a stream's samples map to colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Colour {
    pub(crate) bt709: bool,
    pub(crate) full_range: bool,
}

impl Colour {
    /// From the VUI's `matrix_coefficients` and `video_full_range_flag`, for a picture
    /// `height` lines tall.
    pub(crate) fn from_vui(matrix: Option<u8>, full_range: bool, height: u32) -> Self {
        let bt709 = match matrix {
            Some(1) => true,
            Some(5 | 6) => false,
            _ => height >= 720,
        };
        Self { bt709, full_range }
    }

    /// The frame metadata for NV12 handed on as it is.
    pub(crate) fn info(self) -> ColorInfo {
        ColorInfo {
            space: if self.bt709 {
                ColorSpace::Bt709
            } else {
                ColorSpace::Bt601
            },
            range: if self.full_range {
                ColorRange::Full
            } else {
                ColorRange::Limited
            },
        }
    }

    /// 16.16 fixed-point coefficients: Y scale, then V→R, U→G, V→G, U→B.
    fn coefficients(self) -> [i32; 5] {
        let (kr, kb) = if self.bt709 {
            (0.2126_f64, 0.0722_f64)
        } else {
            (0.299, 0.114)
        };
        let kg = 1.0 - kr - kb;
        let (y_scale, c_scale) = if self.full_range {
            (1.0, 1.0)
        } else {
            (255.0 / 219.0, 255.0 / 224.0)
        };
        let fixed = |v: f64| (v * 65_536.0).round() as i32;
        [
            fixed(y_scale),
            fixed(2.0 * (1.0 - kr) * c_scale),
            fixed(-2.0 * kb * (1.0 - kb) / kg * c_scale),
            fixed(-2.0 * kr * (1.0 - kr) / kg * c_scale),
            fixed(2.0 * (1.0 - kb) * c_scale),
        ]
    }
}

/// An NV12 picture in memory: two planes with their strides.
pub(crate) struct Nv12<'a> {
    pub(crate) y: &'a [u8],
    pub(crate) y_stride: usize,
    pub(crate) uv: &'a [u8],
    pub(crate) uv_stride: usize,
}

/// Builds a frame of `size` from the picture's top-left `(left, top)` corner, in
/// `format` (NV12, RGBA or BGRA).
///
/// # Errors
/// [`PlatformError::Unsupported`] for another format, and [`PlatformError::Media`] when
/// the picture is smaller than the frame.
pub(crate) fn frame(
    src: &Nv12<'_>,
    (left, top): (usize, usize),
    size: FrameSize,
    format: PixelFormat,
    colour: Colour,
    pts: MediaTime,
) -> Result<VideoFrame, PlatformError> {
    let w = size.width() as usize;
    let h = size.height() as usize;
    let short = || PlatformError::Os {
        call: "vaMapBuffer",
        code: None,
        message: "the decoded picture is smaller than its frame".into(),
    };
    // Chroma sits on even positions: start the crop on one.
    let (left, top) = (left & !1, top & !1);
    let y_row = |r: usize| -> Result<&[u8], PlatformError> {
        let at = (top + r) * src.y_stride + left;
        src.y.get(at..at + w).ok_or_else(short)
    };
    let uv_row = |r: usize| -> Result<&[u8], PlatformError> {
        let at = (top / 2 + r / 2) * src.uv_stride + left;
        src.uv.get(at..at + w.div_ceil(2) * 2).ok_or_else(short)
    };
    match format {
        PixelFormat::Nv12 => {
            let mut y = Vec::with_capacity(w * h);
            for r in 0..h {
                y.extend_from_slice(y_row(r)?);
            }
            let cw = w.div_ceil(2) * 2;
            let mut uv = Vec::with_capacity(cw * h.div_ceil(2));
            for r in (0..h).step_by(2) {
                uv.extend_from_slice(uv_row(r)?);
            }
            let info = VideoFrameInfo {
                size,
                format,
                color: colour.info(),
                pts,
            };
            Ok(VideoFrame::new(
                info,
                vec![Plane::new(y, w), Plane::new(uv, cw)],
            )?)
        }
        PixelFormat::Rgba8 | PixelFormat::Bgra8 => {
            let [ys, vr, ug, vg, ub] = colour.coefficients();
            let y_off = if colour.full_range { 0 } else { 16 };
            let (ri, bi) = if format == PixelFormat::Rgba8 {
                (0, 2)
            } else {
                (2, 0)
            };
            let mut out = vec![0_u8; w * h * 4];
            for r in 0..h {
                let yr = y_row(r)?;
                let cr = uv_row(r)?;
                let dst = &mut out[r * w * 4..(r + 1) * w * 4];
                for x in 0..w {
                    let yv = (i32::from(yr[x]) - y_off) * ys;
                    let u = i32::from(cr[x & !1]) - 128;
                    let v = i32::from(cr[(x & !1) + 1]) - 128;
                    let px = &mut dst[x * 4..x * 4 + 4];
                    px[ri] = clamp((yv + vr * v + 32_768) >> 16);
                    px[1] = clamp((yv + ug * u + vg * v + 32_768) >> 16);
                    px[bi] = clamp((yv + ub * u + 32_768) >> 16);
                    px[3] = 255;
                }
            }
            let info = VideoFrameInfo {
                size,
                format,
                color: ColorInfo::default(),
                pts,
            };
            Ok(VideoFrame::new(info, vec![Plane::new(out, w * 4)])?)
        }
        other => Err(PlatformError::Unsupported {
            what: format!("decoding media files to {other:?} on Linux"),
        }),
    }
}

fn clamp(v: i32) -> u8 {
    u8::try_from(v.clamp(0, 255)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{FrameSize, MediaTime, PixelFormat, Timebase};

    use super::{Colour, Nv12, frame};

    fn picture(y: u8, u: u8, v: u8) -> (Vec<u8>, Vec<u8>) {
        (vec![y; 8 * 4], [u, v].repeat(4 * 2))
    }

    fn rgba(y: u8, u: u8, v: u8, colour: Colour) -> [u8; 4] {
        let (luma, chroma) = picture(y, u, v);
        let src = Nv12 {
            y: &luma,
            y_stride: 8,
            uv: &chroma,
            uv_stride: 8,
        };
        let f = frame(
            &src,
            (0, 0),
            FrameSize::new(4, 2).expect("size"),
            PixelFormat::Rgba8,
            colour,
            MediaTime::zero(Timebase::MICROS),
        )
        .expect("converts");
        let p = &f.planes()[0].data()[..4];
        [p[0], p[1], p[2], p[3]]
    }

    #[test]
    fn limited_range_black_white_and_red() {
        let c = Colour::from_vui(None, false, 1080);
        assert!(c.bt709);
        assert_eq!(rgba(16, 128, 128, c), [0, 0, 0, 255]);
        assert_eq!(rgba(235, 128, 128, c), [255, 255, 255, 255]);
        // BT.709 red: Y 63, Cb 102, Cr 240.
        let red = rgba(63, 102, 240, c);
        assert!(red[0] > 250 && red[1] < 5 && red[2] < 5, "{red:?}");
    }

    #[test]
    fn full_range_and_bt601() {
        let c = Colour::from_vui(Some(6), true, 1080);
        assert!(!c.bt709);
        assert_eq!(rgba(0, 128, 128, c), [0, 0, 0, 255]);
        assert_eq!(rgba(255, 128, 128, c), [255, 255, 255, 255]);
        // BT.601 full-range green: Y 150, Cb 44, Cr 21.
        let green = rgba(150, 44, 21, c);
        assert!(green[1] > 250 && green[0] < 5 && green[2] < 5, "{green:?}");
        assert!(!Colour::from_vui(None, false, 480).bt709);
    }

    #[test]
    fn nv12_is_cropped_and_copied() {
        let luma: Vec<u8> = (0..32).collect();
        let chroma: Vec<u8> = (100..116).collect();
        let src = Nv12 {
            y: &luma,
            y_stride: 8,
            uv: &chroma,
            uv_stride: 8,
        };
        let f = frame(
            &src,
            (2, 2),
            FrameSize::new(4, 2).expect("size"),
            PixelFormat::Nv12,
            Colour::from_vui(None, false, 4),
            MediaTime::zero(Timebase::MICROS),
        )
        .expect("copies");
        assert_eq!(f.planes()[0].data(), &[18, 19, 20, 21, 26, 27, 28, 29]);
        assert_eq!(f.planes()[1].data(), &[110, 111, 112, 113]);
    }
}
