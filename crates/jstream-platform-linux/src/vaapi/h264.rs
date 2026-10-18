//! What a VA-API H.264 session is configured with, and the parameter buffers and packed
//! headers it submits for each picture (ADR-0060 §6).
//!
//! The stream is I and P frames only, one reference frame and one slice per picture, so
//! packets come out in presentation order with `dts == pts`. Every keyframe is an IDR
//! frame, every `keyframe_interval` frames or when one is requested, and each carries the
//! SPS and PPS so a viewer can join there. Picture order counts are derived from
//! `frame_num` (`pic_order_cnt_type` 2), which needs no B-frames. The SPS signals BT.709
//! limited range, what the engine's frames are in, and the frame rate.
//!
//! Platform-independent: building the buffers needs no driver, so it is tested
//! everywhere.

use jstream_platform::media::{Codec, PixelFormat};
use jstream_platform::{PlatformError, RateControl, VideoEncoderConfig};

use super::H264Profile;
use super::ffi::{
    FrameRateParam, HrdParam, INVALID_ID, Id, MiscParam, PICTURE_H264_SHORT_TERM_REFERENCE,
    PicParamH264, PictureH264, RC_CBR, RC_VBR, RateControlParam, SeqParamH264, SliceParamH264,
    misc,
};

/// The largest IDR period: picture order counts are twice the frames since the last IDR
/// and must fit an `i32`.
const MAX_IDR_PERIOD: u32 = 1 << 30;
/// `pic_init_qp`: the PPS's `pic_init_qp_minus26` is 0.
const PIC_INIT_QP: u8 = 26;
/// `slice_type` values (H.264 Table 7-6, as VA-API takes them).
const SLICE_P: u8 = 0;
const SLICE_I: u8 = 2;

/// How the rate controller spends bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RcMode {
    /// `VA_RC_CBR`.
    Cbr,
    /// `VA_RC_VBR`.
    Vbr,
}

impl RcMode {
    /// The `VAConfigAttribRateControl` bit.
    pub(crate) fn va(self) -> u32 {
        match self {
            Self::Cbr => RC_CBR,
            Self::Vbr => RC_VBR,
        }
    }
}

/// A checked configuration and everything derived from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    /// The frame size, even in both dimensions.
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// The coded size in 16x16 macroblocks; the SPS crops the excess.
    pub(crate) width_mbs: u32,
    pub(crate) height_mbs: u32,
    pub(crate) profile: H264Profile,
    pub(crate) level_idc: u8,
    pub(crate) rc: RcMode,
    /// The peak rate for VBR and the rate for CBR, in bits per second.
    pub(crate) bits_per_second: u32,
    /// The mean rate, in bits per second (the peak for CBR).
    pub(crate) target_bits_per_second: u32,
    /// The mean as a percentage of the peak (100 for CBR).
    pub(crate) target_percentage: u32,
    /// Frames per second as a reduced fraction, numerator first.
    pub(crate) fps: (u32, u32),
    /// The SPS's `num_units_in_tick` and `time_scale`: two ticks per frame.
    pub(crate) tick: (u32, u32),
    /// Frames from one IDR frame to the next.
    pub(crate) idr_period: u32,
    /// `log2_max_frame_num_minus4 + 4`.
    pub(crate) log2_max_frame_num: u32,
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// One row of H.264 Table A-1: `level_idc`, MaxMBPS, MaxFS, MaxDpbMbs and MaxBR (in
/// units of the profile's `cpbBrVclFactor` bits per second). Level 1b is skipped.
const LEVELS: [(u8, u64, u64, u64, u64); 19] = [
    (10, 1_485, 99, 396, 64),
    (11, 3_000, 396, 900, 192),
    (12, 6_000, 396, 2_376, 384),
    (13, 11_880, 396, 2_376, 768),
    (20, 11_880, 396, 2_376, 2_000),
    (21, 19_800, 792, 4_752, 4_000),
    (22, 20_250, 1_620, 8_100, 4_000),
    (30, 40_500, 1_620, 8_100, 10_000),
    (31, 108_000, 3_600, 18_000, 14_000),
    (32, 216_000, 5_120, 20_480, 20_000),
    (40, 245_760, 8_192, 32_768, 20_000),
    (41, 245_760, 8_192, 32_768, 50_000),
    (42, 522_240, 8_704, 34_816, 50_000),
    (50, 589_824, 22_080, 110_400, 135_000),
    (51, 983_040, 36_864, 184_320, 240_000),
    (52, 2_073_600, 36_864, 184_320, 240_000),
    (60, 4_177_920, 139_264, 696_320, 240_000),
    (61, 8_355_840, 139_264, 696_320, 480_000),
    (62, 16_711_680, 139_264, 696_320, 800_000),
];

/// The lowest level that holds `width_mbs` x `height_mbs` at `fps` with one reference
/// frame and a peak of `bps`, or `None` above level 6.2.
pub(crate) fn level(
    profile: H264Profile,
    width_mbs: u32,
    height_mbs: u32,
    fps: (u32, u32),
    bps: u32,
) -> Option<u8> {
    let (w, h) = (u64::from(width_mbs), u64::from(height_mbs));
    let fs = w * h;
    let (num, den) = (u64::from(fps.0), u64::from(fps.1.max(1)));
    let mbps = (fs * num).div_ceil(den);
    // cpbBrVclFactor (Table A-2): 1250 for High, 1000 for Main and Baseline.
    let factor = match profile {
        H264Profile::High => 1_250,
        H264Profile::Main | H264Profile::ConstrainedBaseline => 1_000,
    };
    LEVELS
        .iter()
        .find(|&&(_, max_mbps, max_fs, max_dpb_mbs, max_br)| {
            mbps <= max_mbps
                && fs <= max_fs
                // A-3.1 (f): neither dimension exceeds sqrt(8 * MaxFS) macroblocks.
                && w * w <= 8 * max_fs
                && h * h <= 8 * max_fs
                && max_dpb_mbs >= fs
                && u64::from(bps) <= max_br * factor
        })
        .map(|&(idc, ..)| idc)
}

/// Checks a configuration and derives the session's settings, for an encoder that
/// encodes `profile`.
///
/// # Errors
/// [`PlatformError::Unsupported`] for another codec, an input other than NV12, RGBA or
/// BGRA, odd dimensions, a keyframe interval of 0 or above 2^30, a zero bit rate, a VBR
/// peak below its mean, a frame period with a zero numerator, and a size, frame rate or
/// bit rate above level 6.2.
pub(crate) fn check(
    config: &VideoEncoderConfig,
    profile: H264Profile,
) -> Result<Settings, PlatformError> {
    let unsupported = |what: String| Err(PlatformError::Unsupported { what });
    if config.codec != Codec::H264 {
        return unsupported(format!("{:?} from the H.264 encoder", config.codec));
    }
    if !matches!(
        config.input_format,
        PixelFormat::Nv12 | PixelFormat::Rgba8 | PixelFormat::Bgra8
    ) {
        return unsupported(format!(
            "H.264 from {:?} (the encoder takes NV12, RGBA or BGRA)",
            config.input_format
        ));
    }
    let (width, height) = (config.size.width(), config.size.height());
    if width % 2 != 0 || height % 2 != 0 {
        return unsupported(format!("H.264 at an odd size, {width}x{height}"));
    }
    if config.keyframe_interval == 0 || config.keyframe_interval > MAX_IDR_PERIOD {
        return unsupported(format!(
            "a keyframe interval of {}",
            config.keyframe_interval
        ));
    }
    let (rc, kbps, peak_kbps) = match config.rate_control {
        RateControl::Cbr { kbps } => (RcMode::Cbr, kbps, kbps),
        RateControl::Vbr { kbps, max_kbps } => (RcMode::Vbr, kbps, max_kbps),
        other => return unsupported(format!("rate control {other:?}")),
    };
    if kbps == 0 || peak_kbps < kbps {
        return unsupported(format!("rate control {:?}", config.rate_control));
    }
    let bits_per_second = peak_kbps.saturating_mul(1000);
    let target_bits_per_second = kbps.saturating_mul(1000);
    let target_percentage =
        u32::try_from((u64::from(kbps) * 100 + u64::from(peak_kbps) / 2) / u64::from(peak_kbps))
            .unwrap_or(100)
            .clamp(1, 100);
    // A frame lasts num/den seconds: den/num frames per second, and two ticks of
    // num/(2 den) seconds.
    let period = config.frame_period;
    let (pn, pd) = (u64::from(period.num()), u64::from(period.den()));
    if pn == 0 {
        return unsupported("a frame period of 0".into());
    }
    let g = gcd(pn, pd);
    let (pn, pd) = (pn / g, pd / g);
    let fps = (
        u32::try_from(pd).unwrap_or(u32::MAX),
        u32::try_from(pn).unwrap_or(u32::MAX),
    );
    let Ok(time_scale) = u32::try_from(2 * pd) else {
        return unsupported(format!("a frame period of {}/{}", pn, pd));
    };
    let tick = (fps.1, time_scale);
    let (width_mbs, height_mbs) = (width.div_ceil(16), height.div_ceil(16));
    let Some(level_idc) = level(profile, width_mbs, height_mbs, fps, bits_per_second) else {
        return unsupported(format!(
            "H.264 at {width}x{height}, {}/{} fps and {peak_kbps} kb/s (above level 6.2)",
            fps.0, fps.1
        ));
    };
    // frame_num counts frames since the IDR, wrapping at 2^log2_max_frame_num (4 to 16).
    let log2_max_frame_num =
        (u32::BITS - (config.keyframe_interval - 1).leading_zeros()).clamp(4, 16);
    Ok(Settings {
        width,
        height,
        width_mbs,
        height_mbs,
        profile,
        level_idc,
        rc,
        bits_per_second,
        target_bits_per_second,
        target_percentage,
        fps,
        tick,
        idr_period: config.keyframe_interval,
        log2_max_frame_num,
    })
}

impl Settings {
    /// `profile_idc`.
    pub(crate) fn profile_idc(&self) -> u8 {
        match self.profile {
            H264Profile::ConstrainedBaseline => 66,
            H264Profile::Main => 77,
            H264Profile::High => 100,
        }
    }

    /// CABAC, on every profile but Constrained Baseline.
    pub(crate) fn cabac(&self) -> bool {
        self.profile != H264Profile::ConstrainedBaseline
    }

    /// The 8x8 transform, High only.
    pub(crate) fn transform_8x8(&self) -> bool {
        self.profile == H264Profile::High
    }

    /// The crop from the macroblock-aligned size: right and bottom, in the SPS's units
    /// of two luma samples (4:2:0, frames only).
    pub(crate) fn crop(&self) -> (u32, u32) {
        (
            (self.width_mbs * 16 - self.width) / 2,
            (self.height_mbs * 16 - self.height) / 2,
        )
    }

    /// Macroblocks in a picture.
    pub(crate) fn macroblocks(&self) -> u32 {
        self.width_mbs * self.height_mbs
    }

    /// The largest coded picture to expect: an uncompressed one at the coded size,
    /// plus room for the headers.
    pub(crate) fn coded_buffer_size(&self) -> u32 {
        (self.macroblocks() * 256 * 3 / 2).saturating_add(64 * 1024)
    }
}

/// One picture of the stream: what the GOP made of the next frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Picture {
    pub(crate) idr: bool,
    /// `frame_num`: frames since the IDR, wrapped.
    pub(crate) frame_num: u32,
    /// The picture order count, twice the frames since the IDR (`pic_order_cnt_type` 2).
    pub(crate) poc: i32,
    /// `idr_pic_id` of the IDR frame this picture follows (or is).
    pub(crate) idr_pic_id: u16,
}

/// Decides each frame's picture type: an IDR every `idr_period` frames, or at the next
/// frame after a request, which also restarts the cadence. P frames otherwise.
#[derive(Clone, Debug)]
pub(crate) struct Gop {
    idr_period: u32,
    max_frame_num: u32,
    /// Frames since the last IDR, which is the next frame's index in its GOP.
    next: u32,
    idr_pic_id: u16,
    started: bool,
    forced: bool,
}

impl Gop {
    pub(crate) fn new(s: &Settings) -> Self {
        Self {
            idr_period: s.idr_period,
            max_frame_num: 1 << s.log2_max_frame_num,
            next: 0,
            idr_pic_id: 0,
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
            if self.started {
                self.idr_pic_id = self.idr_pic_id.wrapping_add(1);
            }
            self.started = true;
            self.forced = false;
            self.next = 0;
        }
        let n = self.next;
        self.next += 1;
        Picture {
            idr,
            frame_num: n % self.max_frame_num,
            // n < idr_period <= 2^30, so 2n fits.
            poc: i32::try_from(2 * u64::from(n)).unwrap_or(i32::MAX),
            idr_pic_id: self.idr_pic_id,
        }
    }
}

/// The `seq_fields` union of `VAEncSequenceParameterBufferH264`.
pub(crate) fn seq_fields(log2_max_frame_num: u32) -> u32 {
    let chroma_format_idc = 1; // 4:2:0, bits 0-1
    let frame_mbs_only = 1 << 2;
    let direct_8x8_inference = 1 << 5;
    let log2_max_frame_num_minus4 = (log2_max_frame_num - 4) << 6; // bits 6-9
    let pic_order_cnt_type = 2 << 10; // bits 10-11
    chroma_format_idc
        | frame_mbs_only
        | direct_8x8_inference
        | log2_max_frame_num_minus4
        | pic_order_cnt_type
}

/// `log2_max_mv_length_horizontal` and `_vertical`: no restriction.
const LOG2_MAX_MV_LENGTH: u32 = 15;

/// The `vui_fields` union of `VAEncSequenceParameterBufferH264`.
pub(crate) fn vui_fields() -> u32 {
    let aspect_ratio_info_present = 1;
    let timing_info_present = 1 << 1;
    let bitstream_restriction = 1 << 2;
    let mv_h = LOG2_MAX_MV_LENGTH << 3; // bits 3-7
    let mv_v = LOG2_MAX_MV_LENGTH << 8; // bits 8-12
    let fixed_frame_rate = 1 << 13;
    let mvs_over_pic_boundaries = 1 << 15;
    aspect_ratio_info_present
        | timing_info_present
        | bitstream_restriction
        | mv_h
        | mv_v
        | fixed_frame_rate
        | mvs_over_pic_boundaries
}

/// The `pic_fields` union of `VAEncPictureParameterBufferH264`. Every picture is a
/// reference.
pub(crate) fn pic_fields(s: &Settings, idr: bool) -> u32 {
    let idr_pic = u32::from(idr);
    let reference_pic = 1 << 1; // bits 1-2
    let entropy = u32::from(s.cabac()) << 3;
    let transform_8x8 = u32::from(s.transform_8x8()) << 8;
    let deblocking_filter_control_present = 1 << 9;
    idr_pic | reference_pic | entropy | transform_8x8 | deblocking_filter_control_present
}

/// The `rc_flags` union of `VAEncMiscParameterRateControl`: frames are never skipped,
/// since every frame the engine sends becomes a packet.
pub(crate) fn rc_flags() -> u32 {
    1 << 1 // disable_frame_skip
}

/// `VAEncSequenceParameterBufferH264`, matching [`sps`].
pub(crate) fn sequence(s: &Settings) -> SeqParamH264 {
    let (crop_right, crop_bottom) = s.crop();
    SeqParamH264 {
        seq_parameter_set_id: 0,
        level_idc: s.level_idc,
        intra_period: s.idr_period,
        intra_idr_period: s.idr_period,
        ip_period: 1,
        // The mean; the rate-control buffer gives the peak.
        bits_per_second: s.target_bits_per_second,
        max_num_ref_frames: 1,
        picture_width_in_mbs: u16::try_from(s.width_mbs).unwrap_or(u16::MAX),
        picture_height_in_mbs: u16::try_from(s.height_mbs).unwrap_or(u16::MAX),
        seq_fields: seq_fields(s.log2_max_frame_num),
        bit_depth_luma_minus8: 0,
        bit_depth_chroma_minus8: 0,
        num_ref_frames_in_pic_order_cnt_cycle: 0,
        offset_for_non_ref_pic: 0,
        offset_for_top_to_bottom_field: 0,
        offset_for_ref_frame: [0; 256],
        frame_cropping_flag: u8::from(crop_right != 0 || crop_bottom != 0),
        frame_crop_left_offset: 0,
        frame_crop_right_offset: crop_right,
        frame_crop_top_offset: 0,
        frame_crop_bottom_offset: crop_bottom,
        vui_parameters_present_flag: 1,
        vui_fields: vui_fields(),
        aspect_ratio_idc: 1, // square samples
        sar_width: 1,
        sar_height: 1,
        num_units_in_tick: s.tick.0,
        time_scale: s.tick.1,
        va_reserved: [0; 4],
    }
}

/// A reconstructed picture as libva refers to it.
fn va_picture(surface: Id, p: &Picture, flags: u32) -> PictureH264 {
    PictureH264 {
        picture_id: surface,
        frame_idx: p.frame_num,
        flags,
        top_field_order_cnt: p.poc,
        bottom_field_order_cnt: p.poc,
        va_reserved: [0; 4],
    }
}

/// `VAEncPictureParameterBufferH264` for `p`, reconstructed into `recon` and predicted
/// from `reference` (its surface and picture) unless it is an IDR frame, written to
/// `coded_buf`. Matches [`pps`].
pub(crate) fn picture(
    s: &Settings,
    p: &Picture,
    recon: Id,
    reference: Option<(Id, &Picture)>,
    coded_buf: Id,
) -> PicParamH264 {
    let mut reference_frames = [PictureH264::INVALID; 16];
    if let (false, Some((surface, r))) = (p.idr, reference) {
        reference_frames[0] = va_picture(surface, r, PICTURE_H264_SHORT_TERM_REFERENCE);
    }
    PicParamH264 {
        curr_pic: va_picture(recon, p, 0),
        reference_frames,
        coded_buf,
        pic_parameter_set_id: 0,
        seq_parameter_set_id: 0,
        last_picture: 0,
        frame_num: u16::try_from(p.frame_num).unwrap_or(u16::MAX),
        pic_init_qp: PIC_INIT_QP,
        num_ref_idx_l0_active_minus1: 0,
        num_ref_idx_l1_active_minus1: 0,
        chroma_qp_index_offset: 0,
        second_chroma_qp_index_offset: 0,
        pic_fields: pic_fields(s, p.idr),
        va_reserved: [0; 4],
    }
}

/// `VAEncSliceParameterBufferH264`: one slice covering the picture, an I slice for an
/// IDR frame and a P slice predicted from `reference` otherwise.
pub(crate) fn slice(
    s: &Settings,
    p: &Picture,
    reference: Option<(Id, &Picture)>,
) -> SliceParamH264 {
    let mut ref_pic_list0 = [PictureH264::INVALID; 32];
    let predicted = match (p.idr, reference) {
        (false, Some((surface, r))) => {
            ref_pic_list0[0] = va_picture(surface, r, PICTURE_H264_SHORT_TERM_REFERENCE);
            true
        }
        _ => false,
    };
    SliceParamH264 {
        macroblock_address: 0,
        num_macroblocks: s.macroblocks(),
        macroblock_info: INVALID_ID,
        slice_type: if predicted { SLICE_P } else { SLICE_I },
        pic_parameter_set_id: 0,
        idr_pic_id: p.idr_pic_id,
        pic_order_cnt_lsb: 0,
        delta_pic_order_cnt_bottom: 0,
        delta_pic_order_cnt: [0; 2],
        direct_spatial_mv_pred_flag: 0,
        num_ref_idx_active_override_flag: 0,
        num_ref_idx_l0_active_minus1: 0,
        num_ref_idx_l1_active_minus1: 0,
        ref_pic_list0,
        ref_pic_list1: [PictureH264::INVALID; 32],
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
        cabac_init_idc: 0,
        slice_qp_delta: 0,
        disable_deblocking_filter_idc: 0,
        slice_alpha_c0_offset_div2: 0,
        slice_beta_offset_div2: 0,
        va_reserved: [0; 4],
    }
}

/// The rate controller's settings, with a one-second window.
pub(crate) fn rate_control(s: &Settings) -> MiscParam<RateControlParam> {
    MiscParam {
        kind: misc::RATE_CONTROL,
        data: RateControlParam {
            bits_per_second: s.bits_per_second,
            target_percentage: s.target_percentage,
            window_size: 1000,
            rc_flags: rc_flags(),
            ..RateControlParam::default()
        },
    }
}

/// The coded picture buffer: one second at the peak rate, starting three quarters full.
pub(crate) fn hrd(s: &Settings) -> MiscParam<HrdParam> {
    MiscParam {
        kind: misc::HRD,
        data: HrdParam {
            initial_buffer_fullness: s.bits_per_second / 4 * 3,
            buffer_size: s.bits_per_second,
            va_reserved: [0; 4],
        },
    }
}

/// `VAEncMiscParameterFrameRate`'s `framerate`: the denominator in the high 16 bits and
/// the numerator in the low 16. A fraction that doesn't fit is rounded to thousandths
/// of a frame per second, then to whole frames.
pub(crate) fn va_framerate((num, den): (u32, u32)) -> u32 {
    let pack = |n: u64, d: u64| -> Option<u32> {
        let g = gcd(n, d).max(1);
        let (n, d) = (n / g, d / g);
        (n <= 0xFFFF && d <= 0xFFFF && n > 0).then_some(((d as u32) << 16) | n as u32)
    };
    let (n, d) = (u64::from(num), u64::from(den.max(1)));
    pack(n, d)
        .or_else(|| pack((n * 1000 + d / 2) / d, 1000))
        .unwrap_or_else(|| {
            let whole = ((n + d / 2) / d).clamp(1, 0xFFFF);
            (1 << 16) | whole as u32
        })
}

/// The rate controller's frame rate.
pub(crate) fn frame_rate(s: &Settings) -> MiscParam<FrameRateParam> {
    MiscParam {
        kind: misc::FRAME_RATE,
        data: FrameRateParam {
            framerate: va_framerate(s.fps),
            framerate_flags: 0,
            va_reserved: [0; 4],
        },
    }
}

/// Writes an RBSP most significant bit first.
#[derive(Debug, Default)]
pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    /// Bits used in the last byte, 0 when it is full (or there is none).
    used: u32,
}

impl BitWriter {
    /// The low `n` bits of `value` (`u(n)`), `n` at most 32.
    pub(crate) fn bits(&mut self, value: u32, n: u32) {
        for i in (0..n).rev() {
            let bit = (value >> i) & 1;
            if self.used == 0 {
                self.bytes.push(0);
            }
            if let Some(last) = self.bytes.last_mut() {
                *last |= (bit as u8) << (7 - self.used);
            }
            self.used = (self.used + 1) % 8;
        }
    }

    /// One flag (`u(1)`).
    pub(crate) fn flag(&mut self, on: bool) {
        self.bits(u32::from(on), 1);
    }

    /// Unsigned Exp-Golomb (`ue(v)`).
    pub(crate) fn ue(&mut self, value: u32) {
        let v = u64::from(value) + 1;
        let len = u64::BITS - v.leading_zeros(); // bits in v
        self.bits(0, len - 1);
        // v < 2^33: write it in two parts.
        if len > 32 {
            self.bits((v >> 32) as u32, len - 32);
            self.bits(v as u32, 32);
        } else {
            self.bits(v as u32, len);
        }
    }

    /// Signed Exp-Golomb (`se(v)`).
    pub(crate) fn se(&mut self, value: i32) {
        let mapped = if value > 0 {
            (i64::from(value) * 2 - 1) as u32
        } else {
            (i64::from(value) * -2) as u32
        };
        self.ue(mapped);
    }

    /// `rbsp_trailing_bits()`: a one, then zeros to the byte boundary.
    pub(crate) fn trailing(&mut self) {
        self.bits(1, 1);
        if self.used != 0 {
            self.bits(0, 8 - self.used);
        }
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// An Annex B NAL unit: a four-byte start code, the header, and `rbsp` with emulation
/// prevention bytes inserted.
pub(crate) fn nal(nal_ref_idc: u8, nal_unit_type: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rbsp.len() + rbsp.len() / 64 + 5);
    out.extend_from_slice(&[0, 0, 0, 1, (nal_ref_idc & 3) << 5 | (nal_unit_type & 0x1F)]);
    let mut zeros = 0;
    for &b in rbsp {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// The sequence parameter set, as an Annex B NAL unit (`nal_unit_type` 7).
pub(crate) fn sps(s: &Settings) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.bits(u32::from(s.profile_idc()), 8);
    // constraint_set0..5_flag: Constrained Baseline sets 0 and 1, Main sets 1.
    let constraints = match s.profile {
        H264Profile::ConstrainedBaseline => 0b1100_0000,
        H264Profile::Main => 0b0100_0000,
        H264Profile::High => 0,
    };
    w.bits(constraints, 8); // six flags and reserved_zero_2bits
    w.bits(u32::from(s.level_idc), 8);
    w.ue(0); // seq_parameter_set_id
    if s.profile == H264Profile::High {
        w.ue(1); // chroma_format_idc: 4:2:0
        w.ue(0); // bit_depth_luma_minus8
        w.ue(0); // bit_depth_chroma_minus8
        w.flag(false); // qpprime_y_zero_transform_bypass_flag
        w.flag(false); // seq_scaling_matrix_present_flag
    }
    w.ue(s.log2_max_frame_num - 4);
    w.ue(2); // pic_order_cnt_type
    w.ue(1); // max_num_ref_frames
    w.flag(false); // gaps_in_frame_num_value_allowed_flag
    w.ue(s.width_mbs - 1);
    w.ue(s.height_mbs - 1); // pic_height_in_map_units_minus1 (frames only)
    w.flag(true); // frame_mbs_only_flag
    w.flag(true); // direct_8x8_inference_flag
    let (crop_right, crop_bottom) = s.crop();
    let crop = crop_right != 0 || crop_bottom != 0;
    w.flag(crop);
    if crop {
        w.ue(0);
        w.ue(crop_right);
        w.ue(0);
        w.ue(crop_bottom);
    }
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
    w.bits(1, 8); // matrix_coefficients: BT.709
    w.flag(false); // chroma_loc_info_present_flag: left-sited, the default
    w.flag(true); // timing_info_present_flag
    w.bits(s.tick.0, 32); // num_units_in_tick
    w.bits(s.tick.1, 32); // time_scale
    w.flag(true); // fixed_frame_rate_flag
    w.flag(false); // nal_hrd_parameters_present_flag
    w.flag(false); // vcl_hrd_parameters_present_flag
    w.flag(false); // pic_struct_present_flag
    w.flag(true); // bitstream_restriction_flag
    w.flag(true); // motion_vectors_over_pic_boundaries_flag
    w.ue(0); // max_bytes_per_pic_denom
    w.ue(0); // max_bits_per_mb_denom
    w.ue(LOG2_MAX_MV_LENGTH); // log2_max_mv_length_horizontal
    w.ue(LOG2_MAX_MV_LENGTH); // log2_max_mv_length_vertical
    w.ue(0); // max_num_reorder_frames: no B-frames
    w.ue(1); // max_dec_frame_buffering
    w.trailing();
    nal(3, 7, &w.into_bytes())
}

/// The picture parameter set, as an Annex B NAL unit (`nal_unit_type` 8).
pub(crate) fn pps(s: &Settings) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.ue(0); // pic_parameter_set_id
    w.ue(0); // seq_parameter_set_id
    w.flag(s.cabac()); // entropy_coding_mode_flag
    w.flag(false); // bottom_field_pic_order_in_frame_present_flag
    w.ue(0); // num_slice_groups_minus1
    w.ue(0); // num_ref_idx_l0_default_active_minus1
    w.ue(0); // num_ref_idx_l1_default_active_minus1
    w.flag(false); // weighted_pred_flag
    w.bits(0, 2); // weighted_bipred_idc
    w.se(i32::from(PIC_INIT_QP) - 26); // pic_init_qp_minus26
    w.se(0); // pic_init_qs_minus26
    w.se(0); // chroma_qp_index_offset
    w.flag(true); // deblocking_filter_control_present_flag
    w.flag(false); // constrained_intra_pred_flag
    w.flag(false); // redundant_pic_cnt_present_flag
    if s.transform_8x8() {
        w.flag(true); // transform_8x8_mode_flag
        w.flag(false); // pic_scaling_matrix_present_flag
        w.se(0); // second_chroma_qp_index_offset
    }
    w.trailing();
    nal(3, 8, &w.into_bytes())
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{Codec, FrameSize, PixelFormat, Timebase};
    use jstream_platform::{HardwarePreference, PlatformError, RateControl, VideoEncoderConfig};

    use super::{
        BitWriter, Gop, Picture, RcMode, Settings, check, frame_rate, hrd, level, nal, pic_fields,
        picture, pps, rate_control, rc_flags, seq_fields, sequence, slice, sps, va_framerate,
        vui_fields,
    };
    use crate::H264Profile;
    use crate::vaapi::ffi::{INVALID_ID, PICTURE_H264_INVALID, misc};

    fn config() -> VideoEncoderConfig {
        VideoEncoderConfig {
            codec: Codec::H264,
            size: FrameSize::new(1920, 1080).expect("size"),
            input_format: PixelFormat::Nv12,
            frame_period: Timebase::new(1001, 30_000).expect("29.97"),
            rate_control: RateControl::Cbr { kbps: 6000 },
            keyframe_interval: 60,
            hardware: HardwarePreference::Prefer,
        }
    }

    fn settings() -> Settings {
        check(&config(), H264Profile::High).expect("valid")
    }

    #[test]
    fn derives_the_session_settings() {
        let s = settings();
        assert_eq!((s.width_mbs, s.height_mbs), (120, 68));
        // 1080 is 67.5 macroblocks: the last 8 rows are cropped, in units of 2.
        assert_eq!(s.crop(), (0, 4));
        assert_eq!(s.fps, (30_000, 1001));
        assert_eq!(s.tick, (1001, 60_000));
        assert_eq!(
            (s.rc, s.bits_per_second, s.target_percentage),
            (RcMode::Cbr, 6_000_000, 100)
        );
        assert_eq!(s.level_idc, 40);
        assert_eq!(s.idr_period, 60);
        assert_eq!(s.log2_max_frame_num, 6);
        let mut c = config();
        c.rate_control = RateControl::Vbr {
            kbps: 4000,
            max_kbps: 8000,
        };
        c.frame_period = Timebase::new(2, 120).expect("60");
        c.keyframe_interval = 2;
        let s = check(&c, H264Profile::Main).expect("valid");
        assert_eq!(
            (s.rc, s.bits_per_second, s.target_percentage),
            (RcMode::Vbr, 8_000_000, 50)
        );
        assert_eq!(s.target_bits_per_second, 4_000_000);
        assert_eq!(sequence(&s).bits_per_second, 4_000_000);
        assert_eq!((s.fps, s.tick), ((60, 1), (1, 120)));
        // 1080p60 needs level 4.2.
        assert_eq!(s.level_idc, 42);
        // frame_num needs at least four bits.
        assert_eq!(s.log2_max_frame_num, 4);
        assert!(!s.cabac() || s.profile == H264Profile::Main);
        assert!(!s.transform_8x8());
    }

    #[test]
    fn takes_the_compositors_rgb_output() {
        for format in [PixelFormat::Rgba8, PixelFormat::Bgra8] {
            let mut c = config();
            c.input_format = format;
            assert_eq!(check(&c, H264Profile::High).expect("accepted"), settings());
        }
    }

    #[test]
    fn refuses_what_the_session_cant_encode() {
        let refused = |c: VideoEncoderConfig| {
            matches!(
                check(&c, H264Profile::High),
                Err(PlatformError::Unsupported { .. })
            )
        };
        let mut c = config();
        c.codec = Codec::Hevc;
        assert!(refused(c));
        let mut c = config();
        c.input_format = PixelFormat::I420;
        assert!(refused(c));
        let mut c = config();
        c.size = FrameSize::new(1921, 1080).expect("size");
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
        // Beyond level 6.2: 16K, and more than 1 Gb/s on High (800 000 * 1250).
        let mut c = config();
        c.size = FrameSize::new(15_360, 8_640).expect("size");
        assert!(refused(c));
        let mut c = config();
        c.rate_control = RateControl::Cbr { kbps: 1_000_001 };
        assert!(refused(c));
    }

    #[test]
    fn picks_the_lowest_level_that_fits() {
        let fps = |n| (n, 1);
        // 720p30 is level 3.1, 720p60 3.2, 1080p30 4, 4K30 5.1, 4K60 5.2.
        assert_eq!(
            level(H264Profile::High, 80, 45, fps(30), 4_000_000),
            Some(31)
        );
        assert_eq!(
            level(H264Profile::High, 80, 45, fps(60), 4_000_000),
            Some(32)
        );
        assert_eq!(
            level(H264Profile::High, 120, 68, fps(30), 6_000_000),
            Some(40)
        );
        assert_eq!(
            level(H264Profile::High, 240, 135, fps(30), 20_000_000),
            Some(51)
        );
        assert_eq!(
            level(H264Profile::High, 240, 135, fps(60), 20_000_000),
            Some(52)
        );
        // The bit rate raises it: 1080p30 at 25 Mb/s exceeds level 4 on High (20 000 *
        // 1250) but not 4.1.
        assert_eq!(
            level(H264Profile::High, 120, 68, fps(30), 25_000_000),
            Some(40)
        );
        assert_eq!(
            level(H264Profile::High, 120, 68, fps(30), 25_000_001),
            Some(41)
        );
        assert_eq!(
            level(H264Profile::Main, 120, 68, fps(30), 20_000_001),
            Some(41)
        );
        // Too wide for the frame-size limit of level 4.1 (sqrt(8 * 8192) = 256 MBs), not
        // of 4.2.
        assert_eq!(level(H264Profile::High, 257, 4, fps(1), 1_000), Some(42));
    }

    #[test]
    fn bitfields_match_libva() {
        // The values a C program built against libva 2.20.0's headers printed with the
        // same fields set (ffi's module docs).
        assert_eq!(seq_fields(8), 2341);
        assert_eq!(vui_fields(), 44_927);
        let s = settings();
        assert_eq!(pic_fields(&s, true), 779);
        assert_eq!(pic_fields(&s, false), 778);
        assert_eq!(rc_flags(), 2);
    }

    #[test]
    fn the_sequence_buffer_matches_the_sps() {
        let s = settings();
        let seq = sequence(&s);
        assert_eq!(seq.level_idc, 40);
        assert_eq!(
            (seq.intra_period, seq.intra_idr_period, seq.ip_period),
            (60, 60, 1)
        );
        assert_eq!(seq.bits_per_second, 6_000_000);
        assert_eq!(seq.max_num_ref_frames, 1);
        assert_eq!(
            (seq.picture_width_in_mbs, seq.picture_height_in_mbs),
            (120, 68)
        );
        assert_eq!(seq.seq_fields, seq_fields(6));
        assert_eq!(seq.frame_cropping_flag, 1);
        assert_eq!(
            (seq.frame_crop_right_offset, seq.frame_crop_bottom_offset),
            (0, 4)
        );
        assert_eq!((seq.num_units_in_tick, seq.time_scale), (1001, 60_000));
        assert_eq!(seq.vui_parameters_present_flag, 1);
        assert_eq!(seq.aspect_ratio_idc, 1);
        let mut c = config();
        c.size = FrameSize::new(1280, 720).expect("size");
        let seq = sequence(&check(&c, H264Profile::High).expect("valid"));
        assert_eq!(seq.frame_cropping_flag, 0);
    }

    #[test]
    fn gop_makes_idr_frames_on_cadence_and_on_request() {
        let mut c = config();
        c.keyframe_interval = 3;
        let s = check(&c, H264Profile::High).expect("valid");
        let mut gop = Gop::new(&s);
        let mut pics: Vec<Picture> = (0..4).map(|_| gop.next_picture()).collect();
        // Several requests before a frame ask for one IDR, and restart the cadence.
        gop.force_idr();
        gop.force_idr();
        pics.extend((0..4).map(|_| gop.next_picture()));
        let summary: Vec<_> = pics
            .iter()
            .map(|p| (p.idr, p.frame_num, p.poc, p.idr_pic_id))
            .collect();
        assert_eq!(
            summary,
            [
                (true, 0, 0, 0),
                (false, 1, 2, 0),
                (false, 2, 4, 0),
                (true, 0, 0, 1),
                (true, 0, 0, 2),
                (false, 1, 2, 2),
                (false, 2, 4, 2),
                (true, 0, 0, 3),
            ]
        );
    }

    #[test]
    fn frame_num_wraps_and_poc_keeps_counting() {
        let mut c = config();
        c.keyframe_interval = 100;
        let s = check(&c, H264Profile::High).expect("valid");
        // 100 frames need 7 bits.
        assert_eq!(s.log2_max_frame_num, 7);
        let mut c = config();
        c.keyframe_interval = 1000;
        let mut s = check(&c, H264Profile::High).expect("valid");
        s.log2_max_frame_num = 4;
        let mut gop = Gop::new(&s);
        let pics: Vec<Picture> = (0..20).map(|_| gop.next_picture()).collect();
        assert_eq!(pics[16].frame_num, 0);
        assert_eq!(pics[17].frame_num, 1);
        assert_eq!(pics[17].poc, 34);
        assert!(!pics[16].idr);
    }

    #[test]
    fn picture_and_slice_buffers_reference_the_previous_frame() {
        let s = settings();
        let mut gop = Gop::new(&s);
        let idr = gop.next_picture();
        let p = gop.next_picture();
        let pic = picture(&s, &idr, 10, None, 99);
        assert_eq!(pic.curr_pic.picture_id, 10);
        assert_eq!(pic.coded_buf, 99);
        assert_eq!(pic.pic_init_qp, 26);
        assert!(
            pic.reference_frames
                .iter()
                .all(|r| r.picture_id == INVALID_ID && r.flags == PICTURE_H264_INVALID)
        );
        let sl = slice(&s, &idr, None);
        assert_eq!(sl.slice_type, 2);
        assert_eq!(sl.num_macroblocks, 120 * 68);
        assert_eq!(sl.macroblock_info, INVALID_ID);
        // An IDR frame ignores the reference it is given.
        assert_eq!(slice(&s, &idr, Some((10, &idr))).slice_type, 2);

        let pic = picture(&s, &p, 11, Some((10, &idr)), 98);
        assert_eq!((pic.curr_pic.picture_id, pic.frame_num), (11, 1));
        assert_eq!(pic.curr_pic.top_field_order_cnt, 2);
        assert_eq!(pic.reference_frames[0].picture_id, 10);
        assert_eq!(pic.reference_frames[0].flags, 0x8);
        assert_eq!(pic.reference_frames[1].picture_id, INVALID_ID);
        assert_eq!(pic.pic_fields & 1, 0);
        let sl = slice(&s, &p, Some((10, &idr)));
        assert_eq!(sl.slice_type, 0);
        assert_eq!(sl.ref_pic_list0[0].picture_id, 10);
        assert_eq!(sl.ref_pic_list0[0].frame_idx, 0);
        assert_eq!(sl.ref_pic_list0[1].picture_id, INVALID_ID);
        assert!(sl.ref_pic_list1.iter().all(|r| r.picture_id == INVALID_ID));
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
        assert_eq!(rc.data.rc_flags, 2);
        let h = hrd(&s);
        assert_eq!(h.kind, misc::HRD);
        assert_eq!(
            (h.data.buffer_size, h.data.initial_buffer_fullness),
            (6_000_000, 4_500_000)
        );
        let f = frame_rate(&s);
        assert_eq!(f.kind, misc::FRAME_RATE);
        assert_eq!(f.data.framerate, (1001 << 16) | 30_000);
    }

    #[test]
    fn frame_rates_pack_into_sixteen_bits_each() {
        assert_eq!(va_framerate((60, 1)), (1 << 16) | 60);
        assert_eq!(va_framerate((120, 2)), (1 << 16) | 60);
        assert_eq!(va_framerate((30_000, 1001)), (1001 << 16) | 30_000);
        // 100000/3 fps doesn't fit: thousandths, reduced (33333.333 -> 33333/1).
        assert_eq!(va_framerate((100_000, 3)), (1 << 16) | 33_333);
        // 1/70000 fps (one frame every 70 000 s) rounds up to 1 fps.
        assert_eq!(va_framerate((1, 70_000)), (1 << 16) | 1);
    }

    #[test]
    fn exp_golomb_codes() {
        let bits = |f: &dyn Fn(&mut BitWriter)| {
            let mut w = BitWriter::default();
            f(&mut w);
            w.trailing();
            w.into_bytes()
        };
        // ue: 0 -> 1, 1 -> 010, 2 -> 011, 3 -> 00100, 7 -> 0001000.
        assert_eq!(bits(&|w| w.ue(0)), [0b1100_0000]);
        assert_eq!(bits(&|w| w.ue(1)), [0b0101_0000]);
        assert_eq!(bits(&|w| w.ue(2)), [0b0111_0000]);
        assert_eq!(bits(&|w| w.ue(3)), [0b0010_0100]);
        assert_eq!(bits(&|w| w.ue(7)), [0b0001_0001]);
        // se: 1 -> 010, -1 -> 011, 2 -> 00100.
        assert_eq!(bits(&|w| w.se(1)), [0b0101_0000]);
        assert_eq!(bits(&|w| w.se(-1)), [0b0111_0000]);
        assert_eq!(bits(&|w| w.se(2)), [0b0010_0100]);
        // The largest value: 32 zeros, then 33 bits of 2^32.
        let max = bits(&|w| w.ue(u32::MAX));
        assert_eq!(max.len(), 9);
        assert_eq!(&max[..4], &[0; 4]);
        assert_eq!(max[4], 0x80);
        // A full byte needs no padding beyond its trailing bit.
        assert_eq!(bits(&|w| w.bits(0xAB, 8)), [0xAB, 0x80]);
    }

    #[test]
    fn nal_units_get_emulation_prevention() {
        assert_eq!(
            nal(3, 7, &[0, 0, 1, 0, 0, 0, 0, 0, 4, 0, 0]),
            [0, 0, 0, 1, 0x67, 0, 0, 3, 1, 0, 0, 3, 0, 0, 3, 0, 4, 0, 0]
        );
        assert_eq!(
            nal(3, 8, &[0xCE, 0x3C, 0x80]),
            [0, 0, 0, 1, 0x68, 0xCE, 0x3C, 0x80]
        );
    }

    #[test]
    fn parameter_sets_for_1080p_high() {
        let s = settings();
        // FFmpeg's trace_headers parses these as intended (H.264 7.3.2.1.1, 7.3.2.2,
        // E.1.1): High 4.0, frame_num in 6 bits, POC type 2, one reference frame, 120x68
        // MBs cropped by 4 (8 rows) at the bottom, square samples, BT.709 limited range,
        // 1001/60000 ticks at a fixed rate, no reordering; CABAC and the 8x8 transform.
        assert_eq!(
            sps(&s),
            [
                0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x00, 0x28, 0xAC, 0x6D, 0x00, 0xF0, 0x04, 0x4F,
                0xCB, 0x80, 0xB5, 0x01, 0x01, 0x01, 0x40, 0x00, 0x00, 0xFA, 0x40, 0x00, 0x3A, 0x98,
                0x23, 0xC2, 0x01, 0x0A, 0x80,
            ]
        );
        assert_eq!(pps(&s), [0x00, 0x00, 0x00, 0x01, 0x68, 0xEE, 0x3C, 0xB0]);
        // Constrained Baseline: CAVLC, no 8x8 transform.
        let cb = check(&config(), H264Profile::ConstrainedBaseline).expect("valid");
        assert_eq!(&sps(&cb)[4..8], &[0x67, 66, 0xC0, 40]);
        assert_eq!(pps(&cb), [0x00, 0x00, 0x00, 0x01, 0x68, 0xCE, 0x3C, 0x80]);
        let main = check(&config(), H264Profile::Main).expect("valid");
        assert_eq!(&sps(&main)[4..8], &[0x67, 77, 0x40, 40]);
        assert_eq!(pps(&main), [0x00, 0x00, 0x00, 0x01, 0x68, 0xEE, 0x3C, 0x80]);
    }
}
