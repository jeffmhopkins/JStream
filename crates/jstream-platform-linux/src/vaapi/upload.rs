//! Writing CPU frames into a mapped VA image as NV12, the encoder's input.
//!
//! NV12 frames are copied row by row. RGBA (the compositor's output, which the engine
//! hands the encoder) and BGRA are converted on the way, straight into the image, by
//! `jstream_platform::nv12`: BT.709 limited range with left-sited chroma, the converter
//! Windows' encoder CPU path uses too, so both platforms encode the same pixels.
//!
//! Platform-independent, so it is tested everywhere.

use jstream_platform::PlatformError;
use jstream_platform::media::{PixelFormat, VideoFrame};
pub(crate) use jstream_platform::nv12::Nv12Image;
use jstream_platform::nv12::{RgbOrder, rgb_to_nv12_image};

/// Writes `frame` into `dst` as NV12: copied when it is NV12, converted when it is RGBA
/// or BGRA. The frame must be the image's size.
///
/// # Errors
/// [`PlatformError::Unsupported`] for another pixel format, and
/// [`PlatformError::InvalidState`] when the frame's size isn't the image's.
pub(crate) fn write_nv12(frame: &VideoFrame, dst: &mut Nv12Image<'_>) -> Result<(), PlatformError> {
    let info = frame.info();
    let (width, height) = (dst.width(), dst.height());
    if info.size.width() as usize != width || info.size.height() as usize != height {
        return Err(PlatformError::InvalidState {
            what: "VA-API NV12 upload",
            state: "the frame isn't the encoder's size",
        });
    }
    let order = match info.format {
        PixelFormat::Nv12 => {
            for (plane, rows) in [(0, height), (1, height / 2)] {
                for y in 0..rows {
                    // `VideoFrame` checks its NV12 planes hold `height` and `height / 2`
                    // rows of at least `width` bytes, and `Nv12Image` that its rows are
                    // in its buffer and `width` bytes long.
                    let src = u32::try_from(y).ok().and_then(|y| frame.row(plane, y));
                    if let (Some(src), Some(row)) = (src, dst.row_mut(plane, y)) {
                        row.copy_from_slice(&src[..width]);
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
    rgb_to_nv12_image(plane.data(), plane.stride(), order, dst)
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
