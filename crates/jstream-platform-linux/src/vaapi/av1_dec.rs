//! The VA-API parameters of one AV1 frame to decode (ADR-0072 (b)), from its sequence
//! and frame headers ([`SequenceHeader`], [`FrameHeader`]): the picture's parameters,
//! and one slice per tile. The same fields FFmpeg's VA-API hwaccel fills, so every
//! driver reads them as it is used to.

use crate::media::av1::{FrameHeader, KEY_FRAME, SEG_LVL_MAX, SequenceHeader, TileGroup};

use super::ffi::{
    DecPicParamAv1, DecSliceParamAv1, FilmGrainAv1, INVALID_ID, Id, SegmentationAv1,
    WarpedMotionAv1,
};

/// The surfaces a frame decodes into and references.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Surfaces {
    /// The frame as decoded, which later frames reference.
    pub(crate) current: Id,
    /// The frame as shown: `current` with film grain added, or `current` itself.
    pub(crate) display: Id,
    /// The frame in each reference slot.
    pub(crate) references: [Id; 8],
}

/// `FrameRestorationType` (`AV1_RESTORE_*`: none 0, Wiener 1, self-guided 2,
/// switchable 3) for each coded `lr_type` (none, switchable, Wiener, self-guided).
const REMAP_LR_TYPE: [u16; 4] = [0, 3, 1, 2];

/// The most a segment feature's value may be, by feature, and whether it is signed.
const FEATURE_MAX: [i16; SEG_LVL_MAX] = [255, 63, 63, 63, 63, 7, 0, 0];
const FEATURE_SIGNED: [bool; SEG_LVL_MAX] = [true, true, true, true, true, false, false, false];

/// The picture parameter buffer, for an 8-bit stream.
#[allow(clippy::too_many_lines, reason = "one struct, field for field")]
pub(crate) fn picture_params(
    seq: &SequenceHeader,
    h: &FrameHeader,
    surfaces: Surfaces,
) -> DecPicParamAv1 {
    let b = |v: bool| u32::from(v);
    let c = &seq.color;
    let seq_info_fields = b(seq.still_picture)
        | b(seq.use_128x128_superblock) << 1
        | b(seq.enable_filter_intra) << 2
        | b(seq.enable_intra_edge_filter) << 3
        | b(seq.enable_interintra_compound) << 4
        | b(seq.enable_masked_compound) << 5
        | b(seq.enable_dual_filter) << 6
        | b(seq.enable_order_hint) << 7
        | b(seq.enable_jnt_comp) << 8
        | b(seq.enable_cdef) << 9
        | b(c.mono_chrome) << 10
        | b(c.color_range) << 11
        | b(c.subsampling_x) << 12
        | b(c.subsampling_y) << 13
        | u32::from(c.chroma_sample_position & 1) << 14
        | b(seq.film_grain_params_present) << 15;
    let pic_info_fields = u32::from(h.frame_type & 3)
        | b(h.show_frame) << 2
        | b(h.showable_frame) << 3
        | b(h.error_resilient_mode) << 4
        | b(h.disable_cdf_update) << 5
        | b(h.allow_screen_content_tools) << 6
        | b(h.force_integer_mv) << 7
        | b(h.allow_intrabc) << 8
        | b(h.use_superres) << 9
        | b(h.allow_high_precision_mv) << 10
        | b(h.is_motion_mode_switchable) << 11
        | b(h.use_ref_frame_mvs) << 12
        | b(h.disable_frame_end_update_cdf) << 13
        | b(h.uniform_tile_spacing) << 14
        | b(h.allow_warped_motion) << 15;
    let mode_control_fields = b(h.delta_q_present)
        | u32::from(h.delta_q_res & 3) << 1
        | b(h.delta_lf_present) << 3
        | u32::from(h.delta_lf_res & 3) << 4
        | b(h.delta_lf_multi) << 6
        | u32::from(h.tx_mode & 3) << 7
        | b(h.reference_select) << 9
        | b(h.reduced_tx_set) << 10
        | b(h.skip_mode_present) << 11;
    let loop_restoration_fields = REMAP_LR_TYPE[usize::from(h.lr_type[0] & 3)]
        | REMAP_LR_TYPE[usize::from(h.lr_type[1] & 3)] << 2
        | REMAP_LR_TYPE[usize::from(h.lr_type[2] & 3)] << 4
        | u16::from(h.lr_unit_shift & 3) << 6
        | u16::from(h.lr_uv_shift & 1) << 8;
    let qmatrix_fields = u16::from(h.using_qmatrix)
        | u16::from(h.qm_y & 15) << 1
        | u16::from(h.qm_u & 15) << 5
        | u16::from(h.qm_v & 15) << 9;
    let loop_filter_info_fields = (h.loop_filter_sharpness & 7)
        | u8::from(h.loop_filter_delta_enabled) << 3
        | u8::from(h.loop_filter_delta_update) << 4;

    // A shown key frame references nothing.
    let references = if h.frame_type == KEY_FRAME && h.show_frame {
        [INVALID_ID; 8]
    } else {
        surfaces.references
    };

    let mut seg_info = SegmentationAv1 {
        segment_info_fields: b(h.segmentation_enabled)
            | b(h.segmentation_update_map) << 1
            | b(h.segmentation_temporal_update) << 2
            | b(h.segmentation_update_data) << 3,
        ..SegmentationAv1::default()
    };
    for i in 0..8 {
        for j in 0..SEG_LVL_MAX {
            seg_info.feature_mask[i] |= u8::from(h.feature_enabled[i][j]) << j;
            let min = if FEATURE_SIGNED[j] {
                -FEATURE_MAX[j]
            } else {
                0
            };
            seg_info.feature_data[i][j] = h.feature_value[i][j].clamp(min, FEATURE_MAX[j]);
        }
    }

    let mut width_in_sbs_minus_1 = [0_u16; 63];
    for (out, w) in width_in_sbs_minus_1.iter_mut().zip(&h.width_in_sbs) {
        *out = w.saturating_sub(1);
    }
    let mut height_in_sbs_minus_1 = [0_u16; 63];
    for (out, w) in height_in_sbs_minus_1.iter_mut().zip(&h.height_in_sbs) {
        *out = w.saturating_sub(1);
    }

    let mut wm = [WarpedMotionAv1::default(); 7];
    for (i, out) in wm.iter_mut().enumerate() {
        out.wmtype = u32::from(h.gm_type[i + 1]);
        out.wmmat[..6].copy_from_slice(&h.gm_params[i + 1]);
        out.invalid = u8::from(h.gm_invalid[i + 1]);
    }

    let mut cdef_y_strengths = [0; 8];
    let mut cdef_uv_strengths = [0; 8];
    let n = 1 << h.cdef_bits;
    cdef_y_strengths[..n].copy_from_slice(&h.cdef_y_strengths[..n]);
    cdef_uv_strengths[..n].copy_from_slice(&h.cdef_uv_strengths[..n]);

    let mut ref_frame_idx = [0; 7];
    ref_frame_idx.copy_from_slice(&h.ref_frame_idx);
    let tiles = h.tile_cols * h.tile_rows;

    DecPicParamAv1 {
        profile: seq.profile,
        order_hint_bits_minus_1: u8::try_from(seq.order_hint_bits.saturating_sub(1)).unwrap_or(0),
        // 8-bit.
        bit_depth_idx: 0,
        matrix_coefficients: c.matrix_coefficients,
        seq_info_fields,
        current_frame: surfaces.current,
        current_display_picture: surfaces.display,
        anchor_frames_num: 0,
        anchor_frames_list: 0,
        frame_width_minus1: u16::try_from(h.upscaled_width.saturating_sub(1)).unwrap_or(0),
        frame_height_minus1: u16::try_from(h.frame_height.saturating_sub(1)).unwrap_or(0),
        output_frame_width_in_tiles_minus_1: 0,
        output_frame_height_in_tiles_minus_1: 0,
        ref_frame_map: references,
        ref_frame_idx,
        primary_ref_frame: h.primary_ref_frame,
        order_hint: u8::try_from(h.order_hint).unwrap_or(0),
        seg_info,
        film_grain_info: film_grain(h),
        tile_cols: u8::try_from(h.tile_cols).unwrap_or(u8::MAX),
        tile_rows: u8::try_from(h.tile_rows).unwrap_or(u8::MAX),
        width_in_sbs_minus_1,
        height_in_sbs_minus_1,
        tile_count_minus_1: u16::try_from(tiles.saturating_sub(1)).unwrap_or(0),
        context_update_tile_id: h.context_update_tile_id,
        pic_info_fields,
        superres_scale_denominator: h.superres_denom,
        interp_filter: h.interpolation_filter,
        filter_level: [h.loop_filter_level[0], h.loop_filter_level[1]],
        filter_level_u: h.loop_filter_level[2],
        filter_level_v: h.loop_filter_level[3],
        loop_filter_info_fields,
        ref_deltas: h.loop_filter_ref_deltas,
        mode_deltas: h.loop_filter_mode_deltas,
        base_qindex: h.base_q_idx,
        y_dc_delta_q: h.delta_q_y_dc,
        u_dc_delta_q: h.delta_q_u_dc,
        u_ac_delta_q: h.delta_q_u_ac,
        v_dc_delta_q: h.delta_q_v_dc,
        v_ac_delta_q: h.delta_q_v_ac,
        qmatrix_fields,
        mode_control_fields,
        cdef_damping_minus_3: h.cdef_damping_minus_3,
        cdef_bits: h.cdef_bits,
        cdef_y_strengths,
        cdef_uv_strengths,
        loop_restoration_fields,
        wm,
        va_reserved: [0; 8],
    }
}

/// The film grain the driver adds to the shown frame.
fn film_grain(h: &FrameHeader) -> FilmGrainAv1 {
    let g = &h.film_grain;
    let b = |v: bool| u32::from(v);
    let mut out = FilmGrainAv1 {
        film_grain_info_fields: b(g.apply_grain)
            | b(g.chroma_scaling_from_luma) << 1
            | u32::from(g.grain_scaling_minus_8 & 3) << 2
            | u32::from(g.ar_coeff_lag & 3) << 4
            | u32::from(g.ar_coeff_shift_minus_6 & 3) << 6
            | u32::from(g.grain_scale_shift & 3) << 8
            | b(g.overlap_flag) << 10
            | b(g.clip_to_restricted_range) << 11,
        grain_seed: g.grain_seed,
        num_y_points: g.num_y_points,
        num_cb_points: g.num_cb_points,
        num_cr_points: g.num_cr_points,
        cb_mult: g.cb_mult,
        cb_luma_mult: g.cb_luma_mult,
        cb_offset: g.cb_offset,
        cr_mult: g.cr_mult,
        cr_luma_mult: g.cr_luma_mult,
        cr_offset: g.cr_offset,
        ..FilmGrainAv1::default()
    };
    if g.apply_grain {
        out.point_y_value = g.point_y_value;
        out.point_y_scaling = g.point_y_scaling;
        out.point_cb_value = g.point_cb_value;
        out.point_cb_scaling = g.point_cb_scaling;
        out.point_cr_value = g.point_cr_value;
        out.point_cr_scaling = g.point_cr_scaling;
        out.ar_coeffs_y = g.ar_coeffs_y;
        out.ar_coeffs_cb = g.ar_coeffs_cb;
        out.ar_coeffs_cr = g.ar_coeffs_cr;
    }
    out
}

/// The slice parameters of a tile group: one per tile, each at its offset in the tile
/// group's data.
pub(crate) fn slice_params(group: &TileGroup) -> Vec<DecSliceParamAv1> {
    group
        .tiles
        .iter()
        .map(|t| DecSliceParamAv1 {
            slice_data_size: t.size,
            slice_data_offset: t.offset,
            // VA_SLICE_DATA_FLAG_ALL: the tile is all in this buffer.
            slice_data_flag: 0,
            tile_row: t.row,
            tile_column: t.column,
            tg_start: group.tg_start,
            tg_end: group.tg_end,
            anchor_frame_idx: 0,
            tile_idx_in_tile_list: 0,
            va_reserved: [0; 4],
        })
        .collect()
}

#[cfg(test)]
mod tests;
