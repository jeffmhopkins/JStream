use std::fs::File;
use std::path::{Path, PathBuf};

use super::{
    AFFINE, Frame, FrameHeader, IDENTITY, INTER_FRAME, Parser, ROTZOOM, RefSlot, SequenceHeader,
    obus, shear_valid,
};
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

/// Every frame of a stream, in decoding order.
fn frames(name: &str) -> Vec<Frame> {
    let mut parser = Parser::new();
    samples(name)
        .iter()
        .flat_map(|s| parser.parse(s).expect("parses"))
        .collect()
}

/// What a test checks of each frame header, as FFmpeg's `trace_headers` prints it.
#[derive(Debug, PartialEq, Eq)]
enum F {
    /// `frame_type`, `show_frame`, `order_hint`, `refresh_frame_flags`, `base_q_idx`.
    New(u8, bool, u32, u8, u8),
    /// `show_existing_frame` of a slot.
    Existing(usize),
}

impl F {
    const fn new(kind: u8, show: bool, hint: u32, refresh: u8, q: u8) -> Self {
        Self::New(kind, show, hint, refresh, q)
    }

    const fn existing(slot: usize) -> Self {
        Self::Existing(slot)
    }
}

fn summary(h: &FrameHeader) -> F {
    match h.show_existing {
        Some(slot) => F::Existing(slot),
        None => F::New(
            h.frame_type,
            h.show_frame,
            h.order_hint,
            h.refresh_frame_flags,
            h.base_q_idx,
        ),
    }
}

/// libaom, 208x120 in 2x2 tiles, with hidden alternate references shown later.
const AOM: [F; 35] = [
    F::new(0, true, 0, 0xff, 16),
    F::new(1, true, 1, 0x02, 69),
    F::new(1, true, 2, 0x04, 69),
    F::new(1, true, 3, 0x08, 96),
    F::new(1, true, 4, 0x10, 69),
    F::new(1, true, 5, 0x20, 96),
    F::new(1, true, 6, 0x40, 96),
    F::new(1, true, 7, 0x80, 109),
    F::new(1, true, 8, 0x02, 112),
    F::new(1, true, 9, 0x04, 117),
    F::new(1, true, 10, 0x08, 117),
    F::new(1, true, 11, 0x10, 120),
    F::new(1, true, 12, 0x20, 117),
    F::new(1, true, 13, 0x40, 119),
    F::new(1, true, 14, 0x80, 119),
    F::new(1, true, 15, 0x04, 120),
    F::new(1, true, 16, 0x08, 67),
    F::new(1, false, 29, 0x01, 67),
    F::new(1, false, 22, 0x10, 93),
    F::new(1, false, 19, 0x20, 106),
    F::new(1, true, 17, 0x40, 123),
    F::new(1, true, 18, 0x80, 123),
    F::existing(5),
    F::new(1, true, 20, 0x04, 123),
    F::new(1, true, 21, 0x40, 123),
    F::existing(4),
    F::new(1, false, 25, 0x80, 105),
    F::new(1, true, 23, 0x20, 123),
    F::new(1, true, 24, 0x04, 122),
    F::existing(7),
    F::new(1, false, 27, 0x40, 111),
    F::new(1, true, 26, 0x10, 122),
    F::existing(6),
    F::new(1, true, 28, 0x20, 115),
    F::new(1, true, 29, 0x00, 123),
];

/// libaom on a zooming, panning picture: global motion, loop restoration,
/// segmentation and delta q.
const GM: [F; 34] = [
    F::new(0, true, 0, 0xff, 90),
    F::new(1, false, 15, 0x02, 97),
    F::new(1, false, 7, 0x04, 107),
    F::new(1, false, 3, 0x08, 111),
    F::new(1, true, 1, 0x10, 125),
    F::new(1, true, 2, 0x20, 124),
    F::existing(3),
    F::new(1, false, 5, 0x40, 121),
    F::new(1, true, 4, 0x80, 126),
    F::existing(6),
    F::new(1, true, 6, 0x10, 125),
    F::existing(2),
    F::new(1, false, 11, 0x20, 115),
    F::new(1, false, 9, 0x08, 122),
    F::new(1, true, 8, 0x80, 127),
    F::existing(3),
    F::new(1, true, 10, 0x40, 127),
    F::existing(5),
    F::new(1, false, 13, 0x10, 122),
    F::new(1, true, 12, 0x04, 127),
    F::existing(4),
    F::new(1, true, 14, 0x80, 124),
    F::new(1, true, 15, 0x00, 128),
    F::new(1, false, 22, 0x08, 108),
    F::new(1, false, 18, 0x40, 118),
    F::new(1, true, 16, 0x20, 124),
    F::new(1, true, 17, 0x04, 125),
    F::existing(6),
    F::new(1, false, 20, 0x10, 123),
    F::new(1, true, 19, 0x80, 126),
    F::existing(4),
    F::new(1, true, 21, 0x20, 125),
    F::new(1, true, 22, 0x00, 128),
    F::new(1, true, 23, 0x04, 128),
];

#[test]
fn frame_headers_read_as_ffmpeg_reads_them() {
    // Every value FFmpeg's CBS reader traces (`-bsf:v trace_headers`) was checked
    // against this parser's, element for element, for the three test streams; these
    // are the fields that steer decoding.
    for (name, want) in [("av1-aom.webm", &AOM[..]), ("av1-gm.webm", &GM[..])] {
        let got: Vec<F> = frames(name).iter().map(|f| summary(&f.header)).collect();
        assert_eq!(got, want, "{name}");
    }
}

#[test]
fn the_sequence_header_reads_as_ffmpeg_reads_it() {
    let mut parser = Parser::new();
    parser.parse(&samples("av1-aom.webm")[0]).expect("parses");
    let s = parser.sequence().expect("a sequence header");
    assert_eq!(
        (s.profile, s.max_frame_width, s.max_frame_height),
        (0, 208, 120)
    );
    assert_eq!(s.order_hint_bits, 7);
    assert!(s.enable_order_hint && s.enable_cdef && s.enable_ref_frame_mvs);
    assert!(!s.enable_restoration && !s.film_grain_params_present);
    assert_eq!(s.color.bit_depth, 8);
    assert!(s.color.subsampling_x && s.color.subsampling_y);
}

#[test]
fn global_motion_is_decoded_against_the_primary_references() {
    // As dav1d decodes the same frames (`Dav1dFrameHeader::gmv`).
    let decoded: Vec<FrameHeader> = frames("av1-gm.webm")
        .into_iter()
        .map(|f| f.header)
        .filter(|h| h.show_existing.is_none())
        .collect();
    let gm = |h: &FrameHeader, r: usize| (h.gm_type[r], h.gm_params[r]);
    assert_eq!(
        gm(&decoded[1], 1),
        (ROTZOOM, [1_785_856, 524_288, 57_344, -48, 48, 57_344])
    );
    assert_eq!(decoded[1].primary_ref_frame, 7);
    // These two are coded as differences from their primary reference's parameters.
    assert_eq!(decoded[3].primary_ref_frame, 4);
    assert_eq!(
        gm(&decoded[3], 1),
        (ROTZOOM, [352_256, 44_032, 63_696, 10, -10, 63_696])
    );
    assert_eq!(
        gm(&decoded[3], 5),
        (ROTZOOM, [-571_392, -163_840, 68_152, 8, -8, 68_152])
    );
    assert_eq!(decoded[13].primary_ref_frame, 2);
    assert_eq!(
        gm(&decoded[13], 7),
        (ROTZOOM, [-226_304, 232_448, 66_626, -66, 66, 66_626])
    );
    assert_eq!(decoded[3].gm_type[2], IDENTITY);
    assert!(!decoded[3].gm_invalid[1]);
}

#[test]
fn segmentation_without_new_data_keeps_the_primary_references() {
    // The last frame codes `segmentation_update_data` 0 and takes its alternate
    // quantizers from its primary reference, as dav1d does.
    let last = frames("av1-gm.webm")
        .into_iter()
        .map(|f| f.header)
        .rfind(|h| h.show_existing.is_none())
        .expect("frames");
    assert!(last.segmentation_enabled && !last.segmentation_update_data);
    let alt_q: Vec<i16> = (0..8).map(|i| last.feature_value[i][0]).collect();
    assert_eq!(alt_q, [-52, -39, -22, 0, 8, 15, 24, 34]);
}

#[test]
fn film_grain_is_read_for_every_shown_or_showable_frame() {
    let all = frames("av1-grain.webm");
    let decoded: Vec<&FrameHeader> = all
        .iter()
        .map(|f| &f.header)
        .filter(|h| h.show_existing.is_none())
        .collect();
    assert!(decoded.iter().all(|h| h.film_grain.apply_grain));
    // A hidden frame shown later keeps its grain in its slot.
    let mut shown_again = all.iter().filter(|f| f.header.show_existing.is_some());
    assert!(shown_again.clone().count() > 0);
    assert!(shown_again.all(|f| f.header.film_grain.apply_grain));
    assert_eq!(decoded[0].film_grain.grain_seed, 7391);
}

#[test]
fn short_signalling_picks_references_by_order_hint() {
    // §7.8, worked by hand: hints 0, 1, 2, 4, 6, 10, 12, 16 in slots 0-7, a frame at 8
    // naming slot 4 LAST and slot 0 GOLDEN. ALTREF takes the latest backward (16),
    // BWDREF and ALTREF2 the earliest (10, 12), LAST2 and LAST3 the latest forward
    // left (4, 2).
    let seq = SequenceHeader {
        enable_order_hint: true,
        order_hint_bits: 7,
        ..SequenceHeader::default()
    };
    let mut parser = Parser::new();
    for (slot, hint) in parser.slots.iter_mut().zip([0, 1, 2, 4, 6, 10, 12, 16]) {
        *slot = RefSlot {
            valid: true,
            order_hint: hint,
            ..RefSlot::default()
        };
    }
    assert_eq!(parser.set_frame_refs(&seq, 8, 4, 0), [4, 3, 2, 0, 5, 6, 7]);
    // With nothing backward, every backward reference falls to the forward ones and
    // then to the earliest frame.
    for (slot, hint) in parser.slots.iter_mut().zip([0, 1, 2, 3, 4, 5, 6, 7]) {
        slot.order_hint = hint;
    }
    assert_eq!(parser.set_frame_refs(&seq, 8, 7, 0), [7, 6, 5, 0, 4, 3, 2]);
}

#[test]
fn shear_checks_the_warp_as_ffmpeg_does() {
    let identity = [0, 0, 1 << 16, 0, 0, 1 << 16];
    assert!(shear_valid(&identity));
    // A zoom with a strong shear can't be warped.
    assert!(!shear_valid(&[0, 0, 1 << 16, 1 << 15, 0, 1 << 16]));
    // A non-positive scale is never valid.
    assert!(!shear_valid(&[0, 0, 0, 0, 0, 1 << 16]));
    assert!(shear_valid(&[352_256, 44_032, 63_696, 10, -10, 63_696]));
    let _ = AFFINE;
}

#[test]
fn obus_without_a_size_or_past_the_sample_are_refused() {
    // A temporal delimiter (type 2) with its size, then one without.
    assert_eq!(obus(&[0x12, 0x00]).expect("reads").len(), 1);
    assert!(matches!(obus(&[0x10]), Err(ParseError::Invalid(_))));
    assert!(matches!(
        obus(&[0x12, 0x05, 0x00]),
        Err(ParseError::Truncated)
    ));
    assert!(matches!(obus(&[0x92, 0x00]), Err(ParseError::Invalid(_))));
}

#[test]
fn a_frame_before_any_sequence_header_or_after_a_reset_is_refused() {
    let all = samples("av1-aom.webm");
    let mut parser = Parser::new();
    // The second sample is an inter frame without a sequence header.
    assert!(matches!(parser.parse(&all[1]), Err(ParseError::Invalid(_))));
    parser.parse(&all[0]).expect("parses");
    parser.reset();
    // After a reset, an inter frame references empty slots.
    assert!(matches!(parser.parse(&all[1]), Err(ParseError::Invalid(_))));
    // A key frame starts again.
    assert!(parser.is_key_frame(&all[0]));
    assert!(!parser.is_key_frame(&all[1]));
    parser.parse(&all[0]).expect("parses");
    parser.parse(&all[1]).expect("parses");
}

#[test]
fn damaged_samples_are_errors_not_panics() {
    for sample in samples("av1-gm.webm").iter().take(4) {
        for cut in [1, 2, 3, 5, 9, 17, sample.len() / 2] {
            let mut parser = Parser::new();
            let _ = parser.parse(&sample[..cut.min(sample.len())]);
            let mut flipped = sample.clone();
            if let Some(b) = flipped.get_mut(cut) {
                *b ^= 0x5A;
            }
            let _ = parser.parse(&flipped);
        }
    }
}

#[test]
fn inter_frames_name_seven_references() {
    let all = frames("av1-aom.webm");
    let inter = all
        .iter()
        .find(|f| f.header.frame_type == INTER_FRAME)
        .expect("an inter frame");
    assert!(inter.header.ref_frame_idx.iter().all(|&i| i < 8));
}
