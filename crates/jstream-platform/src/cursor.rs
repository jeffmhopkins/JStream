//! The pointer, when a capture sends it beside its frames ([`CursorCapture::Metadata`]):
//! its picture and position, and [`draw_cursor`], which draws it into a CPU frame.

use std::sync::Arc;

use jstream_media::{FrameSize, PixelFormat, VideoFrame};

/// How a capture shows the pointer ([`CaptureOptions`](crate::CaptureOptions)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CursorCapture {
    /// Drawn into the frames by the OS, where it can: what
    /// [`Capture::start`](crate::Capture::start) does.
    #[default]
    Drawn,
    /// Sent beside the frames, read with
    /// [`CaptureStream::cursor`](crate::CaptureStream::cursor), so the consumer draws it
    /// ([`draw_cursor`]) or leaves it out. A backend that can't send it draws it into the
    /// frames instead, and its streams' `cursor` stays [`CaptureCursor::NotSent`].
    Metadata,
}

/// A pointer's picture: RGBA with straight (not premultiplied) alpha, tightly packed,
/// rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorImage {
    size: FrameSize,
    rgba: Vec<u8>,
}

impl CursorImage {
    /// A picture of `size` from `rgba`, or `None` when `rgba` isn't exactly 4 bytes per
    /// pixel.
    pub fn new(size: FrameSize, rgba: Vec<u8>) -> Option<Self> {
        let len = (size.width() as usize)
            .checked_mul(size.height() as usize)?
            .checked_mul(4)?;
        (rgba.len() == len).then_some(Self { size, rgba })
    }

    /// The picture's size in pixels.
    pub fn size(&self) -> FrameSize {
        self.size
    }

    /// The pixels: R, G, B, A per pixel, `size().width() * 4` bytes per row.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

/// The pointer as a stream sends it beside its frames.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CaptureCursor {
    /// The stream sends no pointer: it is drawn into the frames, or not captured. What a
    /// stream without [`CursorCapture::Metadata`] reports.
    #[default]
    NotSent,
    /// Sent, but not over the captured area, or invisible now.
    Hidden,
    /// Over the captured area: `image`'s top-left corner (the pointer's position less its
    /// hot spot) at `(x, y)` in the frame's pixels. It may lie partly outside the frame.
    Shown {
        /// Pixels from the frame's left edge.
        x: i32,
        /// Pixels from the frame's top edge.
        y: i32,
        /// The picture, shared while the pointer's shape doesn't change.
        image: Arc<CursorImage>,
    },
}

/// Draws `cursor` into `frame`, blending its alpha over the frame's pixels and clipping
/// it to the frame. Returns whether anything was drawn: nothing is for a cursor that
/// isn't [`Shown`](CaptureCursor::Shown), one wholly outside the frame, or a frame that
/// isn't `Bgra8` or `Rgba8` (the formats display captures deliver). The frame's alpha is
/// blended too, so an opaque frame stays opaque.
pub fn draw_cursor(frame: &mut VideoFrame, cursor: &CaptureCursor) -> bool {
    let CaptureCursor::Shown { x, y, image } = cursor else {
        return false;
    };
    let bgra = match frame.info().format {
        PixelFormat::Bgra8 => true,
        PixelFormat::Rgba8 => false,
        _ => return false,
    };
    let frame_size = frame.info().size;
    let (fw, fh) = (
        i64::from(frame_size.width()),
        i64::from(frame_size.height()),
    );
    let (cw, ch) = (
        i64::from(image.size.width()),
        i64::from(image.size.height()),
    );
    let (x, y) = (i64::from(*x), i64::from(*y));
    // The part of the picture inside the frame, in the picture's coordinates.
    let (left, top) = ((-x).max(0), (-y).max(0));
    let (right, bottom) = ((fw - x).min(cw), (fh - y).min(ch));
    if left >= right || top >= bottom {
        return false;
    }
    let cols = (right - left) as usize;
    let image_row = cw as usize * 4;
    for row in top..bottom {
        let Ok(frame_y) = u32::try_from(y + row) else {
            continue;
        };
        let Some(dst) = frame.row_mut(0, frame_y) else {
            continue;
        };
        let src_start = row as usize * image_row + left as usize * 4;
        let dst_start = (x + left) as usize * 4;
        let (Some(src), Some(dst)) = (
            image.rgba.get(src_start..src_start + cols * 4),
            dst.get_mut(dst_start..dst_start + cols * 4),
        ) else {
            continue;
        };
        for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
            let a = u32::from(s[3]);
            if a == 0 {
                continue;
            }
            let [r, g, b] = [s[0], s[1], s[2]];
            let colour = if bgra { [b, g, r] } else { [r, g, b] };
            for (dc, sc) in d.iter_mut().zip(colour) {
                *dc = blend(u32::from(sc), u32::from(*dc), a);
            }
            d[3] = blend(255, u32::from(d[3]), a);
        }
    }
    true
}

/// `src` over `dst` at alpha `a` (0..=255), rounded.
fn blend(src: u32, dst: u32, a: u32) -> u8 {
    let v = (src * a + dst * (255 - a) + 127) / 255;
    u8::try_from(v).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jstream_media::{
        ColorInfo, FrameSize, MediaTime, PixelFormat, Timebase, VideoFrame, VideoFrameInfo,
    };

    use super::{CaptureCursor, CursorImage, draw_cursor};

    fn frame(format: PixelFormat, w: u32, h: u32) -> VideoFrame {
        let mut frame = VideoFrame::zeroed(VideoFrameInfo {
            size: FrameSize::new(w, h).expect("size"),
            format,
            color: ColorInfo::default(),
            pts: MediaTime::new(0, Timebase::new(1, 10_000_000).expect("timebase")),
        })
        .expect("frame");
        for y in 0..h {
            for px in frame.row_mut(0, y).expect("row").chunks_exact_mut(4) {
                px.copy_from_slice(&[10, 20, 30, 255]);
            }
        }
        frame
    }

    fn pixel(frame: &VideoFrame, x: u32, y: u32) -> [u8; 4] {
        let row = frame.row(0, y).expect("row");
        let i = x as usize * 4;
        [row[i], row[i + 1], row[i + 2], row[i + 3]]
    }

    /// A 2x2 picture: opaque red, half-transparent white, transparent, opaque blue.
    fn shown(x: i32, y: i32) -> CaptureCursor {
        let rgba = vec![
            255, 0, 0, 255, /**/ 255, 255, 255, 128, //
            0, 255, 0, 0, /**/ 0, 0, 255, 255,
        ];
        let image = CursorImage::new(FrameSize::new(2, 2).expect("size"), rgba).expect("image");
        CaptureCursor::Shown {
            x,
            y,
            image: Arc::new(image),
        }
    }

    #[test]
    fn a_cursor_image_must_hold_four_bytes_a_pixel() {
        let size = FrameSize::new(2, 2).expect("size");
        assert!(CursorImage::new(size, vec![0; 16]).is_some());
        assert!(CursorImage::new(size, vec![0; 15]).is_none());
    }

    #[test]
    fn the_cursor_is_blended_into_rgba_and_bgra_frames() {
        let mut rgba = frame(PixelFormat::Rgba8, 4, 4);
        assert!(draw_cursor(&mut rgba, &shown(1, 1)));
        assert_eq!(pixel(&rgba, 1, 1), [255, 0, 0, 255], "opaque replaces");
        // 128/255 of white over (10, 20, 30).
        assert_eq!(pixel(&rgba, 2, 1), [133, 138, 143, 255], "alpha blends");
        assert_eq!(pixel(&rgba, 1, 2), [10, 20, 30, 255], "transparent leaves");
        assert_eq!(pixel(&rgba, 2, 2), [0, 0, 255, 255]);
        assert_eq!(
            pixel(&rgba, 0, 0),
            [10, 20, 30, 255],
            "outside is untouched"
        );

        let mut bgra = frame(PixelFormat::Bgra8, 4, 4);
        assert!(draw_cursor(&mut bgra, &shown(1, 1)));
        assert_eq!(
            pixel(&bgra, 1, 1),
            [0, 0, 255, 255],
            "red, in B G R A order"
        );
        assert_eq!(
            pixel(&bgra, 2, 2),
            [255, 0, 0, 255],
            "blue, in B G R A order"
        );
    }

    #[test]
    fn the_cursor_is_clipped_to_the_frame() {
        let mut f = frame(PixelFormat::Rgba8, 4, 4);
        // Only the picture's bottom-right pixel (blue) is inside, at (0, 0).
        assert!(draw_cursor(&mut f, &shown(-1, -1)));
        assert_eq!(pixel(&f, 0, 0), [0, 0, 255, 255]);
        assert_eq!(pixel(&f, 1, 0), [10, 20, 30, 255]);
        // Only the top-left pixel (red) is inside, at (3, 3).
        let mut f = frame(PixelFormat::Rgba8, 4, 4);
        assert!(draw_cursor(&mut f, &shown(3, 3)));
        assert_eq!(pixel(&f, 3, 3), [255, 0, 0, 255]);
        // Wholly outside, hidden or not sent: nothing is drawn.
        let mut f = frame(PixelFormat::Rgba8, 4, 4);
        assert!(!draw_cursor(&mut f, &shown(4, 0)));
        assert!(!draw_cursor(&mut f, &shown(-2, 0)));
        assert!(!draw_cursor(&mut f, &shown(i32::MIN, i32::MAX)));
        assert!(!draw_cursor(&mut f, &CaptureCursor::Hidden));
        assert!(!draw_cursor(&mut f, &CaptureCursor::NotSent));
        assert_eq!(f, frame(PixelFormat::Rgba8, 4, 4));
    }

    #[test]
    fn other_formats_are_left_alone() {
        let mut nv12 = VideoFrame::zeroed(VideoFrameInfo {
            size: FrameSize::new(4, 4).expect("size"),
            format: PixelFormat::Nv12,
            color: ColorInfo::default(),
            pts: MediaTime::new(0, Timebase::new(1, 10_000_000).expect("timebase")),
        })
        .expect("frame");
        let before = nv12.clone();
        assert!(!draw_cursor(&mut nv12, &shown(0, 0)));
        assert_eq!(nv12, before);
    }
}
