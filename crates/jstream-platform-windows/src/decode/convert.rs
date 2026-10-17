//! The Source Reader's numbers in engine terms: packed sizes and rates, 100 ns times, and
//! its RGB32 rows as packed RGBA or BGRA. Platform-independent, so it is tested
//! everywhere.

use jstream_platform::media::{MediaTime, PixelFormat, Rounding, Timebase};

/// Media Foundation's time unit: 100 ns.
pub(crate) fn hns() -> Timebase {
    // 1 / 10^7 s: both parts are non-zero.
    Timebase::new(1, 10_000_000).unwrap_or(Timebase::MICROS)
}

/// A Media Foundation time (100 ns units) as a media time.
pub(crate) fn from_hns(t: i64) -> MediaTime {
    MediaTime::new(t, hns())
}

/// A media time in 100 ns units, rounded down; `None` when it doesn't fit.
pub(crate) fn to_hns(t: MediaTime) -> Option<i64> {
    t.rescale(hns(), Rounding::Down).ok().map(MediaTime::ticks)
}

/// `MF_MT_FRAME_SIZE` and `MF_MT_FRAME_RATE` pack two `u32`s, the first in the high
/// half: width and height, or numerator and denominator.
pub(crate) fn unpack(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, packed as u32)
}

/// The frame period of an `MF_MT_FRAME_RATE` (frames per second as `num / den`), or
/// `None` for a zero rate.
pub(crate) fn frame_period(rate: u64) -> Option<Timebase> {
    let (num, den) = unpack(rate);
    Timebase::new(den, num).ok()
}

/// A host path as Media Foundation's source resolver takes it: without the `\\?\`
/// prefix that `canonicalize` adds on Windows, which the resolver would read as a URL
/// scheme. A verbatim UNC path (`\\?\UNC\server\share`) becomes `\\server\share`.
pub(crate) fn url_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        path.to_owned()
    }
}

/// A rectangle of the decoded picture, in pixels: what is shown of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Area {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl Area {
    /// The whole of a `width` x `height` picture.
    pub(crate) fn whole(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// `MF_MT_MINIMUM_DISPLAY_APERTURE` (offsets and size) as an area of a `width` x
    /// `height` picture, or `None` when it is empty or doesn't fit inside.
    pub(crate) fn aperture(
        (x, y): (i16, i16),
        (w, h): (i32, i32),
        width: u32,
        height: u32,
    ) -> Option<Self> {
        let x = u32::try_from(x).ok()?;
        let y = u32::try_from(y).ok()?;
        let w = u32::try_from(w).ok().filter(|&w| w > 0)?;
        let h = u32::try_from(h).ok().filter(|&h| h > 0)?;
        (x.checked_add(w)? <= width && y.checked_add(h)? <= height).then_some(Self {
            x,
            y,
            width: w,
            height: h,
        })
    }
}

/// Copies `area` of an RGB32 picture (B, G, R and an unused byte per pixel) into packed
/// rows of `format`, top row first, with alpha 255.
///
/// `bytes` holds the picture's rows from the lowest address. `pitch` is the distance
/// from one row to the next one down: negative for a bottom-up picture, whose top row
/// is then the last in `bytes`. `None` when `bytes` is too short for `rows` rows or the
/// format isn't RGBA8 or BGRA8.
pub(crate) fn rgb32(
    bytes: &[u8],
    pitch: i64,
    rows: u32,
    area: Area,
    format: PixelFormat,
) -> Option<Vec<u8>> {
    let swap = match format {
        PixelFormat::Rgba8 => true,
        PixelFormat::Bgra8 => false,
        _ => return None,
    };
    let step = usize::try_from(pitch.unsigned_abs()).ok()?;
    let row_bytes = usize::try_from(area.width).ok()? * 4;
    let mut out = Vec::with_capacity(row_bytes * area.height as usize);
    for y in area.y..area.y.checked_add(area.height)? {
        // The row's index from the lowest address.
        let index = if pitch < 0 {
            rows.checked_sub(y + 1)?
        } else {
            y
        };
        let start = (index as usize)
            .checked_mul(step)?
            .checked_add(area.x as usize * 4)?;
        let row = bytes.get(start..start.checked_add(row_bytes)?)?;
        for px in row.chunks_exact(4) {
            if swap {
                out.extend_from_slice(&[px[2], px[1], px[0], 0xff]);
            } else {
                out.extend_from_slice(&[px[0], px[1], px[2], 0xff]);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x3 RGB32 picture with 12-byte rows (4 bytes of padding): pixel (x, y) is
    /// B = x, G = y, R = 9, and an unused byte of 0.
    fn picture(bottom_up: bool) -> Vec<u8> {
        let mut rows: Vec<Vec<u8>> = (0..3u8)
            .map(|y| {
                let mut row: Vec<u8> = (0..2u8).flat_map(|x| [x, y, 9, 0]).collect();
                row.extend_from_slice(&[0xee; 4]);
                row
            })
            .collect();
        if bottom_up {
            rows.reverse();
        }
        rows.concat()
    }

    #[test]
    fn rgb32_rows_come_out_top_first_with_opaque_alpha() {
        let whole = Area::whole(2, 3);
        let expected: Vec<u8> = (0..3u8)
            .flat_map(|y| (0..2u8).flat_map(move |x| [9, y, x, 0xff]))
            .collect();
        assert_eq!(
            rgb32(&picture(false), 12, 3, whole, PixelFormat::Rgba8),
            Some(expected.clone())
        );
        assert_eq!(
            rgb32(&picture(true), -12, 3, whole, PixelFormat::Rgba8),
            Some(expected),
            "a bottom-up picture"
        );
        let bgra: Vec<u8> = (0..3u8)
            .flat_map(|y| (0..2u8).flat_map(move |x| [x, y, 9, 0xff]))
            .collect();
        assert_eq!(
            rgb32(&picture(false), 12, 3, whole, PixelFormat::Bgra8),
            Some(bgra)
        );
    }

    #[test]
    fn rgb32_crops_to_the_area_and_refuses_short_buffers() {
        let area = Area {
            x: 1,
            y: 1,
            width: 1,
            height: 2,
        };
        assert_eq!(
            rgb32(&picture(true), -12, 3, area, PixelFormat::Rgba8),
            Some(vec![9, 1, 1, 0xff, 9, 2, 1, 0xff])
        );
        let short = &picture(false)[..20];
        assert_eq!(
            rgb32(short, 12, 3, Area::whole(2, 3), PixelFormat::Rgba8),
            None
        );
        assert_eq!(
            rgb32(&picture(false), 12, 3, Area::whole(2, 3), PixelFormat::Nv12),
            None
        );
    }

    #[test]
    fn apertures_must_fit_the_picture() {
        assert_eq!(
            Area::aperture((0, 0), (1920, 1080), 1920, 1088),
            Some(Area::whole(1920, 1080))
        );
        assert_eq!(Area::aperture((0, 8), (1920, 1088), 1920, 1088), None);
        assert_eq!(Area::aperture((-1, 0), (10, 10), 1920, 1088), None);
        assert_eq!(Area::aperture((0, 0), (0, 10), 1920, 1088), None);
    }

    #[test]
    fn verbatim_prefixes_are_dropped() {
        assert_eq!(url_path(r"\\?\C:\clips\a.mp4"), r"C:\clips\a.mp4");
        assert_eq!(url_path(r"\\?\UNC\nas\clips\a.mp4"), r"\\nas\clips\a.mp4");
        assert_eq!(url_path(r"C:\clips\a.mp4"), r"C:\clips\a.mp4");
    }

    #[test]
    fn packed_values_and_times_convert() {
        assert_eq!(unpack((1920 << 32) | 1080), (1920, 1080));
        assert_eq!(
            frame_period((30_000 << 32) | 1001),
            Timebase::new(1001, 30_000).ok()
        );
        assert_eq!(frame_period(1), None, "a zero numerator is no rate");
        let second = from_hns(10_000_000);
        assert_eq!(second, MediaTime::new(1, Timebase::new(1, 1).expect("1 s")));
        assert_eq!(to_hns(MediaTime::new(40, Timebase::MILLIS)), Some(400_000));
    }
}
