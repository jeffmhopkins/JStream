//! The converter's reference values (Windows' #424 tests), for both destinations.

use crate::PlatformError;
use crate::media::{
    ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, Timebase, VideoFrame,
    VideoFrameInfo,
};

use super::{Nv12Image, RgbOrder, bgra_to_nv12, even_size, rgb_to_nv12, rgb_to_nv12_image};

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
fn rgba_converts_like_bgra() {
    // The left-sited test's image, so every channel of every sample counts.
    let rgb = [(255, 0, 0), (0, 255, 0), (0, 0, 255), (40, 90, 200)];
    let bgra: Vec<u8> = rgb.iter().flat_map(|&(r, g, b)| [b, g, r, 255]).collect();
    let rgba: Vec<u8> = rgb.iter().flat_map(|&(r, g, b)| [r, g, b, 255]).collect();
    let (mut a, mut b) = (frame(4, 2), frame(4, 2));
    bgra_to_nv12(&[bgra.clone(), bgra].concat(), 16, &mut a).expect("BGRA");
    rgb_to_nv12(&[rgba.clone(), rgba].concat(), 16, RgbOrder::Rgba, &mut b).expect("RGBA");
    assert_eq!(a.planes(), b.planes());
    assert_eq!(a.row(0, 0).expect("row"), &[63, 173, 32, 91]);
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

/// Converts `src` into a `w` x `h` image with padded pitches and a gap between the
/// planes (as drivers lay them out), and returns the tight NV12 bytes.
fn convert_image(src: &[u8], stride: usize, order: RgbOrder, w: usize, h: usize) -> Vec<u8> {
    let pitch = w + 16;
    let uv_offset = pitch * (h + 2);
    let mut data = vec![0xEE; uv_offset + pitch * h / 2];
    let mut img = Nv12Image::new(&mut data, [0, uv_offset], [pitch, pitch], w, h).expect("image");
    rgb_to_nv12_image(src, stride, order, &mut img).expect("convert");
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
fn an_image_gets_the_same_pixels_as_a_frame() {
    let (w, h, stride) = (6, 4, 40);
    let mut src = vec![0xEE; stride * h];
    for y in 0..h {
        for x in 0..w {
            let o = y * stride + x * 4;
            let px = [(x * 40) as u8, (y * 60) as u8, (x * y * 10) as u8, 255];
            src[o..o + 4].copy_from_slice(&px);
        }
    }
    let mut f = frame(w as u32, h as u32);
    rgb_to_nv12(&src, stride, RgbOrder::Rgba, &mut f).expect("frame");
    let mut tight = Vec::new();
    for y in 0..h as u32 {
        tight.extend_from_slice(f.row(0, y).expect("row"));
    }
    for y in 0..h as u32 / 2 {
        tight.extend_from_slice(f.row(1, y).expect("row"));
    }
    assert_eq!(convert_image(&src, stride, RgbOrder::Rgba, w, h), tight);
    // The reference red of `known_colours_match_bt709_limited`.
    let red = [255, 0, 0, 255].repeat(4);
    assert_eq!(
        convert_image(&red, 8, RgbOrder::Rgba, 2, 2),
        [63, 63, 63, 63, 102, 240]
    );
}

#[test]
fn images_that_dont_fit_are_refused() {
    let mut data = vec![0; 64];
    // Planes beyond the buffer, or a pitch narrower than the image.
    assert!(Nv12Image::new(&mut data, [0, 60], [4, 4], 4, 4).is_err());
    assert!(Nv12Image::new(&mut data, [0, 16], [2, 4], 4, 4).is_err());
    let mut img = Nv12Image::new(&mut data, [0, 16], [4, 4], 4, 4).expect("image");
    assert_eq!((img.width(), img.height()), (4, 4));
    assert_eq!(img.row_mut(1, 1).map(|r| r.len()), Some(4));
    assert!(img.row_mut(1, 2).is_none());
    assert!(img.row_mut(0, 4).is_none());
    assert!(img.row_mut(2, 0).is_none());
    // A source too short for the image.
    assert!(matches!(
        rgb_to_nv12_image(&[0; 63], 16, RgbOrder::Rgba, &mut img),
        Err(PlatformError::InvalidState { .. })
    ));
}
