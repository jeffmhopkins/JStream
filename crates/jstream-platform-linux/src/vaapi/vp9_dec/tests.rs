use std::fs::File;
use std::path::Path;

use super::{picture_params, slice_params};
use crate::media::vp9::{Header, Parser, frames};
use crate::vaapi::ffi::INVALID_ID;

/// The first two frames' headers of a committed stream: a key frame and an inter frame.
fn headers() -> (Header, Header) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/media/testdata/vp9-er.webm");
    let mut webm = crate::media::mkv::open(File::open(path).expect("opens"))
        .expect("reads")
        .expect("is a WebM file");
    let track = webm.video.take().expect("video");
    let mut parser = Parser::new();
    let mut parse = |i: usize| {
        let data = webm.read(&track, &track.samples[i]).expect("reads");
        parser.parse(frames(&data)[0]).expect("parses")
    };
    (parse(0), parse(1))
}

#[test]
fn packs_pic_fields_as_libvas_bitfields() {
    // Each field's value, set alone, from a C program built against libva 2.20.0's
    // `va_dec_vp9.h`.
    let (key, inter) = headers();
    let fields = |h: &Header| picture_params(h, &[INVALID_ID; 8]).pic_fields;
    let base = {
        let mut h = key.clone();
        h.subsampling = (false, false);
        h.show_frame = false;
        h.error_resilient = false;
        h.frame_parallel_decoding_mode = false;
        h.refresh_frame_context = false;
        h.base_q_idx = 1;
        h
    };
    assert_eq!(fields(&base), 0);
    let with = |f: &dyn Fn(&mut Header)| {
        let mut h = base.clone();
        f(&mut h);
        fields(&h)
    };
    assert_eq!(with(&|h| h.subsampling.0 = true), 0x0000_0001);
    assert_eq!(with(&|h| h.subsampling.1 = true), 0x0000_0002);
    assert_eq!(with(&|h| h.key_frame = false), 0x0000_0004);
    assert_eq!(with(&|h| h.show_frame = true), 0x0000_0008);
    assert_eq!(with(&|h| h.error_resilient = true), 0x0000_0010);
    assert_eq!(with(&|h| h.intra_only = true), 0x0000_0020);
    assert_eq!(with(&|h| h.allow_high_precision_mv = true), 0x0000_0040);
    assert_eq!(with(&|h| h.interp_filter = 1), 0x0000_0080);
    assert_eq!(
        with(&|h| h.frame_parallel_decoding_mode = true),
        0x0000_0400
    );
    assert_eq!(with(&|h| h.reset_frame_context = 1), 0x0000_0800);
    assert_eq!(with(&|h| h.refresh_frame_context = true), 0x0000_2000);
    assert_eq!(with(&|h| h.frame_context_idx = 1), 0x0000_4000);
    assert_eq!(with(&|h| h.segmentation.enabled = true), 0x0001_0000);
    assert_eq!(
        with(&|h| h.segmentation.temporal_update = true),
        0x0002_0000
    );
    assert_eq!(with(&|h| h.segmentation.update_map = true), 0x0004_0000);
    assert_eq!(with(&|h| h.ref_frame_idx[0] = 1), 0x0008_0000);
    assert_eq!(with(&|h| h.ref_frame_sign_bias[0] = true), 0x0040_0000);
    assert_eq!(with(&|h| h.ref_frame_idx[1] = 1), 0x0080_0000);
    assert_eq!(with(&|h| h.ref_frame_sign_bias[1] = true), 0x0400_0000);
    assert_eq!(with(&|h| h.ref_frame_idx[2] = 1), 0x0800_0000);
    assert_eq!(with(&|h| h.ref_frame_sign_bias[2] = true), 0x4000_0000);
    assert_eq!(with(&|h| h.base_q_idx = 0), 0x8000_0000);

    // A real inter frame: shown, 4:2:0, regular interpolation or its own.
    let p = picture_params(&inter, &[7; 8]);
    assert_eq!(p.reference_frames, [7; 8]);
    assert_eq!((p.frame_width, p.frame_height), (96, 64));
    assert_eq!(p.pic_fields & 0xF, 0b1111);
    assert_eq!(
        usize::from(p.frame_header_length_in_bytes),
        inter.uncompressed_size
    );
    assert_eq!(p.first_partition_size, inter.compressed_size);
    assert_eq!((p.profile, p.bit_depth), (0, 8));
}

#[test]
fn the_slice_is_the_whole_frame_with_every_segment() {
    let (key, _) = headers();
    let s = slice_params(&key, 1234);
    assert_eq!(
        (s.slice_data_size, s.slice_data_offset, s.slice_data_flag),
        (1234, 0, 0)
    );
    // Without segmentation every segment is the frame's own.
    let first = s.seg_param[0];
    assert!(
        s.seg_param
            .iter()
            .all(|p| p.luma_ac_quant_scale == first.luma_ac_quant_scale)
    );
    assert_eq!(first.segment_flags, 0);

    let mut h = key;
    h.segmentation.enabled = true;
    h.segmentation.feature_enabled[2][2] = true;
    h.segmentation.feature_data[2][2] = 1;
    h.segmentation.feature_enabled[2][3] = true;
    // segment_reference_enabled, segment_reference = 1, skipped: 0b1011.
    assert_eq!(slice_params(&h, 1).seg_param[2].segment_flags, 0b1011);
}
