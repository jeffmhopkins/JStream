//! What a VA-API HEVC session is configured with, and the parameter buffers and packed
//! headers it submits for each picture (ADR-0060 §6).
//!
//! The stream is shaped like the H.264 one (`h264`): I and P frames only, one reference
//! frame and one slice per picture, so packets come out in presentation order with
//! `dts == pts`. Every keyframe is an IDR frame (`IDR_W_RADL`), every
//! `keyframe_interval` frames or when one is requested, and each carries the VPS, SPS
//! and PPS so a viewer can join there. Picture order counts are the frames since the
//! IDR. The parameter sets are Main profile, Main tier, with BT.709 limited range and
//! the frame rate in the VUI; [`slice_header`] writes each picture's slice segment
//! header for a driver that takes packed slice headers.
//!
//! What a driver can code is read from its config attributes into [`Tools`]: the coding
//! tree, coding and transform block sizes, the coding tools it supports (AMP, SAO,
//! temporal motion vector prediction, transform skip, CU QP deltas), and whether it
//! codes P slices. A driver that codes none (`VA_PREDICTION_DIRECTION_BI_NOT_EMPTY`) gets
//! each P frame as a B slice whose two lists hold the same reference, as FFmpeg's
//! VA-API encoder does. A driver that advertises nothing gets FFmpeg's defaults.
//!
//! Platform-independent: building the buffers needs no driver, so it is tested
//! everywhere.

use jstream_platform::media::{Codec, PixelFormat};
use jstream_platform::{PlatformError, VideoEncoderConfig};

use super::ffi::{
    ATTRIB_NOT_SUPPORTED, FrameRateParam, HrdParam, Id, MiscParam, PICTURE_HEVC_RPS_ST_CURR_BEFORE,
    PREDICTION_DIRECTION_BI_NOT_EMPTY, PicParamHevc, PictureHevc, RateControlParam, SeqParamHevc,
    SliceParamHevc,
};
use super::h264::{
    BitWriter, Rate, annex_b, frame_rate_buffer, hrd_buffer, rate, rate_control_buffer,
};

/// The largest IDR period: picture order counts are the frames since the last IDR and
/// stay well inside an `i32`.
const MAX_IDR_PERIOD: u32 = 1 << 30;
/// `general_profile_idc`: Main.
const PROFILE_MAIN: u8 = 1;
/// `pic_init_qp`: the PPS's `init_qp_minus26` is 0.
const PIC_INIT_QP: u8 = 26;
/// `log2_max_pic_order_cnt_lsb_minus4 + 4`, as FFmpeg writes it.
const LOG2_MAX_POC_LSB: u32 = 12;
/// `log2_max_mv_length_horizontal` and `_vertical`: no restriction.
const LOG2_MAX_MV_LENGTH: u32 = 15;
/// `slice_type` values (H.265 Table 7-7).
const SLICE_B: u8 = 0;
const SLICE_P: u8 = 1;
const SLICE_I: u8 = 2;
/// `nal_unit_type` values (H.265 Table 7-1).
const TRAIL_R: u8 = 1;
const IDR_W_RADL: u8 = 19;
const VPS_NUT: u8 = 32;
const SPS_NUT: u8 = 33;
const PPS_NUT: u8 = 34;

/// What the driver codes, from its config attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tools {
    /// `CtbLog2SizeY`: 4 to 6.
    pub(crate) log2_ctb: u32,
    /// `MinCbLog2SizeY`: 3 up to `log2_ctb`.
    pub(crate) log2_min_cb: u32,
    /// The luma transform sizes, `log2` of 4x4 to 32x32.
    pub(crate) log2_min_tb: u32,
    pub(crate) log2_max_tb: u32,
    pub(crate) depth_inter: u32,
    pub(crate) depth_intra: u32,
    /// The coded size is a multiple of this, in luma samples.
    pub(crate) alignment: u32,
    pub(crate) amp: bool,
    pub(crate) sao: bool,
    pub(crate) temporal_mvp: bool,
    pub(crate) transform_skip: bool,
    /// `cu_qp_delta_enabled_flag`: the rate controller adjusts QP inside a picture.
    pub(crate) cu_qp_delta: bool,
    pub(crate) cu_qp_delta_depth: u32,
    /// P frames are coded as B slices with the reference in both lists.
    pub(crate) gpb: bool,
}

impl Tools {
    /// What FFmpeg assumes of a driver that advertises nothing (the first Intel HEVC
    /// encoder's capabilities): coding blocks of 8x8 to 32x32 with the coded size a
    /// multiple of 16, transforms of 4x4 to 32x32, AMP, CU QP deltas for the rate
    /// controller, and no SAO or temporal motion vector prediction.
    pub(crate) const DEFAULT: Self = Self {
        log2_ctb: 5,
        log2_min_cb: 3,
        log2_min_tb: 2,
        log2_max_tb: 5,
        depth_inter: 3,
        depth_intra: 3,
        alignment: 16,
        amp: true,
        sao: false,
        temporal_mvp: false,
        transform_skip: false,
        cu_qp_delta: true,
        cu_qp_delta_depth: 0,
        gpb: false,
    };

    /// The tools of a driver whose `VAConfigAttribEncHEVCFeatures`,
    /// `VAConfigAttribEncHEVCBlockSizes` and `VAConfigAttribPredictionDirection` values
    /// are these (`VA_ATTRIB_NOT_SUPPORTED` when it doesn't advertise one). A tool is
    /// used when the driver supports or requires it; PCM, which it may support, isn't.
    pub(crate) fn from_attributes(features: u32, block_sizes: u32, prediction: u32) -> Self {
        let mut t = Self::DEFAULT;
        // Two bits per field, lowest first (`VAConfigAttribValEncHEVCFeatures`).
        let advertised = features != ATTRIB_NOT_SUPPORTED;
        if advertised {
            let on = |field: u32| (features >> (2 * field)) & 3 != 0;
            t.amp = on(2);
            t.sao = on(3);
            t.temporal_mvp = on(5);
            t.transform_skip = on(10);
            t.cu_qp_delta = on(11);
        }
        // Two bits per field (`VAConfigAttribValEncHEVCBlockSizes`).
        if block_sizes != ATTRIB_NOT_SUPPORTED {
            let b = |field: u32| (block_sizes >> (2 * field)) & 3;
            t.log2_ctb = (b(0) + 3).clamp(4, 6);
            t.log2_min_cb = (b(2) + 3).min(t.log2_ctb);
            // MaxTbLog2SizeY is at most min(CtbLog2SizeY, 5), MinTbLog2SizeY below
            // MinCbLog2SizeY.
            t.log2_max_tb = (b(3) + 2).min(t.log2_ctb).min(5);
            t.log2_min_tb = (b(4) + 2).min(t.log2_min_cb - 1).min(t.log2_max_tb);
            let max_depth = t.log2_ctb - t.log2_min_tb;
            t.depth_inter = b(5).min(max_depth);
            t.depth_intra = b(7).min(max_depth);
            t.alignment = 1 << t.log2_min_cb;
        }
        // Down to the smallest coding block, where the driver says it does CU QP deltas.
        t.cu_qp_delta_depth = if advertised && t.cu_qp_delta {
            t.log2_ctb - t.log2_min_cb
        } else {
            0
        };
        t.gpb = prediction != ATTRIB_NOT_SUPPORTED
            && prediction & PREDICTION_DIRECTION_BI_NOT_EMPTY != 0;
        t
    }
}

/// A checked configuration and everything derived from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    /// The frame size, even in both dimensions.
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// The coded size, a multiple of [`Tools::alignment`]; the SPS's conformance window
    /// crops the excess.
    pub(crate) coded_width: u32,
    pub(crate) coded_height: u32,
    pub(crate) tools: Tools,
    /// `general_level_idc`: 30 times the level.
    pub(crate) level_idc: u8,
    pub(crate) rate: Rate,
    /// The VPS's and VUI's `num_units_in_tick` and `time_scale`: one tick per frame.
    pub(crate) tick: (u32, u32),
    /// Frames from one IDR frame to the next.
    pub(crate) idr_period: u32,
}

/// One row of H.265 Tables A.8 and A.9, Main tier: `general_level_idc`, MaxLumaPs,
/// MaxLumaSr and MaxBR (in units of 1000 bits per second, Main's `CpbBrVclFactor`).
const LEVELS: [(u8, u64, u64, u64); 13] = [
    (30, 36_864, 552_960, 128),
    (60, 122_880, 3_686_400, 1_500),
    (63, 245_760, 7_372_800, 3_000),
    (90, 552_960, 16_588_800, 6_000),
    (93, 983_040, 33_177_600, 10_000),
    (120, 2_228_224, 66_846_720, 12_000),
    (123, 2_228_224, 133_693_440, 20_000),
    (150, 8_912_896, 267_386_880, 25_000),
    (153, 8_912_896, 534_773_760, 40_000),
    (156, 8_912_896, 1_069_547_520, 60_000),
    (180, 35_651_584, 1_069_547_520, 60_000),
    (183, 35_651_584, 2_139_095_040, 120_000),
    (186, 35_651_584, 4_278_190_080, 240_000),
];

/// The lowest Main-tier level that holds a `width` x `height` coded picture at `fps`
/// with a peak of `bps`, or `None` above level 6.2. One reference frame fits every
/// level's decoded picture buffer.
pub(crate) fn level(width: u32, height: u32, fps: (u32, u32), bps: u32) -> Option<u8> {
    let (w, h) = (u64::from(width), u64::from(height));
    let ps = w * h;
    let (num, den) = (u64::from(fps.0), u64::from(fps.1.max(1)));
    let sr = (ps * num).div_ceil(den);
    LEVELS
        .iter()
        .find(|&&(_, max_ps, max_sr, max_br)| {
            ps <= max_ps
                // A.4.1 (b), (c): neither dimension exceeds sqrt(8 * MaxLumaPs).
                && w * w <= 8 * max_ps
                && h * h <= 8 * max_ps
                && sr <= max_sr
                && u64::from(bps) <= max_br * 1000
        })
        .map(|&(idc, ..)| idc)
}

/// Checks a configuration and derives the session's settings, for a driver that codes
/// `tools`.
///
/// # Errors
/// [`PlatformError::Unsupported`] for another codec, an input other than NV12, RGBA or
/// BGRA, odd dimensions, a keyframe interval of 0 or above 2^30, rate control other than
/// CBR and VBR, a zero bit rate, a VBR peak below its mean, a frame period with a zero
/// numerator, and a size, frame rate or bit rate above level 6.2.
pub(crate) fn check(config: &VideoEncoderConfig, tools: Tools) -> Result<Settings, PlatformError> {
    let unsupported = |what: String| Err(PlatformError::Unsupported { what });
    if config.codec != Codec::Hevc {
        return unsupported(format!("{:?} from the HEVC encoder", config.codec));
    }
    if !matches!(
        config.input_format,
        PixelFormat::Nv12 | PixelFormat::Rgba8 | PixelFormat::Bgra8
    ) {
        return unsupported(format!(
            "HEVC from {:?} (the encoder takes NV12, RGBA or BGRA)",
            config.input_format
        ));
    }
    let (width, height) = (config.size.width(), config.size.height());
    if width % 2 != 0 || height % 2 != 0 {
        return unsupported(format!("HEVC at an odd size, {width}x{height}"));
    }
    if config.keyframe_interval == 0 || config.keyframe_interval > MAX_IDR_PERIOD {
        return unsupported(format!(
            "a keyframe interval of {}",
            config.keyframe_interval
        ));
    }
    let rate = rate(config)?;
    let (coded_width, coded_height) = (
        width.next_multiple_of(tools.alignment),
        height.next_multiple_of(tools.alignment),
    );
    let Some(level_idc) = level(coded_width, coded_height, rate.fps, rate.bits_per_second) else {
        return unsupported(format!(
            "HEVC at {width}x{height}, {}/{} fps and {} kb/s (above level 6.2)",
            rate.fps.0, rate.fps.1, rate.peak_kbps
        ));
    };
    Ok(Settings {
        width,
        height,
        coded_width,
        coded_height,
        tools,
        level_idc,
        rate,
        // A frame lasts num/den seconds: one tick of num/den.
        tick: (rate.fps.1, rate.fps.0),
        idr_period: config.keyframe_interval,
    })
}

impl Settings {
    /// The conformance window's right and bottom offsets, in units of two luma samples
    /// (4:2:0).
    pub(crate) fn crop(&self) -> (u32, u32) {
        (
            (self.coded_width - self.width) / 2,
            (self.coded_height - self.height) / 2,
        )
    }

    /// Coding tree blocks in a picture.
    pub(crate) fn ctus(&self) -> u32 {
        let ctb = 1 << self.tools.log2_ctb;
        self.coded_width.div_ceil(ctb) * self.coded_height.div_ceil(ctb)
    }

    /// The largest coded picture to expect: an uncompressed one at the coded size,
    /// plus room for the headers.
    pub(crate) fn coded_buffer_size(&self) -> u32 {
        (self.coded_width * self.coded_height * 3 / 2).saturating_add(64 * 1024)
    }
}

/// One picture of the stream: what the GOP made of the next frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Picture {
    pub(crate) idr: bool,
    /// The picture order count: frames since the IDR.
    pub(crate) poc: i32,
}

/// Decides each frame's picture type: an IDR every `idr_period` frames, or at the next
/// frame after a request, which also restarts the cadence. P frames otherwise.
#[derive(Clone, Debug)]
pub(crate) struct Gop {
    idr_period: u32,
    /// Frames since the last IDR, which is the next frame's index in its GOP.
    next: u32,
    started: bool,
    forced: bool,
}

impl Gop {
    pub(crate) fn new(s: &Settings) -> Self {
        Self {
            idr_period: s.idr_period,
            next: 0,
            started: false,
            forced: false,
        }
    }

    /// Makes the next frame an IDR frame.
    pub(crate) fn force_idr(&mut self) {
        self.forced = true;
    }

    /// The next frame's picture.
    pub(crate) fn next_picture(&mut self) -> Picture {
        let idr = self.forced || !self.started || self.next >= self.idr_period;
        if idr {
            self.started = true;
            self.forced = false;
            self.next = 0;
        }
        let n = self.next;
        self.next += 1;
        Picture {
            idr,
            // n < idr_period <= 2^30.
            poc: i32::try_from(n).unwrap_or(i32::MAX),
        }
    }
}

/// The `seq_fields` union of `VAEncSequenceParameterBufferHEVC`.
pub(crate) fn seq_fields(t: &Tools) -> u32 {
    let chroma_format_idc = 1; // 4:2:0, bits 0-1
    let amp = u32::from(t.amp) << 11;
    let sao = u32::from(t.sao) << 12;
    let temporal_mvp = u32::from(t.temporal_mvp) << 15;
    chroma_format_idc | amp | sao | temporal_mvp
}

/// The `vui_fields` union of `VAEncSequenceParameterBufferHEVC`.
pub(crate) fn vui_fields() -> u32 {
    let aspect_ratio_info_present = 1;
    let timing_info_present = 1 << 3;
    let bitstream_restriction = 1 << 4;
    let mvs_over_pic_boundaries = 1 << 6;
    let restricted_ref_pic_lists = 1 << 7;
    let mv_h = LOG2_MAX_MV_LENGTH << 8; // bits 8-12
    let mv_v = LOG2_MAX_MV_LENGTH << 13; // bits 13-17
    aspect_ratio_info_present
        | timing_info_present
        | bitstream_restriction
        | mvs_over_pic_boundaries
        | restricted_ref_pic_lists
        | mv_h
        | mv_v
}

/// The `pic_fields` union of `VAEncPictureParameterBufferHEVC`. Every picture is a
/// reference; `coding_type` is 1 for I and 2 for P (also when coded as a B slice).
pub(crate) fn pic_fields(t: &Tools, predicted: bool) -> u32 {
    let idr_pic = u32::from(!predicted);
    let coding_type = if predicted { 2 } else { 1 } << 1; // bits 1-3
    let reference_pic = 1 << 4;
    let transform_skip = u32::from(t.transform_skip) << 8;
    let cu_qp_delta = u32::from(t.cu_qp_delta) << 9;
    idr_pic | coding_type | reference_pic | transform_skip | cu_qp_delta
}

/// The `slice_fields` union of `VAEncSliceParameterBufferHEVC`: the last (only) slice,
/// with SAO and, on predicted pictures, temporal MVP from list 0 where the tools have
/// them.
pub(crate) fn slice_fields(t: &Tools, predicted: bool) -> u32 {
    let last_slice_of_pic = 1;
    let temporal_mvp = u32::from(predicted && t.temporal_mvp) << 4;
    let sao = u32::from(t.sao) * (0b11 << 5); // luma and chroma
    let collocated_from_l0 = u32::from(predicted) << 13;
    last_slice_of_pic | temporal_mvp | sao | collocated_from_l0
}

/// `VAEncSequenceParameterBufferHEVC`, matching [`sps`].
pub(crate) fn sequence(s: &Settings) -> SeqParamHevc {
    let t = &s.tools;
    SeqParamHevc {
        general_profile_idc: PROFILE_MAIN,
        general_level_idc: s.level_idc,
        general_tier_flag: 0,
        intra_period: s.idr_period,
        intra_idr_period: s.idr_period,
        ip_period: 1,
        // The mean; the rate-control buffer gives the peak.
        bits_per_second: s.rate.target_bits_per_second,
        pic_width_in_luma_samples: u16::try_from(s.coded_width).unwrap_or(u16::MAX),
        pic_height_in_luma_samples: u16::try_from(s.coded_height).unwrap_or(u16::MAX),
        seq_fields: seq_fields(t),
        log2_min_luma_coding_block_size_minus3: (t.log2_min_cb - 3) as u8,
        log2_diff_max_min_luma_coding_block_size: (t.log2_ctb - t.log2_min_cb) as u8,
        log2_min_transform_block_size_minus2: (t.log2_min_tb - 2) as u8,
        log2_diff_max_min_transform_block_size: (t.log2_max_tb - t.log2_min_tb) as u8,
        max_transform_hierarchy_depth_inter: t.depth_inter as u8,
        max_transform_hierarchy_depth_intra: t.depth_intra as u8,
        pcm_sample_bit_depth_luma_minus1: 0,
        pcm_sample_bit_depth_chroma_minus1: 0,
        log2_min_pcm_luma_coding_block_size_minus3: 0,
        log2_max_pcm_luma_coding_block_size_minus3: 0,
        vui_parameters_present_flag: 1,
        vui_fields: vui_fields(),
        aspect_ratio_idc: 1, // square samples
        sar_width: 1,
        sar_height: 1,
        vui_num_units_in_tick: s.tick.0,
        vui_time_scale: s.tick.1,
        min_spatial_segmentation_idc: 0,
        max_bytes_per_pic_denom: 0,
        max_bits_per_min_cu_denom: 0,
        scc_fields: 0,
        va_reserved: [0; 7],
    }
}

/// A reconstructed picture as libva refers to it.
fn va_picture(surface: Id, p: &Picture, flags: u32) -> PictureHevc {
    PictureHevc {
        picture_id: surface,
        pic_order_cnt: p.poc,
        flags,
        va_reserved: [0; 4],
    }
}

/// The reference a picture is predicted from: none for an IDR frame.
fn predicted_from<'a>(
    p: &Picture,
    reference: Option<(Id, &'a Picture)>,
) -> Option<(Id, &'a Picture)> {
    reference.filter(|_| !p.idr)
}

/// `VAEncPictureParameterBufferHEVC` for `p`, reconstructed into `recon` and predicted
/// from `reference` (its surface and picture) unless it is an IDR frame, written to
/// `coded_buf`. Matches [`pps`].
pub(crate) fn picture(
    s: &Settings,
    p: &Picture,
    recon: Id,
    reference: Option<(Id, &Picture)>,
    coded_buf: Id,
) -> PicParamHevc {
    let t = &s.tools;
    let mut reference_frames = [PictureHevc::INVALID; 15];
    let predicted = predicted_from(p, reference);
    if let Some((surface, r)) = predicted {
        reference_frames[0] = va_picture(surface, r, PICTURE_HEVC_RPS_ST_CURR_BEFORE);
    }
    PicParamHevc {
        decoded_curr_pic: va_picture(recon, p, 0),
        reference_frames,
        coded_buf,
        // The reference (list 0, entry 0) with temporal MVP; none without.
        collocated_ref_pic_index: if t.temporal_mvp { 0 } else { 0xFF },
        last_picture: 0,
        pic_init_qp: PIC_INIT_QP,
        diff_cu_qp_delta_depth: t.cu_qp_delta_depth as u8,
        pps_cb_qp_offset: 0,
        pps_cr_qp_offset: 0,
        num_tile_columns_minus1: 0,
        num_tile_rows_minus1: 0,
        column_width_minus1: [0; 19],
        row_height_minus1: [0; 21],
        log2_parallel_merge_level_minus2: 0,
        ctu_max_bitsize_allowed: 0,
        num_ref_idx_l0_default_active_minus1: 0,
        num_ref_idx_l1_default_active_minus1: 0,
        slice_pic_parameter_set_id: 0,
        nal_unit_type: if p.idr { IDR_W_RADL } else { TRAIL_R },
        pic_fields: pic_fields(t, !p.idr),
        hierarchical_level_plus1: 0,
        va_byte_reserved: 0,
        scc_fields: 0,
        va_reserved: [0; 15],
    }
}

/// The `slice_type` of `p`: I for an IDR frame or a picture with no reference, P
/// otherwise, or B on a driver that codes no P slices.
fn slice_type(t: &Tools, predicted: bool) -> u8 {
    match (predicted, t.gpb) {
        (false, _) => SLICE_I,
        (true, false) => SLICE_P,
        (true, true) => SLICE_B,
    }
}

/// `VAEncSliceParameterBufferHEVC`: one slice covering the picture, an I slice for an
/// IDR frame and a P slice predicted from `reference` otherwise (a B slice with the
/// reference in both lists on a driver that codes no P slices).
pub(crate) fn slice(
    s: &Settings,
    p: &Picture,
    reference: Option<(Id, &Picture)>,
) -> SliceParamHevc {
    let t = &s.tools;
    let mut ref_pic_list0 = [PictureHevc::INVALID; 15];
    let mut ref_pic_list1 = [PictureHevc::INVALID; 15];
    let predicted = predicted_from(p, reference);
    if let Some((surface, r)) = predicted {
        ref_pic_list0[0] = va_picture(surface, r, PICTURE_HEVC_RPS_ST_CURR_BEFORE);
        if t.gpb {
            ref_pic_list1[0] = ref_pic_list0[0];
        }
    }
    SliceParamHevc {
        slice_segment_address: 0,
        num_ctu_in_slice: s.ctus(),
        slice_type: slice_type(t, predicted.is_some()),
        slice_pic_parameter_set_id: 0,
        num_ref_idx_l0_active_minus1: 0,
        num_ref_idx_l1_active_minus1: 0,
        ref_pic_list0,
        ref_pic_list1,
        luma_log2_weight_denom: 0,
        delta_chroma_log2_weight_denom: 0,
        delta_luma_weight_l0: [0; 15],
        luma_offset_l0: [0; 15],
        delta_chroma_weight_l0: [[0; 2]; 15],
        chroma_offset_l0: [[0; 2]; 15],
        delta_luma_weight_l1: [0; 15],
        luma_offset_l1: [0; 15],
        delta_chroma_weight_l1: [[0; 2]; 15],
        chroma_offset_l1: [[0; 2]; 15],
        max_num_merge_cand: 5,
        slice_qp_delta: 0,
        slice_cb_qp_offset: 0,
        slice_cr_qp_offset: 0,
        slice_beta_offset_div2: 0,
        slice_tc_offset_div2: 0,
        slice_fields: slice_fields(t, predicted.is_some()),
        pred_weight_table_bit_offset: 0,
        pred_weight_table_bit_length: 0,
        va_reserved: [0; 6],
    }
}

/// The rate controller's settings, with a one-second window.
pub(crate) fn rate_control(s: &Settings) -> MiscParam<RateControlParam> {
    rate_control_buffer(s.rate.bits_per_second, s.rate.target_percentage)
}

/// The coded picture buffer: one second at the peak rate, starting three quarters full.
pub(crate) fn hrd(s: &Settings) -> MiscParam<HrdParam> {
    hrd_buffer(s.rate.bits_per_second)
}

/// The rate controller's frame rate.
pub(crate) fn frame_rate(s: &Settings) -> MiscParam<FrameRateParam> {
    frame_rate_buffer(s.rate.fps)
}

/// An Annex B NAL unit of `nal_unit_type`, layer 0 and temporal layer 0.
fn nal(nal_unit_type: u8, rbsp: &[u8]) -> Vec<u8> {
    // forbidden_zero_bit, nal_unit_type (6), nuh_layer_id (6), nuh_temporal_id_plus1 (3).
    annex_b(&[(nal_unit_type & 0x3F) << 1, 1], rbsp)
}

/// `profile_tier_level(1, 0)`: Main profile (compatible with Main 10), Main tier,
/// progressive frames.
fn profile_tier_level(w: &mut BitWriter, level_idc: u8) {
    w.bits(0, 2); // general_profile_space
    w.flag(false); // general_tier_flag: Main
    w.bits(u32::from(PROFILE_MAIN), 5); // general_profile_idc
    // general_profile_compatibility_flag[j]: Main (1) and Main 10 (2).
    w.bits(0x6000_0000, 32);
    w.flag(true); // general_progressive_source_flag
    w.flag(false); // general_interlaced_source_flag
    w.flag(true); // general_non_packed_constraint_flag
    w.flag(true); // general_frame_only_constraint_flag
    // general_reserved_zero_7bits, general_one_picture_only_constraint_flag and
    // general_reserved_zero_35bits (compatible with Main 10), then general_inbld_flag.
    w.bits(0, 32);
    w.bits(0, 12);
    w.bits(u32::from(level_idc), 8); // general_level_idc
}

/// The video parameter set, as an Annex B NAL unit (`nal_unit_type` 32).
pub(crate) fn vps(s: &Settings) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.bits(0, 4); // vps_video_parameter_set_id
    w.flag(true); // vps_base_layer_internal_flag
    w.flag(true); // vps_base_layer_available_flag
    w.bits(0, 6); // vps_max_layers_minus1
    w.bits(0, 3); // vps_max_sub_layers_minus1
    w.flag(true); // vps_temporal_id_nesting_flag
    w.bits(0xFFFF, 16); // vps_reserved_0xffff_16bits
    profile_tier_level(&mut w, s.level_idc);
    w.flag(false); // vps_sub_layer_ordering_info_present_flag
    w.ue(1); // vps_max_dec_pic_buffering_minus1: the reference and the picture
    w.ue(0); // vps_max_num_reorder_pics: no B-frames
    w.ue(0); // vps_max_latency_increase_plus1
    w.bits(0, 6); // vps_max_layer_id
    w.ue(0); // vps_num_layer_sets_minus1
    w.flag(true); // vps_timing_info_present_flag
    w.bits(s.tick.0, 32); // vps_num_units_in_tick
    w.bits(s.tick.1, 32); // vps_time_scale
    w.flag(true); // vps_poc_proportional_to_timing_flag
    w.ue(0); // vps_num_ticks_poc_diff_one_minus1
    w.ue(0); // vps_num_hrd_parameters
    w.flag(false); // vps_extension_flag
    w.trailing();
    nal(VPS_NUT, &w.into_bytes())
}

/// The sequence parameter set, as an Annex B NAL unit (`nal_unit_type` 33).
pub(crate) fn sps(s: &Settings) -> Vec<u8> {
    let t = &s.tools;
    let mut w = BitWriter::default();
    w.bits(0, 4); // sps_video_parameter_set_id
    w.bits(0, 3); // sps_max_sub_layers_minus1
    w.flag(true); // sps_temporal_id_nesting_flag
    profile_tier_level(&mut w, s.level_idc);
    w.ue(0); // sps_seq_parameter_set_id
    w.ue(1); // chroma_format_idc: 4:2:0
    w.ue(s.coded_width); // pic_width_in_luma_samples
    w.ue(s.coded_height); // pic_height_in_luma_samples
    let (crop_right, crop_bottom) = s.crop();
    let crop = crop_right != 0 || crop_bottom != 0;
    w.flag(crop); // conformance_window_flag
    if crop {
        w.ue(0);
        w.ue(crop_right);
        w.ue(0);
        w.ue(crop_bottom);
    }
    w.ue(0); // bit_depth_luma_minus8
    w.ue(0); // bit_depth_chroma_minus8
    w.ue(LOG2_MAX_POC_LSB - 4); // log2_max_pic_order_cnt_lsb_minus4
    w.flag(false); // sps_sub_layer_ordering_info_present_flag
    w.ue(1); // sps_max_dec_pic_buffering_minus1
    w.ue(0); // sps_max_num_reorder_pics
    w.ue(0); // sps_max_latency_increase_plus1
    w.ue(t.log2_min_cb - 3); // log2_min_luma_coding_block_size_minus3
    w.ue(t.log2_ctb - t.log2_min_cb); // log2_diff_max_min_luma_coding_block_size
    w.ue(t.log2_min_tb - 2); // log2_min_luma_transform_block_size_minus2
    w.ue(t.log2_max_tb - t.log2_min_tb); // log2_diff_max_min_luma_transform_block_size
    w.ue(t.depth_inter); // max_transform_hierarchy_depth_inter
    w.ue(t.depth_intra); // max_transform_hierarchy_depth_intra
    w.flag(false); // scaling_list_enabled_flag
    w.flag(t.amp); // amp_enabled_flag
    w.flag(t.sao); // sample_adaptive_offset_enabled_flag
    w.flag(false); // pcm_enabled_flag
    w.ue(0); // num_short_term_ref_pic_sets: each slice header carries its own
    w.flag(false); // long_term_ref_pics_present_flag
    w.flag(t.temporal_mvp); // sps_temporal_mvp_enabled_flag
    w.flag(false); // strong_intra_smoothing_enabled_flag
    w.flag(true); // vui_parameters_present_flag
    // vui_parameters()
    w.flag(true); // aspect_ratio_info_present_flag
    w.bits(1, 8); // aspect_ratio_idc: 1:1
    w.flag(false); // overscan_info_present_flag
    w.flag(true); // video_signal_type_present_flag
    w.bits(5, 3); // video_format: unspecified
    w.flag(false); // video_full_range_flag: limited
    w.flag(true); // colour_description_present_flag
    w.bits(1, 8); // colour_primaries: BT.709
    w.bits(1, 8); // transfer_characteristics: BT.709
    w.bits(1, 8); // matrix_coeffs: BT.709
    w.flag(false); // chroma_loc_info_present_flag: left-sited, the default
    w.flag(false); // neutral_chroma_indication_flag
    w.flag(false); // field_seq_flag
    w.flag(false); // frame_field_info_present_flag
    w.flag(false); // default_display_window_flag
    w.flag(true); // vui_timing_info_present_flag
    w.bits(s.tick.0, 32); // vui_num_units_in_tick
    w.bits(s.tick.1, 32); // vui_time_scale
    w.flag(true); // vui_poc_proportional_to_timing_flag
    w.ue(0); // vui_num_ticks_poc_diff_one_minus1
    w.flag(false); // vui_hrd_parameters_present_flag
    w.flag(true); // bitstream_restriction_flag
    w.flag(false); // tiles_fixed_structure_flag
    w.flag(true); // motion_vectors_over_pic_boundaries_flag
    w.flag(true); // restricted_ref_pic_lists_flag
    w.ue(0); // min_spatial_segmentation_idc
    w.ue(0); // max_bytes_per_pic_denom
    w.ue(0); // max_bits_per_min_cu_denom
    w.ue(LOG2_MAX_MV_LENGTH); // log2_max_mv_length_horizontal
    w.ue(LOG2_MAX_MV_LENGTH); // log2_max_mv_length_vertical
    w.flag(false); // sps_extension_present_flag
    w.trailing();
    nal(SPS_NUT, &w.into_bytes())
}

/// The picture parameter set, as an Annex B NAL unit (`nal_unit_type` 34).
pub(crate) fn pps(s: &Settings) -> Vec<u8> {
    let t = &s.tools;
    let mut w = BitWriter::default();
    w.ue(0); // pps_pic_parameter_set_id
    w.ue(0); // pps_seq_parameter_set_id
    w.flag(false); // dependent_slice_segments_enabled_flag
    w.flag(false); // output_flag_present_flag
    w.bits(0, 3); // num_extra_slice_header_bits
    w.flag(false); // sign_data_hiding_enabled_flag
    w.flag(false); // cabac_init_present_flag
    w.ue(0); // num_ref_idx_l0_default_active_minus1
    w.ue(0); // num_ref_idx_l1_default_active_minus1
    w.se(i32::from(PIC_INIT_QP) - 26); // init_qp_minus26
    w.flag(false); // constrained_intra_pred_flag
    w.flag(t.transform_skip); // transform_skip_enabled_flag
    w.flag(t.cu_qp_delta); // cu_qp_delta_enabled_flag
    if t.cu_qp_delta {
        w.ue(t.cu_qp_delta_depth); // diff_cu_qp_delta_depth
    }
    w.se(0); // pps_cb_qp_offset
    w.se(0); // pps_cr_qp_offset
    w.flag(false); // pps_slice_chroma_qp_offsets_present_flag
    w.flag(false); // weighted_pred_flag
    w.flag(false); // weighted_bipred_flag
    w.flag(false); // transquant_bypass_enabled_flag
    w.flag(false); // tiles_enabled_flag
    w.flag(false); // entropy_coding_sync_enabled_flag
    w.flag(false); // pps_loop_filter_across_slices_enabled_flag
    w.flag(false); // deblocking_filter_control_present_flag
    w.flag(false); // pps_scaling_list_data_present_flag
    w.flag(false); // lists_modification_present_flag
    w.ue(0); // log2_parallel_merge_level_minus2
    w.flag(false); // slice_segment_header_extension_present_flag
    w.flag(false); // pps_extension_present_flag
    w.trailing();
    nal(PPS_NUT, &w.into_bytes())
}

/// The VPS, SPS and PPS, sent together as the packed sequence header on IDR frames.
pub(crate) fn parameter_sets(s: &Settings) -> Vec<u8> {
    [vps(s), sps(s), pps(s)].concat()
}

/// The slice segment header of `p`'s one slice, as the start of an Annex B NAL unit
/// (the driver appends the slice data). Matches [`slice`]; it ends byte-aligned
/// (`byte_alignment()`).
pub(crate) fn slice_header(
    s: &Settings,
    p: &Picture,
    reference: Option<(Id, &Picture)>,
) -> Vec<u8> {
    let t = &s.tools;
    let predicted = predicted_from(p, reference);
    let kind = slice_type(t, predicted.is_some());
    let mut w = BitWriter::default();
    w.flag(true); // first_slice_segment_in_pic_flag
    if p.idr {
        w.flag(false); // no_output_of_prior_pics_flag
    }
    w.ue(0); // slice_pic_parameter_set_id
    w.ue(u32::from(kind)); // slice_type
    if !p.idr {
        let lsb_mask = (1u32 << LOG2_MAX_POC_LSB) - 1;
        w.bits(p.poc as u32 & lsb_mask, LOG2_MAX_POC_LSB); // slice_pic_order_cnt_lsb
        w.flag(false); // short_term_ref_pic_set_sps_flag
        // st_ref_pic_set(0): the reference, if any, before the picture.
        match predicted {
            Some((_, r)) => {
                w.ue(1); // num_negative_pics
                w.ue(0); // num_positive_pics
                w.ue((p.poc - r.poc - 1).max(0) as u32); // delta_poc_s0_minus1
                w.flag(true); // used_by_curr_pic_s0_flag
            }
            None => {
                w.ue(0); // num_negative_pics
                w.ue(0); // num_positive_pics
            }
        }
        if t.temporal_mvp {
            w.flag(predicted.is_some()); // slice_temporal_mvp_enabled_flag
        }
    }
    if t.sao {
        w.flag(true); // slice_sao_luma_flag
        w.flag(true); // slice_sao_chroma_flag
    }
    if predicted.is_some() {
        w.flag(false); // num_ref_idx_active_override_flag
        if kind == SLICE_B {
            w.flag(false); // mvd_l1_zero_flag
        }
        if t.temporal_mvp && kind == SLICE_B {
            w.flag(true); // collocated_from_l0_flag
        }
        w.ue(0); // five_minus_max_num_merge_cand
    }
    w.se(0); // slice_qp_delta
    w.trailing(); // byte_alignment(): a one, then zeros
    nal(if p.idr { IDR_W_RADL } else { TRAIL_R }, &w.into_bytes())
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{Codec, FrameSize, PixelFormat, Timebase};
    use jstream_platform::{HardwarePreference, PlatformError, RateControl, VideoEncoderConfig};

    use super::{
        Gop, Picture, Settings, Tools, check, frame_rate, hrd, level, parameter_sets, pic_fields,
        picture, pps, rate_control, seq_fields, sequence, slice, slice_fields, slice_header, sps,
        vps, vui_fields,
    };
    use crate::vaapi::ffi::{ATTRIB_NOT_SUPPORTED, INVALID_ID, PICTURE_HEVC_INVALID, misc};
    use crate::vaapi::h264::RcMode;

    fn config() -> VideoEncoderConfig {
        VideoEncoderConfig {
            codec: Codec::Hevc,
            size: FrameSize::new(1920, 1080).expect("size"),
            input_format: PixelFormat::Nv12,
            frame_period: Timebase::new(1001, 30_000).expect("29.97"),
            rate_control: RateControl::Cbr { kbps: 6000 },
            keyframe_interval: 60,
            hardware: HardwarePreference::Prefer,
        }
    }

    fn settings() -> Settings {
        check(&config(), Tools::DEFAULT).expect("valid")
    }

    /// The tools of a driver advertising AMP, SAO (required), PCM, temporal MVP and CU
    /// QP deltas, 64x64 coding trees down to 8x8 coding blocks, transforms of 4x4 to
    /// 32x32 and transform depths of 2 (inter) and 1 (intra): the values a C program
    /// built against libva 2.20.0's headers printed for those fields (ffi's module docs).
    fn advertised() -> Tools {
        Tools::from_attributes(4_195_728, 18_631, ATTRIB_NOT_SUPPORTED)
    }

    #[test]
    fn derives_the_session_settings() {
        let s = settings();
        // 1080 is coded at 1088 by default: 4 units of 2 rows are cropped.
        assert_eq!((s.coded_width, s.coded_height), (1920, 1088));
        assert_eq!(s.crop(), (0, 4));
        // 60 x 34 coding trees of 32x32.
        assert_eq!(s.ctus(), 60 * 34);
        assert_eq!(s.rate.fps, (30_000, 1001));
        assert_eq!(s.tick, (1001, 30_000));
        assert_eq!(
            (s.rate.rc, s.rate.bits_per_second, s.rate.target_percentage),
            (RcMode::Cbr, 6_000_000, 100)
        );
        // Level 4.
        assert_eq!(s.level_idc, 120);
        assert_eq!(s.idr_period, 60);
        // A driver with 8x8 coding blocks codes 1080 rows as they are, in 64x64 trees.
        let s = check(&config(), advertised()).expect("valid");
        assert_eq!((s.coded_width, s.coded_height), (1920, 1080));
        assert_eq!(s.crop(), (0, 0));
        assert_eq!(s.ctus(), 30 * 17);
        let mut c = config();
        c.rate_control = RateControl::Vbr {
            kbps: 4000,
            max_kbps: 8000,
        };
        c.frame_period = Timebase::new(2, 120).expect("60");
        let s = check(&c, Tools::DEFAULT).expect("valid");
        assert_eq!(
            (s.rate.rc, s.rate.bits_per_second, s.rate.target_percentage),
            (RcMode::Vbr, 8_000_000, 50)
        );
        assert_eq!(sequence(&s).bits_per_second, 4_000_000);
        assert_eq!((s.rate.fps, s.tick), ((60, 1), (1, 60)));
        // 1080p60 needs level 4.1.
        assert_eq!(s.level_idc, 123);
    }

    #[test]
    fn takes_the_compositors_rgb_output() {
        for format in [PixelFormat::Rgba8, PixelFormat::Bgra8] {
            let mut c = config();
            c.input_format = format;
            assert_eq!(check(&c, Tools::DEFAULT).expect("accepted"), settings());
        }
    }

    #[test]
    fn refuses_what_the_session_cant_encode() {
        let refused = |c: VideoEncoderConfig| {
            matches!(
                check(&c, Tools::DEFAULT),
                Err(PlatformError::Unsupported { .. })
            )
        };
        let mut c = config();
        c.codec = Codec::H264;
        assert!(refused(c));
        let mut c = config();
        c.input_format = PixelFormat::I420;
        assert!(refused(c));
        let mut c = config();
        c.size = FrameSize::new(1920, 1081).expect("size");
        assert!(refused(c));
        let mut c = config();
        c.keyframe_interval = 0;
        assert!(refused(c));
        let mut c = config();
        c.keyframe_interval = (1 << 30) + 1;
        assert!(refused(c));
        let mut c = config();
        c.rate_control = RateControl::Vbr {
            kbps: 4000,
            max_kbps: 3000,
        };
        assert!(refused(c));
        let mut c = config();
        c.rate_control = RateControl::Cbr { kbps: 0 };
        assert!(refused(c));
        // Beyond level 6.2: 16K, and more than 240 Mb/s.
        let mut c = config();
        c.size = FrameSize::new(15_360, 8_640).expect("size");
        assert!(refused(c));
        let mut c = config();
        c.rate_control = RateControl::Cbr { kbps: 240_001 };
        assert!(refused(c));
    }

    #[test]
    fn picks_the_lowest_level_that_fits() {
        let fps = |n| (n, 1);
        // 720p30 is level 3.1, 1080p30 4, 1080p60 4.1, 4K30 5, 4K60 5.1, 8K30 6.
        assert_eq!(level(1280, 720, fps(30), 4_000_000), Some(93));
        assert_eq!(level(1920, 1088, fps(30), 6_000_000), Some(120));
        assert_eq!(level(1920, 1088, fps(60), 6_000_000), Some(123));
        assert_eq!(level(3840, 2160, fps(30), 20_000_000), Some(150));
        assert_eq!(level(3840, 2160, fps(60), 20_000_000), Some(153));
        assert_eq!(level(7680, 4320, fps(30), 40_000_000), Some(180));
        // The bit rate raises it: 1080p30 above 12 Mb/s needs level 4.1.
        assert_eq!(level(1920, 1088, fps(30), 12_000_000), Some(120));
        assert_eq!(level(1920, 1088, fps(30), 12_000_001), Some(123));
        // Too wide for level 4 (sqrt(8 * 2 228 224) = 4222 samples), not for 5.
        assert_eq!(level(4224, 16, fps(1), 1_000), Some(150));
        assert_eq!(level(15_360, 8_640, fps(30), 1_000), None);
    }

    #[test]
    fn tools_follow_the_drivers_attributes() {
        assert_eq!(
            Tools::from_attributes(
                ATTRIB_NOT_SUPPORTED,
                ATTRIB_NOT_SUPPORTED,
                ATTRIB_NOT_SUPPORTED
            ),
            Tools::DEFAULT
        );
        let t = advertised();
        assert_eq!(
            (t.log2_ctb, t.log2_min_cb, t.log2_min_tb, t.log2_max_tb),
            (6, 3, 2, 5)
        );
        assert_eq!((t.depth_inter, t.depth_intra, t.alignment), (2, 1, 8));
        // SAO is used whether supported or required; PCM never.
        assert!(t.amp && t.sao && t.temporal_mvp && t.cu_qp_delta);
        assert!(!t.transform_skip && !t.gpb);
        // CU QP deltas down to the 8x8 coding blocks.
        assert_eq!(t.cu_qp_delta_depth, 3);
        // A driver that advertises no tools gets none of the optional ones.
        let none = Tools::from_attributes(0, ATTRIB_NOT_SUPPORTED, ATTRIB_NOT_SUPPORTED);
        assert!(!none.amp && !none.sao && !none.temporal_mvp && !none.cu_qp_delta);
        // VA_PREDICTION_DIRECTION_PREVIOUS | VA_PREDICTION_DIRECTION_BI_NOT_EMPTY.
        assert!(Tools::from_attributes(ATTRIB_NOT_SUPPORTED, ATTRIB_NOT_SUPPORTED, 0x5).gpb);
        assert!(!Tools::from_attributes(ATTRIB_NOT_SUPPORTED, ATTRIB_NOT_SUPPORTED, 0x1).gpb);
        // Block sizes outside what HEVC allows are brought inside it: 8x8 coding trees
        // become 16x16, and the transforms stay below the coding blocks.
        let small = Tools::from_attributes(ATTRIB_NOT_SUPPORTED, 0, ATTRIB_NOT_SUPPORTED);
        assert_eq!(
            (
                small.log2_ctb,
                small.log2_min_cb,
                small.log2_min_tb,
                small.log2_max_tb
            ),
            (4, 3, 2, 2)
        );
    }

    #[test]
    fn bitfields_match_libva() {
        // The values a C program built against libva 2.20.0's headers printed with the
        // same fields set (ffi's module docs).
        let all = Tools {
            sao: true,
            temporal_mvp: true,
            ..Tools::DEFAULT
        };
        assert_eq!(seq_fields(&all), 38_913);
        assert_eq!(seq_fields(&Tools::DEFAULT), 2049);
        assert_eq!(vui_fields(), 126_937);
        assert_eq!(pic_fields(&Tools::DEFAULT, false), 531);
        let skip = Tools {
            transform_skip: true,
            ..Tools::DEFAULT
        };
        assert_eq!(pic_fields(&skip, true), 788);
        assert_eq!(slice_fields(&all, false), 97);
        assert_eq!(slice_fields(&all, true), 8305);
    }

    #[test]
    fn the_sequence_buffer_matches_the_sps() {
        let s = settings();
        let seq = sequence(&s);
        assert_eq!(
            (
                seq.general_profile_idc,
                seq.general_level_idc,
                seq.general_tier_flag
            ),
            (1, 120, 0)
        );
        assert_eq!(
            (seq.intra_period, seq.intra_idr_period, seq.ip_period),
            (60, 60, 1)
        );
        assert_eq!(seq.bits_per_second, 6_000_000);
        assert_eq!(
            (
                seq.pic_width_in_luma_samples,
                seq.pic_height_in_luma_samples
            ),
            (1920, 1088)
        );
        assert_eq!(seq.seq_fields, seq_fields(&Tools::DEFAULT));
        assert_eq!(
            (
                seq.log2_min_luma_coding_block_size_minus3,
                seq.log2_diff_max_min_luma_coding_block_size,
                seq.log2_min_transform_block_size_minus2,
                seq.log2_diff_max_min_transform_block_size,
                seq.max_transform_hierarchy_depth_inter,
                seq.max_transform_hierarchy_depth_intra,
            ),
            (0, 2, 0, 3, 3, 3)
        );
        assert_eq!(
            (seq.vui_num_units_in_tick, seq.vui_time_scale),
            (1001, 30_000)
        );
        assert_eq!(
            (seq.vui_parameters_present_flag, seq.aspect_ratio_idc),
            (1, 1)
        );
    }

    #[test]
    fn gop_makes_idr_frames_on_cadence_and_on_request() {
        let mut c = config();
        c.keyframe_interval = 3;
        let s = check(&c, Tools::DEFAULT).expect("valid");
        let mut gop = Gop::new(&s);
        let mut pics: Vec<Picture> = (0..4).map(|_| gop.next_picture()).collect();
        // Several requests before a frame ask for one IDR, and restart the cadence.
        gop.force_idr();
        gop.force_idr();
        pics.extend((0..4).map(|_| gop.next_picture()));
        let summary: Vec<_> = pics.iter().map(|p| (p.idr, p.poc)).collect();
        assert_eq!(
            summary,
            [
                (true, 0),
                (false, 1),
                (false, 2),
                (true, 0),
                (true, 0),
                (false, 1),
                (false, 2),
                (true, 0),
            ]
        );
    }

    #[test]
    fn picture_and_slice_buffers_reference_the_previous_frame() {
        let s = check(&config(), advertised()).expect("valid");
        let mut gop = Gop::new(&s);
        let idr = gop.next_picture();
        let p = gop.next_picture();
        let pic = picture(&s, &idr, 10, None, 99);
        assert_eq!(
            (pic.decoded_curr_pic.picture_id, pic.decoded_curr_pic.flags),
            (10, 0)
        );
        assert_eq!(
            (pic.coded_buf, pic.pic_init_qp, pic.nal_unit_type),
            (99, 26, 19)
        );
        assert_eq!(
            (pic.collocated_ref_pic_index, pic.diff_cu_qp_delta_depth),
            (0, 3)
        );
        assert!(
            pic.reference_frames
                .iter()
                .all(|r| r.picture_id == INVALID_ID && r.flags == PICTURE_HEVC_INVALID)
        );
        let sl = slice(&s, &idr, None);
        assert_eq!((sl.slice_type, sl.num_ctu_in_slice), (2, 30 * 17));
        assert_eq!(sl.max_num_merge_cand, 5);
        // An IDR frame ignores the reference it is given.
        assert_eq!(slice(&s, &idr, Some((10, &idr))).slice_type, 2);

        let pic = picture(&s, &p, 11, Some((10, &idr)), 98);
        assert_eq!(
            (
                pic.decoded_curr_pic.picture_id,
                pic.decoded_curr_pic.pic_order_cnt
            ),
            (11, 1)
        );
        assert_eq!(pic.nal_unit_type, 1);
        assert_eq!(
            (
                pic.reference_frames[0].picture_id,
                pic.reference_frames[0].flags
            ),
            (10, 0x10)
        );
        assert_eq!(pic.reference_frames[1].picture_id, INVALID_ID);
        assert_eq!(pic.pic_fields & 1, 0);
        let sl = slice(&s, &p, Some((10, &idr)));
        assert_eq!(sl.slice_type, 1);
        assert_eq!(
            (
                sl.ref_pic_list0[0].picture_id,
                sl.ref_pic_list0[0].pic_order_cnt
            ),
            (10, 0)
        );
        assert_eq!(sl.ref_pic_list0[1].picture_id, INVALID_ID);
        assert!(sl.ref_pic_list1.iter().all(|r| r.picture_id == INVALID_ID));
        // A driver with no P slices gets a B slice with the reference in both lists.
        let gpb = Settings {
            tools: Tools {
                gpb: true,
                ..s.tools
            },
            ..s
        };
        let sl = slice(&gpb, &p, Some((10, &idr)));
        assert_eq!(sl.slice_type, 0);
        assert_eq!(sl.ref_pic_list1[0], sl.ref_pic_list0[0]);
        assert_eq!(sl.ref_pic_list1[1].picture_id, INVALID_ID);
    }

    #[test]
    fn rate_control_buffers_follow_the_config() {
        let s = settings();
        let rc = rate_control(&s);
        assert_eq!(rc.kind, misc::RATE_CONTROL);
        assert_eq!(
            (
                rc.data.bits_per_second,
                rc.data.target_percentage,
                rc.data.window_size
            ),
            (6_000_000, 100, 1000)
        );
        let h = hrd(&s);
        assert_eq!(
            (h.data.buffer_size, h.data.initial_buffer_fullness),
            (6_000_000, 4_500_000)
        );
        assert_eq!(frame_rate(&s).data.framerate, (1001 << 16) | 30_000);
    }

    #[test]
    fn parameter_sets_for_1080p_main() {
        let s = settings();
        // FFmpeg's trace_headers parses these as intended (H.265 7.3.2.1, 7.3.2.2,
        // 7.3.2.3.1, E.2.1): Main profile (Main 10 compatible), Main tier, level 4;
        // 1920x1088 cropped by 4 (8 rows) at the bottom; coding blocks of 8x8 to 32x32,
        // transforms of 4x4 to 32x32 at depth 3; AMP, no SAO or temporal MVP; square
        // samples, BT.709 limited range, 1001/30000 ticks; no reordering; CU QP deltas.
        assert_eq!(
            vps(&s),
            [
                0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0x0C, 0x01, 0xFF, 0xFF, 0x01, 0x60, 0x00, 0x00,
                0x03, 0x00, 0xB0, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x78, 0x2C, 0x0C, 0x00,
                0x00, 0x0F, 0xA4, 0x00, 0x01, 0xD4, 0xC3, 0xA0,
            ]
        );
        assert_eq!(
            sps(&s),
            [
                0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0xB0,
                0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x78, 0xA0, 0x03, 0xC0, 0x80, 0x11, 0x07,
                0xCB, 0x89, 0x2E, 0xE4, 0x21, 0x12, 0x30, 0x16, 0xA0, 0x20, 0x20, 0x20, 0x80, 0x00,
                0x01, 0xF4, 0x80, 0x00, 0x3A, 0x98, 0x6B, 0xE1, 0x00, 0x82,
            ]
        );
        assert_eq!(
            pps(&s),
            [0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xC0, 0x73, 0xC0, 0x09]
        );
        assert_eq!(parameter_sets(&s), [vps(&s), sps(&s), pps(&s)].concat());
        // With the advertised tools: 1920x1080 uncropped, 64x64 coding trees down to
        // 8x8, transform depths 2 and 1, SAO and temporal MVP; CU QP deltas at depth 3.
        let a = check(&config(), advertised()).expect("valid");
        assert_eq!(
            sps(&a),
            [
                0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0xB0,
                0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x78, 0xA0, 0x03, 0xC0, 0x80, 0x10, 0xE5,
                0x89, 0x2E, 0x49, 0x1A, 0x6A, 0xC0, 0x5A, 0x80, 0x80, 0x80, 0x82, 0x00, 0x00, 0x07,
                0xD2, 0x00, 0x00, 0xEA, 0x61, 0xAF, 0x84, 0x02, 0x08,
            ]
        );
        assert_eq!(
            pps(&a),
            [
                0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xC0, 0x72, 0x4C, 0x00, 0x90
            ]
        );
    }

    #[test]
    fn slice_headers_for_each_kind_of_picture() {
        let s = check(&config(), advertised()).expect("valid");
        let mut gop = Gop::new(&s);
        let idr = gop.next_picture();
        let p = gop.next_picture();
        // FFmpeg's trace_headers parses these after the parameter sets, up to their
        // byte alignment (H.265 7.3.6.1): an IDR_W_RADL I slice with SAO; a TRAIL_R P
        // slice at POC 1 predicted from POC 0 with temporal MVP and SAO; on a driver
        // with no P slices, the same picture as a B slice, collocated from list 0.
        assert_eq!(
            slice_header(&s, &idr, None),
            [0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xAF, 0xC0]
        );
        assert_eq!(
            slice_header(&s, &p, Some((10, &idr))),
            [0x00, 0x00, 0x00, 0x01, 0x02, 0x01, 0xD0, 0x00, 0x97, 0xEE]
        );
        let gpb = Settings {
            tools: Tools {
                gpb: true,
                ..s.tools
            },
            ..s
        };
        assert_eq!(
            slice_header(&gpb, &p, Some((10, &idr))),
            [0x00, 0x00, 0x00, 0x01, 0x02, 0x01, 0xE0, 0x02, 0x5F, 0x9E]
        );
        // With the default tools (no SAO), the IDR slice header fits one byte.
        let d = settings();
        assert_eq!(
            slice_header(&d, &idr, None),
            [0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xAF]
        );
        assert_eq!(
            slice_header(&d, &p, Some((10, &idr))),
            [0x00, 0x00, 0x00, 0x01, 0x02, 0x01, 0xD0, 0x00, 0x97, 0x70]
        );
    }
}
