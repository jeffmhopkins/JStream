//! The VA-API parameters of one VP9 frame to decode (ADR-0072 (b)), from its
//! uncompressed header ([`Header`]): the picture's parameter buffer, and one slice that
//! is the whole frame, with each segment's filter levels and dequantizers. The same
//! fields FFmpeg's VA-API hwaccel fills, so every driver reads them as it is used to.
//! The driver reads the compressed header and the tiles from the frame itself.

use crate::media::vp9::{Header, NUM_REF_FRAMES};

use super::ffi::{DecPicParamVp9, DecSliceParamVp9, Id, SegmentParamVp9};

/// The picture parameter buffer. `slots[i]` is the surface in reference slot `i`
/// (`INVALID_ID` for an empty one).
pub(crate) fn picture_params(h: &Header, slots: &[Id; NUM_REF_FRAMES]) -> DecPicParamVp9 {
    let seg = &h.segmentation;
    let b = |v: bool| u32::from(v);
    let n = u32::from;
    let pic_fields = b(h.subsampling.0)
        | b(h.subsampling.1) << 1
        | b(!h.key_frame) << 2
        | b(h.show_frame) << 3
        | b(h.error_resilient) << 4
        | b(h.intra_only) << 5
        | b(h.allow_high_precision_mv) << 6
        | n(h.interp_filter & 7) << 7
        | b(h.frame_parallel_decoding_mode) << 10
        | n(h.reset_frame_context & 3) << 11
        | b(h.refresh_frame_context) << 13
        | n(h.frame_context_idx & 3) << 14
        | b(seg.enabled) << 16
        | b(seg.temporal_update) << 17
        | b(seg.update_map) << 18
        | n(h.ref_frame_idx[0] & 7) << 19
        | b(h.ref_frame_sign_bias[0]) << 22
        | n(h.ref_frame_idx[1] & 7) << 23
        | b(h.ref_frame_sign_bias[1]) << 26
        | n(h.ref_frame_idx[2] & 7) << 27
        | b(h.ref_frame_sign_bias[2]) << 30
        | b(h.lossless()) << 31;
    DecPicParamVp9 {
        frame_width: h.width,
        frame_height: h.height,
        reference_frames: *slots,
        pic_fields,
        filter_level: h.filter_level,
        sharpness_level: h.sharpness,
        log2_tile_rows: h.tile_rows_log2,
        log2_tile_columns: h.tile_cols_log2,
        frame_header_length_in_bytes: u8::try_from(h.uncompressed_size).unwrap_or(u8::MAX),
        first_partition_size: h.compressed_size,
        mb_segment_tree_probs: seg.tree_probs,
        segment_pred_probs: seg.pred_probs,
        profile: h.profile,
        bit_depth: h.bit_depth,
        va_reserved: [0; 8],
    }
}

/// The slice parameter buffer: the whole frame of `size` bytes.
pub(crate) fn slice_params(h: &Header, size: usize) -> DecSliceParamVp9 {
    let mut seg_param = [SegmentParamVp9::default(); 8];
    for (out, s) in seg_param.iter_mut().zip(h.segments()) {
        *out = SegmentParamVp9 {
            segment_flags: u16::from(s.reference_enabled)
                | u16::from(s.reference & 3) << 1
                | u16::from(s.skip) << 3,
            filter_level: s.filter_level,
            luma_ac_quant_scale: s.luma_ac,
            luma_dc_quant_scale: s.luma_dc,
            chroma_ac_quant_scale: s.chroma_ac,
            chroma_dc_quant_scale: s.chroma_dc,
            va_reserved: [0; 4],
        };
    }
    DecSliceParamVp9 {
        slice_data_size: u32::try_from(size).unwrap_or(u32::MAX),
        slice_data_offset: 0,
        // VA_SLICE_DATA_FLAG_ALL: the slice is all in this buffer.
        slice_data_flag: 0,
        seg_param,
        va_reserved: [0; 4],
    }
}

#[cfg(test)]
mod tests;
