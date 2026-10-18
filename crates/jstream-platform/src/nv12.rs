//! BGRA or RGBA to NV12 on the CPU, shared by every platform crate: Windows' capture
//! fallback path (ARCH › GPU pipeline, step 6) and its H.264 encoder's CPU path, and
//! Linux's VA-API upload. One converter, so every platform encodes the same pixels.
//!
//! Capture delivers 8-bit BGRA, and the compositor 8-bit RGBA. The encoders' CPU paths
//! take NV12 in BT.709 limited range (`ColorInfo::default()`), so that is what this
//! produces. The samples are treated as gamma-encoded R'G'B', as every capture and encode
//! pipeline does.
//!
//! Chroma is **left-sited**, the H.264 default (`chroma_sample_location_type` 0, which
//! the encoders leave unsignalled): each chroma sample sits on its block's left column,
//! between the block's two rows. So it filters each of the two rows with a `[1 2 1]` tap
//! centred on that column (the image's edge repeats its first column), and averages the
//! rows. NV12 needs even dimensions, so the caller crops an odd source to the even size
//! below it ([`even_size`]).
//!
//! The destination is either a [`VideoFrame`] ([`rgb_to_nv12`]) or an [`Nv12Image`], NV12
//! laid out in one buffer as a driver maps it ([`rgb_to_nv12_image`]).

use crate::PlatformError;
use crate::media::{ColorInfo, PixelFormat, VideoFrame};

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
#[must_use]
pub fn even_size(width: u32, height: u32) -> Option<(u32, u32)> {
    let (w, h) = (width & !1, height & !1);
    (w > 0 && h > 0).then_some((w, h))
}

/// Where red, green and blue sit in a packed 8-bit pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RgbOrder {
    /// Blue, green, red, alpha: Windows.Graphics.Capture's frames.
    Bgra,
    /// Red, green, blue, alpha: the compositor's frames.
    Rgba,
}

impl RgbOrder {
    /// The byte offsets of red, green and blue in a pixel.
    fn offsets(self) -> [usize; 3] {
        match self {
            Self::Bgra => [2, 1, 0],
            Self::Rgba => [0, 1, 2],
        }
    }
}

/// An NV12 image in one buffer, such as mapped driver memory: the luma and interleaved
/// chroma planes at their offsets, rows `pitch` bytes apart (as `vaDeriveImage` or
/// `vaCreateImage` lays it out on Linux).
#[derive(Debug)]
pub struct Nv12Image<'a> {
    data: &'a mut [u8],
    offsets: [usize; 2],
    pitches: [usize; 2],
    width: usize,
    height: usize,
}

impl<'a> Nv12Image<'a> {
    /// Wraps `data` as a `width` x `height` NV12 image (both even), plane 0 (luma) and
    /// plane 1 (chroma) at `offsets`, rows `pitches` apart.
    ///
    /// # Errors
    /// [`PlatformError::InvalidState`] when a plane doesn't fit `data` or a pitch is
    /// narrower than the image.
    pub fn new(
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
                what: "NV12 image",
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

    /// The width in pixels, which is also each row's length in bytes.
    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    /// The height in pixels: the luma plane's rows (the chroma plane has half).
    #[must_use]
    pub fn height(&self) -> usize {
        self.height
    }

    /// Row `y` of `plane` (0 luma, 1 chroma), `width` bytes, or `None` when either is out
    /// of range.
    pub fn row_mut(&mut self, plane: usize, y: usize) -> Option<&mut [u8]> {
        let rows = match plane {
            0 => self.height,
            1 => self.height / 2,
            _ => return None,
        };
        if y >= rows {
            return None;
        }
        // The constructor checked that every row is in bounds.
        let start = self.offsets[plane] + y * self.pitches[plane];
        self.data.get_mut(start..start + self.width)
    }
}

/// Converts the top-left `width` x `height` pixels of a BGRA image into `frame`, whose
/// size gives `width` and `height`. Rows of `src` are `stride` bytes apart; alpha is
/// ignored.
///
/// # Errors
/// As [`rgb_to_nv12`].
pub fn bgra_to_nv12(
    src: &[u8],
    stride: usize,
    frame: &mut VideoFrame,
) -> Result<(), PlatformError> {
    rgb_to_nv12(src, stride, RgbOrder::Bgra, frame)
}

/// Converts the top-left `width` x `height` pixels of a packed 8-bit image in `order`
/// into `frame`, whose size gives `width` and `height`. Rows of `src` are `stride` bytes
/// apart; alpha is ignored.
///
/// # Errors
/// [`PlatformError::Unsupported`] when `frame` isn't NV12 in BT.709 limited range or has
/// an odd dimension, and [`PlatformError::InvalidState`] when `src` or `stride` is too
/// small for the frame.
pub fn rgb_to_nv12(
    src: &[u8],
    stride: usize,
    order: RgbOrder,
    frame: &mut VideoFrame,
) -> Result<(), PlatformError> {
    let info = *frame.info();
    if info.format != PixelFormat::Nv12 || info.color != ColorInfo::default() {
        return Err(PlatformError::Unsupported {
            what: format!(
                "{order:?} to {:?} in {:?} (the converter writes BT.709 limited NV12)",
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
    convert(src, stride, order, &mut Dst::Frame(frame))
}

/// Converts a packed 8-bit image in `order`, rows `stride` bytes apart, into `dst`, whose
/// size gives the pixels converted. Alpha is ignored.
///
/// # Errors
/// [`PlatformError::InvalidState`] when `src` or `stride` is too small for the image.
pub fn rgb_to_nv12_image(
    src: &[u8],
    stride: usize,
    order: RgbOrder,
    dst: &mut Nv12Image<'_>,
) -> Result<(), PlatformError> {
    convert(src, stride, order, &mut Dst::Image(dst))
}

/// Where [`convert`] writes: the two destinations' rows, behind one accessor.
enum Dst<'f, 'i, 'd> {
    Frame(&'f mut VideoFrame),
    Image(&'i mut Nv12Image<'d>),
}

impl Dst<'_, '_, '_> {
    /// The size in pixels.
    fn size(&self) -> (usize, usize) {
        match self {
            Self::Frame(f) => (
                f.info().size.width() as usize,
                f.info().size.height() as usize,
            ),
            Self::Image(i) => (i.width, i.height),
        }
    }

    /// Row `y` of `plane`, `width` bytes, or `None` when it is out of range.
    fn row_mut(&mut self, plane: usize, y: usize) -> Option<&mut [u8]> {
        match self {
            Self::Frame(f) => f.row_mut(plane, u32::try_from(y).ok()?),
            Self::Image(i) => i.row_mut(plane, y),
        }
    }
}

/// The conversion itself, once the destination is known to be NV12.
fn convert(
    src: &[u8],
    stride: usize,
    order: RgbOrder,
    dst: &mut Dst<'_, '_, '_>,
) -> Result<(), PlatformError> {
    let [ri, gi, bi] = order.offsets();
    let (w, h) = dst.size();
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
        if let Some(row) = dst.row_mut(0, y) {
            for (d, p) in row.iter_mut().zip(s.chunks_exact(4)) {
                let (r, g, b) = (i32::from(p[ri]), i32::from(p[gi]), i32::from(p[bi]));
                let v = 16 + ((Y_R * r + Y_G * g + Y_B * b + (1 << 15)) >> 16);
                *d = v.clamp(16, 235) as u8;
            }
        }
    }
    for y in 0..h / 2 {
        let (top, bottom) = (src_row(2 * y), src_row(2 * y + 1));
        if let Some(row) = dst.row_mut(1, y) {
            for (x, d) in row.chunks_exact_mut(2).enumerate() {
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
                let (sr, sg, sb) = (sum(ri), sum(gi), sum(bi));
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
mod tests;
