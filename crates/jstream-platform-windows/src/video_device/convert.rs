//! Turning a device's raw frame bytes into engine frames: NV12 and I420 are copied plane
//! by plane, and packed 4:2:2 (YUY2) is converted to NV12, by `jstream_platform::yuv`,
//! the converter Linux's V4L2 backend shares.
//!
//! Media Foundation hands over a buffer whose rows are `stride` bytes apart (the pitch of
//! a 2-D buffer, or the type's default stride), with the chroma planes following the luma
//! plane. The output frames are tightly packed.
//!
//! Platform-independent, so it is tested on every platform.

use jstream_platform::PlatformError;
use jstream_platform::media::{PixelFormat, VideoFrame, VideoFrameInfo};
use jstream_platform::yuv;

/// The call a short buffer is reported against.
const LOCK: &str = "IMFMediaBuffer::Lock";

/// How a raw format's bytes are laid out, and so how they become an engine frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    /// NV12, delivered as is.
    Nv12,
    /// I420 (or IYUV), delivered as is.
    I420,
    /// Packed 4:2:2, converted to NV12.
    Yuy2,
}

impl Layout {
    /// The bytes in one row of the first plane with no padding.
    pub(crate) fn min_stride(self, width: u32) -> usize {
        match self {
            Self::Nv12 | Self::I420 => width as usize,
            Self::Yuy2 => width.div_ceil(2) as usize * 4,
        }
    }

    /// The stride of a buffer: the media type's `MF_MT_DEFAULT_STRIDE` when it gives a
    /// usable top-down one, else the minimum. (A negative default stride means bottom-up
    /// rows, which only RGB types use, and the backend delivers none.)
    pub(crate) fn stride(self, width: u32, default: Option<i32>) -> usize {
        let min = self.min_stride(width);
        default
            .and_then(|s| usize::try_from(s).ok())
            .filter(|&s| s >= min)
            .unwrap_or(min)
    }

    /// The engine frame for `src`, with `info`'s size, colour and `pts` (its format is
    /// set from the layout). A copied layout (NV12, I420) fills `spare`'s buffers again
    /// when it is given a frame the engine handed back, instead of allocating.
    pub(crate) fn frame(
        self,
        src: &[u8],
        stride: usize,
        info: VideoFrameInfo,
        spare: Option<VideoFrame>,
    ) -> Result<VideoFrame, PlatformError> {
        match self {
            Self::Nv12 => yuv::copy_planar_into(
                LOCK,
                src,
                stride,
                VideoFrameInfo {
                    format: PixelFormat::Nv12,
                    ..info
                },
                spare,
            ),
            Self::I420 => yuv::copy_planar_into(
                LOCK,
                src,
                stride,
                VideoFrameInfo {
                    format: PixelFormat::I420,
                    ..info
                },
                spare,
            ),
            Self::Yuy2 => yuv::yuy2_to_nv12(LOCK, src, stride, info),
        }
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::PlatformError;
    use jstream_platform::media::{
        ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, Timebase,
        VideoFrameInfo,
    };

    use super::Layout;

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
    fn strides_come_from_the_type_when_usable() {
        assert_eq!(Layout::Nv12.min_stride(1280), 1280);
        assert_eq!(Layout::Yuy2.min_stride(1280), 2560);
        assert_eq!(Layout::Yuy2.min_stride(5), 12);
        assert_eq!(Layout::Nv12.stride(1280, Some(1536)), 1536);
        assert_eq!(Layout::Nv12.stride(1280, None), 1280);
        assert_eq!(Layout::Nv12.stride(1280, Some(-1280)), 1280);
        assert_eq!(
            Layout::Yuy2.stride(1280, Some(1280)),
            2560,
            "too small to be real"
        );
    }

    #[test]
    fn layouts_deliver_engine_formats() {
        let src = [7u8; 8 * 2];
        let i = info(4, 2, PixelFormat::Bgra8);
        assert_eq!(
            Layout::Yuy2
                .frame(&src, 8, i, None)
                .expect("yuy2")
                .info()
                .format,
            PixelFormat::Nv12
        );
        assert_eq!(
            Layout::Nv12
                .frame(&src[..12], 4, i, None)
                .expect("nv12")
                .info()
                .format,
            PixelFormat::Nv12
        );
        assert_eq!(
            Layout::I420
                .frame(&src[..12], 4, i, None)
                .expect("i420")
                .info()
                .format,
            PixelFormat::I420
        );
    }

    /// A copied layout fills a spare frame's buffers again, so the capture thread
    /// allocates no plane for a frame the engine handed back; YUY2, which is converted,
    /// takes none.
    #[test]
    fn copied_layouts_fill_a_spare_frames_buffers_again() {
        let src: Vec<u8> = (0..12).collect();
        for (layout, format) in [
            (Layout::Nv12, PixelFormat::Nv12),
            (Layout::I420, PixelFormat::I420),
        ] {
            let i = info(4, 2, format);
            let first = layout.frame(&src, 4, i, None).expect("a frame");
            let held: Vec<*const u8> = first.planes().iter().map(|p| p.data().as_ptr()).collect();
            let next: Vec<u8> = (50..62).collect();
            let second = layout.frame(&next, 4, i, Some(first)).expect("a frame");
            let now: Vec<*const u8> = second.planes().iter().map(|p| p.data().as_ptr()).collect();
            assert_eq!(now, held, "{layout:?}: no new buffer");
            assert_eq!(second.row(0, 1), Some(&[54, 55, 56, 57][..]), "{layout:?}");
            assert_eq!(second.info().format, format);
        }
        let spare = Layout::Nv12
            .frame(&src, 4, info(4, 2, PixelFormat::Nv12), None)
            .expect("a frame");
        let yuy2 = Layout::Yuy2
            .frame(&[7u8; 16], 8, info(4, 2, PixelFormat::Nv12), Some(spare))
            .expect("yuy2");
        assert_eq!(yuy2.row(0, 0), Some(&[7, 7, 7, 7][..]));
    }

    #[test]
    fn a_short_buffer_is_reported_against_the_lock() {
        assert!(matches!(
            Layout::Nv12.frame(&[0u8; 11], 4, info(4, 2, PixelFormat::Nv12), None),
            Err(PlatformError::Os {
                call: "IMFMediaBuffer::Lock",
                ..
            })
        ));
    }
}
