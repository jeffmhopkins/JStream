//! The contract's format, and the checks on the formats and frames the engine passes,
//! free of Windows calls.

use jstream_platform::media::{FrameSize, PixelFormat, Timebase};
use jstream_platform::{PlatformError, VcamFormat, VideoSurface};
use jstream_vcam_proto::{FPS, HEIGHT, WIDTH};

/// Bytes per BGRA pixel.
const BGRA_BYTES: usize = 4;

/// The one format the contract offers: [`WIDTH`]x[`HEIGHT`] BGRA, one frame per
/// `1/`[`FPS`] s.
pub(crate) fn contract_format() -> Result<VcamFormat, PlatformError> {
    Ok(VcamFormat {
        size: FrameSize::new(WIDTH, HEIGHT)?,
        pixel_format: PixelFormat::Bgra8,
        frame_period: Timebase::per_second(FPS)?,
    })
}

/// Checks that `format` is the contract's ([`contract_format`]).
///
/// # Errors
/// [`PlatformError::Unsupported`], naming the format, for any other.
pub(crate) fn check_format(format: &VcamFormat) -> Result<(), PlatformError> {
    if *format == contract_format()? {
        return Ok(());
    }
    Err(PlatformError::Unsupported {
        what: format!(
            "a {:?} {}x{} virtual camera at {}/{} s per frame (the camera offers BGRA \
             {WIDTH}x{HEIGHT} at 1/{FPS} s)",
            format.pixel_format,
            format.size.width(),
            format.size.height(),
            format.frame_period.num(),
            format.frame_period.den(),
        ),
    })
}

/// A frame's BGRA pixels, checked against the started format: the plane's bytes and the
/// distance between its rows.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pixels<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) stride: usize,
}

/// Checks that `frame` is a system-memory frame of the started `format`, and that its
/// plane holds every row, so a copy of `format`'s size reads only inside it.
///
/// # Errors
/// [`PlatformError::Unsupported`] for a GPU frame (not taken yet), another size or
/// pixel format, or a plane too short for its rows.
pub(crate) fn frame_pixels<'a>(
    frame: &'a VideoSurface,
    format: &VcamFormat,
) -> Result<Pixels<'a>, PlatformError> {
    let VideoSurface::Cpu(frame) = frame else {
        return Err(PlatformError::Unsupported {
            what: "a GPU frame on the virtual camera, which takes system-memory frames for \
                   now"
            .into(),
        });
    };
    let info = frame.info();
    if info.size != format.size || info.format != format.pixel_format {
        return Err(PlatformError::Unsupported {
            what: format!(
                "a {:?} {}x{} frame on a {:?} {}x{} virtual camera",
                info.format,
                info.size.width(),
                info.size.height(),
                format.pixel_format,
                format.size.width(),
                format.size.height()
            ),
        });
    }
    let too_short = || PlatformError::Unsupported {
        what: "a virtual-camera frame whose plane is shorter than its rows".into(),
    };
    let plane = frame.plane(0).ok_or_else(too_short)?;
    let (data, stride) = (plane.data(), plane.stride());
    let width = usize::try_from(info.size.width()).map_err(|_| too_short())?;
    let height = usize::try_from(info.size.height()).map_err(|_| too_short())?;
    let row = width.checked_mul(BGRA_BYTES).ok_or_else(too_short)?;
    let needed = stride
        .checked_mul(height - 1)
        .and_then(|n| n.checked_add(row))
        .ok_or_else(too_short)?;
    if stride < row || data.len() < needed {
        return Err(too_short());
    }
    Ok(Pixels { data, stride })
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{ColorInfo, MediaTime, Plane, VideoFrame, VideoFrameInfo};

    use super::*;

    fn info(width: u32, height: u32, format: PixelFormat) -> VideoFrameInfo {
        VideoFrameInfo {
            size: FrameSize::new(width, height).unwrap(),
            format,
            color: ColorInfo::default(),
            pts: MediaTime::new(7, Timebase::MILLIS),
        }
    }

    #[test]
    fn the_contract_format_is_1080p30_bgra() {
        let f = contract_format().unwrap();
        assert_eq!((f.size.width(), f.size.height()), (1920, 1080));
        assert_eq!(f.pixel_format, PixelFormat::Bgra8);
        assert_eq!(f.frame_period, Timebase::new(1, 30).unwrap());
        assert!(check_format(&f).is_ok());
        // The same period written another way is the same format.
        let same = VcamFormat {
            frame_period: Timebase::new(2, 60).unwrap(),
            ..f
        };
        assert!(check_format(&same).is_ok());
    }

    #[test]
    fn other_formats_are_unsupported() {
        let f = contract_format().unwrap();
        let others = [
            VcamFormat {
                size: FrameSize::new(1280, 720).unwrap(),
                ..f
            },
            VcamFormat {
                pixel_format: PixelFormat::Nv12,
                ..f
            },
            VcamFormat {
                frame_period: Timebase::per_second(60).unwrap(),
                ..f
            },
        ];
        for other in others {
            let err = check_format(&other).unwrap_err();
            assert!(
                matches!(&err, PlatformError::Unsupported { what } if what.contains("1920x1080")),
                "{err}"
            );
        }
    }

    #[test]
    fn a_contract_frame_gives_its_pixels() {
        let f = contract_format().unwrap();
        let frame: VideoSurface = VideoFrame::zeroed(info(1920, 1080, PixelFormat::Bgra8))
            .unwrap()
            .into();
        let px = frame_pixels(&frame, &f).unwrap();
        assert!(px.stride >= 1920 * 4);
        assert!(px.data.len() >= px.stride * 1079 + 1920 * 4);
    }

    #[test]
    fn padded_rows_are_taken() {
        let f = contract_format().unwrap();
        let stride = 1920 * 4 + 64;
        // The last row needs no padding after it.
        let plane = Plane::new(vec![0; stride * 1079 + 1920 * 4], stride);
        let frame: VideoSurface =
            VideoFrame::new(info(1920, 1080, PixelFormat::Bgra8), vec![plane])
                .unwrap()
                .into();
        let px = frame_pixels(&frame, &f).unwrap();
        assert_eq!(px.stride, stride);
    }

    #[test]
    fn mismatched_frames_are_unsupported() {
        let f = contract_format().unwrap();
        for (w, h, format) in [
            (1280, 720, PixelFormat::Bgra8),
            (1920, 1080, PixelFormat::Rgba8),
            (1920, 1080, PixelFormat::Nv12),
        ] {
            let frame: VideoSurface = VideoFrame::zeroed(info(w, h, format)).unwrap().into();
            assert!(matches!(
                frame_pixels(&frame, &f),
                Err(PlatformError::Unsupported { .. })
            ));
        }
    }
}
