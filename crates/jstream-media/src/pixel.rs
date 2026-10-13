//! Pixel formats, frame sizes and plane geometry.

use crate::MediaError;

/// A frame's width and height in pixels; both are non-zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameSize {
    width: u32,
    height: u32,
}

impl FrameSize {
    /// Creates a `width` x `height` size.
    ///
    /// Odd sizes are allowed: subsampled chroma planes round up, so the last chroma
    /// sample covers a partial block.
    ///
    /// # Errors
    /// [`MediaError::ZeroDimension`] when either dimension is zero.
    pub fn new(width: u32, height: u32) -> Result<Self, MediaError> {
        if width == 0 || height == 0 {
            return Err(MediaError::ZeroDimension { width, height });
        }
        Ok(Self { width, height })
    }

    /// The width in pixels; always greater than zero.
    pub fn width(self) -> u32 {
        self.width
    }

    /// The height in pixels; always greater than zero.
    pub fn height(self) -> u32 {
        self.height
    }
}

/// How a format samples chroma relative to luma.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChromaSubsampling {
    /// Every pixel has its own color: RGB formats.
    None,
    /// 4:2:0. One chroma sample per 2x2 block of luma samples.
    Cs420,
}

impl ChromaSubsampling {
    /// The horizontal and vertical subsampling factors, as `log2`.
    pub fn shifts(self) -> (u32, u32) {
        match self {
            Self::None => (0, 0),
            Self::Cs420 => (1, 1),
        }
    }
}

/// An uncompressed pixel format.
///
/// The set covers capture output (BGRA8), compositing readback (RGBA8) and encoder input
/// (NV12, P010, I420); see docs/ARCHITECTURE.md#gpu-pipeline.
///
/// ```
/// use jstream_media::{FrameSize, PixelFormat};
///
/// let size = FrameSize::new(1920, 1080)?;
/// let uv = PixelFormat::Nv12.plane(1, size)?.expect("NV12 has two planes");
/// assert_eq!((uv.width, uv.height, uv.row_bytes), (960, 540, 1920));
/// # Ok::<(), jstream_media::MediaError>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PixelFormat {
    /// 8-bit 4:2:0: a Y plane, then one plane of interleaved U and V.
    Nv12,
    /// 10-bit 4:2:0 in 16-bit little-endian words (value in the high 10 bits): a Y plane,
    /// then one plane of interleaved U and V.
    P010,
    /// 8-bit 4:2:0 with three separate planes: Y, U, V.
    I420,
    /// 8-bit packed B, G, R, A.
    Bgra8,
    /// 8-bit packed R, G, B, A.
    Rgba8,
}

/// The shape of one plane of a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlaneGeometry {
    /// Samples per row. A sample is a pixel for packed formats, or one luma or chroma
    /// position (possibly holding interleaved components) for planar ones.
    pub width: u32,
    /// Number of rows.
    pub height: u32,
    /// Bytes in one row with no padding; the minimum stride.
    pub row_bytes: usize,
}

impl PixelFormat {
    /// Every format, for exhaustive tests and capability lists.
    pub const ALL: [Self; 5] = [Self::Nv12, Self::P010, Self::I420, Self::Bgra8, Self::Rgba8];

    /// The number of planes.
    pub fn plane_count(self) -> usize {
        match self {
            Self::Nv12 | Self::P010 => 2,
            Self::I420 => 3,
            Self::Bgra8 | Self::Rgba8 => 1,
        }
    }

    /// Whether the format stores luma and chroma (YUV) rather than RGB.
    pub fn is_yuv(self) -> bool {
        !matches!(self, Self::Bgra8 | Self::Rgba8)
    }

    /// How chroma is subsampled.
    pub fn chroma_subsampling(self) -> ChromaSubsampling {
        if self.is_yuv() {
            ChromaSubsampling::Cs420
        } else {
            ChromaSubsampling::None
        }
    }

    /// Bytes per stored component: 2 for P010, 1 for the rest.
    pub fn bytes_per_component(self) -> usize {
        match self {
            Self::P010 => 2,
            _ => 1,
        }
    }

    /// Significant bits per component: 10 for P010, 8 for the rest.
    pub fn bit_depth(self) -> u32 {
        match self {
            Self::P010 => 10,
            _ => 8,
        }
    }

    /// Components stored per sample in `plane`, or `None` when the plane doesn't exist.
    pub fn components(self, plane: usize) -> Option<usize> {
        match (self, plane) {
            (Self::Bgra8 | Self::Rgba8, 0) => Some(4),
            (Self::Nv12 | Self::P010 | Self::I420, 0) => Some(1),
            (Self::Nv12 | Self::P010, 1) => Some(2),
            (Self::I420, 1 | 2) => Some(1),
            _ => None,
        }
    }

    /// The geometry of `plane` for a frame of `size`, or `Ok(None)` when the format has
    /// no such plane. Subsampled planes round odd dimensions up.
    ///
    /// # Errors
    /// [`MediaError::Overflow`] when the row size doesn't fit in `usize`.
    pub fn plane(self, plane: usize, size: FrameSize) -> Result<Option<PlaneGeometry>, MediaError> {
        let Some(components) = self.components(plane) else {
            return Ok(None);
        };
        let (sx, sy) = if plane == 0 {
            (0, 0)
        } else {
            self.chroma_subsampling().shifts()
        };
        let width = size.width.div_ceil(1 << sx);
        let height = size.height.div_ceil(1 << sy);
        let row_bytes = usize::try_from(width)
            .ok()
            .and_then(|w| w.checked_mul(components))
            .and_then(|b| b.checked_mul(self.bytes_per_component()))
            .ok_or(MediaError::Overflow { what: "row size" })?;
        Ok(Some(PlaneGeometry {
            width,
            height,
            row_bytes,
        }))
    }

    /// The bytes needed for a whole frame of `size` with no row padding.
    ///
    /// # Errors
    /// [`MediaError::Overflow`] when the total doesn't fit in `usize`.
    pub fn packed_frame_bytes(self, size: FrameSize) -> Result<usize, MediaError> {
        let overflow = MediaError::Overflow { what: "frame size" };
        let mut total: usize = 0;
        for i in 0..self.plane_count() {
            let Some(g) = self.plane(i, size)? else {
                continue;
            };
            let rows = usize::try_from(g.height).map_err(|_| overflow.clone())?;
            total = g
                .row_bytes
                .checked_mul(rows)
                .and_then(|b| total.checked_add(b))
                .ok_or_else(|| overflow.clone())?;
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::{ChromaSubsampling, FrameSize, PixelFormat, PlaneGeometry};
    use crate::MediaError;

    fn size(w: u32, h: u32) -> FrameSize {
        FrameSize::new(w, h).unwrap()
    }

    fn geom(f: PixelFormat, plane: usize, s: FrameSize) -> PlaneGeometry {
        f.plane(plane, s).unwrap().unwrap()
    }

    #[test]
    fn zero_dimensions_are_rejected() {
        assert_eq!(
            FrameSize::new(0, 1080),
            Err(MediaError::ZeroDimension {
                width: 0,
                height: 1080
            })
        );
        assert!(FrameSize::new(1920, 0).is_err());
    }

    #[test]
    fn plane_counts_and_subsampling() {
        use PixelFormat::*;
        assert_eq!(Nv12.plane_count(), 2);
        assert_eq!(P010.plane_count(), 2);
        assert_eq!(I420.plane_count(), 3);
        assert_eq!(Bgra8.plane_count(), 1);
        assert_eq!(Rgba8.plane_count(), 1);
        assert_eq!(Nv12.chroma_subsampling(), ChromaSubsampling::Cs420);
        assert_eq!(Bgra8.chroma_subsampling(), ChromaSubsampling::None);
        assert_eq!(P010.bit_depth(), 10);
        assert_eq!(P010.bytes_per_component(), 2);
        assert_eq!(Rgba8.bit_depth(), 8);
    }

    #[test]
    fn components_exist_exactly_for_each_plane() {
        for f in PixelFormat::ALL {
            for p in 0..f.plane_count() {
                assert!(f.components(p).is_some(), "{f:?} plane {p}");
            }
            assert_eq!(f.components(f.plane_count()), None, "{f:?}");
            assert_eq!(f.plane(f.plane_count(), size(2, 2)), Ok(None));
        }
    }

    #[test]
    fn nv12_1080p() {
        let s = size(1920, 1080);
        assert_eq!(
            geom(PixelFormat::Nv12, 0, s),
            PlaneGeometry {
                width: 1920,
                height: 1080,
                row_bytes: 1920
            }
        );
        assert_eq!(
            geom(PixelFormat::Nv12, 1, s),
            PlaneGeometry {
                width: 960,
                height: 540,
                row_bytes: 1920
            }
        );
        assert_eq!(
            PixelFormat::Nv12.packed_frame_bytes(s).unwrap(),
            1920 * 1080 * 3 / 2
        );
    }

    #[test]
    fn p010_uses_two_bytes_per_component() {
        let s = size(1920, 1080);
        assert_eq!(geom(PixelFormat::P010, 0, s).row_bytes, 3840);
        assert_eq!(geom(PixelFormat::P010, 1, s).row_bytes, 3840);
        assert_eq!(
            PixelFormat::P010.packed_frame_bytes(s).unwrap(),
            1920 * 1080 * 3
        );
    }

    #[test]
    fn i420_has_two_quarter_size_chroma_planes() {
        let s = size(1280, 720);
        for p in [1, 2] {
            assert_eq!(
                geom(PixelFormat::I420, p, s),
                PlaneGeometry {
                    width: 640,
                    height: 360,
                    row_bytes: 640
                }
            );
        }
    }

    #[test]
    fn rgb_is_one_packed_plane() {
        let s = size(3, 2);
        for f in [PixelFormat::Bgra8, PixelFormat::Rgba8] {
            assert_eq!(
                geom(f, 0, s),
                PlaneGeometry {
                    width: 3,
                    height: 2,
                    row_bytes: 12
                }
            );
            assert_eq!(f.packed_frame_bytes(s).unwrap(), 24);
        }
    }

    #[test]
    fn odd_dimensions_round_chroma_up() {
        let s = size(1919, 1081);
        let y = geom(PixelFormat::Nv12, 0, s);
        assert_eq!((y.width, y.height, y.row_bytes), (1919, 1081, 1919));
        let uv = geom(PixelFormat::Nv12, 1, s);
        assert_eq!((uv.width, uv.height, uv.row_bytes), (960, 541, 1920));
        let u = geom(PixelFormat::I420, 2, s);
        assert_eq!((u.width, u.height, u.row_bytes), (960, 541, 960));
        let uv = geom(PixelFormat::P010, 1, size(1, 1));
        assert_eq!((uv.width, uv.height, uv.row_bytes), (1, 1, 4));
    }

    #[test]
    fn huge_sizes_report_overflow_instead_of_wrapping() {
        let s = size(u32::MAX, u32::MAX);
        // The row fits on 64-bit targets, the whole frame does not.
        if usize::BITS >= 64 {
            assert_eq!(
                geom(PixelFormat::Bgra8, 0, s).row_bytes,
                u32::MAX as usize * 4
            );
        }
        assert_eq!(
            PixelFormat::Bgra8.packed_frame_bytes(s),
            Err(MediaError::Overflow { what: "frame size" })
        );
    }
}
