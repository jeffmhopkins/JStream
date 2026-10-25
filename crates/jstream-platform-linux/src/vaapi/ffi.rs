//! libva's H.264 and HEVC encode structs and constants, hand-written (ADR-0060 §8).
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
//! The surface attributes and the DRM PRIME descriptor that import an encode target
//! (`va/va.h`, `va/va_drmcommon.h`) were checked the same way, against the same
//! release.
//!
//! Plain data with no pointers (apart from [`CodedBufferSegment`], which only libva
//! writes, and [`GenericValue`], which carries the descriptor's address), so it builds
//! on every platform and the parameter builders are tested everywhere.

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
/// `VA_ENC_PACKED_HEADER_SLICE`.
pub(crate) const PACKED_HEADER_SLICE: u32 = 0x4;
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
    assert!(size_of::<PictureH264>() == 36);
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
