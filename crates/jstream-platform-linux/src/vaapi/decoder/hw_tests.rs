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

/// x264 with open GOPs: one IDR picture, then a non-IDR I picture every 30 frames.
const OPEN_GOP: [u64; 90] = [
    0xe37c_dafe_cebf_c018,
    0x70e4_60e2_8208_0a51,
    0x0a7f_f6a5_f2b7_ecb2,
    0xec36_2689_5962_ae12,
    0xa842_3a81_ad57_e6f8,
    0x4a1e_0140_e3ba_af26,
    0x24da_86af_1aa3_5761,
    0x71fe_c565_4ecf_7ee2,
    0xbc72_c499_e206_ccf7,
    0x255b_2503_0482_6ddc,
    0xb425_18ee_c8ce_6c56,
    0x4e58_5ef9_8288_d9dd,
    0xdc22_ea81_bdc4_3421,
    0x9a52_99d8_2fb0_9efd,
    0xa1e1_a7bf_6a2d_3613,
    0xd512_f93c_f157_bd50,
    0xe085_ea2e_f790_c45c,
    0x4f96_022d_92cf_89c8,
    0x2cb8_e9bf_2450_795e,
    0x4703_dcd5_ec74_da59,
    0xf047_83d9_fe60_0fa6,
    0x0863_4289_f8e2_1d22,
    0x01c6_6b70_5ddc_ddf2,
    0x49e2_be19_e5d6_cc78,
    0x5e4f_aa36_de18_86d2,
    0x89cd_3c25_046d_d0c7,
    0xe14e_f4f7_d4dc_198a,
    0xb861_951a_7e6c_888e,
    0x7e0d_5f41_f766_549e,
    0x4b20_6025_a8fa_71a9,
    0xd6a7_5ea8_62b3_f3be,
    0xb4c7_0412_35af_5427,
    0x68a5_3b03_5713_511b,
    0x4836_8dc1_192a_d3e5,
    0x1b0a_2492_081c_697b,
    0x9da9_ff46_e825_d406,
    0x76e5_3c4c_f69b_a96e,
    0x90fc_88f4_95b8_4d7d,
    0x5744_18f7_28d5_eb62,
    0xd24d_5b3c_40a4_c52f,
    0xc577_2628_7261_1a9e,
    0xb596_e7c3_8155_f40d,
    0xa2af_b265_a352_80ec,
    0x1578_da60_8345_7a87,
    0x94d4_3164_fb1b_3c88,
    0x4e4f_567e_885b_3a1e,
    0xfd32_b139_4eac_1d96,
    0xb871_4651_2d1d_a79c,
    0x034d_2fef_f4b7_687d,
    0xe313_728a_fdc7_75c7,
    0x86be_0b33_cf10_08db,
    0x7cd0_5210_a91d_848b,
    0x9e50_ccfb_9930_e535,
    0x97eb_4aa6_22c0_ab43,
    0x86d0_7403_a060_d39e,
    0xd263_ab89_9c3e_216d,
    0x11df_d83a_a302_a5d5,
    0x9f31_7710_57c6_239b,
    0x512a_d394_fcf2_f110,
    0x26bb_9653_1f63_7742,
    0x578b_70e0_dd5f_08f7,
    0xdf4f_ea40_42be_d490,
    0x8e1d_2b0c_33e5_5b1a,
    0xd2b5_ed0a_4dc2_abc5,
    0xe547_17e7_9ced_af2f,
    0x6abd_7a8b_27a5_39c6,
    0x8ca8_8825_7ac9_9e58,
    0xd0bc_9b83_4f8b_d969,
    0xe6b1_c0f5_c362_5f91,
    0xd76a_6b39_df1e_6ed3,
    0xa44c_4270_2924_7089,
    0x8ed6_0acd_4588_56fe,
    0x8ab5_b420_bd3d_fd69,
    0x81e3_a5c5_3a24_d750,
    0x676c_458a_3a18_8ad6,
    0xdb37_86c8_7069_e66f,
    0xf098_4f1b_3a7b_6c51,
    0xa890_341e_6a95_2783,
    0x7843_8fe7_37f1_a1e3,
    0x28d7_a6b8_b067_f26f,
    0x81d5_a72d_a30f_6cda,
    0x126f_52d7_9715_11ea,
    0x16f5_48ca_6f9f_141c,
    0xceb5_856e_ae92_61ab,
    0x8446_b8bb_0ba7_51a5,
    0x7a8f_78ca_84de_8a1d,
    0xe204_4be9_0e20_c646,
    0x329b_f862_6600_737e,
    0x265e_544a_9629_9c5a,
    0x4f30_7889_dadf_e9ee,
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

#[test]
#[ignore = "needs a GPU with a VA-API H.264 decoder"]
fn decodes_an_open_gop_stream_and_seeks_to_its_recovery_point() {
    decodes("h264-open-gop.mp4", &OPEN_GOP);
    // A seek to 1 s starts at the non-IDR I picture shown there: the frames from it
    // on, bit for bit, and none of the two leading pictures before it.
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/media/testdata/h264-open-gop.mp4");
    let config = MediaDecoderConfig {
        video_format: PixelFormat::Nv12,
        hardware: HardwarePreference::Require,
        audio: false,
    };
    let mut decoder = LinuxMediaDecoders.open(&path, &config).expect("opens");
    decoder
        .seek(MediaTime::new(1_000_000, Timebase::MICROS))
        .expect("seeks");
    let mut hashes = Vec::new();
    while let Some(frame) = decoder.next_video().expect("decodes") {
        let planes: Vec<&[u8]> = frame.planes().iter().map(|p| p.data()).collect();
        hashes.push(fnv(&planes));
    }
    assert_eq!(hashes, OPEN_GOP[30..]);
}
