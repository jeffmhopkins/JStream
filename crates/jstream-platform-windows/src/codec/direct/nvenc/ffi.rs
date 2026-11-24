//! The part of NVIDIA's NVENC API (`nvEncodeAPI.h`, Video Codec SDK 12.0, API version
//! 12.0) that the direct encoder calls, declared by hand (ADR-0215).
//!
//! Transcribed from `nvEncodeAPI.h` as FFmpeg's `nv-codec-headers` ship it at tag
//! `n12.0.16.1`, which NVIDIA publishes under the MIT licence ("Copyright (c) 2010-2022
//! NVIDIA Corporation"; permission is granted to use, copy, modify and distribute it
//! provided the notice is kept). Only the structs, enums and GUIDs the session uses are
//! here. Each struct keeps the header's reserved arrays, so its size is the header's;
//! the spans of a struct the session never touches are folded into `_skip*` arrays of
//! the same bytes.
//!
//! Every size and every offset the session touches was read from clang's MSVC layout
//! of the header (`clang -cc1 -triple x86_64-pc-windows-msvc -fdump-record-layouts`)
//! and is asserted at compile time below, on every platform, so a wrong transcription
//! fails the build. Bit-fields are plain `u32` words with their bit positions as
//! constants (MSVC packs `uint32_t` bit-fields from the least significant bit, which
//! the same dump shows); the struct versions are the header's macros, evaluated by the
//! same compiler.

// The declarations below are transcribed from NVIDIA's nvEncodeAPI.h, whose notice
// follows, kept as its licence asks:
//
// Copyright (c) 2010-2022 NVIDIA Corporation
//
// Permission is hereby granted, free of charge, to any person obtaining a copy of this
// software and associated documentation files (the "Software"), to deal in the Software
// without restriction, including without limitation the rights to use, copy, modify,
// merge, publish, distribute, sublicense, and/or sell copies of the software, and to
// permit persons to whom the software is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice shall be included in all copies
// or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED,
// INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT
// HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF
// CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE
// OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

use core::ffi::{c_char, c_void};

/// `NVENCSTATUS`.
pub(crate) type Status = u32;
/// `NV_ENC_SUCCESS`.
pub(crate) const SUCCESS: Status = 0;
/// `NV_ENC_ERR_NEED_MORE_INPUT`: the frame was taken and its output comes later.
pub(crate) const ERR_NEED_MORE_INPUT: Status = 17;

/// The names of the `NVENCSTATUS` values, indexed by value.
const STATUS_NAMES: [&str; 26] = [
    "NV_ENC_SUCCESS",
    "NV_ENC_ERR_NO_ENCODE_DEVICE",
    "NV_ENC_ERR_UNSUPPORTED_DEVICE",
    "NV_ENC_ERR_INVALID_ENCODERDEVICE",
    "NV_ENC_ERR_INVALID_DEVICE",
    "NV_ENC_ERR_DEVICE_NOT_EXIST",
    "NV_ENC_ERR_INVALID_PTR",
    "NV_ENC_ERR_INVALID_EVENT",
    "NV_ENC_ERR_INVALID_PARAM",
    "NV_ENC_ERR_INVALID_CALL",
    "NV_ENC_ERR_OUT_OF_MEMORY",
    "NV_ENC_ERR_ENCODER_NOT_INITIALIZED",
    "NV_ENC_ERR_UNSUPPORTED_PARAM",
    "NV_ENC_ERR_LOCK_BUSY",
    "NV_ENC_ERR_NOT_ENOUGH_BUFFER",
    "NV_ENC_ERR_INVALID_VERSION",
    "NV_ENC_ERR_MAP_FAILED",
    "NV_ENC_ERR_NEED_MORE_INPUT",
    "NV_ENC_ERR_ENCODER_BUSY",
    "NV_ENC_ERR_EVENT_NOT_REGISTERD",
    "NV_ENC_ERR_GENERIC",
    "NV_ENC_ERR_INCOMPATIBLE_CLIENT_KEY",
    "NV_ENC_ERR_UNIMPLEMENTED",
    "NV_ENC_ERR_RESOURCE_REGISTER_FAILED",
    "NV_ENC_ERR_RESOURCE_NOT_REGISTERED",
    "NV_ENC_ERR_RESOURCE_NOT_MAPPED",
];

/// The name of an `NVENCSTATUS` value.
pub(crate) fn status_name(status: Status) -> &'static str {
    usize::try_from(status)
        .ok()
        .and_then(|i| STATUS_NAMES.get(i))
        .copied()
        .unwrap_or("an unknown NVENCSTATUS")
}

/// `NVENCAPI_VERSION`: major 12, minor 0.
pub(crate) const API_VERSION: u32 = 12;
/// The version `NvEncodeAPIGetMaxSupportedVersion` must report at least, as it encodes
/// it: major in the upper bits, minor in the low four.
pub(crate) const MIN_SUPPORTED_VERSION: u32 = 12 << 4;

/// `NVENCAPI_STRUCT_VERSION(ver)`.
const fn struct_version(ver: u32) -> u32 {
    API_VERSION | (ver << 16) | (0x7 << 28)
}

/// `NV_ENC_CAPS_PARAM_VER`.
pub(crate) const CAPS_PARAM_VER: u32 = struct_version(1);
/// `NV_ENC_CREATE_BITSTREAM_BUFFER_VER`.
pub(crate) const CREATE_BITSTREAM_BUFFER_VER: u32 = struct_version(1);
/// `NV_ENC_CONFIG_VER`.
pub(crate) const CONFIG_VER: u32 = struct_version(8) | (1 << 31);
/// `NV_ENC_INITIALIZE_PARAMS_VER`.
pub(crate) const INITIALIZE_PARAMS_VER: u32 = struct_version(5) | (1 << 31);
/// `NV_ENC_PRESET_CONFIG_VER`.
pub(crate) const PRESET_CONFIG_VER: u32 = struct_version(4) | (1 << 31);
/// `NV_ENC_PIC_PARAMS_VER`.
pub(crate) const PIC_PARAMS_VER: u32 = struct_version(6) | (1 << 31);
/// `NV_ENC_LOCK_BITSTREAM_VER`.
pub(crate) const LOCK_BITSTREAM_VER: u32 = struct_version(2);
/// `NV_ENC_MAP_INPUT_RESOURCE_VER`.
pub(crate) const MAP_INPUT_RESOURCE_VER: u32 = struct_version(4);
/// `NV_ENC_REGISTER_RESOURCE_VER`.
pub(crate) const REGISTER_RESOURCE_VER: u32 = struct_version(4);
/// `NV_ENC_EVENT_PARAMS_VER`.
pub(crate) const EVENT_PARAMS_VER: u32 = struct_version(1);
/// `NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER`.
pub(crate) const OPEN_ENCODE_SESSION_EX_PARAMS_VER: u32 = struct_version(1);
/// `NV_ENCODE_API_FUNCTION_LIST_VER`.
pub(crate) const FUNCTION_LIST_VER: u32 = struct_version(2);

/// `GUID`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Guid {
    pub(crate) data1: u32,
    pub(crate) data2: u16,
    pub(crate) data3: u16,
    pub(crate) data4: [u8; 8],
}

const fn guid(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Guid {
    Guid {
        data1,
        data2,
        data3,
        data4,
    }
}

/// `NV_ENC_CODEC_H264_GUID`.
pub(crate) const CODEC_H264: Guid = guid(
    0x6bc8_2762,
    0x4e63,
    0x4ca4,
    [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf],
);
/// `NV_ENC_CODEC_HEVC_GUID`.
pub(crate) const CODEC_HEVC: Guid = guid(
    0x790c_dc88,
    0x4522,
    0x4d7b,
    [0x94, 0x25, 0xbd, 0xa9, 0x97, 0x5f, 0x76, 0x03],
);
/// `NV_ENC_CODEC_AV1_GUID`.
pub(crate) const CODEC_AV1: Guid = guid(
    0x0a35_2289,
    0x0aa7,
    0x4759,
    [0x86, 0x2d, 0x5d, 0x15, 0xcd, 0x16, 0xd2, 0x54],
);
/// `NV_ENC_H264_PROFILE_HIGH_GUID`.
pub(crate) const H264_PROFILE_HIGH: Guid = guid(
    0xe7cb_c309,
    0x4f7a,
    0x4b89,
    [0xaf, 0x2a, 0xd5, 0x37, 0xc9, 0x2b, 0xe3, 0x10],
);
/// `NV_ENC_HEVC_PROFILE_MAIN_GUID`.
pub(crate) const HEVC_PROFILE_MAIN: Guid = guid(
    0xb514_c39a,
    0xb55b,
    0x40fa,
    [0x87, 0x8f, 0xf1, 0x25, 0x3b, 0x4d, 0xfd, 0xec],
);
/// `NV_ENC_AV1_PROFILE_MAIN_GUID`.
pub(crate) const AV1_PROFILE_MAIN: Guid = guid(
    0x5f2a_39f5,
    0xf14e,
    0x4f95,
    [0x9a, 0x9e, 0xb7, 0x6d, 0x56, 0x8f, 0xcf, 0x97],
);
/// `NV_ENC_PRESET_P5_GUID`.
pub(crate) const PRESET_P5: Guid = guid(
    0x21c6_e6b4,
    0x297a,
    0x4cba,
    [0x99, 0x8f, 0xb6, 0xcb, 0xde, 0x72, 0xad, 0xe3],
);

/// `NV_ENC_TUNING_INFO_HIGH_QUALITY`.
pub(crate) const TUNING_INFO_HIGH_QUALITY: u32 = 1;
/// `NV_ENC_PARAMS_RC_VBR`.
pub(crate) const RC_VBR: u32 = 0x1;
/// `NV_ENC_PARAMS_RC_CBR`.
pub(crate) const RC_CBR: u32 = 0x2;
/// `NV_ENC_PARAMS_FRAME_FIELD_MODE_FRAME`.
pub(crate) const FRAME_FIELD_MODE_FRAME: u32 = 0x1;
/// `NV_ENC_PIC_STRUCT_FRAME`.
pub(crate) const PIC_STRUCT_FRAME: u32 = 0x1;
/// `NV_ENC_PIC_TYPE_I`.
pub(crate) const PIC_TYPE_I: u32 = 0x2;
/// `NV_ENC_PIC_TYPE_IDR`.
pub(crate) const PIC_TYPE_IDR: u32 = 0x3;
/// `NV_ENC_PIC_FLAG_FORCEIDR`.
pub(crate) const PIC_FLAG_FORCEIDR: u32 = 0x2;
/// `NV_ENC_PIC_FLAG_OUTPUT_SPSPPS`.
pub(crate) const PIC_FLAG_OUTPUT_SPSPPS: u32 = 0x4;
/// `NV_ENC_PIC_FLAG_EOS`.
pub(crate) const PIC_FLAG_EOS: u32 = 0x8;
/// `NV_ENC_BUFFER_FORMAT_NV12`.
pub(crate) const BUFFER_FORMAT_NV12: u32 = 0x1;
/// `NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX`.
pub(crate) const INPUT_RESOURCE_TYPE_DIRECTX: u32 = 0x0;
/// `NV_ENC_INPUT_IMAGE`.
pub(crate) const INPUT_IMAGE: u32 = 0x0;
/// `NV_ENC_DEVICE_TYPE_DIRECTX`.
pub(crate) const DEVICE_TYPE_DIRECTX: u32 = 0x0;
/// `NV_ENC_CAPS_WIDTH_MAX`.
pub(crate) const CAPS_WIDTH_MAX: u32 = 16;
/// `NV_ENC_CAPS_HEIGHT_MAX`.
pub(crate) const CAPS_HEIGHT_MAX: u32 = 17;
/// `NV_ENC_CAPS_ASYNC_ENCODE_SUPPORT`.
pub(crate) const CAPS_ASYNC_ENCODE_SUPPORT: u32 = 30;
/// `NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED`.
pub(crate) const VUI_VIDEO_FORMAT_UNSPECIFIED: u32 = 5;
/// `NV_ENC_VUI_COLOR_PRIMARIES_BT709`, `NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709` and
/// `NV_ENC_VUI_MATRIX_COEFFS_BT709`, which share the value.
pub(crate) const VUI_BT709: u32 = 1;

/// Sets `width` bits of `word` from bit `lo` to `value`, as a `uint32_t` bit-field of
/// that position and width is set.
pub(crate) fn set_bits(word: &mut u32, lo: u32, width: u32, value: u32) {
    let mask = ((1u64 << width) - 1) as u32;
    *word = (*word & !(mask << lo)) | ((value & mask) << lo);
}

/// Reads `width` bits of `word` from bit `lo`.
#[cfg(test)]
pub(crate) fn bits(word: u32, lo: u32, width: u32) -> u32 {
    let mask = ((1u64 << width) - 1) as u32;
    (word >> lo) & mask
}

/// `NV_ENC_CAPS_PARAM`.
#[repr(C)]
pub(crate) struct CapsParam {
    pub(crate) version: u32,
    pub(crate) caps_to_query: u32,
    pub(crate) reserved: [u32; 62],
}

/// `NV_ENC_CREATE_BITSTREAM_BUFFER`.
#[repr(C)]
pub(crate) struct CreateBitstreamBuffer {
    pub(crate) version: u32,
    pub(crate) size: u32,
    pub(crate) memory_heap: u32,
    pub(crate) reserved: u32,
    pub(crate) bitstream_buffer: *mut c_void,
    pub(crate) bitstream_buffer_ptr: *mut c_void,
    pub(crate) reserved1: [u32; 58],
    pub(crate) reserved2: [*mut c_void; 64],
}

/// `NV_ENC_QP`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Qp {
    pub(crate) inter_p: u32,
    pub(crate) inter_b: u32,
    pub(crate) intra: u32,
}

/// `NV_ENC_RC_PARAMS`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct RcParams {
    pub(crate) version: u32,
    pub(crate) rate_control_mode: u32,
    pub(crate) const_qp: Qp,
    pub(crate) average_bit_rate: u32,
    pub(crate) max_bit_rate: u32,
    pub(crate) vbv_buffer_size: u32,
    pub(crate) vbv_initial_delay: u32,
    /// The bit-fields from `enableMinQP` to `reservedBitFields` ([`RcParams::LOOKAHEAD`]
    /// and the other bit constants).
    pub(crate) flags: u32,
    pub(crate) min_qp: Qp,
    pub(crate) max_qp: Qp,
    pub(crate) initial_rc_qp: Qp,
    pub(crate) temporal_layer_idx_mask: u32,
    pub(crate) temporal_layer_qp: [u8; 8],
    pub(crate) target_quality: u8,
    pub(crate) target_quality_lsb: u8,
    pub(crate) lookahead_depth: u16,
    pub(crate) low_delay_key_frame_scale: u8,
    pub(crate) y_dc_qp_index_offset: i8,
    pub(crate) u_dc_qp_index_offset: i8,
    pub(crate) v_dc_qp_index_offset: i8,
    pub(crate) qp_map_mode: u32,
    pub(crate) multi_pass: u32,
    pub(crate) alpha_layer_bitrate_ratio: u32,
    pub(crate) cb_qp_index_offset: i8,
    pub(crate) cr_qp_index_offset: i8,
    pub(crate) reserved2: u16,
    pub(crate) reserved: [u32; 4],
}

impl RcParams {
    /// `enableLookahead`'s bit in [`flags`](Self::flags).
    pub(crate) const LOOKAHEAD: u32 = 5;
    /// `zeroReorderDelay`'s bit.
    pub(crate) const ZERO_REORDER_DELAY: u32 = 9;
}

/// `NV_ENC_CONFIG_H264_VUI_PARAMETERS` (and HEVC's, the same type).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct VuiParameters {
    pub(crate) overscan_info_present_flag: u32,
    pub(crate) overscan_info: u32,
    pub(crate) video_signal_type_present_flag: u32,
    pub(crate) video_format: u32,
    pub(crate) video_full_range_flag: u32,
    pub(crate) colour_description_present_flag: u32,
    pub(crate) colour_primaries: u32,
    pub(crate) transfer_characteristics: u32,
    pub(crate) colour_matrix: u32,
    pub(crate) chroma_sample_location_flag: u32,
    pub(crate) chroma_sample_location_top: u32,
    pub(crate) chroma_sample_location_bot: u32,
    pub(crate) bitstream_restriction_flag: u32,
    pub(crate) timing_info_present_flag: u32,
    pub(crate) num_unit_in_ticks: u32,
    pub(crate) time_scale: u32,
    pub(crate) reserved: [u32; 12],
}

/// `NV_ENC_CONFIG_H264`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct ConfigH264 {
    /// The bit-fields from `enableTemporalSVC` to `reservedBitFields`.
    pub(crate) flags: u32,
    pub(crate) level: u32,
    pub(crate) idr_period: u32,
    /// `separateColourPlaneFlag` to `sliceModeData`.
    pub(crate) _skip1: [u32; 15],
    pub(crate) vui: VuiParameters,
    /// `ltrNumFrames`, `ltrTrustMode`.
    pub(crate) _skip2: [u32; 2],
    pub(crate) chroma_format_idc: u32,
    /// `maxTemporalLayers` to `numRefL1`.
    pub(crate) _skip3: [u32; 4],
    pub(crate) reserved1: [u32; 267],
    pub(crate) reserved2: [*mut c_void; 64],
}

impl ConfigH264 {
    /// `outputAUD`'s bit in [`flags`](Self::flags).
    pub(crate) const OUTPUT_AUD: u32 = 6;
    /// `disableSPSPPS`'s bit.
    pub(crate) const DISABLE_SPSPPS: u32 = 7;
    /// `repeatSPSPPS`'s bit.
    pub(crate) const REPEAT_SPSPPS: u32 = 12;
}

/// `NV_ENC_CONFIG_HEVC`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct ConfigHevc {
    pub(crate) level: u32,
    pub(crate) tier: u32,
    pub(crate) min_cu_size: u32,
    pub(crate) max_cu_size: u32,
    /// The bit-fields from `useConstrainedIntraPred` to `reserved`.
    pub(crate) flags: u32,
    pub(crate) idr_period: u32,
    /// `intraRefreshPeriod` to `maxTemporalLayersMinus1`.
    pub(crate) _skip1: [u32; 10],
    pub(crate) vui: VuiParameters,
    /// `ltrTrustMode` to `numRefL1`.
    pub(crate) _skip2: [u32; 4],
    pub(crate) reserved1: [u32; 214],
    pub(crate) reserved2: [*mut c_void; 64],
}

impl ConfigHevc {
    /// `outputAUD`'s bit in [`flags`](Self::flags).
    pub(crate) const OUTPUT_AUD: u32 = 4;
    /// `disableSPSPPS`'s bit.
    pub(crate) const DISABLE_SPSPPS: u32 = 6;
    /// `repeatSPSPPS`'s bit.
    pub(crate) const REPEAT_SPSPPS: u32 = 7;
    /// `chromaFormatIDC`'s first bit (two bits).
    pub(crate) const CHROMA_FORMAT_IDC: u32 = 9;
    /// `pixelBitDepthMinus8`'s first bit (three bits).
    pub(crate) const PIXEL_BIT_DEPTH_MINUS8: u32 = 11;
}

/// `NV_ENC_CONFIG_AV1`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct ConfigAv1 {
    pub(crate) level: u32,
    pub(crate) tier: u32,
    pub(crate) min_part_size: u32,
    pub(crate) max_part_size: u32,
    /// The bit-fields from `outputAnnexBFormat` to `reserved`.
    pub(crate) flags: u32,
    pub(crate) idr_period: u32,
    /// `intraRefreshPeriod` to `numTileRows`.
    pub(crate) _skip1: [u32; 5],
    pub(crate) tile_widths: *mut u32,
    pub(crate) tile_heights: *mut u32,
    pub(crate) max_temporal_layers_minus1: u32,
    pub(crate) color_primaries: u32,
    pub(crate) transfer_characteristics: u32,
    pub(crate) matrix_coefficients: u32,
    pub(crate) color_range: u32,
    pub(crate) chroma_sample_position: u32,
    pub(crate) use_b_frames_as_ref: u32,
    pub(crate) film_grain_params: *mut c_void,
    pub(crate) num_fwd_refs: u32,
    pub(crate) num_bwd_refs: u32,
    pub(crate) reserved1: [u32; 235],
    pub(crate) reserved2: [*mut c_void; 62],
}

impl ConfigAv1 {
    /// `outputAnnexBFormat`'s bit in [`flags`](Self::flags).
    pub(crate) const OUTPUT_ANNEXB_FORMAT: u32 = 0;
    /// `disableSeqHdr`'s bit.
    pub(crate) const DISABLE_SEQ_HDR: u32 = 4;
    /// `repeatSeqHdr`'s bit.
    pub(crate) const REPEAT_SEQ_HDR: u32 = 5;
    /// `chromaFormatIDC`'s first bit (two bits).
    pub(crate) const CHROMA_FORMAT_IDC: u32 = 7;
    /// `inputPixelBitDepthMinus8`'s first bit (three bits).
    pub(crate) const INPUT_PIXEL_BIT_DEPTH_MINUS8: u32 = 12;
    /// `pixelBitDepthMinus8`'s first bit (three bits).
    pub(crate) const PIXEL_BIT_DEPTH_MINUS8: u32 = 15;
}

/// `NV_ENC_CODEC_CONFIG`. The header's other members (`h264MeOnly`, `hevcMeOnly`,
/// `reserved[320]`) are smaller than `h264` and never used.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) union CodecConfig {
    pub(crate) h264: ConfigH264,
    pub(crate) hevc: ConfigHevc,
    pub(crate) av1: ConfigAv1,
}

/// `NV_ENC_CONFIG`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Config {
    pub(crate) version: u32,
    pub(crate) profile_guid: Guid,
    pub(crate) gop_length: u32,
    pub(crate) frame_interval_p: i32,
    pub(crate) mono_chrome_encoding: u32,
    pub(crate) frame_field_mode: u32,
    pub(crate) mv_precision: u32,
    pub(crate) rc_params: RcParams,
    pub(crate) codec: CodecConfig,
    pub(crate) reserved: [u32; 278],
    pub(crate) reserved2: [*mut c_void; 64],
}

/// `NV_ENC_PRESET_CONFIG`.
#[repr(C)]
pub(crate) struct PresetConfig {
    pub(crate) version: u32,
    pub(crate) preset_cfg: Config,
    pub(crate) reserved1: [u32; 255],
    pub(crate) reserved2: [*mut c_void; 64],
}

/// `NV_ENC_INITIALIZE_PARAMS`.
#[repr(C)]
pub(crate) struct InitializeParams {
    pub(crate) version: u32,
    pub(crate) encode_guid: Guid,
    pub(crate) preset_guid: Guid,
    pub(crate) encode_width: u32,
    pub(crate) encode_height: u32,
    pub(crate) dar_width: u32,
    pub(crate) dar_height: u32,
    pub(crate) frame_rate_num: u32,
    pub(crate) frame_rate_den: u32,
    pub(crate) enable_encode_async: u32,
    pub(crate) enable_ptd: u32,
    /// The bit-fields from `reportSliceOffsets` to `reservedBitFields`.
    pub(crate) flags: u32,
    pub(crate) priv_data_size: u32,
    pub(crate) priv_data: *mut c_void,
    pub(crate) encode_config: *mut Config,
    pub(crate) max_encode_width: u32,
    pub(crate) max_encode_height: u32,
    /// `maxMEHintCountsPerBlock[2]`.
    pub(crate) max_me_hint_counts_per_block: [u32; 8],
    pub(crate) tuning_info: u32,
    pub(crate) buffer_format: u32,
    pub(crate) reserved: [u32; 287],
    pub(crate) reserved2: [*mut c_void; 64],
}

/// `NV_ENC_CODEC_PIC_PARAMS`, never filled: picture type decision is the encoder's
/// (`enablePTD`), so the per-codec picture parameters stay zero.
#[repr(C, align(8))]
pub(crate) struct CodecPicParams {
    pub(crate) bytes: [u32; 388],
}

/// `NV_ENC_PIC_PARAMS`.
#[repr(C)]
pub(crate) struct PicParams {
    pub(crate) version: u32,
    pub(crate) input_width: u32,
    pub(crate) input_height: u32,
    pub(crate) input_pitch: u32,
    pub(crate) encode_pic_flags: u32,
    pub(crate) frame_idx: u32,
    pub(crate) input_time_stamp: u64,
    pub(crate) input_duration: u64,
    pub(crate) input_buffer: *mut c_void,
    pub(crate) output_bitstream: *mut c_void,
    pub(crate) completion_event: *mut c_void,
    pub(crate) buffer_fmt: u32,
    pub(crate) picture_struct: u32,
    pub(crate) picture_type: u32,
    pub(crate) codec_pic_params: CodecPicParams,
    /// `meHintCountsPerBlock[2]`.
    pub(crate) me_hint_counts_per_block: [u32; 8],
    pub(crate) me_external_hints: *mut c_void,
    pub(crate) reserved1: [u32; 6],
    pub(crate) reserved2: [*mut c_void; 2],
    pub(crate) qp_delta_map: *mut i8,
    pub(crate) qp_delta_map_size: u32,
    pub(crate) reserved_bit_fields: u32,
    pub(crate) me_hint_ref_pic_dist: [u16; 2],
    pub(crate) alpha_buffer: *mut c_void,
    pub(crate) me_external_sb_hints: *mut c_void,
    pub(crate) me_sb_hints_count: u32,
    pub(crate) reserved3: [u32; 285],
    pub(crate) reserved4: [*mut c_void; 58],
}

/// `NV_ENC_LOCK_BITSTREAM`.
#[repr(C)]
pub(crate) struct LockBitstream {
    pub(crate) version: u32,
    /// The bit-fields `doNotWait` (bit 0), `ltrFrame`, `getRCStats` and the reserved ones.
    pub(crate) flags: u32,
    pub(crate) output_bitstream: *mut c_void,
    pub(crate) slice_offsets: *mut u32,
    pub(crate) frame_idx: u32,
    pub(crate) hw_encode_status: u32,
    pub(crate) num_slices: u32,
    pub(crate) bitstream_size_in_bytes: u32,
    pub(crate) output_time_stamp: u64,
    pub(crate) output_duration: u64,
    pub(crate) bitstream_buffer_ptr: *mut c_void,
    pub(crate) picture_type: u32,
    pub(crate) picture_struct: u32,
    pub(crate) frame_avg_qp: u32,
    pub(crate) frame_satd: u32,
    pub(crate) ltr_frame_idx: u32,
    pub(crate) ltr_frame_bitmap: u32,
    pub(crate) temporal_id: u32,
    pub(crate) reserved: [u32; 12],
    pub(crate) intra_mb_count: u32,
    pub(crate) inter_mb_count: u32,
    pub(crate) average_mvx: i32,
    pub(crate) average_mvy: i32,
    pub(crate) alpha_layer_size_in_bytes: u32,
    pub(crate) reserved1: [u32; 218],
    pub(crate) reserved2: [*mut c_void; 64],
}

/// `NV_ENC_MAP_INPUT_RESOURCE`.
#[repr(C)]
pub(crate) struct MapInputResource {
    pub(crate) version: u32,
    pub(crate) sub_resource_index: u32,
    pub(crate) input_resource: *mut c_void,
    pub(crate) registered_resource: *mut c_void,
    pub(crate) mapped_resource: *mut c_void,
    pub(crate) mapped_buffer_fmt: u32,
    pub(crate) reserved1: [u32; 251],
    pub(crate) reserved2: [*mut c_void; 63],
}

/// `NV_ENC_REGISTER_RESOURCE`.
#[repr(C)]
pub(crate) struct RegisterResource {
    pub(crate) version: u32,
    pub(crate) resource_type: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pitch: u32,
    pub(crate) sub_resource_index: u32,
    pub(crate) resource_to_register: *mut c_void,
    pub(crate) registered_resource: *mut c_void,
    pub(crate) buffer_format: u32,
    pub(crate) buffer_usage: u32,
    pub(crate) p_input_fence_point: *mut c_void,
    pub(crate) reserved1: [u32; 247],
    pub(crate) reserved2: [*mut c_void; 61],
}

/// `NV_ENC_EVENT_PARAMS`.
#[repr(C)]
pub(crate) struct EventParams {
    pub(crate) version: u32,
    pub(crate) reserved: u32,
    pub(crate) completion_event: *mut c_void,
    pub(crate) reserved1: [u32; 253],
    pub(crate) reserved2: [*mut c_void; 64],
}

/// `NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS`.
#[repr(C)]
pub(crate) struct OpenEncodeSessionExParams {
    pub(crate) version: u32,
    pub(crate) device_type: u32,
    pub(crate) device: *mut c_void,
    pub(crate) reserved: *mut c_void,
    pub(crate) api_version: u32,
    pub(crate) reserved1: [u32; 253],
    pub(crate) reserved2: [*mut c_void; 64],
}

/// An encoder session handle.
pub(crate) type Encoder = *mut c_void;

/// `PNVENCGETENCODEGUIDS`.
pub(crate) type GetEncodeGuids =
    unsafe extern "system" fn(Encoder, *mut Guid, u32, *mut u32) -> Status;
/// `PNVENCGETENCODEGUIDCOUNT`.
pub(crate) type GetEncodeGuidCount = unsafe extern "system" fn(Encoder, *mut u32) -> Status;
/// `PNVENCGETENCODECAPS`.
pub(crate) type GetEncodeCaps =
    unsafe extern "system" fn(Encoder, Guid, *mut CapsParam, *mut i32) -> Status;
/// `PNVENCGETENCODEPRESETCONFIGEX`.
pub(crate) type GetEncodePresetConfigEx =
    unsafe extern "system" fn(Encoder, Guid, Guid, u32, *mut PresetConfig) -> Status;
/// `PNVENCINITIALIZEENCODER`.
pub(crate) type InitializeEncoder =
    unsafe extern "system" fn(Encoder, *mut InitializeParams) -> Status;
/// `PNVENCCREATEBITSTREAMBUFFER`.
pub(crate) type CreateBitstreamBufferFn =
    unsafe extern "system" fn(Encoder, *mut CreateBitstreamBuffer) -> Status;
/// `PNVENCDESTROYBITSTREAMBUFFER`, `PNVENCUNLOCKBITSTREAM`, `PNVENCUNMAPINPUTRESOURCE`
/// and `PNVENCUNREGISTERRESOURCE`: each takes the session and one handle.
pub(crate) type HandleFn = unsafe extern "system" fn(Encoder, *mut c_void) -> Status;
/// `PNVENCENCODEPICTURE`.
pub(crate) type EncodePicture = unsafe extern "system" fn(Encoder, *mut PicParams) -> Status;
/// `PNVENCLOCKBITSTREAM`.
pub(crate) type LockBitstreamFn = unsafe extern "system" fn(Encoder, *mut LockBitstream) -> Status;
/// `PNVENCREGISTERASYNCEVENT` and `PNVENCUNREGISTERASYNCEVENT`.
pub(crate) type AsyncEventFn = unsafe extern "system" fn(Encoder, *mut EventParams) -> Status;
/// `PNVENCMAPINPUTRESOURCE`.
pub(crate) type MapInputResourceFn =
    unsafe extern "system" fn(Encoder, *mut MapInputResource) -> Status;
/// `PNVENCDESTROYENCODER`.
pub(crate) type DestroyEncoder = unsafe extern "system" fn(Encoder) -> Status;
/// `PNVENCOPENENCODESESSIONEX`.
pub(crate) type OpenEncodeSessionEx =
    unsafe extern "system" fn(*mut OpenEncodeSessionExParams, *mut Encoder) -> Status;
/// `PNVENCREGISTERRESOURCE`.
pub(crate) type RegisterResourceFn =
    unsafe extern "system" fn(Encoder, *mut RegisterResource) -> Status;
/// `PNVENCGETLASTERROR`.
pub(crate) type GetLastErrorString = unsafe extern "system" fn(Encoder) -> *const c_char;

/// `NvEncodeAPIGetMaxSupportedVersion`.
pub(crate) type GetMaxSupportedVersion = unsafe extern "system" fn(*mut u32) -> Status;
/// `NvEncodeAPICreateInstance`.
pub(crate) type CreateInstance = unsafe extern "system" fn(*mut FunctionList) -> Status;

/// `NV_ENCODE_API_FUNCTION_LIST`. Entries the session never calls are plain pointers.
#[repr(C)]
pub(crate) struct FunctionList {
    pub(crate) version: u32,
    pub(crate) reserved: u32,
    pub(crate) nv_enc_open_encode_session: *mut c_void,
    pub(crate) nv_enc_get_encode_guid_count: Option<GetEncodeGuidCount>,
    pub(crate) nv_enc_get_encode_profile_guid_count: *mut c_void,
    pub(crate) nv_enc_get_encode_profile_guids: *mut c_void,
    pub(crate) nv_enc_get_encode_guids: Option<GetEncodeGuids>,
    pub(crate) nv_enc_get_input_format_count: *mut c_void,
    pub(crate) nv_enc_get_input_formats: *mut c_void,
    pub(crate) nv_enc_get_encode_caps: Option<GetEncodeCaps>,
    pub(crate) nv_enc_get_encode_preset_count: *mut c_void,
    pub(crate) nv_enc_get_encode_preset_guids: *mut c_void,
    pub(crate) nv_enc_get_encode_preset_config: *mut c_void,
    pub(crate) nv_enc_initialize_encoder: Option<InitializeEncoder>,
    pub(crate) nv_enc_create_input_buffer: *mut c_void,
    pub(crate) nv_enc_destroy_input_buffer: *mut c_void,
    pub(crate) nv_enc_create_bitstream_buffer: Option<CreateBitstreamBufferFn>,
    pub(crate) nv_enc_destroy_bitstream_buffer: Option<HandleFn>,
    pub(crate) nv_enc_encode_picture: Option<EncodePicture>,
    pub(crate) nv_enc_lock_bitstream: Option<LockBitstreamFn>,
    pub(crate) nv_enc_unlock_bitstream: Option<HandleFn>,
    pub(crate) nv_enc_lock_input_buffer: *mut c_void,
    pub(crate) nv_enc_unlock_input_buffer: *mut c_void,
    pub(crate) nv_enc_get_encode_stats: *mut c_void,
    pub(crate) nv_enc_get_sequence_params: *mut c_void,
    pub(crate) nv_enc_register_async_event: Option<AsyncEventFn>,
    pub(crate) nv_enc_unregister_async_event: Option<AsyncEventFn>,
    pub(crate) nv_enc_map_input_resource: Option<MapInputResourceFn>,
    pub(crate) nv_enc_unmap_input_resource: Option<HandleFn>,
    pub(crate) nv_enc_destroy_encoder: Option<DestroyEncoder>,
    pub(crate) nv_enc_invalidate_ref_frames: *mut c_void,
    pub(crate) nv_enc_open_encode_session_ex: Option<OpenEncodeSessionEx>,
    pub(crate) nv_enc_register_resource: Option<RegisterResourceFn>,
    pub(crate) nv_enc_unregister_resource: Option<HandleFn>,
    pub(crate) nv_enc_reconfigure_encoder: *mut c_void,
    pub(crate) reserved1: *mut c_void,
    pub(crate) nv_enc_create_mv_buffer: *mut c_void,
    pub(crate) nv_enc_destroy_mv_buffer: *mut c_void,
    pub(crate) nv_enc_run_motion_estimation_only: *mut c_void,
    pub(crate) nv_enc_get_last_error_string: Option<GetLastErrorString>,
    pub(crate) nv_enc_set_io_cuda_streams: *mut c_void,
    pub(crate) nv_enc_get_encode_preset_config_ex: Option<GetEncodePresetConfigEx>,
    pub(crate) nv_enc_get_sequence_param_ex: *mut c_void,
    pub(crate) reserved2: [*mut c_void; 277],
}

/// Plain C data for which all-zero bytes are a valid value: integers, raw pointers,
/// `Option`s of function pointers and arrays and unions of them. The API asks for its
/// parameter structs zeroed before the fields are set.
pub(crate) trait Zeroed: Sized {
    /// The all-zero value.
    fn zeroed() -> Self;
}

macro_rules! zeroed {
    ($($t:ty),* $(,)?) => {$(
        impl Zeroed for $t {
            fn zeroed() -> Self {
                // SAFETY: every field of the type is an integer, a raw pointer, an
                // `Option` of a function pointer, or an array, struct or union of those,
                // for all of which all-zero bytes are a valid value (null, `None`, 0).
                unsafe { core::mem::zeroed() }
            }
        }
    )*};
}

zeroed!(
    CapsParam,
    CreateBitstreamBuffer,
    Config,
    PresetConfig,
    InitializeParams,
    PicParams,
    LockBitstream,
    MapInputResource,
    RegisterResource,
    EventParams,
    OpenEncodeSessionExParams,
    FunctionList,
);

// The sizes and offsets of clang's MSVC layout of the header (module docs).
const _: () = {
    use core::mem::{align_of, offset_of, size_of};
    assert!(size_of::<Guid>() == 16);
    assert!(size_of::<CapsParam>() == 256);
    assert!(offset_of!(CapsParam, caps_to_query) == 4);

    assert!(size_of::<CreateBitstreamBuffer>() == 776);
    assert!(offset_of!(CreateBitstreamBuffer, bitstream_buffer) == 16);
    assert!(offset_of!(CreateBitstreamBuffer, reserved1) == 32);
    assert!(offset_of!(CreateBitstreamBuffer, reserved2) == 264);

    assert!(size_of::<Qp>() == 12);
    assert!(size_of::<RcParams>() == 128);
    assert!(align_of::<RcParams>() == 4);
    assert!(offset_of!(RcParams, rate_control_mode) == 4);
    assert!(offset_of!(RcParams, average_bit_rate) == 20);
    assert!(offset_of!(RcParams, max_bit_rate) == 24);
    assert!(offset_of!(RcParams, vbv_buffer_size) == 28);
    assert!(offset_of!(RcParams, vbv_initial_delay) == 32);
    assert!(offset_of!(RcParams, flags) == 36);
    assert!(offset_of!(RcParams, min_qp) == 40);
    assert!(offset_of!(RcParams, temporal_layer_qp) == 80);
    assert!(offset_of!(RcParams, lookahead_depth) == 90);
    assert!(offset_of!(RcParams, qp_map_mode) == 96);
    assert!(offset_of!(RcParams, multi_pass) == 100);
    assert!(offset_of!(RcParams, cb_qp_index_offset) == 108);
    assert!(offset_of!(RcParams, reserved) == 112);

    assert!(size_of::<VuiParameters>() == 112);
    assert!(offset_of!(VuiParameters, video_signal_type_present_flag) == 8);
    assert!(offset_of!(VuiParameters, video_format) == 12);
    assert!(offset_of!(VuiParameters, video_full_range_flag) == 16);
    assert!(offset_of!(VuiParameters, colour_description_present_flag) == 20);
    assert!(offset_of!(VuiParameters, colour_primaries) == 24);
    assert!(offset_of!(VuiParameters, transfer_characteristics) == 28);
    assert!(offset_of!(VuiParameters, colour_matrix) == 32);
    assert!(offset_of!(VuiParameters, reserved) == 64);

    assert!(size_of::<ConfigH264>() == 1792);
    assert!(offset_of!(ConfigH264, idr_period) == 8);
    assert!(offset_of!(ConfigH264, vui) == 72);
    assert!(offset_of!(ConfigH264, chroma_format_idc) == 192);
    assert!(offset_of!(ConfigH264, reserved1) == 212);
    assert!(offset_of!(ConfigH264, reserved2) == 1280);

    assert!(size_of::<ConfigHevc>() == 1560);
    assert!(offset_of!(ConfigHevc, flags) == 16);
    assert!(offset_of!(ConfigHevc, idr_period) == 20);
    assert!(offset_of!(ConfigHevc, vui) == 64);
    assert!(offset_of!(ConfigHevc, reserved1) == 192);
    assert!(offset_of!(ConfigHevc, reserved2) == 1048);

    assert!(size_of::<ConfigAv1>() == 1552);
    assert!(offset_of!(ConfigAv1, flags) == 16);
    assert!(offset_of!(ConfigAv1, idr_period) == 20);
    assert!(offset_of!(ConfigAv1, tile_widths) == 48);
    assert!(offset_of!(ConfigAv1, color_primaries) == 68);
    assert!(offset_of!(ConfigAv1, transfer_characteristics) == 72);
    assert!(offset_of!(ConfigAv1, matrix_coefficients) == 76);
    assert!(offset_of!(ConfigAv1, color_range) == 80);
    assert!(offset_of!(ConfigAv1, film_grain_params) == 96);
    assert!(offset_of!(ConfigAv1, reserved1) == 112);
    assert!(offset_of!(ConfigAv1, reserved2) == 1056);

    assert!(size_of::<CodecConfig>() == 1792);
    assert!(align_of::<CodecConfig>() == 8);

    assert!(size_of::<Config>() == 3584);
    assert!(offset_of!(Config, profile_guid) == 4);
    assert!(offset_of!(Config, gop_length) == 20);
    assert!(offset_of!(Config, frame_interval_p) == 24);
    assert!(offset_of!(Config, frame_field_mode) == 32);
    assert!(offset_of!(Config, rc_params) == 40);
    assert!(offset_of!(Config, codec) == 168);
    assert!(offset_of!(Config, reserved) == 1960);
    assert!(offset_of!(Config, reserved2) == 3072);

    assert!(size_of::<PresetConfig>() == 5128);
    assert!(offset_of!(PresetConfig, preset_cfg) == 8);
    assert!(offset_of!(PresetConfig, reserved1) == 3592);
    assert!(offset_of!(PresetConfig, reserved2) == 4616);

    assert!(size_of::<InitializeParams>() == 1808);
    assert!(offset_of!(InitializeParams, encode_guid) == 4);
    assert!(offset_of!(InitializeParams, preset_guid) == 20);
    assert!(offset_of!(InitializeParams, encode_width) == 36);
    assert!(offset_of!(InitializeParams, dar_width) == 44);
    assert!(offset_of!(InitializeParams, frame_rate_num) == 52);
    assert!(offset_of!(InitializeParams, enable_encode_async) == 60);
    assert!(offset_of!(InitializeParams, enable_ptd) == 64);
    assert!(offset_of!(InitializeParams, flags) == 68);
    assert!(offset_of!(InitializeParams, priv_data) == 80);
    assert!(offset_of!(InitializeParams, encode_config) == 88);
    assert!(offset_of!(InitializeParams, max_encode_width) == 96);
    assert!(offset_of!(InitializeParams, max_me_hint_counts_per_block) == 104);
    assert!(offset_of!(InitializeParams, tuning_info) == 136);
    assert!(offset_of!(InitializeParams, buffer_format) == 140);
    assert!(offset_of!(InitializeParams, reserved) == 144);
    assert!(offset_of!(InitializeParams, reserved2) == 1296);

    assert!(size_of::<CodecPicParams>() == 1552);
    assert!(size_of::<PicParams>() == 3360);
    assert!(offset_of!(PicParams, input_width) == 4);
    assert!(offset_of!(PicParams, encode_pic_flags) == 16);
    assert!(offset_of!(PicParams, input_time_stamp) == 24);
    assert!(offset_of!(PicParams, input_buffer) == 40);
    assert!(offset_of!(PicParams, output_bitstream) == 48);
    assert!(offset_of!(PicParams, completion_event) == 56);
    assert!(offset_of!(PicParams, buffer_fmt) == 64);
    assert!(offset_of!(PicParams, picture_struct) == 68);
    assert!(offset_of!(PicParams, picture_type) == 72);
    assert!(offset_of!(PicParams, codec_pic_params) == 80);
    assert!(offset_of!(PicParams, me_hint_counts_per_block) == 1632);
    assert!(offset_of!(PicParams, me_external_hints) == 1664);
    assert!(offset_of!(PicParams, qp_delta_map) == 1712);
    assert!(offset_of!(PicParams, me_hint_ref_pic_dist) == 1728);
    assert!(offset_of!(PicParams, alpha_buffer) == 1736);
    assert!(offset_of!(PicParams, me_sb_hints_count) == 1752);
    assert!(offset_of!(PicParams, reserved3) == 1756);
    assert!(offset_of!(PicParams, reserved4) == 2896);

    assert!(size_of::<LockBitstream>() == 1544);
    assert!(offset_of!(LockBitstream, flags) == 4);
    assert!(offset_of!(LockBitstream, output_bitstream) == 8);
    assert!(offset_of!(LockBitstream, bitstream_size_in_bytes) == 36);
    assert!(offset_of!(LockBitstream, output_time_stamp) == 40);
    assert!(offset_of!(LockBitstream, bitstream_buffer_ptr) == 56);
    assert!(offset_of!(LockBitstream, picture_type) == 64);
    assert!(offset_of!(LockBitstream, reserved) == 92);
    assert!(offset_of!(LockBitstream, intra_mb_count) == 140);
    assert!(offset_of!(LockBitstream, reserved1) == 160);
    assert!(offset_of!(LockBitstream, reserved2) == 1032);

    assert!(size_of::<MapInputResource>() == 1544);
    assert!(offset_of!(MapInputResource, registered_resource) == 16);
    assert!(offset_of!(MapInputResource, mapped_resource) == 24);
    assert!(offset_of!(MapInputResource, mapped_buffer_fmt) == 32);
    assert!(offset_of!(MapInputResource, reserved2) == 1040);

    assert!(size_of::<RegisterResource>() == 1536);
    assert!(offset_of!(RegisterResource, resource_type) == 4);
    assert!(offset_of!(RegisterResource, width) == 8);
    assert!(offset_of!(RegisterResource, pitch) == 16);
    assert!(offset_of!(RegisterResource, sub_resource_index) == 20);
    assert!(offset_of!(RegisterResource, resource_to_register) == 24);
    assert!(offset_of!(RegisterResource, registered_resource) == 32);
    assert!(offset_of!(RegisterResource, buffer_format) == 40);
    assert!(offset_of!(RegisterResource, buffer_usage) == 44);
    assert!(offset_of!(RegisterResource, p_input_fence_point) == 48);
    assert!(offset_of!(RegisterResource, reserved2) == 1048);

    assert!(size_of::<EventParams>() == 1544);
    assert!(offset_of!(EventParams, completion_event) == 8);
    assert!(offset_of!(EventParams, reserved2) == 1032);

    assert!(size_of::<OpenEncodeSessionExParams>() == 1552);
    assert!(offset_of!(OpenEncodeSessionExParams, device_type) == 4);
    assert!(offset_of!(OpenEncodeSessionExParams, device) == 8);
    assert!(offset_of!(OpenEncodeSessionExParams, api_version) == 24);
    assert!(offset_of!(OpenEncodeSessionExParams, reserved2) == 1040);

    assert!(size_of::<FunctionList>() == 2552);
    assert!(offset_of!(FunctionList, nv_enc_get_encode_guid_count) == 16);
    assert!(offset_of!(FunctionList, nv_enc_get_encode_guids) == 40);
    assert!(offset_of!(FunctionList, nv_enc_get_encode_caps) == 64);
    assert!(offset_of!(FunctionList, nv_enc_initialize_encoder) == 96);
    assert!(offset_of!(FunctionList, nv_enc_create_bitstream_buffer) == 120);
    assert!(offset_of!(FunctionList, nv_enc_destroy_bitstream_buffer) == 128);
    assert!(offset_of!(FunctionList, nv_enc_encode_picture) == 136);
    assert!(offset_of!(FunctionList, nv_enc_lock_bitstream) == 144);
    assert!(offset_of!(FunctionList, nv_enc_unlock_bitstream) == 152);
    assert!(offset_of!(FunctionList, nv_enc_register_async_event) == 192);
    assert!(offset_of!(FunctionList, nv_enc_unregister_async_event) == 200);
    assert!(offset_of!(FunctionList, nv_enc_map_input_resource) == 208);
    assert!(offset_of!(FunctionList, nv_enc_unmap_input_resource) == 216);
    assert!(offset_of!(FunctionList, nv_enc_destroy_encoder) == 224);
    assert!(offset_of!(FunctionList, nv_enc_open_encode_session_ex) == 240);
    assert!(offset_of!(FunctionList, nv_enc_register_resource) == 248);
    assert!(offset_of!(FunctionList, nv_enc_unregister_resource) == 256);
    assert!(offset_of!(FunctionList, nv_enc_get_last_error_string) == 304);
    assert!(offset_of!(FunctionList, nv_enc_get_encode_preset_config_ex) == 320);
    assert!(offset_of!(FunctionList, reserved2) == 336);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_versions_match_the_headers_macros() {
        // As clang evaluated the header's macros (module docs).
        assert_eq!(CAPS_PARAM_VER, 0x7001_000C);
        assert_eq!(CONFIG_VER, 0xF008_000C);
        assert_eq!(INITIALIZE_PARAMS_VER, 0xF005_000C);
        assert_eq!(PRESET_CONFIG_VER, 0xF004_000C);
        assert_eq!(PIC_PARAMS_VER, 0xF006_000C);
        assert_eq!(LOCK_BITSTREAM_VER, 0x7002_000C);
        assert_eq!(MAP_INPUT_RESOURCE_VER, 0x7004_000C);
        assert_eq!(REGISTER_RESOURCE_VER, 0x7004_000C);
        assert_eq!(EVENT_PARAMS_VER, 0x7001_000C);
        assert_eq!(OPEN_ENCODE_SESSION_EX_PARAMS_VER, 0x7001_000C);
        assert_eq!(FUNCTION_LIST_VER, 0x7002_000C);
        assert_eq!(CREATE_BITSTREAM_BUFFER_VER, 0x7001_000C);
    }

    #[test]
    fn bit_fields_set_only_their_bits() {
        let mut w = u32::MAX;
        set_bits(&mut w, 9, 2, 1);
        assert_eq!(bits(w, 9, 2), 1);
        assert_eq!(w, !(1 << 10));
        let mut w = 0;
        set_bits(&mut w, 15, 3, 0xFF);
        assert_eq!(w, 0b111 << 15);
        set_bits(&mut w, 0, 32, 7);
        assert_eq!(w, 7);
    }

    #[test]
    fn statuses_have_names() {
        assert_eq!(status_name(SUCCESS), "NV_ENC_SUCCESS");
        assert_eq!(
            status_name(ERR_NEED_MORE_INPUT),
            "NV_ENC_ERR_NEED_MORE_INPUT"
        );
        assert_eq!(status_name(25), "NV_ENC_ERR_RESOURCE_NOT_MAPPED");
        assert_eq!(status_name(99), "an unknown NVENCSTATUS");
    }
}
