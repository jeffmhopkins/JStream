//! The VA-API parameters of one H.264 picture to decode (ADR-0072 (b)), from the
//! planner's [`PicturePlan`]: the picture's parameter buffer, its scaling lists, and
//! each slice's parameters. The same fields FFmpeg's VA-API hwaccel fills, from the
//! same syntax elements, so every driver reads them as it is used to.
//!
//! `slice_data_bit_offset` counts the bits of the slice header in the unescaped NAL
//! unit, its header byte included, and the slice data buffer is the escaped unit without
//! a start code. Drivers take emulation-prevention bytes into account themselves.

use crate::media::h264::dpb::{Frame, PicturePlan, SlicePlan};
use crate::media::h264::slice::{SliceType, Weights};

use super::ffi::{
    DecPicParamH264, DecSliceParamH264, INVALID_ID, Id, IqMatrixH264,
    PICTURE_H264_LONG_TERM_REFERENCE, PICTURE_H264_SHORT_TERM_REFERENCE, PictureH264,
};

/// `slots[slot]`: the surface a frame is in.
fn surface(slots: &[Id], frame: &Frame) -> Id {
    frame
        .slot
        .and_then(|s| slots.get(s).copied())
        .unwrap_or(INVALID_ID)
}

/// A reference frame as VA-API describes it.
fn reference(slots: &[Id], frame: &Frame) -> PictureH264 {
    let (frame_idx, flags) = match frame.long_term {
        Some(idx) => (idx, PICTURE_H264_LONG_TERM_REFERENCE),
        None => (frame.frame_num, PICTURE_H264_SHORT_TERM_REFERENCE),
    };
    PictureH264 {
        picture_id: surface(slots, frame),
        frame_idx,
        flags,
        top_field_order_cnt: frame.top_poc,
        bottom_field_order_cnt: frame.bottom_poc,
        va_reserved: [0; 4],
    }
}

/// The picture parameter buffer.
pub(crate) fn picture_params(plan: &PicturePlan, slots: &[Id]) -> DecPicParamH264 {
    let sps = &plan.sps;
    let pps = &plan.pps;
    let mut reference_frames = [PictureH264::INVALID; 16];
    for (out, frame) in reference_frames.iter_mut().zip(&plan.references) {
        *out = reference(slots, frame);
    }
    let first = plan.slices.first().map(|s| &s.header);
    let seq_fields = u32::from(sps.chroma_format_idc & 3)
        | u32::from(sps.gaps_in_frame_num_allowed) << 3
        | u32::from(sps.frame_mbs_only) << 4
        | u32::from(sps.mb_adaptive_frame_field) << 5
        | u32::from(sps.direct_8x8_inference) << 6
        | u32::from(sps.level_idc >= 31) << 7
        | u32::from(sps.log2_max_frame_num_minus4 & 0xF) << 8
        | u32::from(sps.pic_order_cnt_type & 3) << 12
        | u32::from(sps.log2_max_pic_order_cnt_lsb_minus4 & 0xF) << 14
        | u32::from(sps.delta_pic_order_always_zero) << 18;
    let pic_fields = u32::from(pps.entropy_coding_mode)
        | u32::from(pps.weighted_pred) << 1
        | u32::from(pps.weighted_bipred_idc & 3) << 2
        | u32::from(pps.transform_8x8_mode) << 4
        | u32::from(pps.constrained_intra_pred) << 6
        | u32::from(pps.bottom_field_pic_order_in_frame_present) << 7
        | u32::from(pps.deblocking_filter_control_present) << 8
        | u32::from(pps.redundant_pic_cnt_present) << 9
        | u32::from(first.is_some_and(|h| h.is_reference())) << 10;
    DecPicParamH264 {
        curr_pic: PictureH264 {
            picture_id: slots.get(plan.slot).copied().unwrap_or(INVALID_ID),
            frame_idx: plan.current.frame_num,
            flags: 0,
            top_field_order_cnt: plan.current.top_poc,
            bottom_field_order_cnt: plan.current.bottom_poc,
            va_reserved: [0; 4],
        },
        reference_frames,
        picture_width_in_mbs_minus1: u16::try_from(sps.width_in_mbs - 1).unwrap_or(0),
        picture_height_in_mbs_minus1: u16::try_from(sps.height_in_mbs - 1).unwrap_or(0),
        bit_depth_luma_minus8: sps.bit_depth_luma_minus8,
        bit_depth_chroma_minus8: sps.bit_depth_chroma_minus8,
        num_ref_frames: sps.max_num_ref_frames,
        seq_fields,
        num_slice_groups_minus1: 0,
        slice_group_map_type: 0,
        slice_group_change_rate_minus1: 0,
        pic_init_qp_minus26: pps.pic_init_qp_minus26,
        pic_init_qs_minus26: pps.pic_init_qs_minus26,
        chroma_qp_index_offset: pps.chroma_qp_index_offset,
        second_chroma_qp_index_offset: pps.second_chroma_qp_index_offset,
        pic_fields,
        frame_num: u16::try_from(plan.current.frame_num).unwrap_or(0),
        va_reserved: [0; 8],
    }
}

/// The scaling lists.
pub(crate) fn iq_matrix(plan: &PicturePlan) -> IqMatrixH264 {
    let lists = plan.pps.scaling_lists(&plan.sps);
    IqMatrixH264 {
        scaling_list_4x4: lists.list4x4,
        scaling_list_8x8: lists.list8x8,
        va_reserved: [0; 4],
    }
}

/// One list's weights into VA-API's arrays.
fn weights(
    w: &Weights,
    luma_weight: &mut [i16; 32],
    luma_offset: &mut [i16; 32],
    chroma_weight: &mut [[i16; 2]; 32],
    chroma_offset: &mut [[i16; 2]; 32],
) -> (u8, u8) {
    for (i, (lw, lo)) in w.luma.iter().take(32).enumerate() {
        luma_weight[i] = *lw;
        luma_offset[i] = *lo;
    }
    for (i, pair) in w.chroma.iter().take(32).enumerate() {
        for (j, (cw, co)) in pair.iter().enumerate() {
            chroma_weight[i][j] = *cw;
            chroma_offset[i][j] = *co;
        }
    }
    (u8::from(w.luma_flag), u8::from(w.chroma_flag))
}

/// One slice's parameter buffer. Its data buffer is `slice.nal` alone, so the data
/// starts at offset 0.
pub(crate) fn slice_params(
    plan: &PicturePlan,
    slice: &SlicePlan,
    slots: &[Id],
) -> DecSliceParamH264 {
    let h = &slice.header;
    let mut ref_pic_list0 = [PictureH264::INVALID; 32];
    let mut ref_pic_list1 = [PictureH264::INVALID; 32];
    for (out, frame) in ref_pic_list0.iter_mut().zip(&slice.list0) {
        *out = reference(slots, frame);
    }
    for (out, frame) in ref_pic_list1.iter_mut().zip(&slice.list1) {
        *out = reference(slots, frame);
    }
    let mut p = DecSliceParamH264 {
        slice_data_size: u32::try_from(slice.nal.len()).unwrap_or(u32::MAX),
        slice_data_offset: 0,
        slice_data_flag: 0,
        slice_data_bit_offset: u16::try_from(h.header_bits).unwrap_or(u16::MAX),
        first_mb_in_slice: u16::try_from(h.first_mb).unwrap_or(u16::MAX),
        slice_type: h.slice_type as u8,
        direct_spatial_mv_pred_flag: u8::from(h.direct_spatial_mv_pred),
        num_ref_idx_l0_active_minus1: if slice.list0.is_empty() {
            0
        } else {
            h.num_ref_idx_l0_active_minus1
        },
        num_ref_idx_l1_active_minus1: if slice.list1.is_empty() {
            0
        } else {
            h.num_ref_idx_l1_active_minus1
        },
        cabac_init_idc: h.cabac_init_idc,
        slice_qp_delta: h.slice_qp_delta,
        disable_deblocking_filter_idc: h.disable_deblocking_filter_idc,
        slice_alpha_c0_offset_div2: h.slice_alpha_c0_offset_div2,
        slice_beta_offset_div2: h.slice_beta_offset_div2,
        ref_pic_list0,
        ref_pic_list1,
        luma_log2_weight_denom: 0,
        chroma_log2_weight_denom: 0,
        luma_weight_l0_flag: 0,
        luma_weight_l0: [0; 32],
        luma_offset_l0: [0; 32],
        chroma_weight_l0_flag: 0,
        chroma_weight_l0: [[0; 2]; 32],
        chroma_offset_l0: [[0; 2]; 32],
        luma_weight_l1_flag: 0,
        luma_weight_l1: [0; 32],
        luma_offset_l1: [0; 32],
        chroma_weight_l1_flag: 0,
        chroma_weight_l1: [[0; 2]; 32],
        chroma_offset_l1: [[0; 2]; 32],
        va_reserved: [0; 4],
    };
    match &h.weights {
        Some(table) => {
            p.luma_log2_weight_denom = table.luma_log2_denom;
            p.chroma_log2_weight_denom = table.chroma_log2_denom;
            (p.luma_weight_l0_flag, p.chroma_weight_l0_flag) = weights(
                &table.l0,
                &mut p.luma_weight_l0,
                &mut p.luma_offset_l0,
                &mut p.chroma_weight_l0,
                &mut p.chroma_offset_l0,
            );
            (p.luma_weight_l1_flag, p.chroma_weight_l1_flag) = weights(
                &table.l1,
                &mut p.luma_weight_l1,
                &mut p.luma_offset_l1,
                &mut p.chroma_weight_l1,
                &mut p.chroma_offset_l1,
            );
        }
        None => {
            // No explicit table: the default weights of each active reference, with the
            // denominator implicit bi-prediction uses (5) where it applies, as FFmpeg
            // fills them. The flags stay clear, so the driver weighs as the PPS says.
            let denom =
                if plan.pps.weighted_bipred_idc == 2 && slice.header.slice_type == SliceType::B {
                    5
                } else {
                    0
                };
            p.luma_log2_weight_denom = denom;
            p.chroma_log2_weight_denom = denom;
            let w = 1_i16 << denom;
            for i in 0..slice.list0.len().min(32) {
                p.luma_weight_l0[i] = w;
                p.chroma_weight_l0[i] = [w; 2];
            }
            for i in 0..slice.list1.len().min(32) {
                p.luma_weight_l1[i] = w;
                p.chroma_weight_l1[i] = [w; 2];
            }
        }
    }
    p
}

#[cfg(test)]
mod tests;
