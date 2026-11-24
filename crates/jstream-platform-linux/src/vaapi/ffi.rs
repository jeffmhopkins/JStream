//! libva's H.264, HEVC and AV1 encode structs, its H.264, VP8, VP9 and AV1 decode structs
//! (ADR-0072), and their constants, hand-written (ADR-0060 §8).
//!
//! Each struct mirrors its C declaration field for field, `#[repr(C)]`, with C bitfield
//! unions as one `u32` (or `u16`) that `h264` and `hevc` pack. They were checked against
//! the headers of **libva 2.20.0** (VA-API 1.20.0, `va/va.h`, `va/va_enc_h264.h` and
//! `va/va_enc_hevc.h`) on x86_64 by compiling a C program that printed every `sizeof`,
//! the offsets of the fields after each padding gap, the constants, and the value of
//! each bitfield union with known fields set. The sizes and offsets are asserted at
//! compile time below, and the bitfield values in `h264`'s and `hevc`'s tests. libva only appends to these
//! structs (each ends in reserved padding), so a later libva reads them the same way.
//!
//! Bitfields are packed lowest bit first, as GCC and Clang lay them out on every Linux
//! ABI JStream supports (x86_64, aarch64).
//!
//! The H.264 decode structs (`va/va.h`'s `VAPictureParameterBufferH264`,
//! `VAIQMatrixBufferH264` and `VASliceParameterBufferH264`) were checked the same way,
//! against the same release, and their sizes and offsets are asserted below. So were
//! the VP9 decode structs (`va/va_dec_vp9.h`), with the bit of each `pic_fields` and
//! `segment_flags` field printed one at a time, and the VP8 ones (`va/va_dec_vp8.h`,
//! and `VAIQMatrixBufferVP8` and `VAProbabilityDataBufferVP8` from `va/va.h`), with
//! the bit of each `pic_fields` field printed the same way. So were the AV1 ones
//! (`va/va_dec_av1.h`), with a bit of each bitfield union printed the same way, and
//! the AV1 encode ones (`va/va_enc_av1.h`), with each bitfield `av1` packs, and the
//! `VAConfigAttribValEncAV1Ext2` fields it reads, printed the same way.
//!
//! The surface attributes and the DRM PRIME descriptor that import an encode target
//! (`va/va.h`, `va/va_drmcommon.h`) were checked the same way, against the same
//! release.
//!
//! Plain data with no pointers (apart from [`CodedBufferSegment`], which only libva
//! writes, [`GenericValue`], which carries the descriptor's address, and
//! [`DecPicParamAv1`]'s `anchor_frames_list`, which is always null), so it builds
//! on every platform and the parameter builders are tested everywhere.

/// `VAEntrypointVLD`: decoding.
pub(crate) const ENTRYPOINT_VLD: i32 = 1;
/// `VA_PICTURE_H264_LONG_TERM_REFERENCE`.
pub(crate) const PICTURE_H264_LONG_TERM_REFERENCE: u32 = 0x10;

/// The decode buffer types (`VABufferType`).
pub(crate) mod dec_buffer {
    /// `VAPictureParameterBufferType`.
    pub(crate) const PICTURE_PARAMETER: i32 = 0;
    /// `VAIQMatrixBufferType`.
    pub(crate) const IQ_MATRIX: i32 = 1;
    /// `VASliceParameterBufferType`.
    pub(crate) const SLICE_PARAMETER: i32 = 4;
    /// `VASliceDataBufferType`.
    pub(crate) const SLICE_DATA: i32 = 5;
    /// `VAProbabilityBufferType`: VP8's coefficient probabilities.
    pub(crate) const PROBABILITY: i32 = 13;
}

/// `VAPictureParameterBufferH264`: one picture to decode.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct DecPicParamH264 {
    pub(crate) curr_pic: PictureH264,
    pub(crate) reference_frames: [PictureH264; 16],
    pub(crate) picture_width_in_mbs_minus1: u16,
    pub(crate) picture_height_in_mbs_minus1: u16,
    pub(crate) bit_depth_luma_minus8: u8,
    pub(crate) bit_depth_chroma_minus8: u8,
    pub(crate) num_ref_frames: u8,
    /// `seq_fields`: `chroma_format_idc` (bits 0-1), `residual_colour_transform_flag`
    /// (2), `gaps_in_frame_num_value_allowed_flag` (3), `frame_mbs_only_flag` (4),
    /// `mb_adaptive_frame_field_flag` (5), `direct_8x8_inference_flag` (6),
    /// `MinLumaBiPredSize8x8` (7), `log2_max_frame_num_minus4` (8-11),
    /// `pic_order_cnt_type` (12-13), `log2_max_pic_order_cnt_lsb_minus4` (14-17),
    /// `delta_pic_order_always_zero_flag` (18).
    pub(crate) seq_fields: u32,
    pub(crate) num_slice_groups_minus1: u8,
    pub(crate) slice_group_map_type: u8,
    pub(crate) slice_group_change_rate_minus1: u16,
    pub(crate) pic_init_qp_minus26: i8,
    pub(crate) pic_init_qs_minus26: i8,
    pub(crate) chroma_qp_index_offset: i8,
    pub(crate) second_chroma_qp_index_offset: i8,
    /// `pic_fields`: `entropy_coding_mode_flag` (bit 0), `weighted_pred_flag` (1),
    /// `weighted_bipred_idc` (2-3), `transform_8x8_mode_flag` (4), `field_pic_flag`
    /// (5), `constrained_intra_pred_flag` (6), `pic_order_present_flag` (7),
    /// `deblocking_filter_control_present_flag` (8), `redundant_pic_cnt_present_flag`
    /// (9), `reference_pic_flag` (10).
    pub(crate) pic_fields: u32,
    pub(crate) frame_num: u16,
    pub(crate) va_reserved: [u32; 8],
}

/// `VAIQMatrixBufferH264`: the scaling lists, in zig-zag order.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct IqMatrixH264 {
    pub(crate) scaling_list_4x4: [[u8; 16]; 6],
    pub(crate) scaling_list_8x8: [[u8; 64]; 2],
    pub(crate) va_reserved: [u32; 4],
}

/// `VASliceParameterBufferH264`: one slice to decode.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct DecSliceParamH264 {
    pub(crate) slice_data_size: u32,
    pub(crate) slice_data_offset: u32,
    pub(crate) slice_data_flag: u32,
    pub(crate) slice_data_bit_offset: u16,
    pub(crate) first_mb_in_slice: u16,
    pub(crate) slice_type: u8,
    pub(crate) direct_spatial_mv_pred_flag: u8,
    pub(crate) num_ref_idx_l0_active_minus1: u8,
    pub(crate) num_ref_idx_l1_active_minus1: u8,
    pub(crate) cabac_init_idc: u8,
    pub(crate) slice_qp_delta: i8,
    pub(crate) disable_deblocking_filter_idc: u8,
    pub(crate) slice_alpha_c0_offset_div2: i8,
    pub(crate) slice_beta_offset_div2: i8,
    pub(crate) ref_pic_list0: [PictureH264; 32],
    pub(crate) ref_pic_list1: [PictureH264; 32],
    pub(crate) luma_log2_weight_denom: u8,
    pub(crate) chroma_log2_weight_denom: u8,
    pub(crate) luma_weight_l0_flag: u8,
    pub(crate) luma_weight_l0: [i16; 32],
    pub(crate) luma_offset_l0: [i16; 32],
    pub(crate) chroma_weight_l0_flag: u8,
    pub(crate) chroma_weight_l0: [[i16; 2]; 32],
    pub(crate) chroma_offset_l0: [[i16; 2]; 32],
    pub(crate) luma_weight_l1_flag: u8,
    pub(crate) luma_weight_l1: [i16; 32],
    pub(crate) luma_offset_l1: [i16; 32],
    pub(crate) chroma_weight_l1_flag: u8,
    pub(crate) chroma_weight_l1: [[i16; 2]; 32],
    pub(crate) chroma_offset_l1: [[i16; 2]; 32],
    pub(crate) va_reserved: [u32; 4],
}

/// `VADecPictureParameterBufferVP9`: one VP9 frame to decode.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct DecPicParamVp9 {
    pub(crate) frame_width: u16,
    pub(crate) frame_height: u16,
    pub(crate) reference_frames: [Id; 8],
    /// `pic_fields`: `subsampling_x` (bit 0), `subsampling_y` (1), `frame_type` (2),
    /// `show_frame` (3), `error_resilient_mode` (4), `intra_only` (5),
    /// `allow_high_precision_mv` (6), `mcomp_filter_type` (7-9),
    /// `frame_parallel_decoding_mode` (10), `reset_frame_context` (11-12),
    /// `refresh_frame_context` (13), `frame_context_idx` (14-15),
    /// `segmentation_enabled` (16), `segmentation_temporal_update` (17),
    /// `segmentation_update_map` (18), `last_ref_frame` (19-21),
    /// `last_ref_frame_sign_bias` (22), `golden_ref_frame` (23-25),
    /// `golden_ref_frame_sign_bias` (26), `alt_ref_frame` (27-29),
    /// `alt_ref_frame_sign_bias` (30), `lossless_flag` (31).
    pub(crate) pic_fields: u32,
    pub(crate) filter_level: u8,
    pub(crate) sharpness_level: u8,
    pub(crate) log2_tile_rows: u8,
    pub(crate) log2_tile_columns: u8,
    pub(crate) frame_header_length_in_bytes: u8,
    pub(crate) first_partition_size: u16,
    pub(crate) mb_segment_tree_probs: [u8; 7],
    pub(crate) segment_pred_probs: [u8; 3],
    pub(crate) profile: u8,
    pub(crate) bit_depth: u8,
    pub(crate) va_reserved: [u32; 8],
}

/// `VASegmentParameterVP9`: one segment's parameters.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SegmentParamVp9 {
    /// `segment_reference_enabled` (bit 0), `segment_reference` (1-2),
    /// `segment_reference_skipped` (3).
    pub(crate) segment_flags: u16,
    pub(crate) filter_level: [[u8; 2]; 4],
    pub(crate) luma_ac_quant_scale: i16,
    pub(crate) luma_dc_quant_scale: i16,
    pub(crate) chroma_ac_quant_scale: i16,
    pub(crate) chroma_dc_quant_scale: i16,
    pub(crate) va_reserved: [u32; 4],
}

/// `VASliceParameterBufferVP9`: the whole frame, as one slice.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct DecSliceParamVp9 {
    pub(crate) slice_data_size: u32,
    pub(crate) slice_data_offset: u32,
    pub(crate) slice_data_flag: u32,
    pub(crate) seg_param: [SegmentParamVp9; 8],
    pub(crate) va_reserved: [u32; 4],
}

/// `VABoolCoderContextVPX`: the boolean decoder's state where the frame header ends.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BoolCoderContextVpx {
    pub(crate) range: u8,
    pub(crate) value: u8,
    pub(crate) count: u8,
}

/// `VAPictureParameterBufferVP8`: one VP8 frame to decode.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct DecPicParamVp8 {
    pub(crate) frame_width: u32,
    pub(crate) frame_height: u32,
    pub(crate) last_ref_frame: Id,
    pub(crate) golden_ref_frame: Id,
    pub(crate) alt_ref_frame: Id,
    pub(crate) out_of_loop_frame: Id,
    /// `pic_fields`: `key_frame` (bit 0, set for an inter frame), `version` (1-3),
    /// `segmentation_enabled` (4), `update_mb_segmentation_map` (5),
    /// `update_segment_feature_data` (6), `filter_type` (7), `sharpness_level` (8-10),
    /// `loop_filter_adj_enable` (11), `mode_ref_lf_delta_update` (12),
    /// `sign_bias_golden` (13), `sign_bias_alternate` (14), `mb_no_coeff_skip` (15),
    /// `loop_filter_disable` (16).
    pub(crate) pic_fields: u32,
    pub(crate) mb_segment_tree_probs: [u8; 3],
    pub(crate) loop_filter_level: [u8; 4],
    pub(crate) loop_filter_deltas_ref_frame: [i8; 4],
    pub(crate) loop_filter_deltas_mode: [i8; 4],
    pub(crate) prob_skip_false: u8,
    pub(crate) prob_intra: u8,
    pub(crate) prob_last: u8,
    pub(crate) prob_gf: u8,
    pub(crate) y_mode_probs: [u8; 4],
    pub(crate) uv_mode_probs: [u8; 3],
    pub(crate) mv_probs: [[u8; 19]; 2],
    pub(crate) bool_coder_ctx: BoolCoderContextVpx,
    pub(crate) va_reserved: [u32; 4],
}

/// `VASliceParameterBufferVP8`: the frame's partitions, as one slice.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct DecSliceParamVp8 {
    pub(crate) slice_data_size: u32,
    pub(crate) slice_data_offset: u32,
    pub(crate) slice_data_flag: u32,
    pub(crate) macroblock_offset: u32,
    pub(crate) num_of_partitions: u8,
    pub(crate) partition_size: [u32; 9],
    pub(crate) va_reserved: [u32; 4],
}

/// `VAProbabilityDataBufferVP8`: the coefficient probabilities, by coefficient band.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct ProbabilityDataVp8 {
    pub(crate) dct_coeff_probs: [[[[u8; 11]; 3]; 8]; 4],
    pub(crate) va_reserved: [u32; 4],
}

/// `VAIQMatrixBufferVP8`: each segment's quantizer indices (Y AC, Y DC, Y2 DC, Y2 AC,
/// UV DC, UV AC).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct IqMatrixVp8 {
    pub(crate) quantization_index: [[u16; 6]; 4],
    pub(crate) va_reserved: [u32; 4],
}

/// `VASegmentationStructAV1`: the segmentation features.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SegmentationAv1 {
    /// `segment_info_fields`: `enabled` (bit 0), `update_map` (1), `temporal_update`
    /// (2), `update_data` (3).
    pub(crate) segment_info_fields: u32,
    pub(crate) feature_data: [[i16; 8]; 8],
    /// Each segment's enabled features, a bit each.
    pub(crate) feature_mask: [u8; 8],
    pub(crate) va_reserved: [u32; 4],
}

/// `VAFilmGrainStructAV1`: the film grain to apply to the shown frame.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FilmGrainAv1 {
    /// `film_grain_info_fields`: `apply_grain` (bit 0), `chroma_scaling_from_luma`
    /// (1), `grain_scaling_minus_8` (2-3), `ar_coeff_lag` (4-5), `ar_coeff_shift_minus_6`
    /// (6-7), `grain_scale_shift` (8-9), `overlap_flag` (10), `clip_to_restricted_range`
    /// (11).
    pub(crate) film_grain_info_fields: u32,
    pub(crate) grain_seed: u16,
    pub(crate) num_y_points: u8,
    pub(crate) point_y_value: [u8; 14],
    pub(crate) point_y_scaling: [u8; 14],
    pub(crate) num_cb_points: u8,
    pub(crate) point_cb_value: [u8; 10],
    pub(crate) point_cb_scaling: [u8; 10],
    pub(crate) num_cr_points: u8,
    pub(crate) point_cr_value: [u8; 10],
    pub(crate) point_cr_scaling: [u8; 10],
    pub(crate) ar_coeffs_y: [i8; 24],
    pub(crate) ar_coeffs_cb: [i8; 25],
    pub(crate) ar_coeffs_cr: [i8; 25],
    pub(crate) cb_mult: u8,
    pub(crate) cb_luma_mult: u8,
    pub(crate) cb_offset: u16,
    pub(crate) cr_mult: u8,
    pub(crate) cr_luma_mult: u8,
    pub(crate) cr_offset: u16,
    pub(crate) va_reserved: [u32; 4],
}

/// `VAWarpedMotionParamsAV1`: one reference's global motion.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WarpedMotionAv1 {
    /// `VAAV1TransformationType`: identity, translation, rotzoom, affine.
    pub(crate) wmtype: u32,
    pub(crate) wmmat: [i32; 8],
    pub(crate) invalid: u8,
    pub(crate) va_reserved: [u32; 4],
}

/// `VADecPictureParameterBufferAV1`: one AV1 frame to decode.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct DecPicParamAv1 {
    pub(crate) profile: u8,
    pub(crate) order_hint_bits_minus_1: u8,
    pub(crate) bit_depth_idx: u8,
    pub(crate) matrix_coefficients: u8,
    /// `seq_info_fields`: `still_picture` (bit 0), `use_128x128_superblock` (1),
    /// `enable_filter_intra` (2), `enable_intra_edge_filter` (3),
    /// `enable_interintra_compound` (4), `enable_masked_compound` (5),
    /// `enable_dual_filter` (6), `enable_order_hint` (7), `enable_jnt_comp` (8),
    /// `enable_cdef` (9), `mono_chrome` (10), `color_range` (11), `subsampling_x` (12),
    /// `subsampling_y` (13), `chroma_sample_position` (14), `film_grain_params_present`
    /// (15).
    pub(crate) seq_info_fields: u32,
    pub(crate) current_frame: Id,
    pub(crate) current_display_picture: Id,
    pub(crate) anchor_frames_num: u8,
    /// `VASurfaceID *anchor_frames_list`: large-scale tiles only, so always null here.
    /// A `usize` keeps the struct plain data; it is a pointer's size and alignment on
    /// every Linux ABI JStream supports.
    pub(crate) anchor_frames_list: usize,
    pub(crate) frame_width_minus1: u16,
    pub(crate) frame_height_minus1: u16,
    pub(crate) output_frame_width_in_tiles_minus_1: u16,
    pub(crate) output_frame_height_in_tiles_minus_1: u16,
    pub(crate) ref_frame_map: [Id; 8],
    pub(crate) ref_frame_idx: [u8; 7],
    pub(crate) primary_ref_frame: u8,
    pub(crate) order_hint: u8,
    pub(crate) seg_info: SegmentationAv1,
    pub(crate) film_grain_info: FilmGrainAv1,
    pub(crate) tile_cols: u8,
    pub(crate) tile_rows: u8,
    pub(crate) width_in_sbs_minus_1: [u16; 63],
    pub(crate) height_in_sbs_minus_1: [u16; 63],
    pub(crate) tile_count_minus_1: u16,
    pub(crate) context_update_tile_id: u16,
    /// `pic_info_fields`: `frame_type` (bits 0-1), `show_frame` (2), `showable_frame`
    /// (3), `error_resilient_mode` (4), `disable_cdf_update` (5),
    /// `allow_screen_content_tools` (6), `force_integer_mv` (7), `allow_intrabc` (8),
    /// `use_superres` (9), `allow_high_precision_mv` (10), `is_motion_mode_switchable`
    /// (11), `use_ref_frame_mvs` (12), `disable_frame_end_update_cdf` (13),
    /// `uniform_tile_spacing_flag` (14), `allow_warped_motion` (15), `large_scale_tile`
    /// (16).
    pub(crate) pic_info_fields: u32,
    pub(crate) superres_scale_denominator: u8,
    pub(crate) interp_filter: u8,
    pub(crate) filter_level: [u8; 2],
    pub(crate) filter_level_u: u8,
    pub(crate) filter_level_v: u8,
    /// `loop_filter_info_fields`: `sharpness_level` (bits 0-2), `mode_ref_delta_enabled`
    /// (3), `mode_ref_delta_update` (4).
    pub(crate) loop_filter_info_fields: u8,
    pub(crate) ref_deltas: [i8; 8],
    pub(crate) mode_deltas: [i8; 2],
    pub(crate) base_qindex: u8,
    pub(crate) y_dc_delta_q: i8,
    pub(crate) u_dc_delta_q: i8,
    pub(crate) u_ac_delta_q: i8,
    pub(crate) v_dc_delta_q: i8,
    pub(crate) v_ac_delta_q: i8,
    /// `qmatrix_fields`: `using_qmatrix` (bit 0), `qm_y` (1-4), `qm_u` (5-8), `qm_v`
    /// (9-12).
    pub(crate) qmatrix_fields: u16,
    /// `mode_control_fields`: `delta_q_present_flag` (bit 0), `log2_delta_q_res` (1-2),
    /// `delta_lf_present_flag` (3), `log2_delta_lf_res` (4-5), `delta_lf_multi` (6),
    /// `tx_mode` (7-8), `reference_select` (9), `reduced_tx_set_used` (10),
    /// `skip_mode_present` (11).
    pub(crate) mode_control_fields: u32,
    pub(crate) cdef_damping_minus_3: u8,
    pub(crate) cdef_bits: u8,
    pub(crate) cdef_y_strengths: [u8; 8],
    pub(crate) cdef_uv_strengths: [u8; 8],
    /// `loop_restoration_fields`: `yframe_restoration_type` (bits 0-1),
    /// `cbframe_restoration_type` (2-3), `crframe_restoration_type` (4-5),
    /// `lr_unit_shift` (6-7), `lr_uv_shift` (8).
    pub(crate) loop_restoration_fields: u16,
    pub(crate) wm: [WarpedMotionAv1; 7],
    pub(crate) va_reserved: [u32; 8],
}

/// `VASliceParameterBufferAV1`: one tile.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DecSliceParamAv1 {
    pub(crate) slice_data_size: u32,
    pub(crate) slice_data_offset: u32,
    pub(crate) slice_data_flag: u32,
    pub(crate) tile_row: u16,
    pub(crate) tile_column: u16,
    pub(crate) tg_start: u16,
    pub(crate) tg_end: u16,
    pub(crate) anchor_frame_idx: u8,
    pub(crate) tile_idx_in_tile_list: u16,
    pub(crate) va_reserved: [u32; 4],
}

/// `VAStatus`: `int`.
pub(crate) type Status = i32;
/// `VASurfaceID`, `VABufferID`, `VAConfigID`, `VAContextID`, `VAImageID`: `unsigned int`.
pub(crate) type Id = u32;

/// `VA_STATUS_SUCCESS`.
pub(crate) const STATUS_SUCCESS: Status = 0;
/// `VA_INVALID_ID`, also `VA_INVALID_SURFACE`.
pub(crate) const INVALID_ID: Id = 0xFFFF_FFFF;
/// `VA_RT_FORMAT_YUV420`.
pub(crate) const RT_FORMAT_YUV420: u32 = 0x1;
/// `VA_FOURCC_NV12`: `'N' | 'V' << 8 | '1' << 16 | '2' << 24`.
pub(crate) const FOURCC_NV12: u32 = 0x3231_564E;
/// `VA_LSB_FIRST`.
pub(crate) const LSB_FIRST: u32 = 1;
/// `VA_PROGRESSIVE`, the `vaCreateContext` flag.
pub(crate) const PROGRESSIVE: i32 = 0x1;
/// `VA_ATTRIB_NOT_SUPPORTED`.
pub(crate) const ATTRIB_NOT_SUPPORTED: u32 = 0x8000_0000;
/// `VA_RC_CBR`.
pub(crate) const RC_CBR: u32 = 0x2;
/// `VA_RC_VBR`.
pub(crate) const RC_VBR: u32 = 0x4;
/// `VA_ENC_PACKED_HEADER_SEQUENCE`.
pub(crate) const PACKED_HEADER_SEQUENCE: u32 = 0x1;
/// `VA_ENC_PACKED_HEADER_PICTURE`.
pub(crate) const PACKED_HEADER_PICTURE: u32 = 0x2;
/// `VA_ENC_PACKED_HEADER_SLICE`.
pub(crate) const PACKED_HEADER_SLICE: u32 = 0x4;
/// `VA_FEATURE_REQUIRED`: a two-bit field of `VAConfigAttribValEncHEVCFeatures` whose
/// tool the driver always codes.
pub(crate) const FEATURE_REQUIRED: u32 = 2;
/// `VA_PREDICTION_DIRECTION_BI_NOT_EMPTY`: the driver codes no P slices, so a P frame is
/// coded as a B slice with the same reference in both lists.
pub(crate) const PREDICTION_DIRECTION_BI_NOT_EMPTY: u32 = 0x4;
/// `VA_PICTURE_H264_INVALID`.
pub(crate) const PICTURE_H264_INVALID: u32 = 0x1;
/// `VA_PICTURE_H264_SHORT_TERM_REFERENCE`.
pub(crate) const PICTURE_H264_SHORT_TERM_REFERENCE: u32 = 0x8;
/// `VA_PICTURE_HEVC_INVALID`.
pub(crate) const PICTURE_HEVC_INVALID: u32 = 0x1;
/// `VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE`: a short-term reference before the picture.
pub(crate) const PICTURE_HEVC_RPS_ST_CURR_BEFORE: u32 = 0x10;
/// `VA_CODED_BUF_STATUS_SLICE_OVERFLOW_MASK`: the picture didn't fit the coded buffer.
pub(crate) const CODED_BUF_STATUS_SLICE_OVERFLOW_MASK: u32 = 0x200;
/// `VASurfaceRendering` (`VASurfaceStatus`): the surface is still being encoded.
pub(crate) const SURFACE_RENDERING: i32 = 1;
/// `VASurfaceSkipped` (`VASurfaceStatus`): the rate controller dropped the frame.
pub(crate) const SURFACE_SKIPPED: i32 = 8;

/// `VASurfaceAttribType` values, and what goes with them.
pub(crate) mod surface_attrib {
    /// `VASurfaceAttribMemoryType`: an integer of `VA_SURFACE_ATTRIB_MEM_TYPE_*` bits.
    pub(crate) const MEMORY_TYPE: i32 = 6;
    /// `VASurfaceAttribExternalBufferDescriptor`: a pointer to the descriptor.
    pub(crate) const EXTERNAL_BUFFER_DESCRIPTOR: i32 = 7;
    /// `VA_SURFACE_ATTRIB_SETTABLE`.
    pub(crate) const SETTABLE: u32 = 0x2;
    /// `VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2`: a [`super::DrmPrimeSurfaceDescriptor`].
    pub(crate) const MEM_TYPE_DRM_PRIME_2: i32 = 0x4000_0000;
}

/// `VAGenericValueType` values.
pub(crate) mod generic {
    /// `VAGenericValueTypeInteger`.
    pub(crate) const INTEGER: i32 = 1;
    /// `VAGenericValueTypePointer`.
    pub(crate) const POINTER: i32 = 3;
}

/// `DRM_FORMAT_NV12` (`drm_fourcc.h`): the same four characters as [`FOURCC_NV12`].
pub(crate) const DRM_FORMAT_NV12: u32 = 0x3231_564E;

/// `VAConfigAttribType` values.
pub(crate) mod attrib {
    /// `VAConfigAttribRTFormat`.
    pub(crate) const RT_FORMAT: i32 = 0;
    /// `VAConfigAttribRateControl`.
    pub(crate) const RATE_CONTROL: i32 = 5;
    /// `VAConfigAttribEncPackedHeaders`.
    pub(crate) const ENC_PACKED_HEADERS: i32 = 10;
    /// `VAConfigAttribPredictionDirection`.
    pub(crate) const PREDICTION_DIRECTION: i32 = 39;
    /// `VAConfigAttribEncHEVCFeatures`: a `VAConfigAttribValEncHEVCFeatures`.
    pub(crate) const ENC_HEVC_FEATURES: i32 = 50;
    /// `VAConfigAttribEncHEVCBlockSizes`: a `VAConfigAttribValEncHEVCBlockSizes`.
    pub(crate) const ENC_HEVC_BLOCK_SIZES: i32 = 51;
    /// `VAConfigAttribEncAV1Ext2`: a `VAConfigAttribValEncAV1Ext2`.
    pub(crate) const ENC_AV1_EXT2: i32 = 54;
}

/// `VABufferType` values.
pub(crate) mod buffer {
    /// `VAEncCodedBufferType`.
    pub(crate) const CODED: i32 = 21;
    /// `VAEncSequenceParameterBufferType`.
    pub(crate) const SEQUENCE_PARAMETER: i32 = 22;
    /// `VAEncPictureParameterBufferType`.
    pub(crate) const PICTURE_PARAMETER: i32 = 23;
    /// `VAEncSliceParameterBufferType`.
    pub(crate) const SLICE_PARAMETER: i32 = 24;
    /// `VAEncPackedHeaderParameterBufferType`.
    pub(crate) const PACKED_HEADER_PARAMETER: i32 = 25;
    /// `VAEncPackedHeaderDataBufferType`.
    pub(crate) const PACKED_HEADER_DATA: i32 = 26;
    /// `VAEncMiscParameterBufferType`.
    pub(crate) const MISC_PARAMETER: i32 = 27;
}

/// `VAEncPackedHeaderType` values.
pub(crate) mod packed {
    /// `VAEncPackedHeaderSequence`.
    pub(crate) const SEQUENCE: u32 = 1;
    /// `VAEncPackedHeaderPicture` (AV1's frame header OBU).
    pub(crate) const PICTURE: u32 = 2;
    /// `VAEncPackedHeaderSlice` (`VAEncPackedHeaderHEVC_Slice`).
    pub(crate) const SLICE: u32 = 3;
}

/// `VAEncMiscParameterType` values.
pub(crate) mod misc {
    /// `VAEncMiscParameterTypeFrameRate`.
    pub(crate) const FRAME_RATE: u32 = 0;
    /// `VAEncMiscParameterTypeRateControl`.
    pub(crate) const RATE_CONTROL: u32 = 1;
    /// `VAEncMiscParameterTypeHRD`.
    pub(crate) const HRD: u32 = 5;
}

/// `VAConfigAttrib`: `{ VAConfigAttribType type; uint32_t value; }`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ConfigAttrib {
    pub(crate) kind: i32,
    pub(crate) value: u32,
}

/// `VAPictureH264`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PictureH264 {
    pub(crate) picture_id: Id,
    pub(crate) frame_idx: u32,
    pub(crate) flags: u32,
    pub(crate) top_field_order_cnt: i32,
    pub(crate) bottom_field_order_cnt: i32,
    pub(crate) va_reserved: [u32; 4],
}

impl PictureH264 {
    /// An unused entry: `VA_INVALID_SURFACE` with `VA_PICTURE_H264_INVALID`.
    pub(crate) const INVALID: Self = Self {
        picture_id: INVALID_ID,
        frame_idx: 0,
        flags: PICTURE_H264_INVALID,
        top_field_order_cnt: 0,
        bottom_field_order_cnt: 0,
        va_reserved: [0; 4],
    };
}

/// `VAEncSequenceParameterBufferH264`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SeqParamH264 {
    pub(crate) seq_parameter_set_id: u8,
    pub(crate) level_idc: u8,
    pub(crate) intra_period: u32,
    pub(crate) intra_idr_period: u32,
    pub(crate) ip_period: u32,
    pub(crate) bits_per_second: u32,
    pub(crate) max_num_ref_frames: u32,
    pub(crate) picture_width_in_mbs: u16,
    pub(crate) picture_height_in_mbs: u16,
    /// The `seq_fields` bitfield union.
    pub(crate) seq_fields: u32,
    pub(crate) bit_depth_luma_minus8: u8,
    pub(crate) bit_depth_chroma_minus8: u8,
    pub(crate) num_ref_frames_in_pic_order_cnt_cycle: u8,
    pub(crate) offset_for_non_ref_pic: i32,
    pub(crate) offset_for_top_to_bottom_field: i32,
    pub(crate) offset_for_ref_frame: [i32; 256],
    pub(crate) frame_cropping_flag: u8,
    pub(crate) frame_crop_left_offset: u32,
    pub(crate) frame_crop_right_offset: u32,
    pub(crate) frame_crop_top_offset: u32,
    pub(crate) frame_crop_bottom_offset: u32,
    pub(crate) vui_parameters_present_flag: u8,
    /// The `vui_fields` bitfield union.
    pub(crate) vui_fields: u32,
    pub(crate) aspect_ratio_idc: u8,
    pub(crate) sar_width: u32,
    pub(crate) sar_height: u32,
    pub(crate) num_units_in_tick: u32,
    pub(crate) time_scale: u32,
    pub(crate) va_reserved: [u32; 4],
}

/// `VAEncPictureParameterBufferH264`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PicParamH264 {
    pub(crate) curr_pic: PictureH264,
    pub(crate) reference_frames: [PictureH264; 16],
    pub(crate) coded_buf: Id,
    pub(crate) pic_parameter_set_id: u8,
    pub(crate) seq_parameter_set_id: u8,
    pub(crate) last_picture: u8,
    pub(crate) frame_num: u16,
    pub(crate) pic_init_qp: u8,
    pub(crate) num_ref_idx_l0_active_minus1: u8,
    pub(crate) num_ref_idx_l1_active_minus1: u8,
    pub(crate) chroma_qp_index_offset: i8,
    pub(crate) second_chroma_qp_index_offset: i8,
    /// The `pic_fields` bitfield union.
    pub(crate) pic_fields: u32,
    pub(crate) va_reserved: [u32; 4],
}

/// `VAEncSliceParameterBufferH264`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SliceParamH264 {
    pub(crate) macroblock_address: u32,
    pub(crate) num_macroblocks: u32,
    pub(crate) macroblock_info: Id,
    pub(crate) slice_type: u8,
    pub(crate) pic_parameter_set_id: u8,
    pub(crate) idr_pic_id: u16,
    pub(crate) pic_order_cnt_lsb: u16,
    pub(crate) delta_pic_order_cnt_bottom: i32,
    pub(crate) delta_pic_order_cnt: [i32; 2],
    pub(crate) direct_spatial_mv_pred_flag: u8,
    pub(crate) num_ref_idx_active_override_flag: u8,
    pub(crate) num_ref_idx_l0_active_minus1: u8,
    pub(crate) num_ref_idx_l1_active_minus1: u8,
    pub(crate) ref_pic_list0: [PictureH264; 32],
    pub(crate) ref_pic_list1: [PictureH264; 32],
    pub(crate) luma_log2_weight_denom: u8,
    pub(crate) chroma_log2_weight_denom: u8,
    pub(crate) luma_weight_l0_flag: u8,
    pub(crate) luma_weight_l0: [i16; 32],
    pub(crate) luma_offset_l0: [i16; 32],
    pub(crate) chroma_weight_l0_flag: u8,
    pub(crate) chroma_weight_l0: [[i16; 2]; 32],
    pub(crate) chroma_offset_l0: [[i16; 2]; 32],
    pub(crate) luma_weight_l1_flag: u8,
    pub(crate) luma_weight_l1: [i16; 32],
    pub(crate) luma_offset_l1: [i16; 32],
    pub(crate) chroma_weight_l1_flag: u8,
    pub(crate) chroma_weight_l1: [[i16; 2]; 32],
    pub(crate) chroma_offset_l1: [[i16; 2]; 32],
    pub(crate) cabac_init_idc: u8,
    pub(crate) slice_qp_delta: i8,
    pub(crate) disable_deblocking_filter_idc: u8,
    pub(crate) slice_alpha_c0_offset_div2: i8,
    pub(crate) slice_beta_offset_div2: i8,
    pub(crate) va_reserved: [u32; 4],
}

/// `VAPictureHEVC`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PictureHevc {
    pub(crate) picture_id: Id,
    pub(crate) pic_order_cnt: i32,
    pub(crate) flags: u32,
    pub(crate) va_reserved: [u32; 4],
}

impl PictureHevc {
    /// An unused entry: `VA_INVALID_SURFACE` with `VA_PICTURE_HEVC_INVALID`.
    pub(crate) const INVALID: Self = Self {
        picture_id: INVALID_ID,
        pic_order_cnt: 0,
        flags: PICTURE_HEVC_INVALID,
        va_reserved: [0; 4],
    };
}

/// `VAEncSequenceParameterBufferHEVC`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SeqParamHevc {
    pub(crate) general_profile_idc: u8,
    pub(crate) general_level_idc: u8,
    pub(crate) general_tier_flag: u8,
    pub(crate) intra_period: u32,
    pub(crate) intra_idr_period: u32,
    pub(crate) ip_period: u32,
    pub(crate) bits_per_second: u32,
    pub(crate) pic_width_in_luma_samples: u16,
    pub(crate) pic_height_in_luma_samples: u16,
    /// The `seq_fields` bitfield union.
    pub(crate) seq_fields: u32,
    pub(crate) log2_min_luma_coding_block_size_minus3: u8,
    pub(crate) log2_diff_max_min_luma_coding_block_size: u8,
    pub(crate) log2_min_transform_block_size_minus2: u8,
    pub(crate) log2_diff_max_min_transform_block_size: u8,
    pub(crate) max_transform_hierarchy_depth_inter: u8,
    pub(crate) max_transform_hierarchy_depth_intra: u8,
    pub(crate) pcm_sample_bit_depth_luma_minus1: u32,
    pub(crate) pcm_sample_bit_depth_chroma_minus1: u32,
    pub(crate) log2_min_pcm_luma_coding_block_size_minus3: u32,
    pub(crate) log2_max_pcm_luma_coding_block_size_minus3: u32,
    pub(crate) vui_parameters_present_flag: u8,
    /// The `vui_fields` bitfield union.
    pub(crate) vui_fields: u32,
    pub(crate) aspect_ratio_idc: u8,
    pub(crate) sar_width: u32,
    pub(crate) sar_height: u32,
    pub(crate) vui_num_units_in_tick: u32,
    pub(crate) vui_time_scale: u32,
    pub(crate) min_spatial_segmentation_idc: u16,
    pub(crate) max_bytes_per_pic_denom: u8,
    pub(crate) max_bits_per_min_cu_denom: u8,
    /// The `scc_fields` bitfield union.
    pub(crate) scc_fields: u32,
    pub(crate) va_reserved: [u32; 7],
}

/// `VAEncPictureParameterBufferHEVC`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PicParamHevc {
    pub(crate) decoded_curr_pic: PictureHevc,
    pub(crate) reference_frames: [PictureHevc; 15],
    pub(crate) coded_buf: Id,
    pub(crate) collocated_ref_pic_index: u8,
    pub(crate) last_picture: u8,
    pub(crate) pic_init_qp: u8,
    pub(crate) diff_cu_qp_delta_depth: u8,
    pub(crate) pps_cb_qp_offset: i8,
    pub(crate) pps_cr_qp_offset: i8,
    pub(crate) num_tile_columns_minus1: u8,
    pub(crate) num_tile_rows_minus1: u8,
    pub(crate) column_width_minus1: [u8; 19],
    pub(crate) row_height_minus1: [u8; 21],
    pub(crate) log2_parallel_merge_level_minus2: u8,
    pub(crate) ctu_max_bitsize_allowed: u8,
    pub(crate) num_ref_idx_l0_default_active_minus1: u8,
    pub(crate) num_ref_idx_l1_default_active_minus1: u8,
    pub(crate) slice_pic_parameter_set_id: u8,
    pub(crate) nal_unit_type: u8,
    /// The `pic_fields` bitfield union.
    pub(crate) pic_fields: u32,
    pub(crate) hierarchical_level_plus1: u8,
    pub(crate) va_byte_reserved: u8,
    /// The `scc_fields` bitfield union (16 bits).
    pub(crate) scc_fields: u16,
    pub(crate) va_reserved: [u32; 15],
}

/// `VAEncSliceParameterBufferHEVC`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SliceParamHevc {
    pub(crate) slice_segment_address: u32,
    pub(crate) num_ctu_in_slice: u32,
    pub(crate) slice_type: u8,
    pub(crate) slice_pic_parameter_set_id: u8,
    pub(crate) num_ref_idx_l0_active_minus1: u8,
    pub(crate) num_ref_idx_l1_active_minus1: u8,
    pub(crate) ref_pic_list0: [PictureHevc; 15],
    pub(crate) ref_pic_list1: [PictureHevc; 15],
    pub(crate) luma_log2_weight_denom: u8,
    pub(crate) delta_chroma_log2_weight_denom: i8,
    pub(crate) delta_luma_weight_l0: [i8; 15],
    pub(crate) luma_offset_l0: [i8; 15],
    pub(crate) delta_chroma_weight_l0: [[i8; 2]; 15],
    pub(crate) chroma_offset_l0: [[i8; 2]; 15],
    pub(crate) delta_luma_weight_l1: [i8; 15],
    pub(crate) luma_offset_l1: [i8; 15],
    pub(crate) delta_chroma_weight_l1: [[i8; 2]; 15],
    pub(crate) chroma_offset_l1: [[i8; 2]; 15],
    pub(crate) max_num_merge_cand: u8,
    pub(crate) slice_qp_delta: i8,
    pub(crate) slice_cb_qp_offset: i8,
    pub(crate) slice_cr_qp_offset: i8,
    pub(crate) slice_beta_offset_div2: i8,
    pub(crate) slice_tc_offset_div2: i8,
    /// The `slice_fields` bitfield union.
    pub(crate) slice_fields: u32,
    pub(crate) pred_weight_table_bit_offset: u32,
    pub(crate) pred_weight_table_bit_length: u32,
    pub(crate) va_reserved: [u32; 6],
}

/// `VAEncMiscParameterRateControl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RateControlParam {
    pub(crate) bits_per_second: u32,
    pub(crate) target_percentage: u32,
    pub(crate) window_size: u32,
    pub(crate) initial_qp: u32,
    pub(crate) min_qp: u32,
    pub(crate) basic_unit_size: u32,
    /// The `rc_flags` bitfield union.
    pub(crate) rc_flags: u32,
    pub(crate) icq_quality_factor: u32,
    pub(crate) max_qp: u32,
    pub(crate) quality_factor: u32,
    pub(crate) target_frame_size: u32,
    pub(crate) va_reserved: [u32; 4],
}

/// `VAEncMiscParameterFrameRate`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FrameRateParam {
    /// Denominator in the high 16 bits, numerator in the low 16.
    pub(crate) framerate: u32,
    /// The `framerate_flags` bitfield union.
    pub(crate) framerate_flags: u32,
    pub(crate) va_reserved: [u32; 4],
}

/// `VAEncMiscParameterHRD`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HrdParam {
    pub(crate) initial_buffer_fullness: u32,
    pub(crate) buffer_size: u32,
    pub(crate) va_reserved: [u32; 4],
}

/// `VAEncMiscParameterBuffer` followed by its payload: `{ VAEncMiscParameterType type;
/// uint32_t data[]; }`, `data` being `T`. Every payload here is 4-byte aligned, so it
/// starts right after `type`, where libva reads it.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MiscParam<T> {
    pub(crate) kind: u32,
    pub(crate) data: T,
}

/// `VAEncSequenceParameterBufferAV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SeqParamAv1 {
    pub(crate) seq_profile: u8,
    pub(crate) seq_level_idx: u8,
    pub(crate) seq_tier: u8,
    pub(crate) hierarchical_flag: u8,
    pub(crate) intra_period: u32,
    pub(crate) ip_period: u32,
    pub(crate) bits_per_second: u32,
    /// The `seq_fields` bitfield union.
    pub(crate) seq_fields: u32,
    pub(crate) order_hint_bits_minus_1: u8,
    pub(crate) va_reserved: [u32; 16],
}

/// `VAEncSegParamAV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SegParamAv1 {
    /// The `seg_flags` bitfield union.
    pub(crate) seg_flags: u8,
    pub(crate) segment_number: u8,
    pub(crate) feature_data: [[i16; 8]; 8],
    pub(crate) feature_mask: [u8; 8],
    pub(crate) va_reserved: [u32; 4],
}

/// `VAEncPictureParameterBufferAV1`. Its `wm` is the decoder's [`WarpedMotionAv1`],
/// which `VAEncWarpedMotionParamsAV1` matches field for field.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct PicParamAv1 {
    pub(crate) frame_width_minus_1: u16,
    pub(crate) frame_height_minus_1: u16,
    pub(crate) reconstructed_frame: Id,
    pub(crate) coded_buf: Id,
    /// The surfaces in the eight reference slots, `INVALID_ID` where unused.
    pub(crate) reference_frames: [Id; 8],
    pub(crate) ref_frame_idx: [u8; 7],
    pub(crate) hierarchical_level_plus1: u8,
    pub(crate) primary_ref_frame: u8,
    pub(crate) order_hint: u8,
    pub(crate) refresh_frame_flags: u8,
    pub(crate) reserved8bits1: u8,
    /// The `VARefFrameCtrlAV1` unions for list 0 and list 1.
    pub(crate) ref_frame_ctrl_l0: u32,
    pub(crate) ref_frame_ctrl_l1: u32,
    /// The `picture_flags` bitfield union.
    pub(crate) picture_flags: u32,
    pub(crate) seg_id_block_size: u8,
    pub(crate) num_tile_groups_minus1: u8,
    pub(crate) temporal_id: u8,
    pub(crate) filter_level: [u8; 2],
    pub(crate) filter_level_u: u8,
    pub(crate) filter_level_v: u8,
    /// The `loop_filter_flags` bitfield union.
    pub(crate) loop_filter_flags: u8,
    pub(crate) superres_scale_denominator: u8,
    pub(crate) interpolation_filter: u8,
    pub(crate) ref_deltas: [i8; 8],
    pub(crate) mode_deltas: [i8; 2],
    pub(crate) base_qindex: u8,
    pub(crate) y_dc_delta_q: i8,
    pub(crate) u_dc_delta_q: i8,
    pub(crate) u_ac_delta_q: i8,
    pub(crate) v_dc_delta_q: i8,
    pub(crate) v_ac_delta_q: i8,
    pub(crate) min_base_qindex: u8,
    pub(crate) max_base_qindex: u8,
    /// The `qmatrix_flags` bitfield union.
    pub(crate) qmatrix_flags: u16,
    pub(crate) reserved16bits1: u16,
    /// The `mode_control_flags` bitfield union.
    pub(crate) mode_control_flags: u32,
    pub(crate) segments: SegParamAv1,
    pub(crate) tile_cols: u8,
    pub(crate) tile_rows: u8,
    pub(crate) reserved16bits2: u16,
    pub(crate) width_in_sbs_minus_1: [u16; 63],
    pub(crate) height_in_sbs_minus_1: [u16; 63],
    pub(crate) context_update_tile_id: u16,
    pub(crate) cdef_damping_minus_3: u8,
    pub(crate) cdef_bits: u8,
    pub(crate) cdef_y_strengths: [u8; 8],
    pub(crate) cdef_uv_strengths: [u8; 8],
    /// The `loop_restoration_flags` bitfield union.
    pub(crate) loop_restoration_flags: u16,
    pub(crate) wm: [WarpedMotionAv1; 7],
    /// Where the driver rewrites what its rate controller chooses in the packed frame
    /// header: bit offsets into the frame header OBU, and the OBU size field's byte
    /// offset into the packed headers.
    pub(crate) bit_offset_qindex: u32,
    pub(crate) bit_offset_segmentation: u32,
    pub(crate) bit_offset_loopfilter_params: u32,
    pub(crate) bit_offset_cdef_params: u32,
    pub(crate) size_in_bits_cdef_params: u32,
    pub(crate) byte_offset_frame_hdr_obu_size: u32,
    pub(crate) size_in_bits_frame_hdr_obu: u32,
    /// The `tile_group_obu_hdr_info` bitfield union.
    pub(crate) tile_group_obu_hdr_info: u8,
    pub(crate) number_skip_frames: u8,
    pub(crate) reserved16bits3: u16,
    pub(crate) skip_frames_reduced_size: i32,
    pub(crate) va_reserved: [u32; 16],
}

impl PicParamAv1 {
    /// Every field zero and every reference slot empty.
    pub(crate) const EMPTY: Self = Self {
        frame_width_minus_1: 0,
        frame_height_minus_1: 0,
        reconstructed_frame: INVALID_ID,
        coded_buf: INVALID_ID,
        reference_frames: [INVALID_ID; 8],
        ref_frame_idx: [0; 7],
        hierarchical_level_plus1: 0,
        primary_ref_frame: 0,
        order_hint: 0,
        refresh_frame_flags: 0,
        reserved8bits1: 0,
        ref_frame_ctrl_l0: 0,
        ref_frame_ctrl_l1: 0,
        picture_flags: 0,
        seg_id_block_size: 0,
        num_tile_groups_minus1: 0,
        temporal_id: 0,
        filter_level: [0; 2],
        filter_level_u: 0,
        filter_level_v: 0,
        loop_filter_flags: 0,
        superres_scale_denominator: 0,
        interpolation_filter: 0,
        ref_deltas: [0; 8],
        mode_deltas: [0; 2],
        base_qindex: 0,
        y_dc_delta_q: 0,
        u_dc_delta_q: 0,
        u_ac_delta_q: 0,
        v_dc_delta_q: 0,
        v_ac_delta_q: 0,
        min_base_qindex: 0,
        max_base_qindex: 0,
        qmatrix_flags: 0,
        reserved16bits1: 0,
        mode_control_flags: 0,
        segments: SegParamAv1 {
            seg_flags: 0,
            segment_number: 0,
            feature_data: [[0; 8]; 8],
            feature_mask: [0; 8],
            va_reserved: [0; 4],
        },
        tile_cols: 0,
        tile_rows: 0,
        reserved16bits2: 0,
        width_in_sbs_minus_1: [0; 63],
        height_in_sbs_minus_1: [0; 63],
        context_update_tile_id: 0,
        cdef_damping_minus_3: 0,
        cdef_bits: 0,
        cdef_y_strengths: [0; 8],
        cdef_uv_strengths: [0; 8],
        loop_restoration_flags: 0,
        wm: [WarpedMotionAv1 {
            wmtype: 0,
            wmmat: [0; 8],
            invalid: 0,
            va_reserved: [0; 4],
        }; 7],
        bit_offset_qindex: 0,
        bit_offset_segmentation: 0,
        bit_offset_loopfilter_params: 0,
        bit_offset_cdef_params: 0,
        size_in_bits_cdef_params: 0,
        byte_offset_frame_hdr_obu_size: 0,
        size_in_bits_frame_hdr_obu: 0,
        tile_group_obu_hdr_info: 0,
        number_skip_frames: 0,
        reserved16bits3: 0,
        skip_frames_reduced_size: 0,
        va_reserved: [0; 16],
    };
}

/// `VAEncTileGroupBufferAV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TileGroupAv1 {
    pub(crate) tg_start: u8,
    pub(crate) tg_end: u8,
    pub(crate) va_reserved: [u32; 4],
}

/// `VAEncPackedHeaderParameterBuffer`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PackedHeaderParam {
    pub(crate) kind: u32,
    pub(crate) bit_length: u32,
    pub(crate) has_emulation_bytes: u8,
    pub(crate) va_reserved: [u32; 4],
}

/// `VACodedBufferSegment`, one link of the list `vaMapBuffer` returns for a coded
/// buffer.
#[repr(C)]
#[derive(Debug)]
pub(crate) struct CodedBufferSegment {
    pub(crate) size: u32,
    pub(crate) bit_offset: u32,
    pub(crate) status: u32,
    pub(crate) reserved: u32,
    pub(crate) buf: *mut core::ffi::c_void,
    pub(crate) next: *mut core::ffi::c_void,
    pub(crate) va_reserved: [u32; 4],
}

/// The value union of `VAGenericValue`: `int32_t i`, `float f`, `void *p` or a
/// function pointer. Only `i` and `p` are written here.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) union GenericValueUnion {
    pub(crate) i: i32,
    pub(crate) p: *mut core::ffi::c_void,
}

/// `VAGenericValue`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct GenericValue {
    pub(crate) kind: i32,
    pub(crate) value: GenericValueUnion,
}

/// `VASurfaceAttrib`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct SurfaceAttrib {
    pub(crate) kind: i32,
    pub(crate) flags: u32,
    pub(crate) value: GenericValue,
}

impl SurfaceAttrib {
    /// A settable integer attribute.
    pub(crate) fn integer(kind: i32, i: i32) -> Self {
        Self {
            kind,
            flags: surface_attrib::SETTABLE,
            value: GenericValue {
                kind: generic::INTEGER,
                value: GenericValueUnion { i },
            },
        }
    }

    /// A settable pointer attribute.
    pub(crate) fn pointer(kind: i32, p: *mut core::ffi::c_void) -> Self {
        Self {
            kind,
            flags: surface_attrib::SETTABLE,
            value: GenericValue {
                kind: generic::POINTER,
                value: GenericValueUnion { p },
            },
        }
    }
}

/// One DRM object of a [`DrmPrimeSurfaceDescriptor`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DrmObject {
    pub(crate) fd: i32,
    pub(crate) size: u32,
    pub(crate) drm_format_modifier: u64,
}

/// One layer of a [`DrmPrimeSurfaceDescriptor`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DrmLayer {
    pub(crate) drm_format: u32,
    pub(crate) num_planes: u32,
    pub(crate) object_index: [u32; 4],
    pub(crate) offset: [u32; 4],
    pub(crate) pitch: [u32; 4],
}

/// `VADRMPRIMESurfaceDescriptor`: a surface made of DMA-BUFs, to import with
/// `vaCreateSurfaces`. libva doesn't close the descriptors.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DrmPrimeSurfaceDescriptor {
    pub(crate) fourcc: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) num_objects: u32,
    pub(crate) objects: [DrmObject; 4],
    pub(crate) num_layers: u32,
    pub(crate) layers: [DrmLayer; 4],
}

/// An NV12 surface held in one DMA-BUF: its Y and CbCr planes at their offsets and
/// pitches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Nv12Object {
    pub(crate) fd: i32,
    pub(crate) size: u32,
    pub(crate) modifier: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) offsets: [u32; 2],
    pub(crate) pitches: [u32; 2],
}

impl DrmPrimeSurfaceDescriptor {
    /// `object` as libva imports it: one object, and one NV12 layer of two planes in
    /// it (the composed form, which `vaExportSurfaceHandle` also gives).
    pub(crate) fn nv12(object: Nv12Object) -> Self {
        let mut desc = Self {
            fourcc: FOURCC_NV12,
            width: object.width,
            height: object.height,
            num_objects: 1,
            num_layers: 1,
            ..Self::default()
        };
        desc.objects[0] = DrmObject {
            fd: object.fd,
            size: object.size,
            drm_format_modifier: object.modifier,
        };
        desc.layers[0] = DrmLayer {
            drm_format: DRM_FORMAT_NV12,
            num_planes: 2,
            object_index: [0; 4],
            offset: [object.offsets[0], object.offsets[1], 0, 0],
            pitch: [object.pitches[0], object.pitches[1], 0, 0],
        };
        desc
    }
}

/// `VAImageFormat`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ImageFormat {
    pub(crate) fourcc: u32,
    pub(crate) byte_order: u32,
    pub(crate) bits_per_pixel: u32,
    pub(crate) depth: u32,
    pub(crate) red_mask: u32,
    pub(crate) green_mask: u32,
    pub(crate) blue_mask: u32,
    pub(crate) alpha_mask: u32,
    pub(crate) va_reserved: [u32; 4],
}

impl ImageFormat {
    /// NV12, 12 bits per pixel.
    pub(crate) const NV12: Self = Self {
        fourcc: FOURCC_NV12,
        byte_order: LSB_FIRST,
        bits_per_pixel: 12,
        depth: 0,
        red_mask: 0,
        green_mask: 0,
        blue_mask: 0,
        alpha_mask: 0,
        va_reserved: [0; 4],
    };
}

/// `VAImage`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Image {
    pub(crate) image_id: Id,
    pub(crate) format: ImageFormat,
    pub(crate) buf: Id,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) data_size: u32,
    pub(crate) num_planes: u32,
    pub(crate) pitches: [u32; 3],
    pub(crate) offsets: [u32; 3],
    pub(crate) num_palette_entries: i32,
    pub(crate) entry_bytes: i32,
    pub(crate) component_order: [i8; 4],
    pub(crate) va_reserved: [u32; 4],
}

impl Image {
    /// An image `vaDeriveImage` or `vaCreateImage` fills in.
    pub(crate) const EMPTY: Self = Self {
        image_id: INVALID_ID,
        format: ImageFormat::NV12,
        buf: INVALID_ID,
        width: 0,
        height: 0,
        data_size: 0,
        num_planes: 0,
        pitches: [0; 3],
        offsets: [0; 3],
        num_palette_entries: 0,
        entry_bytes: 0,
        component_order: [0; 4],
        va_reserved: [0; 4],
    };
}

// The layout libva 2.20.0's headers give on x86_64 (see the module docs).
const _: () = {
    use core::mem::{align_of, offset_of, size_of};

    assert!(size_of::<ConfigAttrib>() == 8);
    assert!(size_of::<SeqParamAv1>() == 88);
    assert!(offset_of!(SeqParamAv1, intra_period) == 4);
    assert!(offset_of!(SeqParamAv1, seq_fields) == 16);
    assert!(offset_of!(SeqParamAv1, order_hint_bits_minus_1) == 20);
    assert!(offset_of!(SeqParamAv1, va_reserved) == 24);
    assert!(size_of::<SegParamAv1>() == 156);
    assert!(offset_of!(SegParamAv1, feature_mask) == 130);
    assert!(offset_of!(SegParamAv1, va_reserved) == 140);
    assert!(size_of::<WarpedMotionAv1>() == 56);
    assert!(offset_of!(WarpedMotionAv1, invalid) == 36);
    assert!(size_of::<PicParamAv1>() == 1032);
    assert!(offset_of!(PicParamAv1, reference_frames) == 12);
    assert!(offset_of!(PicParamAv1, ref_frame_idx) == 44);
    assert!(offset_of!(PicParamAv1, refresh_frame_flags) == 54);
    assert!(offset_of!(PicParamAv1, ref_frame_ctrl_l0) == 56);
    assert!(offset_of!(PicParamAv1, picture_flags) == 64);
    assert!(offset_of!(PicParamAv1, filter_level) == 71);
    assert!(offset_of!(PicParamAv1, ref_deltas) == 78);
    assert!(offset_of!(PicParamAv1, base_qindex) == 88);
    assert!(offset_of!(PicParamAv1, min_base_qindex) == 94);
    assert!(offset_of!(PicParamAv1, qmatrix_flags) == 96);
    assert!(offset_of!(PicParamAv1, mode_control_flags) == 100);
    assert!(offset_of!(PicParamAv1, segments) == 104);
    assert!(offset_of!(PicParamAv1, tile_cols) == 260);
    assert!(offset_of!(PicParamAv1, width_in_sbs_minus_1) == 264);
    assert!(offset_of!(PicParamAv1, height_in_sbs_minus_1) == 390);
    assert!(offset_of!(PicParamAv1, context_update_tile_id) == 516);
    assert!(offset_of!(PicParamAv1, cdef_y_strengths) == 520);
    assert!(offset_of!(PicParamAv1, loop_restoration_flags) == 536);
    assert!(offset_of!(PicParamAv1, wm) == 540);
    assert!(offset_of!(PicParamAv1, bit_offset_qindex) == 932);
    assert!(offset_of!(PicParamAv1, byte_offset_frame_hdr_obu_size) == 952);
    assert!(offset_of!(PicParamAv1, size_in_bits_frame_hdr_obu) == 956);
    assert!(offset_of!(PicParamAv1, tile_group_obu_hdr_info) == 960);
    assert!(offset_of!(PicParamAv1, skip_frames_reduced_size) == 964);
    assert!(offset_of!(PicParamAv1, va_reserved) == 968);
    assert!(size_of::<TileGroupAv1>() == 20);
    assert!(size_of::<PictureH264>() == 36);
    assert!(size_of::<DecPicParamH264>() == 672);
    assert!(offset_of!(DecPicParamH264, reference_frames) == 36);
    assert!(offset_of!(DecPicParamH264, picture_width_in_mbs_minus1) == 612);
    assert!(offset_of!(DecPicParamH264, num_ref_frames) == 618);
    assert!(offset_of!(DecPicParamH264, seq_fields) == 620);
    assert!(offset_of!(DecPicParamH264, num_slice_groups_minus1) == 624);
    assert!(offset_of!(DecPicParamH264, slice_group_change_rate_minus1) == 626);
    assert!(offset_of!(DecPicParamH264, pic_init_qp_minus26) == 628);
    assert!(offset_of!(DecPicParamH264, pic_fields) == 632);
    assert!(offset_of!(DecPicParamH264, frame_num) == 636);
    assert!(offset_of!(DecPicParamH264, va_reserved) == 640);
    assert!(size_of::<DecPicParamVp8>() == 112);
    assert!(offset_of!(DecPicParamVp8, pic_fields) == 24);
    assert!(offset_of!(DecPicParamVp8, mb_segment_tree_probs) == 28);
    assert!(offset_of!(DecPicParamVp8, loop_filter_level) == 31);
    assert!(offset_of!(DecPicParamVp8, loop_filter_deltas_ref_frame) == 35);
    assert!(offset_of!(DecPicParamVp8, prob_skip_false) == 43);
    assert!(offset_of!(DecPicParamVp8, y_mode_probs) == 47);
    assert!(offset_of!(DecPicParamVp8, mv_probs) == 54);
    assert!(offset_of!(DecPicParamVp8, bool_coder_ctx) == 92);
    assert!(offset_of!(DecPicParamVp8, va_reserved) == 96);
    assert!(size_of::<DecSliceParamVp8>() == 72);
    assert!(offset_of!(DecSliceParamVp8, macroblock_offset) == 12);
    assert!(offset_of!(DecSliceParamVp8, num_of_partitions) == 16);
    assert!(offset_of!(DecSliceParamVp8, partition_size) == 20);
    assert!(offset_of!(DecSliceParamVp8, va_reserved) == 56);
    assert!(size_of::<ProbabilityDataVp8>() == 1072);
    assert!(offset_of!(ProbabilityDataVp8, va_reserved) == 1056);
    assert!(size_of::<IqMatrixVp8>() == 64);
    assert!(offset_of!(IqMatrixVp8, va_reserved) == 48);
    assert!(size_of::<SegmentationAv1>() == 156);
    assert!(offset_of!(SegmentationAv1, feature_data) == 4);
    assert!(offset_of!(SegmentationAv1, feature_mask) == 132);
    assert!(offset_of!(SegmentationAv1, va_reserved) == 140);
    assert!(size_of::<FilmGrainAv1>() == 176);
    assert!(offset_of!(FilmGrainAv1, grain_seed) == 4);
    assert!(offset_of!(FilmGrainAv1, point_y_scaling) == 21);
    assert!(offset_of!(FilmGrainAv1, num_cr_points) == 56);
    assert!(offset_of!(FilmGrainAv1, ar_coeffs_y) == 77);
    assert!(offset_of!(FilmGrainAv1, ar_coeffs_cr) == 126);
    assert!(offset_of!(FilmGrainAv1, cb_mult) == 151);
    assert!(offset_of!(FilmGrainAv1, cb_offset) == 154);
    assert!(offset_of!(FilmGrainAv1, cr_offset) == 158);
    assert!(offset_of!(FilmGrainAv1, va_reserved) == 160);
    assert!(size_of::<WarpedMotionAv1>() == 56);
    assert!(offset_of!(WarpedMotionAv1, wmmat) == 4);
    assert!(offset_of!(WarpedMotionAv1, invalid) == 36);
    assert!(offset_of!(WarpedMotionAv1, va_reserved) == 40);
    // `anchor_frames_list` is a pointer: these hold on 64-bit ABIs.
    #[cfg(target_pointer_width = "64")]
    {
        assert!(size_of::<DecPicParamAv1>() == 1160);
        assert!(offset_of!(DecPicParamAv1, seq_info_fields) == 4);
        assert!(offset_of!(DecPicParamAv1, current_display_picture) == 12);
        assert!(offset_of!(DecPicParamAv1, anchor_frames_num) == 16);
        assert!(offset_of!(DecPicParamAv1, anchor_frames_list) == 24);
        assert!(offset_of!(DecPicParamAv1, frame_width_minus1) == 32);
        assert!(offset_of!(DecPicParamAv1, output_frame_width_in_tiles_minus_1) == 36);
        assert!(offset_of!(DecPicParamAv1, ref_frame_map) == 40);
        assert!(offset_of!(DecPicParamAv1, ref_frame_idx) == 72);
        assert!(offset_of!(DecPicParamAv1, primary_ref_frame) == 79);
        assert!(offset_of!(DecPicParamAv1, order_hint) == 80);
        assert!(offset_of!(DecPicParamAv1, seg_info) == 84);
        assert!(offset_of!(DecPicParamAv1, film_grain_info) == 240);
        assert!(offset_of!(DecPicParamAv1, tile_cols) == 416);
        assert!(offset_of!(DecPicParamAv1, width_in_sbs_minus_1) == 418);
        assert!(offset_of!(DecPicParamAv1, height_in_sbs_minus_1) == 544);
        assert!(offset_of!(DecPicParamAv1, tile_count_minus_1) == 670);
        assert!(offset_of!(DecPicParamAv1, context_update_tile_id) == 672);
        assert!(offset_of!(DecPicParamAv1, pic_info_fields) == 676);
        assert!(offset_of!(DecPicParamAv1, superres_scale_denominator) == 680);
        assert!(offset_of!(DecPicParamAv1, filter_level) == 682);
        assert!(offset_of!(DecPicParamAv1, loop_filter_info_fields) == 686);
        assert!(offset_of!(DecPicParamAv1, ref_deltas) == 687);
        assert!(offset_of!(DecPicParamAv1, mode_deltas) == 695);
        assert!(offset_of!(DecPicParamAv1, base_qindex) == 697);
        assert!(offset_of!(DecPicParamAv1, y_dc_delta_q) == 698);
        assert!(offset_of!(DecPicParamAv1, qmatrix_fields) == 704);
        assert!(offset_of!(DecPicParamAv1, mode_control_fields) == 708);
        assert!(offset_of!(DecPicParamAv1, cdef_damping_minus_3) == 712);
        assert!(offset_of!(DecPicParamAv1, cdef_y_strengths) == 714);
        assert!(offset_of!(DecPicParamAv1, cdef_uv_strengths) == 722);
        assert!(offset_of!(DecPicParamAv1, loop_restoration_fields) == 730);
        assert!(offset_of!(DecPicParamAv1, wm) == 732);
        assert!(offset_of!(DecPicParamAv1, va_reserved) == 1124);
    }
    assert!(size_of::<DecSliceParamAv1>() == 40);
    assert!(offset_of!(DecSliceParamAv1, tile_row) == 12);
    assert!(offset_of!(DecSliceParamAv1, tile_column) == 14);
    assert!(offset_of!(DecSliceParamAv1, tg_start) == 16);
    assert!(offset_of!(DecSliceParamAv1, tg_end) == 18);
    assert!(offset_of!(DecSliceParamAv1, anchor_frame_idx) == 20);
    assert!(offset_of!(DecSliceParamAv1, tile_idx_in_tile_list) == 22);
    assert!(offset_of!(DecSliceParamAv1, va_reserved) == 24);
    assert!(size_of::<DecPicParamVp9>() == 92);
    assert!(offset_of!(DecPicParamVp9, reference_frames) == 4);
    assert!(offset_of!(DecPicParamVp9, pic_fields) == 36);
    assert!(offset_of!(DecPicParamVp9, filter_level) == 40);
    assert!(offset_of!(DecPicParamVp9, frame_header_length_in_bytes) == 44);
    assert!(offset_of!(DecPicParamVp9, first_partition_size) == 46);
    assert!(offset_of!(DecPicParamVp9, mb_segment_tree_probs) == 48);
    assert!(offset_of!(DecPicParamVp9, segment_pred_probs) == 55);
    assert!(offset_of!(DecPicParamVp9, profile) == 58);
    assert!(offset_of!(DecPicParamVp9, bit_depth) == 59);
    assert!(offset_of!(DecPicParamVp9, va_reserved) == 60);
    assert!(size_of::<SegmentParamVp9>() == 36);
    assert!(offset_of!(SegmentParamVp9, filter_level) == 2);
    assert!(offset_of!(SegmentParamVp9, luma_ac_quant_scale) == 10);
    assert!(offset_of!(SegmentParamVp9, va_reserved) == 20);
    assert!(size_of::<DecSliceParamVp9>() == 316);
    assert!(offset_of!(DecSliceParamVp9, seg_param) == 12);
    assert!(offset_of!(DecSliceParamVp9, va_reserved) == 300);
    assert!(size_of::<IqMatrixH264>() == 240);
    assert!(offset_of!(IqMatrixH264, scaling_list_8x8) == 96);
    assert!(size_of::<DecSliceParamH264>() == 3128);
    assert!(offset_of!(DecSliceParamH264, slice_data_bit_offset) == 12);
    assert!(offset_of!(DecSliceParamH264, slice_type) == 16);
    assert!(offset_of!(DecSliceParamH264, slice_beta_offset_div2) == 24);
    assert!(offset_of!(DecSliceParamH264, ref_pic_list0) == 28);
    assert!(offset_of!(DecSliceParamH264, ref_pic_list1) == 1180);
    assert!(offset_of!(DecSliceParamH264, luma_log2_weight_denom) == 2332);
    assert!(offset_of!(DecSliceParamH264, luma_weight_l0) == 2336);
    assert!(offset_of!(DecSliceParamH264, luma_offset_l0) == 2400);
    assert!(offset_of!(DecSliceParamH264, chroma_weight_l0_flag) == 2464);
    assert!(offset_of!(DecSliceParamH264, chroma_weight_l0) == 2466);
    assert!(offset_of!(DecSliceParamH264, chroma_offset_l0) == 2594);
    assert!(offset_of!(DecSliceParamH264, luma_weight_l1_flag) == 2722);
    assert!(offset_of!(DecSliceParamH264, luma_weight_l1) == 2724);
    assert!(offset_of!(DecSliceParamH264, chroma_weight_l1) == 2854);
    assert!(offset_of!(DecSliceParamH264, chroma_offset_l1) == 2982);
    assert!(offset_of!(DecSliceParamH264, va_reserved) == 3112);
    assert!(size_of::<SeqParamH264>() == 1132);
    assert!(offset_of!(SeqParamH264, intra_period) == 4);
    assert!(offset_of!(SeqParamH264, seq_fields) == 28);
    assert!(offset_of!(SeqParamH264, offset_for_non_ref_pic) == 36);
    assert!(offset_of!(SeqParamH264, offset_for_ref_frame) == 44);
    assert!(offset_of!(SeqParamH264, frame_cropping_flag) == 1068);
    assert!(offset_of!(SeqParamH264, frame_crop_left_offset) == 1072);
    assert!(offset_of!(SeqParamH264, vui_parameters_present_flag) == 1088);
    assert!(offset_of!(SeqParamH264, vui_fields) == 1092);
    assert!(offset_of!(SeqParamH264, aspect_ratio_idc) == 1096);
    assert!(offset_of!(SeqParamH264, sar_width) == 1100);
    assert!(offset_of!(SeqParamH264, time_scale) == 1112);
    assert!(size_of::<PicParamH264>() == 648);
    assert!(offset_of!(PicParamH264, reference_frames) == 36);
    assert!(offset_of!(PicParamH264, coded_buf) == 612);
    assert!(offset_of!(PicParamH264, pic_parameter_set_id) == 616);
    assert!(offset_of!(PicParamH264, frame_num) == 620);
    assert!(offset_of!(PicParamH264, pic_init_qp) == 622);
    assert!(offset_of!(PicParamH264, second_chroma_qp_index_offset) == 626);
    assert!(offset_of!(PicParamH264, pic_fields) == 628);
    assert!(size_of::<SliceParamH264>() == 3140);
    assert!(offset_of!(SliceParamH264, macroblock_info) == 8);
    assert!(offset_of!(SliceParamH264, idr_pic_id) == 14);
    assert!(offset_of!(SliceParamH264, delta_pic_order_cnt_bottom) == 20);
    assert!(offset_of!(SliceParamH264, direct_spatial_mv_pred_flag) == 32);
    assert!(offset_of!(SliceParamH264, ref_pic_list0) == 36);
    assert!(offset_of!(SliceParamH264, ref_pic_list1) == 1188);
    assert!(offset_of!(SliceParamH264, luma_log2_weight_denom) == 2340);
    assert!(offset_of!(SliceParamH264, luma_weight_l0) == 2344);
    assert!(offset_of!(SliceParamH264, chroma_weight_l0_flag) == 2472);
    assert!(offset_of!(SliceParamH264, chroma_weight_l0) == 2474);
    assert!(offset_of!(SliceParamH264, luma_weight_l1_flag) == 2730);
    assert!(offset_of!(SliceParamH264, luma_weight_l1) == 2732);
    assert!(offset_of!(SliceParamH264, chroma_weight_l1_flag) == 2860);
    assert!(offset_of!(SliceParamH264, chroma_weight_l1) == 2862);
    assert!(offset_of!(SliceParamH264, cabac_init_idc) == 3118);
    assert!(offset_of!(SliceParamH264, slice_beta_offset_div2) == 3122);
    assert!(offset_of!(SliceParamH264, va_reserved) == 3124);
    assert!(size_of::<PictureHevc>() == 28);
    assert!(offset_of!(PictureHevc, flags) == 8);
    assert!(size_of::<SeqParamHevc>() == 116);
    assert!(offset_of!(SeqParamHevc, intra_period) == 4);
    assert!(offset_of!(SeqParamHevc, bits_per_second) == 16);
    assert!(offset_of!(SeqParamHevc, pic_width_in_luma_samples) == 20);
    assert!(offset_of!(SeqParamHevc, seq_fields) == 24);
    assert!(offset_of!(SeqParamHevc, log2_min_luma_coding_block_size_minus3) == 28);
    assert!(offset_of!(SeqParamHevc, max_transform_hierarchy_depth_intra) == 33);
    assert!(offset_of!(SeqParamHevc, pcm_sample_bit_depth_luma_minus1) == 36);
    assert!(offset_of!(SeqParamHevc, log2_max_pcm_luma_coding_block_size_minus3) == 48);
    assert!(offset_of!(SeqParamHevc, vui_parameters_present_flag) == 52);
    assert!(offset_of!(SeqParamHevc, vui_fields) == 56);
    assert!(offset_of!(SeqParamHevc, aspect_ratio_idc) == 60);
    assert!(offset_of!(SeqParamHevc, sar_width) == 64);
    assert!(offset_of!(SeqParamHevc, vui_time_scale) == 76);
    assert!(offset_of!(SeqParamHevc, min_spatial_segmentation_idc) == 80);
    assert!(offset_of!(SeqParamHevc, max_bits_per_min_cu_denom) == 83);
    assert!(offset_of!(SeqParamHevc, scc_fields) == 84);
    assert!(offset_of!(SeqParamHevc, va_reserved) == 88);
    assert!(size_of::<PicParamHevc>() == 576);
    assert!(offset_of!(PicParamHevc, reference_frames) == 28);
    assert!(offset_of!(PicParamHevc, coded_buf) == 448);
    assert!(offset_of!(PicParamHevc, collocated_ref_pic_index) == 452);
    assert!(offset_of!(PicParamHevc, column_width_minus1) == 460);
    assert!(offset_of!(PicParamHevc, row_height_minus1) == 479);
    assert!(offset_of!(PicParamHevc, log2_parallel_merge_level_minus2) == 500);
    assert!(offset_of!(PicParamHevc, nal_unit_type) == 505);
    assert!(offset_of!(PicParamHevc, pic_fields) == 508);
    assert!(offset_of!(PicParamHevc, hierarchical_level_plus1) == 512);
    assert!(offset_of!(PicParamHevc, scc_fields) == 514);
    assert!(offset_of!(PicParamHevc, va_reserved) == 516);
    assert!(size_of::<SliceParamHevc>() == 1076);
    assert!(offset_of!(SliceParamHevc, slice_type) == 8);
    assert!(offset_of!(SliceParamHevc, ref_pic_list0) == 12);
    assert!(offset_of!(SliceParamHevc, ref_pic_list1) == 432);
    assert!(offset_of!(SliceParamHevc, luma_log2_weight_denom) == 852);
    assert!(offset_of!(SliceParamHevc, delta_luma_weight_l0) == 854);
    assert!(offset_of!(SliceParamHevc, delta_chroma_weight_l0) == 884);
    assert!(offset_of!(SliceParamHevc, delta_luma_weight_l1) == 944);
    assert!(offset_of!(SliceParamHevc, chroma_offset_l1) == 1004);
    assert!(offset_of!(SliceParamHevc, max_num_merge_cand) == 1034);
    assert!(offset_of!(SliceParamHevc, slice_tc_offset_div2) == 1039);
    assert!(offset_of!(SliceParamHevc, slice_fields) == 1040);
    assert!(offset_of!(SliceParamHevc, pred_weight_table_bit_length) == 1048);
    assert!(offset_of!(SliceParamHevc, va_reserved) == 1052);
    assert!(size_of::<RateControlParam>() == 60);
    assert!(offset_of!(RateControlParam, rc_flags) == 24);
    assert!(size_of::<FrameRateParam>() == 24);
    assert!(size_of::<HrdParam>() == 24);
    // `sizeof(VAEncMiscParameterBuffer)` is 4: the payload follows `type` directly.
    assert!(offset_of!(MiscParam<RateControlParam>, data) == 4);
    assert!(size_of::<MiscParam<RateControlParam>>() == 4 + 60);
    assert!(size_of::<PackedHeaderParam>() == 28);
    assert!(offset_of!(PackedHeaderParam, va_reserved) == 12);
    assert!(size_of::<ImageFormat>() == 48);
    assert!(size_of::<Image>() == 120);
    assert!(offset_of!(Image, buf) == 52);
    assert!(offset_of!(Image, pitches) == 68);
    assert!(offset_of!(Image, offsets) == 80);
    assert!(offset_of!(Image, component_order) == 100);
    assert!(align_of::<Image>() == 4);
    assert!(size_of::<DrmObject>() == 16);
    assert!(size_of::<DrmLayer>() == 56);
    assert!(size_of::<DrmPrimeSurfaceDescriptor>() == 312);
    assert!(offset_of!(DrmPrimeSurfaceDescriptor, objects) == 16);
    assert!(offset_of!(DrmPrimeSurfaceDescriptor, num_layers) == 80);
    assert!(offset_of!(DrmPrimeSurfaceDescriptor, layers) == 84);
    #[cfg(target_pointer_width = "64")]
    {
        assert!(size_of::<GenericValue>() == 16);
        assert!(offset_of!(GenericValue, value) == 8);
        assert!(size_of::<SurfaceAttrib>() == 24);
        assert!(offset_of!(SurfaceAttrib, value) == 8);
        assert!(size_of::<CodedBufferSegment>() == 48);
        assert!(offset_of!(CodedBufferSegment, buf) == 16);
        assert!(offset_of!(CodedBufferSegment, next) == 24);
    }
};

#[cfg(test)]
mod tests {
    use super::{
        DRM_FORMAT_NV12, DrmPrimeSurfaceDescriptor, FOURCC_NV12, Nv12Object, SurfaceAttrib,
        generic, surface_attrib,
    };

    #[test]
    fn an_nv12_target_imports_as_one_object_and_one_layer() {
        let desc = DrmPrimeSurfaceDescriptor::nv12(Nv12Object {
            fd: 7,
            size: 1920 * 1088 * 3 / 2,
            modifier: 0,
            width: 1920,
            height: 1080,
            offsets: [0, 1920 * 1088],
            pitches: [1920, 1920],
        });
        assert_eq!(
            (desc.fourcc, desc.width, desc.height),
            (FOURCC_NV12, 1920, 1080)
        );
        assert_eq!((desc.num_objects, desc.num_layers), (1, 1));
        assert_eq!(
            (desc.objects[0].fd, desc.objects[0].drm_format_modifier),
            (7, 0)
        );
        assert_eq!(desc.objects[0].size, 1920 * 1088 * 3 / 2);
        let layer = desc.layers[0];
        assert_eq!((layer.drm_format, layer.num_planes), (DRM_FORMAT_NV12, 2));
        assert_eq!(layer.object_index, [0; 4]);
        assert_eq!(layer.offset, [0, 1920 * 1088, 0, 0]);
        assert_eq!(layer.pitch, [1920, 1920, 0, 0]);
        // The unused entries stay zero, as libva expects.
        assert_eq!(desc.objects[1..], [Default::default(); 3]);
        assert_eq!(desc.layers[1..], [Default::default(); 3]);
    }

    #[test]
    fn surface_attributes_are_settable_and_typed() {
        let memory = SurfaceAttrib::integer(
            surface_attrib::MEMORY_TYPE,
            surface_attrib::MEM_TYPE_DRM_PRIME_2,
        );
        assert_eq!(memory.flags, surface_attrib::SETTABLE);
        assert_eq!(memory.value.kind, generic::INTEGER);
        // SAFETY: `integer` wrote `i`.
        assert_eq!(unsafe { memory.value.value.i }, 0x4000_0000);
        let mut desc = DrmPrimeSurfaceDescriptor::default();
        let pointer = SurfaceAttrib::pointer(
            surface_attrib::EXTERNAL_BUFFER_DESCRIPTOR,
            (&raw mut desc).cast(),
        );
        assert_eq!(pointer.value.kind, generic::POINTER);
        // SAFETY: `pointer` wrote `p`.
        assert_eq!(unsafe { pointer.value.value.p }, (&raw mut desc).cast());
    }
}
