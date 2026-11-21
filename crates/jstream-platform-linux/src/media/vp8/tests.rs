use std::fs::File;
use std::path::{Path, PathBuf};

use super::tables::DEFAULT_COEFF_PROBS;
use super::{Header, MV_PROBS, Parser, Update, is_key_frame};
use crate::media::h264::bits::ParseError;

fn testdata(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/media/testdata")
        .join(name)
}

/// The samples of a WebM file's video track.
fn samples(name: &str) -> Vec<Vec<u8>> {
    let mut webm = crate::media::mkv::open(File::open(testdata(name)).expect("opens"))
        .expect("reads")
        .expect("is a WebM file");
    let track = webm.video.take().expect("video");
    track
        .samples
        .iter()
        .map(|s| webm.read(&track, s).expect("reads"))
        .collect()
}

/// What a test checks of each frame's header. `q` and `refresh` (last 1, golden 2,
/// alternate 4, from this frame) are what libvpx's decoder reports for the same frames
/// (`VPXD_GET_LAST_QUANTIZER`, `VP8D_GET_LAST_REF_UPDATES`); `coder` (range, value,
/// count, macroblock offset) and `first` are what FFmpeg's VP8 decoder records for its
/// VA-API hwaccel (`coder_state_at_header_end`, `header_partition_size`).
#[derive(Debug, PartialEq, Eq)]
struct F {
    key: bool,
    show: bool,
    q: u8,
    refresh: u8,
    partitions: usize,
    seg: bool,
    coder: (u8, u8, u8, u32),
    first: u32,
}

fn summary(h: &Header) -> F {
    F {
        key: h.key_frame,
        show: h.show_frame,
        q: h.base_q,
        refresh: u8::from(h.refresh_last)
            | u8::from(h.golden == Update::Current) << 1
            | u8::from(h.alternate == Update::Current) << 2,
        partitions: h.partitions,
        seg: h.segmentation.enabled,
        coder: (
            h.coder.range,
            h.coder.value,
            h.coder.count,
            h.coder.macroblock_offset,
        ),
        first: h.first_partition_size,
    }
}

fn check(name: &str, expected: &[F]) -> Vec<Header> {
    let mut parser = Parser::new();
    let headers: Vec<Header> = samples(name)
        .iter()
        .map(|s| parser.parse(s).expect("parses"))
        .collect();
    let got: Vec<F> = headers.iter().map(summary).collect();
    assert_eq!(got, expected, "{name}");
    headers
}

#[test]
fn reads_a_two_pass_stream_with_a_hidden_alternate_reference() {
    let headers = check("vp8-altref.webm", &ALTREF);
    assert!(headers.iter().all(|h| (h.width, h.height) == (176, 144)));
    assert!(headers.iter().all(|h| h.version == 0 && !h.filter_simple));
}

#[test]
fn reads_segmentation_and_token_partitions() {
    let headers = check("vp8-parts.webm", &PARTS);
    let first = &headers[0];
    assert_eq!((first.width, first.height), (126, 70));
    assert_eq!(first.version, 1);
    assert!(first.filter_simple);
    // Each segment's levels and quantizer indices, as FFmpeg's hwaccel hands them to
    // VA-API for the same frames.
    let second = &headers[1];
    assert_eq!(second.filter_levels(), [27; 4]);
    assert_eq!(
        second.quantizer_indices(),
        [[81; 6], [40; 6], [81; 6], [81; 6]]
    );
    assert_eq!(headers[2].filter_levels(), [44; 4]);
    assert_eq!(
        headers[2].quantizer_indices(),
        [[92; 6], [46; 6], [92; 6], [92; 6]]
    );
}

#[test]
fn the_partitions_fill_the_frame() {
    for name in ["vp8-altref.webm", "vp8-parts.webm"] {
        let mut parser = Parser::new();
        for sample in samples(name) {
            let h = parser.parse(&sample).expect("parses");
            let tokens: u32 = h.partition_sizes[..h.partitions].iter().sum();
            let table = 3 * (h.partitions - 1);
            let total = h.header_size
                + table
                + usize::try_from(h.first_partition_size + tokens).expect("fits");
            assert_eq!(total, sample.len(), "{name}");
        }
    }
}

#[test]
fn a_frame_that_keeps_no_probabilities_leaves_them_as_they_were() {
    // Every frame of the error-resilient stream decodes with its own updates and
    // drops them, so what carries stays the defaults; the frames still decode with
    // their updates.
    let mut parser = Parser::new();
    let mut updated = false;
    for sample in samples("vp8-parts.webm") {
        let h = parser.parse(&sample).expect("parses");
        updated |= h.probs.coeff != DEFAULT_COEFF_PROBS;
        assert_eq!(parser.probs.coeff, DEFAULT_COEFF_PROBS);
        assert_eq!(parser.probs.mv, MV_PROBS);
    }
    assert!(updated);
}

#[test]
fn an_inter_frame_before_a_key_frame_is_refused() {
    let samples = samples("vp8-altref.webm");
    assert!(is_key_frame(&samples[0]));
    assert!(!is_key_frame(&samples[1]));
    assert_eq!(
        Parser::new().parse(&samples[1]).map(|_| ()),
        Err(ParseError::Invalid(
            "a VP8 inter frame before any key frame"
        ))
    );
    // After a reset too.
    let mut parser = Parser::new();
    parser.parse(&samples[0]).expect("parses");
    parser.reset();
    assert!(parser.parse(&samples[1]).is_err());
}

#[test]
fn a_damaged_frame_is_an_error_not_a_panic() {
    let key = samples("vp8-altref.webm").swap_remove(0);
    let mut parser = Parser::new();
    assert_eq!(
        parser.parse(&key[..2]).map(|_| ()),
        Err(ParseError::Truncated)
    );
    assert_eq!(
        parser.parse(&key[..9]).map(|_| ()),
        Err(ParseError::Truncated)
    );
    let mut bad = key.clone();
    bad[3] = 0;
    assert_eq!(
        parser.parse(&bad).map(|_| ()),
        Err(ParseError::Invalid("a VP8 start code"))
    );
    let mut v4 = key.clone();
    v4[0] = v4[0] & !0x0E | 4 << 1;
    assert_eq!(
        parser.parse(&v4).map(|_| ()),
        Err(ParseError::Unsupported("a VP8 version above 3"))
    );
    // The first partition says it's longer than the frame.
    assert_eq!(
        parser.parse(&key[..100]).map(|_| ()),
        Err(ParseError::Truncated)
    );
    // Every cut and every flipped byte either parses or fails, without a panic.
    for len in 0..key.len().min(700) {
        let _ = Parser::new().parse(&key[..len]);
    }
    for i in 0..key.len().min(700) {
        let mut flipped = key.clone();
        flipped[i] ^= 0xA5;
        let _ = Parser::new().parse(&flipped);
    }
}

const ALTREF: [F; 31] = [
    F {
        key: true,
        show: true,
        q: 4,
        refresh: 7,
        partitions: 1,
        seg: false,
        coder: (250, 61, 0, 2960),
        first: 620,
    },
    F {
        key: false,
        show: false,
        q: 22,
        refresh: 4,
        partitions: 1,
        seg: false,
        coder: (148, 21, 7, 705),
        first: 307,
    },
    F {
        key: false,
        show: true,
        q: 74,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (239, 2, 6, 138),
        first: 90,
    },
    F {
        key: false,
        show: true,
        q: 77,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (170, 36, 1, 143),
        first: 120,
    },
    F {
        key: false,
        show: true,
        q: 80,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (177, 41, 2, 86),
        first: 88,
    },
    F {
        key: false,
        show: true,
        q: 69,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (234, 58, 1, 175),
        first: 193,
    },
    F {
        key: false,
        show: true,
        q: 83,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (254, 36, 2, 102),
        first: 103,
    },
    F {
        key: false,
        show: true,
        q: 69,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (196, 58, 4, 156),
        first: 114,
    },
    F {
        key: false,
        show: true,
        q: 76,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (177, 53, 2, 86),
        first: 96,
    },
    F {
        key: false,
        show: true,
        q: 69,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (192, 172, 6, 122),
        first: 100,
    },
    F {
        key: false,
        show: true,
        q: 74,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (132, 36, 1, 95),
        first: 110,
    },
    F {
        key: false,
        show: true,
        q: 63,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (232, 79, 6, 114),
        first: 124,
    },
    F {
        key: false,
        show: true,
        q: 74,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (177, 44, 2, 86),
        first: 105,
    },
    F {
        key: false,
        show: true,
        q: 60,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (239, 72, 2, 102),
        first: 103,
    },
    F {
        key: false,
        show: true,
        q: 70,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (252, 50, 2, 102),
        first: 105,
    },
    F {
        key: false,
        show: true,
        q: 64,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (177, 34, 2, 86),
        first: 93,
    },
    F {
        key: false,
        show: true,
        q: 83,
        refresh: 3,
        partitions: 1,
        seg: false,
        coder: (177, 4, 4, 84),
        first: 17,
    },
    F {
        key: false,
        show: true,
        q: 83,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (177, 27, 2, 86),
        first: 109,
    },
    F {
        key: false,
        show: true,
        q: 83,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (252, 70, 2, 102),
        first: 111,
    },
    F {
        key: false,
        show: true,
        q: 83,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (156, 39, 1, 95),
        first: 100,
    },
    F {
        key: false,
        show: true,
        q: 83,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (142, 34, 4, 108),
        first: 130,
    },
    F {
        key: false,
        show: true,
        q: 83,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (177, 44, 2, 86),
        first: 99,
    },
    F {
        key: false,
        show: true,
        q: 83,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (162, 36, 5, 99),
        first: 107,
    },
    F {
        key: false,
        show: true,
        q: 83,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (132, 33, 1, 95),
        first: 122,
    },
    F {
        key: false,
        show: true,
        q: 83,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (236, 60, 4, 100),
        first: 133,
    },
    F {
        key: false,
        show: true,
        q: 83,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (132, 35, 1, 95),
        first: 87,
    },
    F {
        key: false,
        show: true,
        q: 83,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (177, 51, 2, 86),
        first: 127,
    },
    F {
        key: false,
        show: true,
        q: 83,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (177, 49, 2, 86),
        first: 109,
    },
    F {
        key: false,
        show: true,
        q: 83,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (177, 55, 2, 86),
        first: 126,
    },
    F {
        key: false,
        show: true,
        q: 83,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (232, 57, 4, 108),
        first: 105,
    },
    F {
        key: false,
        show: true,
        q: 83,
        refresh: 1,
        partitions: 1,
        seg: false,
        coder: (195, 51, 2, 102),
        first: 111,
    },
];

const PARTS: [F; 30] = [
    F {
        key: true,
        show: true,
        q: 4,
        refresh: 7,
        partitions: 4,
        seg: true,
        coder: (250, 133, 1, 3511),
        first: 624,
    },
    F {
        key: false,
        show: true,
        q: 81,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (196, 187, 1, 455),
        first: 110,
    },
    F {
        key: false,
        show: true,
        q: 92,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (196, 34, 5, 467),
        first: 93,
    },
    F {
        key: false,
        show: true,
        q: 102,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (156, 20, 0, 280),
        first: 74,
    },
    F {
        key: false,
        show: true,
        q: 103,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (195, 11, 6, 314),
        first: 66,
    },
    F {
        key: false,
        show: true,
        q: 106,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (195, 1, 1, 311),
        first: 57,
    },
    F {
        key: false,
        show: true,
        q: 109,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (195, 70, 2, 318),
        first: 55,
    },
    F {
        key: false,
        show: true,
        q: 108,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (195, 3, 5, 267),
        first: 56,
    },
    F {
        key: false,
        show: true,
        q: 106,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (195, 1, 0, 320),
        first: 58,
    },
    F {
        key: false,
        show: true,
        q: 108,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (195, 4, 2, 310),
        first: 60,
    },
    F {
        key: true,
        show: true,
        q: 9,
        refresh: 7,
        partitions: 4,
        seg: true,
        coder: (246, 131, 5, 2995),
        first: 569,
    },
    F {
        key: false,
        show: true,
        q: 70,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (196, 53, 4, 476),
        first: 108,
    },
    F {
        key: false,
        show: true,
        q: 73,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (196, 40, 6, 514),
        first: 102,
    },
    F {
        key: false,
        show: true,
        q: 76,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (195, 184, 4, 540),
        first: 100,
    },
    F {
        key: false,
        show: true,
        q: 76,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (196, 25, 0, 520),
        first: 101,
    },
    F {
        key: false,
        show: true,
        q: 79,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (196, 42, 4, 508),
        first: 102,
    },
    F {
        key: false,
        show: true,
        q: 80,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (156, 106, 5, 355),
        first: 72,
    },
    F {
        key: false,
        show: true,
        q: 75,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (196, 155, 7, 521),
        first: 95,
    },
    F {
        key: false,
        show: true,
        q: 79,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (213, 6, 5, 531),
        first: 88,
    },
    F {
        key: false,
        show: true,
        q: 81,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (195, 9, 1, 423),
        first: 81,
    },
    F {
        key: true,
        show: true,
        q: 10,
        refresh: 7,
        partitions: 4,
        seg: true,
        coder: (246, 131, 5, 2811),
        first: 537,
    },
    F {
        key: false,
        show: true,
        q: 104,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (195, 135, 1, 335),
        first: 87,
    },
    F {
        key: false,
        show: true,
        q: 104,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (195, 185, 2, 374),
        first: 90,
    },
    F {
        key: false,
        show: true,
        q: 107,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (195, 182, 1, 327),
        first: 81,
    },
    F {
        key: false,
        show: true,
        q: 107,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (184, 140, 5, 339),
        first: 93,
    },
    F {
        key: false,
        show: true,
        q: 109,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (184, 171, 7, 353),
        first: 88,
    },
    F {
        key: false,
        show: true,
        q: 109,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (156, 111, 5, 299),
        first: 65,
    },
    F {
        key: false,
        show: true,
        q: 104,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (196, 122, 6, 410),
        first: 74,
    },
    F {
        key: false,
        show: true,
        q: 106,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (236, 150, 4, 292),
        first: 64,
    },
    F {
        key: false,
        show: true,
        q: 105,
        refresh: 1,
        partitions: 4,
        seg: true,
        coder: (196, 129, 7, 361),
        first: 70,
    },
];
