//! VA-API decoding on a real GPU (`hw-tests`, run by hand):
//! `cargo test -p jstream-platform-linux --features hw-tests -- --ignored decodes_`.
//!
//! H.264 decoding is exact, so each frame must match FFmpeg's software decode of the
//! same file bit for bit: the hashes below are FNV-1a over each frame's NV12 bytes from
//! `ffmpeg -i <file> -f rawvideo -pix_fmt nv12 -`.

use std::path::Path;

use jstream_platform::media::{MediaTime, PixelFormat, Timebase};
use jstream_platform::{HardwarePreference, MediaDecoderConfig, MediaDecoders};

use crate::media::LinuxMediaDecoders;

const HIGH: [u64; 30] = [
    0x0f47_4287_fcac_270b,
    0xecef_5dc3_a2f0_68fa,
    0x4e36_9dfe_f6ab_d88d,
    0x3d31_e467_6e9d_5d8e,
    0x8b4d_6e7c_b9ea_d0c0,
    0x263f_63b1_55b8_49fc,
    0x0963_4a4e_12b9_98ca,
    0x7fdf_b0a2_3167_0625,
    0x0af7_f6dc_542e_a3ca,
    0x2728_9aec_ce2e_c5dd,
    0x031f_c026_8473_b5c2,
    0xc166_3e3e_d50c_5985,
    0x1505_cc3c_afdb_6254,
    0x80ef_7dfe_d042_3865,
    0xf3cf_4276_09e1_753a,
    0x651f_4b05_fe71_0d92,
    0x636b_4219_19e3_09fd,
    0xa405_539f_79ce_8bb8,
    0xeee8_7962_02c7_eb30,
    0xfde4_b7ee_a17c_b62e,
    0x5ae5_2f7f_3e61_d07e,
    0xe823_1b82_d3d2_ec8a,
    0xe05a_8838_155a_18b2,
    0x1187_5bce_654f_6c20,
    0x2868_bc53_208f_dcc5,
    0xcde2_6809_8e42_d995,
    0x1f7f_ac2a_e1b3_a15b,
    0x3470_d167_38d6_8f72,
    0xbb6c_c312_c618_bda6,
    0x54e1_191f_daaf_a39e,
];

const BASELINE: [u64; 15] = [
    0x27c3_45d6_7747_6aa1,
    0x27c3_45d6_7747_6aa1,
    0x27c3_45d6_7747_6aa1,
    0x27c3_45d6_7747_6aa1,
    0xfa08_da6e_ba5b_b1a6,
    0x1e23_0ae8_0907_8c52,
    0x6071_f75c_c8c8_9602,
    0x29dd_81eb_4fbe_fd99,
    0x4d2f_d930_5eca_3f75,
    0xc82d_64c6_c5d7_a42d,
    0xc149_eb62_22e3_0f3a,
    0xc149_eb62_22e3_0f3a,
    0xc149_eb62_22e3_0f3a,
    0xc149_eb62_22e3_0f3a,
    0xc149_eb62_22e3_0f3a,
];

fn fnv(planes: &[&[u8]]) -> u64 {
    let mut x: u64 = 0xcbf2_9ce4_8422_2325;
    for plane in planes {
        for &b in *plane {
            x ^= u64::from(b);
            x = x.wrapping_mul(0x0100_0000_01b3);
        }
    }
    x
}

fn decodes(name: &str, expected: &[u64]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/media/testdata")
        .join(name);
    let config = MediaDecoderConfig {
        video_format: PixelFormat::Nv12,
        hardware: HardwarePreference::Require,
        audio: false,
    };
    let mut decoder = LinuxMediaDecoders.open(&path, &config).expect("opens");
    for pass in 0..2 {
        let mut hashes = Vec::new();
        let mut last = None;
        while let Some(frame) = decoder.next_video().expect("decodes") {
            let pts = frame.info().pts.as_secs_f64();
            assert!(
                last.is_none_or(|l| pts > l),
                "{name}: out of order at {pts}"
            );
            last = Some(pts);
            let planes: Vec<&[u8]> = frame.planes().iter().map(|p| p.data()).collect();
            hashes.push(fnv(&planes));
        }
        assert_eq!(hashes, expected, "{name}, pass {pass}");
        decoder
            .seek(MediaTime::zero(Timebase::MICROS))
            .expect("seeks");
    }
    // RGBA too.
    let config = MediaDecoderConfig {
        video_format: PixelFormat::Rgba8,
        ..config
    };
    let mut decoder = LinuxMediaDecoders.open(&path, &config).expect("opens");
    let mut frames = 0;
    while decoder.next_video().expect("decodes").is_some() {
        frames += 1;
    }
    assert_eq!(frames, expected.len());
}

#[test]
#[ignore = "needs a GPU with a VA-API H.264 decoder"]
fn decodes_high_profile_b_frames_bit_exact() {
    decodes("h264-high.mp4", &HIGH);
}

#[test]
#[ignore = "needs a GPU with a VA-API H.264 decoder"]
fn decodes_constrained_baseline_bit_exact() {
    decodes("h264-baseline.mov", &BASELINE);
}
