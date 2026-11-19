use std::fs::File;
use std::path::Path;

use super::{Surfaces, picture_params, slice_params};
use crate::media::av1::{Frame, FrameHeader, Parser, SequenceHeader};
use crate::vaapi::ffi::INVALID_ID;

/// Every frame of a test stream, decoded or shown again, with its sample and the
/// sequence header.
fn frames(name: &str) -> (SequenceHeader, Vec<(Frame, Vec<u8>)>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/media/testdata")
        .join(name);
    let mut webm = crate::media::mkv::open(File::open(path).expect("opens"))
        .expect("reads")
        .expect("webm");
    let track = webm.video.take().expect("video");
    let mut parser = Parser::new();
    let mut out = Vec::new();
    for s in &track.samples {
        let data = webm.read(&track, s).expect("reads");
        for f in parser.parse(&data).expect("parses") {
            out.push((f, data.clone()));
        }
    }
    (parser.sequence().cloned().expect("a sequence header"), out)
}

const SURFACES: Surfaces = Surfaces {
    current: 20,
    display: 21,
    references: [10, 11, 12, 13, 14, 15, 16, 17],
};

/// The bit `set` lights in a field, alone.
fn only_bit(field: u32) -> u32 {
    assert_eq!(field.count_ones(), 1, "{field:#x}");
    field.trailing_zeros()
}

#[test]
fn each_flag_lands_on_libvas_bit() {
    // The bits libva's own headers put each field at (printed by a C program built
    // against `va/va_dec_av1.h`, libva 2.20.0).
    let mut seq = SequenceHeader::default();
    seq.still_picture = true;
    let h = FrameHeader::default();
    let p = picture_params(&seq, &h, SURFACES);
    assert_eq!(p.seq_info_fields & 1, 1);
    let mut seq = SequenceHeader::default();
    seq.enable_cdef = true;
    assert_eq!(
        only_bit(picture_params(&seq, &h, SURFACES).seq_info_fields),
        9
    );
    let mut seq = SequenceHeader::default();
    seq.film_grain_params_present = true;
    assert_eq!(
        only_bit(picture_params(&seq, &h, SURFACES).seq_info_fields),
        15
    );

    let seq = SequenceHeader::default();
    let bit = |h: FrameHeader| picture_params(&seq, &h, SURFACES);
    let base = FrameHeader {
        show_frame: false,
        disable_frame_end_update_cdf: false,
        uniform_tile_spacing: false,
        ..FrameHeader::default()
    };
    assert_eq!(base_pic(&seq, &base), 0);
    let p = bit(FrameHeader {
        show_frame: true,
        ..base.clone()
    });
    assert_eq!(only_bit(p.pic_info_fields), 2);
    let p = bit(FrameHeader {
        allow_warped_motion: true,
        ..base.clone()
    });
    assert_eq!(only_bit(p.pic_info_fields), 15);
    let p = bit(FrameHeader {
        loop_filter_delta_update: true,
        ..base.clone()
    });
    assert_eq!(p.loop_filter_info_fields, 1 << 4);
    let p = bit(FrameHeader {
        qm_v: 1,
        ..base.clone()
    });
    assert_eq!(p.qmatrix_fields, 1 << 9);
    let p = bit(FrameHeader {
        skip_mode_present: true,
        ..base.clone()
    });
    assert_eq!(only_bit(p.mode_control_fields), 11);
    let p = bit(FrameHeader {
        lr_uv_shift: 1,
        ..base.clone()
    });
    assert_eq!(p.loop_restoration_fields, 1 << 8);
    let p = bit(FrameHeader {
        segmentation_update_data: true,
        ..base.clone()
    });
    assert_eq!(only_bit(p.seg_info.segment_info_fields), 3);
    let mut grain = base.clone();
    grain.film_grain.clip_to_restricted_range = true;
    assert_eq!(
        only_bit(bit(grain).film_grain_info.film_grain_info_fields),
        11
    );
}

fn base_pic(seq: &SequenceHeader, h: &FrameHeader) -> u32 {
    picture_params(seq, h, SURFACES).pic_info_fields
}

#[test]
fn restoration_types_are_remapped_as_ffmpeg_does() {
    // Coded none, switchable, Wiener, self-guided → AV1_RESTORE_NONE (0), SWITCHABLE
    // (3), WIENER (1), SGRPROJ (2).
    let seq = SequenceHeader::default();
    let h = FrameHeader {
        lr_type: [1, 2, 3],
        ..FrameHeader::default()
    };
    let p = picture_params(&seq, &h, SURFACES);
    assert_eq!(p.loop_restoration_fields & 0x3F, 3 | 1 << 2 | 2 << 4);
}

#[test]
fn segment_features_are_clipped_to_their_range() {
    let seq = SequenceHeader::default();
    let mut h = FrameHeader {
        segmentation_enabled: true,
        ..FrameHeader::default()
    };
    h.feature_enabled[2][0] = true;
    h.feature_value[2][0] = -300;
    h.feature_enabled[2][1] = true;
    h.feature_value[2][1] = 70;
    h.feature_enabled[2][5] = true;
    h.feature_value[2][5] = 9;
    h.feature_enabled[2][6] = true;
    let p = picture_params(&seq, &h, SURFACES);
    assert_eq!(p.seg_info.feature_mask[2], 0b110_0011);
    assert_eq!(&p.seg_info.feature_data[2][..6], &[-255, 63, 0, 0, 0, 7]);
}

#[test]
fn a_shown_key_frame_references_nothing() {
    let (seq, frames) = frames("av1-aom.webm");
    let (key, _) = &frames[0];
    let p = picture_params(&seq, &key.header, SURFACES);
    assert_eq!(p.ref_frame_map, [INVALID_ID; 8]);
    assert_eq!((p.current_frame, p.current_display_picture), (20, 21));
    assert_eq!((p.frame_width_minus1, p.frame_height_minus1), (207, 119));
    let (inter, _) = frames
        .iter()
        .find(|(f, _)| f.header.frame_type == 1)
        .expect("an inter frame");
    let p = picture_params(&seq, &inter.header, SURFACES);
    assert_eq!(p.ref_frame_map, SURFACES.references);
    assert_eq!(p.interp_filter, inter.header.interpolation_filter);
}

#[test]
fn tiles_fill_their_tile_group_after_each_size_field() {
    // 2x2 tiles: each tile but the last follows its tile_size_bytes size field.
    let (seq, frames) = frames("av1-aom.webm");
    let mut checked = 0;
    for (frame, sample) in &frames {
        let h = &frame.header;
        for group in &frame.tile_groups {
            let slices = slice_params(group);
            assert_eq!(slices.len(), usize::from(group.tg_end - group.tg_start) + 1);
            let mut at = 0;
            for (i, s) in slices.iter().enumerate() {
                if i + 1 < slices.len() {
                    at += h.tile_size_bytes;
                }
                assert_eq!(usize::try_from(s.slice_data_offset).unwrap(), at);
                at += usize::try_from(s.slice_data_size).unwrap();
                let n = usize::from(group.tg_start) + i;
                assert_eq!(
                    (usize::from(s.tile_row), usize::from(s.tile_column)),
                    (n / h.tile_cols, n % h.tile_cols)
                );
            }
            assert_eq!(at, group.end - group.start);
            assert!(group.end <= sample.len());
            checked += 1;
        }
        if h.show_existing.is_none() {
            let p = picture_params(&seq, h, SURFACES);
            assert_eq!((p.tile_cols, p.tile_rows), (2, 2));
            assert_eq!(p.tile_count_minus_1, 3);
        }
    }
    assert!(checked >= 25, "{checked}");
}

#[test]
fn film_grain_reaches_the_driver_as_coded() {
    let (seq, frames) = frames("av1-grain.webm");
    let (key, _) = &frames[0];
    let g = &key.header.film_grain;
    assert!(g.apply_grain);
    let p = picture_params(&seq, &key.header, SURFACES).film_grain_info;
    // The first frame's values, as dav1d reads them (`Dav1dFilmGrainData`).
    assert_eq!(p.grain_seed, 7391);
    assert_eq!(p.num_y_points, 2);
    assert_eq!(&p.point_y_value[..2], &[0, 255]);
    assert_eq!(&p.point_y_scaling[..2], &[45, 45]);
    assert_eq!((p.num_cb_points, p.num_cr_points), (2, 2));
    assert_eq!(&p.ar_coeffs_y[..4], &[5, 3, 0, -3]);
    assert_eq!(&p.ar_coeffs_cr[..3], &[-13, -1, -22]);
    // apply, scaling shift 11 (3 from 8), lag 3, shift 7 (1 from 6), overlap.
    assert_eq!(
        p.film_grain_info_fields,
        1 | 3 << 2 | 3 << 4 | 1 << 6 | 1 << 10
    );
    assert_eq!((p.cb_mult, p.cb_luma_mult, p.cb_offset), (128, 192, 256));
}
