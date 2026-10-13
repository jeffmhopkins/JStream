//! CPU-backed video frames.
//!
//! GPU textures never appear here; they belong to the render and platform crates. A
//! [`VideoFrame`] is for CPU paths: readback, upload fallbacks, test patterns and tests.

use crate::{ColorInfo, FrameSize, MediaError, MediaTime, PixelFormat, PlaneGeometry};

/// What a frame is, without its pixels: size, format, color and presentation time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoFrameInfo {
    /// Width and height in pixels.
    pub size: FrameSize,
    /// The pixel format.
    pub format: PixelFormat,
    /// Color space and range.
    pub color: ColorInfo,
    /// Presentation timestamp.
    pub pts: MediaTime,
}

impl VideoFrameInfo {
    /// The geometry of `plane`, or `Ok(None)` when the format has no such plane.
    ///
    /// # Errors
    /// [`MediaError::Overflow`] when the row size doesn't fit in `usize`.
    pub fn plane(&self, plane: usize) -> Result<Option<PlaneGeometry>, MediaError> {
        self.format.plane(plane, self.size)
    }
}

/// One plane's bytes and the distance between the starts of consecutive rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plane {
    data: Vec<u8>,
    stride: usize,
}

impl Plane {
    /// A plane of `data` with rows `stride` bytes apart. It is checked against a format
    /// when it becomes part of a [`VideoFrame`].
    pub fn new(data: Vec<u8>, stride: usize) -> Self {
        Self { data, stride }
    }

    /// The plane's bytes, including any row padding.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// The plane's bytes, mutably.
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Bytes from the start of one row to the start of the next.
    pub fn stride(&self) -> usize {
        self.stride
    }
}

/// A video frame in CPU memory whose planes are checked against its format.
///
/// Invariants, checked by [`VideoFrame::new`]: the plane count matches the format, each
/// stride is at least the plane's row size, and each buffer holds every row (the last
/// row needs no padding).
///
/// ```
/// use jstream_media::{ColorInfo, FrameSize, MediaTime, PixelFormat, Timebase, VideoFrame, VideoFrameInfo};
///
/// let info = VideoFrameInfo {
///     size: FrameSize::new(4, 2)?,
///     format: PixelFormat::Nv12,
///     color: ColorInfo::default(),
///     pts: MediaTime::zero(Timebase::NANOS),
/// };
/// let frame = VideoFrame::zeroed(info)?;
/// assert_eq!(frame.row(1, 0).map(<[u8]>::len), Some(4)); // two interleaved UV pairs
/// assert_eq!(frame.row(1, 1), None); // the chroma plane has one row
/// # Ok::<(), jstream_media::MediaError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoFrame {
    info: VideoFrameInfo,
    planes: Vec<Plane>,
}

impl VideoFrame {
    /// A frame from `planes`, in the format's plane order.
    ///
    /// # Errors
    /// [`MediaError::PlaneCount`], [`MediaError::StrideTooSmall`] or
    /// [`MediaError::PlaneTooSmall`] when a plane doesn't fit the format, and
    /// [`MediaError::Overflow`] when a size doesn't fit in `usize`.
    pub fn new(info: VideoFrameInfo, planes: Vec<Plane>) -> Result<Self, MediaError> {
        let expected = info.format.plane_count();
        if planes.len() != expected {
            return Err(MediaError::PlaneCount {
                format: info.format,
                expected,
                actual: planes.len(),
            });
        }
        for (i, plane) in planes.iter().enumerate() {
            let g = geometry(&info, i)?;
            if plane.stride < g.row_bytes {
                return Err(MediaError::StrideTooSmall {
                    plane: i,
                    stride: plane.stride,
                    min: g.row_bytes,
                });
            }
            let required = required_len(g, plane.stride)?;
            if plane.data.len() < required {
                return Err(MediaError::PlaneTooSmall {
                    plane: i,
                    len: plane.data.len(),
                    required,
                });
            }
        }
        Ok(Self { info, planes })
    }

    /// A frame with tightly packed, zero-filled planes (stride equals row size).
    ///
    /// Zero is black only for RGB formats; for YUV it is a green tint.
    ///
    /// # Errors
    /// [`MediaError::Overflow`] when a plane's size doesn't fit in `usize`.
    pub fn zeroed(info: VideoFrameInfo) -> Result<Self, MediaError> {
        let planes = (0..info.format.plane_count())
            .map(|i| {
                let g = geometry(&info, i)?;
                let len = required_len(g, g.row_bytes)?;
                Ok(Plane::new(vec![0; len], g.row_bytes))
            })
            .collect::<Result<Vec<_>, MediaError>>()?;
        Self::new(info, planes)
    }

    /// The frame's size, format, color and timestamp.
    pub fn info(&self) -> &VideoFrameInfo {
        &self.info
    }

    /// The planes, in the format's plane order.
    pub fn planes(&self) -> &[Plane] {
        &self.planes
    }

    /// Plane `index`, or `None` when the format has no such plane.
    pub fn plane(&self, index: usize) -> Option<&Plane> {
        self.planes.get(index)
    }

    /// Row `y` of `plane` without padding, or `None` when either is out of range.
    pub fn row(&self, plane: usize, y: u32) -> Option<&[u8]> {
        let p = self.planes.get(plane)?;
        let g = geometry(&self.info, plane).ok()?;
        if y >= g.height {
            return None;
        }
        let start = usize::try_from(y).ok()?.checked_mul(p.stride)?;
        p.data.get(start..start.checked_add(g.row_bytes)?)
    }

    /// Row `y` of `plane` without padding, mutably, or `None` when either is out of
    /// range.
    pub fn row_mut(&mut self, plane: usize, y: u32) -> Option<&mut [u8]> {
        let g = geometry(&self.info, plane).ok()?;
        let p = self.planes.get_mut(plane)?;
        if y >= g.height {
            return None;
        }
        let start = usize::try_from(y).ok()?.checked_mul(p.stride)?;
        p.data.get_mut(start..start.checked_add(g.row_bytes)?)
    }

    /// Sets the presentation timestamp.
    pub fn set_pts(&mut self, pts: MediaTime) {
        self.info.pts = pts;
    }

    /// The planes, giving up the frame.
    pub fn into_planes(self) -> Vec<Plane> {
        self.planes
    }
}

/// The geometry of a plane the format is known to have.
fn geometry(info: &VideoFrameInfo, plane: usize) -> Result<PlaneGeometry, MediaError> {
    info.plane(plane)?.ok_or(MediaError::PlaneCount {
        format: info.format,
        expected: info.format.plane_count(),
        actual: plane.saturating_add(1),
    })
}

/// `stride * (height - 1) + row_bytes`: every row, with no padding after the last.
fn required_len(g: PlaneGeometry, stride: usize) -> Result<usize, MediaError> {
    let overflow = MediaError::Overflow { what: "plane size" };
    // `height` is at least 1 because frame sizes are non-zero.
    let rows_before_last =
        usize::try_from(g.height.saturating_sub(1)).map_err(|_| overflow.clone())?;
    stride
        .checked_mul(rows_before_last)
        .and_then(|b| b.checked_add(g.row_bytes))
        .ok_or(overflow)
}

#[cfg(test)]
mod tests {
    use super::{Plane, VideoFrame, VideoFrameInfo};
    use crate::{ColorInfo, FrameSize, MediaError, MediaTime, PixelFormat, Timebase};

    fn info(format: PixelFormat, w: u32, h: u32) -> VideoFrameInfo {
        VideoFrameInfo {
            size: FrameSize::new(w, h).unwrap(),
            format,
            color: ColorInfo::default(),
            pts: MediaTime::new(42, Timebase::NANOS),
        }
    }

    #[test]
    fn zeroed_frames_are_valid_for_every_format_and_odd_sizes() {
        for f in PixelFormat::ALL {
            for (w, h) in [(1, 1), (3, 5), (1920, 1080), (1919, 1081)] {
                let frame = VideoFrame::zeroed(info(f, w, h)).unwrap();
                assert_eq!(frame.planes().len(), f.plane_count());
                let total: usize = frame.planes().iter().map(|p| p.data().len()).sum();
                assert_eq!(total, f.packed_frame_bytes(frame.info().size).unwrap());
            }
        }
    }

    #[test]
    fn padded_strides_are_accepted() {
        let i = info(PixelFormat::Nv12, 1918, 2);
        // Y rows of 1918 bytes padded to 2048; UV rows of 1918 padded to 2048.
        let y = Plane::new(vec![0; 2048 + 1918], 2048);
        let uv = Plane::new(vec![7; 1918], 2048); // one chroma row: no padding needed
        let f = VideoFrame::new(i, vec![y, uv]).unwrap();
        assert_eq!(f.row(0, 1).map(<[u8]>::len), Some(1918));
        assert_eq!(f.row(1, 0), Some(&[7u8; 1918][..]));
    }

    #[test]
    fn wrong_plane_count_is_rejected() {
        let i = info(PixelFormat::I420, 2, 2);
        let err = VideoFrame::new(i, vec![Plane::new(vec![0; 4], 2)]).unwrap_err();
        assert_eq!(
            err,
            MediaError::PlaneCount {
                format: PixelFormat::I420,
                expected: 3,
                actual: 1
            }
        );
    }

    #[test]
    fn short_stride_is_rejected() {
        let i = info(PixelFormat::Bgra8, 3, 2);
        let err = VideoFrame::new(i, vec![Plane::new(vec![0; 64], 11)]).unwrap_err();
        assert_eq!(
            err,
            MediaError::StrideTooSmall {
                plane: 0,
                stride: 11,
                min: 12
            }
        );
    }

    #[test]
    fn short_stride_on_chroma_plane_is_rejected() {
        // 5x3 NV12: chroma is 3x2 samples, 6 bytes per row.
        let i = info(PixelFormat::Nv12, 5, 3);
        let y = Plane::new(vec![0; 15], 5);
        let uv = Plane::new(vec![0; 12], 5);
        assert_eq!(
            VideoFrame::new(i, vec![y, uv]).unwrap_err(),
            MediaError::StrideTooSmall {
                plane: 1,
                stride: 5,
                min: 6
            }
        );
    }

    #[test]
    fn short_buffer_is_rejected() {
        let i = info(PixelFormat::Rgba8, 2, 3);
        // stride 16: needs 16 * 2 + 8 = 40 bytes
        let err = VideoFrame::new(i, vec![Plane::new(vec![0; 39], 16)]).unwrap_err();
        assert_eq!(
            err,
            MediaError::PlaneTooSmall {
                plane: 0,
                len: 39,
                required: 40
            }
        );
        assert!(VideoFrame::new(i, vec![Plane::new(vec![0; 40], 16)]).is_ok());
    }

    #[test]
    fn enormous_stride_reports_overflow() {
        let i = info(PixelFormat::Rgba8, 1, 3);
        let err = VideoFrame::new(i, vec![Plane::new(vec![0; 4], usize::MAX)]).unwrap_err();
        assert_eq!(err, MediaError::Overflow { what: "plane size" });
    }

    #[test]
    fn rows_out_of_range_are_none() {
        let mut f = VideoFrame::zeroed(info(PixelFormat::I420, 3, 3)).unwrap();
        assert!(f.row(0, 2).is_some());
        assert!(f.row(0, 3).is_none());
        assert!(f.row(2, 1).is_some(), "chroma has ceil(3/2) = 2 rows");
        assert!(f.row(2, 2).is_none());
        assert!(f.row(3, 0).is_none());
        assert!(f.row_mut(3, 0).is_none());
        assert!(f.plane(3).is_none());
    }

    #[test]
    fn rows_are_writable_through_stride() {
        let i = info(PixelFormat::Bgra8, 1, 2);
        let mut f = VideoFrame::new(i, vec![Plane::new(vec![0; 12], 8)]).unwrap();
        f.row_mut(0, 1).unwrap().copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(
            f.plane(0).unwrap().data(),
            &[0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4]
        );
        assert_eq!(f.plane(0).unwrap().stride(), 8);
    }

    #[test]
    fn pts_can_be_updated() {
        let mut f = VideoFrame::zeroed(info(PixelFormat::Rgba8, 1, 1)).unwrap();
        f.set_pts(MediaTime::new(1, Timebase::MILLIS));
        assert_eq!(f.info().pts, MediaTime::new(1_000_000, Timebase::NANOS));
        assert_eq!(f.into_planes().len(), 1);
    }
}
