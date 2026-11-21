use std::fs::File;
use std::path::{Path, PathBuf};

use super::{Header, MAX_SEGMENTS, Parser, frames};
use crate::media::h264::bits::ParseError;

fn testdata(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/media/testdata")
        .join(name)
}

/// What a test checks of each frame's header, from FFmpeg's `trace_headers` of the
/// same file (`ffmpeg -v trace -i <file> -c copy -bsf:v trace_headers -f null -`).
#[derive(Debug, PartialEq, Eq)]
enum Expected {
    /// A frame to decode.
    F {
        key: bool,
        show: bool,
        intra_only: bool,
        refresh: u8,
        size: (u16, u16),
        filter_level: u8,
        base_q: u8,
        seg: bool,
        tiles: (u8, u8),
        /// The uncompressed and compressed headers' lengths.
        sizes: (usize, u16),
    },
    /// `show_existing_frame`.
    E(usize),
}
use Expected::{E, F};

fn summary(h: &Header) -> Expected {
    match h.show_existing {
        Some(slot) => E(slot),
        None => F {
            key: h.key_frame,
            show: h.show_frame,
            intra_only: h.intra_only,
            refresh: h.refresh_frame_flags,
            size: (h.width, h.height),
            filter_level: h.filter_level,
            base_q: h.base_q_idx,
            seg: h.segmentation.enabled,
            tiles: (h.tile_cols_log2, h.tile_rows_log2),
            sizes: (h.uncompressed_size, h.compressed_size),
        },
    }
}

/// Every frame's header in `name`, in order, superframes split.
fn headers(name: &str) -> Vec<Header> {
    let mut webm = crate::media::mkv::open(File::open(testdata(name)).expect("opens"))
        .expect("reads")
        .expect("is a WebM file");
    let track = webm.video.take().expect("video");
    let mut parser = Parser::new();
    let mut out = Vec::new();
    for sample in &track.samples {
        let data = webm.read(&track, sample).expect("reads");
        for frame in frames(&data) {
            out.push(parser.parse(frame).expect("parses"));
        }
    }
    out
}

fn check(name: &str, expected: &[Expected]) {
    let got: Vec<Expected> = headers(name).iter().map(summary).collect();
    assert_eq!(got, expected, "{name}");
}

#[test]
fn reads_a_two_pass_stream_with_a_hidden_alternate_reference() {
    // Two-pass libvpx: a hidden alternate reference in a superframe.
    check(
        "vp9-altref.webm",
        &[
            F {
                key: true,
                show: true,
                intra_only: false,
                refresh: 255,
                size: (96, 64),
                filter_level: 2,
                base_q: 61,
                seg: false,
                tiles: (0, 0),
                sizes: (18, 57),
            },
            F {
                key: false,
                show: false,
                intra_only: false,
                refresh: 4,
                size: (96, 64),
                filter_level: 7,
                base_q: 96,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 27),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 9,
                base_q: 186,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 9,
                base_q: 186,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 5,
                base_q: 186,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 9,
                base_q: 189,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 9,
                base_q: 189,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 5,
                base_q: 189,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 5,
                base_q: 189,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 3,
                base_q: 189,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 5,
                base_q: 189,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 6,
                base_q: 204,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 1,
                base_q: 204,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 9,
                base_q: 204,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 5,
                base_q: 204,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 4,
                size: (96, 64),
                filter_level: 0,
                base_q: 206,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 8,
                base_q: 199,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 0,
                base_q: 202,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 7,
                base_q: 203,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 7,
                base_q: 206,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 8,
                base_q: 200,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 12,
                base_q: 206,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 9,
                base_q: 206,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 10,
                base_q: 206,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 10,
                base_q: 206,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 10,
                base_q: 204,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 10,
                base_q: 206,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 10,
                base_q: 206,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 10,
                base_q: 206,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 11,
                base_q: 206,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 11,
                base_q: 206,
                seg: false,
                tiles: (0, 0),
                sizes: (10, 4),
            },
        ],
    );
}

#[test]
fn reads_segmentation_and_tiles() {
    // Cyclic refresh (segmentation) and two tile columns.
    check(
        "vp9-seg-tiles.webm",
        &[
            F {
                key: true,
                show: true,
                intra_only: false,
                refresh: 255,
                size: (544, 64),
                filter_level: 1,
                base_q: 33,
                seg: false,
                tiles: (1, 0),
                sizes: (18, 134),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (544, 64),
                filter_level: 4,
                base_q: 74,
                seg: true,
                tiles: (1, 0),
                sizes: (26, 41),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (544, 64),
                filter_level: 10,
                base_q: 171,
                seg: true,
                tiles: (1, 0),
                sizes: (26, 5),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (544, 64),
                filter_level: 12,
                base_q: 192,
                seg: true,
                tiles: (1, 0),
                sizes: (23, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (544, 64),
                filter_level: 10,
                base_q: 199,
                seg: true,
                tiles: (1, 0),
                sizes: (26, 10),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (544, 64),
                filter_level: 10,
                base_q: 202,
                seg: true,
                tiles: (1, 0),
                sizes: (26, 6),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (544, 64),
                filter_level: 10,
                base_q: 201,
                seg: true,
                tiles: (1, 0),
                sizes: (23, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (544, 64),
                filter_level: 9,
                base_q: 203,
                seg: true,
                tiles: (1, 0),
                sizes: (23, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (544, 64),
                filter_level: 6,
                base_q: 203,
                seg: true,
                tiles: (1, 0),
                sizes: (26, 3),
            },
        ],
    );
}

#[test]
fn reads_error_resilient_frames() {
    // Error-resilient frames: no context refresh, and past independence on each.
    check(
        "vp9-er.webm",
        &[
            F {
                key: true,
                show: true,
                intra_only: false,
                refresh: 255,
                size: (96, 64),
                filter_level: 0,
                base_q: 33,
                seg: false,
                tiles: (0, 0),
                sizes: (18, 73),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 2,
                base_q: 74,
                seg: false,
                tiles: (0, 0),
                sizes: (13, 9),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 9,
                base_q: 171,
                seg: false,
                tiles: (0, 0),
                sizes: (13, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 2,
                base_q: 174,
                seg: false,
                tiles: (0, 0),
                sizes: (13, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 10,
                base_q: 174,
                seg: false,
                tiles: (0, 0),
                sizes: (13, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 10,
                base_q: 177,
                seg: false,
                tiles: (0, 0),
                sizes: (13, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 6,
                base_q: 185,
                seg: false,
                tiles: (0, 0),
                sizes: (13, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 10,
                base_q: 186,
                seg: false,
                tiles: (0, 0),
                sizes: (13, 3),
            },
            F {
                key: false,
                show: true,
                intra_only: false,
                refresh: 1,
                size: (96, 64),
                filter_level: 3,
                base_q: 187,
                seg: false,
                tiles: (0, 0),
                sizes: (13, 3),
            },
        ],
    );
}

#[test]
fn the_hidden_frame_shares_a_superframe_with_the_one_shown() {
    let mut webm = crate::media::mkv::open(File::open(testdata("vp9-altref.webm")).expect("opens"))
        .expect("reads")
        .expect("is a WebM file");
    let track = webm.video.take().expect("video");
    let mut counts = Vec::new();
    for sample in &track.samples {
        counts.push(frames(&webm.read(&track, sample).expect("reads")).len());
    }
    assert_eq!(counts.iter().filter(|&&n| n == 2).count(), 1, "{counts:?}");
    assert_eq!(counts.iter().sum::<usize>(), 31);
}

#[test]
fn splits_a_superframe_by_its_index() {
    // Two frames of 3 and 2 bytes, one-byte sizes: marker 0b110_00_001.
    let marker = 0xC1;
    let sample = [0xAA, 0xAA, 0xAA, 0xBB, 0xBB, marker, 3, 2, marker];
    assert_eq!(frames(&sample), vec![&[0xAA; 3][..], &[0xBB; 2][..]]);
    // An index whose first byte doesn't match is no index: the sample is one frame.
    let lone = [0xAA, 0xAA, 0x00, 3, 2, marker];
    assert_eq!(frames(&lone), vec![&lone[..]]);
    // Sizes past the index keep what fits.
    let short = [0xAA, marker, 1, 9, marker];
    assert_eq!(frames(&short), vec![&[0xAA][..]]);
    assert!(frames(&[]).is_empty());
}

#[test]
fn an_inter_frame_before_a_key_frame_is_refused() {
    // The second frame of a stream, parsed on its own, refers to empty slots.
    let mut webm = crate::media::mkv::open(File::open(testdata("vp9-er.webm")).expect("opens"))
        .expect("reads")
        .expect("is a WebM file");
    let track = webm.video.take().expect("video");
    let second = webm.read(&track, &track.samples[1]).expect("reads");
    assert!(matches!(
        Parser::new().parse(&second),
        Err(ParseError::Unsupported(_))
    ));
    assert!(matches!(
        Parser::new().parse(&[0x00, 0x00]),
        Err(ParseError::Invalid(_))
    ));
    assert_eq!(Parser::new().parse(&[0x82]), Err(ParseError::Truncated));
}

#[test]
fn each_segment_gets_its_quantizers_and_filter_levels() {
    let headers = headers("vp9-seg-tiles.webm");
    let h = headers
        .iter()
        .find(|h| h.segmentation.enabled)
        .expect("a segmented frame");
    let segments = h.segments();
    // Segment 0 of cyclic refresh is the frame's own quantizer.
    let base = usize::from(h.base_q_idx);
    assert_eq!(segments[0].luma_ac, super::AC_QLOOKUP[base]);
    assert_eq!(segments[0].luma_dc, super::DC_QLOOKUP[base]);
    // The boosted segments use another quantizer through ALT_Q.
    assert!(
        (1..MAX_SEGMENTS)
            .any(|i| h.segmentation.feature_enabled[i][0]
                && segments[i].luma_ac != segments[0].luma_ac),
        "{:?}",
        h.segmentation
    );
}

#[test]
fn filter_levels_follow_the_reference_and_mode_deltas() {
    let mut h = headers("vp9-er.webm").remove(0);
    h.filter_level = 40;
    h.deltas.enabled = true;
    h.deltas.reference = [1, 0, -1, -1];
    h.deltas.mode = [0, 2];
    let s = h.segments()[0];
    // Level 40 doubles each delta (40 >> 5 = 1).
    assert_eq!(s.filter_level[0], [42, 42]);
    assert_eq!(s.filter_level[1], [40, 44]);
    assert_eq!(s.filter_level[2], [38, 42]);
    h.deltas.enabled = false;
    assert_eq!(h.segments()[0].filter_level, [[40; 2]; 4]);
    // A segment's absolute level, clamped.
    h.segmentation.enabled = true;
    h.segmentation.abs_delta = true;
    h.segmentation.feature_enabled[3][1] = true;
    h.segmentation.feature_data[3][1] = 70;
    assert_eq!(h.segments()[3].filter_level, [[63; 2]; 4]);
}

#[test]
fn finds_key_frames_from_their_first_bits() {
    let headers = headers("vp9-altref.webm");
    let mut webm = crate::media::mkv::open(File::open(testdata("vp9-altref.webm")).expect("opens"))
        .expect("reads")
        .expect("is a WebM file");
    let track = webm.video.take().expect("video");
    let mut keys = Vec::new();
    for sample in &track.samples {
        for frame in frames(&webm.read(&track, sample).expect("reads")) {
            keys.push(super::is_key_frame(frame));
        }
    }
    let expected: Vec<bool> = headers.iter().map(|h| h.key_frame).collect();
    assert_eq!(keys, expected);
    assert!(!super::is_key_frame(&[]));
}
