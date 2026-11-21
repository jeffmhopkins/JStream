use super::super::ffi::{INVALID_ID, PICTURE_H264_SHORT_TERM_REFERENCE};
use super::{iq_matrix, picture_params, slice_params};
use crate::media::h264::dpb::Planner;
use crate::media::h264::{AvcConfig, split_length_prefixed};
use crate::media::mp4::{Codec, tests::open};

#[test]
fn fills_the_parameters_of_a_b_pyramid_stream() {
    let mut mp4 = open("h264-high.mp4");
    let video = mp4.video.clone().expect("video");
    let Codec::Avc(record) = &video.codec else {
        panic!("not H.264");
    };
    let config = AvcConfig::parse(record).expect("avcC");
    let mut planner = Planner::new(18);
    for set in &config.parameter_sets {
        planner.parameter_set(set).expect("parameter set");
    }
    let slots: Vec<u32> = (100..118).collect();
    let mut saw_b = false;
    for (i, sample) in video.samples.iter().enumerate() {
        let data = mp4.read(&video, sample).expect("reads");
        let nals = split_length_prefixed(&data, config.length_size);
        let (_, plan) = planner.submit(&nals, sample.pts).expect("plans");
        let plan = plan.expect("a picture");
        let pic = picture_params(&plan, &slots);
        assert_eq!(pic.curr_pic.picture_id, slots[plan.slot]);
        assert_eq!(pic.picture_width_in_mbs_minus1, 7);
        assert_eq!(pic.picture_height_in_mbs_minus1, 5);
        assert_eq!(pic.num_ref_frames, 4);
        // 4:2:0, frame_mbs_only, direct_8x8_inference.
        assert_eq!(pic.seq_fields & 0x53, 0x51);
        // CABAC and the 8x8 transform (x264's High defaults).
        assert_eq!(pic.pic_fields & 0x11, 0x11);
        let refs = pic
            .reference_frames
            .iter()
            .filter(|r| r.picture_id != INVALID_ID)
            .count();
        assert_eq!(refs, plan.references.len());
        if i == 0 {
            assert_eq!(refs, 0);
            assert_eq!(pic.pic_fields >> 10 & 1, 1, "an IDR picture is a reference");
        }
        assert_eq!(iq_matrix(&plan).scaling_list_4x4, [[16; 16]; 6]);
        for slice in &plan.slices {
            let p = slice_params(&plan, slice, &slots);
            assert_eq!(p.slice_data_size as usize, slice.nal.len());
            assert_eq!(
                usize::from(p.slice_data_bit_offset),
                slice.header.header_bits
            );
            if p.slice_type == 1 {
                saw_b = true;
                let l0 = usize::from(p.num_ref_idx_l0_active_minus1) + 1;
                let l1 = usize::from(p.num_ref_idx_l1_active_minus1) + 1;
                for r in &p.ref_pic_list0[..l0] {
                    assert!(slots.contains(&r.picture_id));
                    assert_eq!(r.flags, PICTURE_H264_SHORT_TERM_REFERENCE);
                }
                for r in &p.ref_pic_list1[..l1] {
                    assert!(slots.contains(&r.picture_id));
                }
                assert_eq!(p.ref_pic_list0[l0].picture_id, INVALID_ID);
            }
        }
        planner.finish();
    }
    assert!(saw_b);
}
