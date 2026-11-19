//! The VA-API parameters of one VP8 frame to decode (ADR-0072 (b)), from its header
//! ([`Header`]): the picture's parameters, the coefficient probabilities, each
//! segment's quantizer indices, and one slice that is the frame's partitions. The same
//! fields FFmpeg's VA-API hwaccel fills, so every driver reads them as it is used to.

use crate::media::vp8::{Header, KEY_FRAME_UV_MODE_PROBS, KEY_FRAME_Y_MODE_PROBS};

use super::ffi::{
    BoolCoderContextVpx, DecPicParamVp8, DecSliceParamVp8, INVALID_ID, Id, IqMatrixVp8,
    ProbabilityDataVp8,
};

/// The surfaces a frame references.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct References {
    pub(crate) last: Id,
    pub(crate) golden: Id,
    pub(crate) alternate: Id,
}

impl References {
    /// None: a key frame's.
    pub(crate) const NONE: Self = Self {
        last: INVALID_ID,
        golden: INVALID_ID,
        alternate: INVALID_ID,
    };
}

/// The picture parameter buffer.
pub(crate) fn picture_params(h: &Header, refs: References) -> DecPicParamVp8 {
    let seg = &h.segmentation;
    let b = |v: bool| u32::from(v);
    // libva's `key_frame` bit is set for an inter frame, as FFmpeg sets it.
    let pic_fields = b(!h.key_frame)
        | u32::from(h.version & 7) << 1
        | b(seg.enabled) << 4
        | b(seg.update_map) << 5
        | b(seg.update_data) << 6
        | b(h.filter_simple) << 7
        | u32::from(h.sharpness & 7) << 8
        | b(h.lf_adjust) << 11
        | b(h.lf_delta_update) << 12
        | b(h.sign_bias_golden) << 13
        | b(h.sign_bias_alternate) << 14
        | b(h.mb_no_coeff_skip) << 15
        | b(h.filter_level == 0) << 16;
    let (y_mode_probs, uv_mode_probs) = if h.key_frame {
        (KEY_FRAME_Y_MODE_PROBS, KEY_FRAME_UV_MODE_PROBS)
    } else {
        (h.probs.y_mode, h.probs.uv_mode)
    };
    DecPicParamVp8 {
        frame_width: u32::from(h.width),
        frame_height: u32::from(h.height),
        last_ref_frame: refs.last,
        golden_ref_frame: refs.golden,
        alt_ref_frame: refs.alternate,
        out_of_loop_frame: INVALID_ID,
        pic_fields,
        mb_segment_tree_probs: h.probs.segment_tree,
        loop_filter_level: h.filter_levels(),
        loop_filter_deltas_ref_frame: h.lf_deltas.reference,
        loop_filter_deltas_mode: h.lf_deltas.mode,
        prob_skip_false: h.prob_skip_false,
        prob_intra: h.prob_intra,
        prob_last: h.prob_last,
        prob_gf: h.prob_golden,
        y_mode_probs,
        uv_mode_probs,
        mv_probs: h.probs.mv,
        bool_coder_ctx: BoolCoderContextVpx {
            range: h.coder.range,
            value: h.coder.value,
            count: h.coder.count,
        },
        va_reserved: [0; 4],
    }
}

/// The coefficient probabilities this frame decodes with.
pub(crate) fn probabilities(h: &Header) -> ProbabilityDataVp8 {
    ProbabilityDataVp8 {
        dct_coeff_probs: h.probs.coeff,
        va_reserved: [0; 4],
    }
}

/// Each segment's quantizer indices.
pub(crate) fn iq_matrix(h: &Header) -> IqMatrixVp8 {
    IqMatrixVp8 {
        quantization_index: h.quantizer_indices(),
        va_reserved: [0; 4],
    }
}

/// The slice parameter buffer: the partitions, from the first one's start (the slice
/// data is the frame after its first `header_size` bytes), `size` bytes in all.
pub(crate) fn slice_params(h: &Header, size: usize) -> DecSliceParamVp8 {
    let offset = h.coder.macroblock_offset;
    let mut partition_size = [0_u32; 9];
    // The first partition's bytes from the one the macroblocks start in.
    partition_size[0] = h.first_partition_size.saturating_sub(offset.div_ceil(8));
    partition_size[1..].copy_from_slice(&h.partition_sizes);
    DecSliceParamVp8 {
        slice_data_size: u32::try_from(size).unwrap_or(u32::MAX),
        slice_data_offset: 0,
        // VA_SLICE_DATA_FLAG_ALL: the slice is all in this buffer.
        slice_data_flag: 0,
        macroblock_offset: offset,
        num_of_partitions: u8::try_from(h.partitions + 1).unwrap_or(9),
        partition_size,
        va_reserved: [0; 4],
    }
}

#[cfg(test)]
mod tests;
