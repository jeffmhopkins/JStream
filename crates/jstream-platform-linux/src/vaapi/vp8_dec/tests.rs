use std::fs::File;
use std::path::Path;

use super::{References, iq_matrix, picture_params, probabilities, slice_params};
use crate::media::vp8::{Header, Parser};
use crate::vaapi::ffi::{DecPicParamVp8, DecSliceParamVp8};

fn pic_bytes(p: &DecPicParamVp8) -> Vec<u8> {
    let mut out = Vec::new();
    for v in [
        p.frame_width,
        p.frame_height,
        p.last_ref_frame,
        p.golden_ref_frame,
        p.alt_ref_frame,
        p.out_of_loop_frame,
        p.pic_fields,
    ] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&p.mb_segment_tree_probs);
    out.extend_from_slice(&p.loop_filter_level);
    out.extend(p.loop_filter_deltas_ref_frame.map(|v| v.to_le_bytes()[0]));
    out.extend(p.loop_filter_deltas_mode.map(|v| v.to_le_bytes()[0]));
    out.extend_from_slice(&[p.prob_skip_false, p.prob_intra, p.prob_last, p.prob_gf]);
    out.extend_from_slice(&p.y_mode_probs);
    out.extend_from_slice(&p.uv_mode_probs);
    out.extend_from_slice(p.mv_probs.as_flattened());
    out.extend_from_slice(&[
        p.bool_coder_ctx.range,
        p.bool_coder_ctx.value,
        p.bool_coder_ctx.count,
    ]);
    out.push(0);
    out
}

fn slice_bytes(s: &DecSliceParamVp8) -> Vec<u8> {
    let mut out = Vec::new();
    for v in [
        s.slice_data_size,
        s.slice_data_offset,
        s.slice_data_flag,
        s.macroblock_offset,
    ] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&[s.num_of_partitions, 0, 0, 0]);
    for v in s.partition_size {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut x: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        x ^= u64::from(b);
        x = x.wrapping_mul(0x0100_0000_01b3);
    }
    x
}

/// The first frames of `vp8-altref.webm`'s video, with their headers.
fn frames(count: usize) -> Vec<(Vec<u8>, Header)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/media/testdata/vp8-altref.webm");
    let mut webm = crate::media::mkv::open(File::open(path).expect("opens"))
        .expect("reads")
        .expect("webm");
    let track = webm.video.take().expect("video");
    let mut parser = Parser::new();
    track.samples[..count]
        .iter()
        .map(|s| {
            let data = webm.read(&track, s).expect("reads");
            let header = parser.parse(&data).expect("parses");
            (data, header)
        })
        .collect()
}

/// The buffers FFmpeg's VA-API hwaccel (`vaapi_vp8.c`) builds for one frame of
/// `vp8-altref.webm`, as bytes in hex, with every surface 0. The coefficient
/// probabilities are hashed (FNV-1a). Its segment tree probabilities, unused while
/// segmentation is off, are FFmpeg's zeroed context's; these are 255, as RFC 6386
/// leaves an unread probability.
struct Want {
    frame: usize,
    pp: &'static str,
    prob: u64,
    iq: &'static str,
    sp: &'static str,
}

const WANT: [Want; 3] = [
    Want {
        frame: 0,
        pp: "b0000000900000000000000000000000000000000000000000980100ffffff000000000200fefe04fe020483000000919ca3808e72b7a280e192ac93d6279c8081844b91b2ceeffefea480ccaa77eb8ce6e48082824a94b4cbecfefefa3d0000",
        prob: 0x0aeb_e85f_36a2_bd66,
        iq: "040004000400040004000400040004000400040004000400040004000400040004000400040004000400040004000400",
        sp: "f71200000000000000000000900b000002000000fa0000008b10000000000000000000000000000000000000000000000000000000000000",
    },
    Want {
        frame: 1,
        pp: "b0000000900000000000000000000000000000000000000001880000ffffff020202020200fefe04fe02049038ff8070568c25a265cca280e192ac93d6279cec81844b91b2ceeffefea480ccaa77eb8ce6e48082824a94b4cbecfefe94150700",
        prob: 0xf190_b00d_e090_0650,
        iq: "160016001600160016001600160016001600160016001600160016001600160016001600160016001600160016001600",
        sp: "f20800000000000000000000c102000002000000da000000bf07000000000000000000000000000000000000000000000000000000000000",
    },
    Want {
        frame: 2,
        pp: "b0000000900000000000000000000000000000000000000001c80000ffffff0a0a0a0a0200fefe04fe02043101f20170568c25a265cca280e192ac93d6279cec81844b91b2ceeffefea480ccaa77eb8ce6e48082824a94b4cbecfefeef020600",
        prob: 0xf6ed_cf5d_6d69_7925,
        iq: "4a004a004a004a004a004a004a004a004a004a004a004a004a004a004a004a004a004a004a004a004a004a004a004a00",
        sp: "8d01000000000000000000008a00000002000000480000003301000000000000000000000000000000000000000000000000000000000000",
    },
];

#[test]
fn builds_the_buffers_ffmpeg_does() {
    let frames = frames(3);
    let none = References {
        last: 0,
        golden: 0,
        alternate: 0,
    };
    for want in WANT {
        let (data, h) = &frames[want.frame];
        let mut pp = picture_params(h, none);
        pp.out_of_loop_frame = 0;
        assert_eq!(hex(&pic_bytes(&pp)), want.pp, "frame {}", want.frame);
        let prob = probabilities(h);
        let prob = prob
            .dct_coeff_probs
            .as_flattened()
            .as_flattened()
            .as_flattened();
        assert_eq!(fnv(prob), want.prob, "frame {}", want.frame);
        let iq: Vec<u8> = iq_matrix(h)
            .quantization_index
            .as_flattened()
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert_eq!(hex(&iq), want.iq, "frame {}", want.frame);
        let sp = slice_params(h, data.len() - h.header_size);
        assert_eq!(hex(&slice_bytes(&sp)), want.sp, "frame {}", want.frame);
    }
}

#[test]
fn references_go_in_their_slots_and_a_key_frame_uses_the_fixed_mode_probabilities() {
    let frames = frames(3);
    let refs = References {
        last: 7,
        golden: 8,
        alternate: 9,
    };
    let inter = picture_params(&frames[2].1, refs);
    assert_eq!(
        (
            inter.last_ref_frame,
            inter.golden_ref_frame,
            inter.alt_ref_frame
        ),
        (7, 8, 9)
    );
    assert_eq!(inter.out_of_loop_frame, crate::vaapi::ffi::INVALID_ID);
    // libva's `key_frame` bit is set for an inter frame.
    assert_eq!(inter.pic_fields & 1, 1);
    let key = picture_params(&frames[0].1, References::NONE);
    assert_eq!(key.pic_fields & 1, 0);
    assert_eq!(key.y_mode_probs, [145, 156, 163, 128]);
    assert_eq!(key.uv_mode_probs, [142, 114, 183]);
    assert_eq!(key.last_ref_frame, crate::vaapi::ffi::INVALID_ID);
}
