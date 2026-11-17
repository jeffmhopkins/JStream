//! H.264 sequence and picture parameter sets (ITU-T H.264 §7.3.2.1, §7.3.2.2, Annex E),
//! with the scaling lists resolved by the fall-back rules of Table 7-2.

use super::bits::{BitReader, ParseError, Result};

/// `Flat_4x4_16` and `Flat_8x8_16`.
const FLAT: u8 = 16;

/// `Default_4x4_Intra`, in zig-zag (bitstream) order.
const DEFAULT_4X4_INTRA: [u8; 16] = [
    6, 13, 13, 20, 20, 20, 28, 28, 28, 28, 32, 32, 32, 37, 37, 42,
];
/// `Default_4x4_Inter`.
const DEFAULT_4X4_INTER: [u8; 16] = [
    10, 14, 14, 20, 20, 20, 24, 24, 24, 24, 27, 27, 27, 30, 30, 34,
];
/// `Default_8x8_Intra`.
const DEFAULT_8X8_INTRA: [u8; 64] = [
    6, 10, 10, 13, 11, 13, 16, 16, 16, 16, 18, 18, 18, 18, 18, 23, 23, 23, 23, 23, 23, 25, 25, 25,
    25, 25, 25, 25, 27, 27, 27, 27, 27, 27, 27, 27, 29, 29, 29, 29, 29, 29, 29, 31, 31, 31, 31, 31,
    31, 33, 33, 33, 33, 33, 36, 36, 36, 36, 38, 38, 38, 40, 40, 42,
];
/// `Default_8x8_Inter`.
const DEFAULT_8X8_INTER: [u8; 64] = [
    9, 13, 13, 15, 13, 15, 17, 17, 17, 17, 19, 19, 19, 19, 19, 21, 21, 21, 21, 21, 21, 22, 22, 22,
    22, 22, 22, 22, 24, 24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 27, 27, 27, 27, 27,
    27, 28, 28, 28, 28, 28, 30, 30, 30, 30, 32, 32, 32, 33, 33, 35,
];

/// The scaling matrices of a sequence or picture, in zig-zag order as VA-API takes
/// them: six 4x4 lists (intra Y, Cb, Cr, inter Y, Cb, Cr) and the two 8x8 lists of 4:2:0
/// (intra Y, inter Y).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScalingLists {
    pub(crate) list4x4: [[u8; 16]; 6],
    pub(crate) list8x8: [[u8; 64]; 2],
}

impl ScalingLists {
    /// `Flat_4x4_16` and `Flat_8x8_16` everywhere: no scaling matrix.
    pub(crate) fn flat() -> Self {
        Self {
            list4x4: [[FLAT; 16]; 6],
            list8x8: [[FLAT; 64]; 2],
        }
    }

    /// Reads `seq_scaling_matrix_present_flag`'s or `pic_scaling_matrix_present_flag`'s
    /// lists. `fallback` is rule B's (the sequence's lists, for a picture) or `None` for
    /// rule A (the defaults). `count` is how many lists the syntax carries (6 without the
    /// 8x8 transform, 8 with it, 12 for 4:4:4).
    fn read(r: &mut BitReader<'_>, count: usize, fallback: Option<&Self>) -> Result<Self> {
        let mut out = Self::flat();
        for i in 0..count {
            let present = r.flag()?;
            if i < 6 {
                let default = if i < 3 {
                    DEFAULT_4X4_INTRA
                } else {
                    DEFAULT_4X4_INTER
                };
                out.list4x4[i] = if present {
                    match read_list(r, 16)? {
                        Some(list) => list.try_into().unwrap_or(default),
                        None => default,
                    }
                } else if i == 0 || i == 3 {
                    fallback.map_or(default, |f| f.list4x4[i])
                } else {
                    out.list4x4[i - 1]
                };
            } else {
                let default = if i % 2 == 0 {
                    DEFAULT_8X8_INTRA
                } else {
                    DEFAULT_8X8_INTER
                };
                let list = if present {
                    match read_list(r, 64)? {
                        Some(list) => list.try_into().unwrap_or(default),
                        None => default,
                    }
                } else if i < 8 {
                    fallback.map_or(default, |f| f.list8x8[i - 6])
                } else {
                    // The 4:4:4 chroma lists: skipped (4:2:0 only).
                    continue;
                };
                if i < 8 {
                    out.list8x8[i - 6] = list;
                }
            }
        }
        // Without the 8x8 lists in the syntax, the 8x8 matrices follow the fall-back too.
        if count < 8 {
            for (j, default) in [DEFAULT_8X8_INTRA, DEFAULT_8X8_INTER].iter().enumerate() {
                out.list8x8[j] = fallback.map_or(*default, |f| f.list8x8[j]);
            }
        }
        Ok(out)
    }
}

/// `scaling_list()`: the list, or `None` when `useDefaultScalingMatrixFlag` is set.
fn read_list(r: &mut BitReader<'_>, size: usize) -> Result<Option<Vec<u8>>> {
    let mut list = Vec::with_capacity(size);
    let mut last: i32 = 8;
    let mut next: i32 = 8;
    for j in 0..size {
        if next != 0 {
            let delta = r.se_range(-128, 127, "delta_scale")?;
            next = (last + delta + 256) % 256;
            if j == 0 && next == 0 {
                return Ok(None);
            }
        }
        let value = if next == 0 { last } else { next };
        list.push(u8::try_from(value).map_err(|_| ParseError::Invalid("a scaling list entry"))?);
        last = value;
    }
    Ok(Some(list))
}

/// A sequence parameter set: what decoding needs of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sps {
    pub(crate) profile_idc: u8,
    pub(crate) constraint_set3: bool,
    pub(crate) level_idc: u8,
    pub(crate) id: u8,
    pub(crate) chroma_format_idc: u8,
    pub(crate) bit_depth_luma_minus8: u8,
    pub(crate) bit_depth_chroma_minus8: u8,
    /// The sequence's lists when it carries them (rule A applied), else `None` (flat).
    pub(crate) scaling: Option<ScalingLists>,
    pub(crate) log2_max_frame_num_minus4: u8,
    pub(crate) pic_order_cnt_type: u8,
    pub(crate) log2_max_pic_order_cnt_lsb_minus4: u8,
    pub(crate) delta_pic_order_always_zero: bool,
    pub(crate) offset_for_non_ref_pic: i32,
    pub(crate) offset_for_top_to_bottom_field: i32,
    pub(crate) offset_for_ref_frame: Vec<i32>,
    pub(crate) max_num_ref_frames: u8,
    pub(crate) gaps_in_frame_num_allowed: bool,
    pub(crate) width_in_mbs: u32,
    /// In frame macroblocks: `(2 - frame_mbs_only_flag) * PicHeightInMapUnits`.
    pub(crate) height_in_mbs: u32,
    pub(crate) frame_mbs_only: bool,
    pub(crate) mb_adaptive_frame_field: bool,
    pub(crate) direct_8x8_inference: bool,
    /// The visible rectangle: left, right, top and bottom crop in luma samples.
    pub(crate) crop: [u32; 4],
    /// `max_num_reorder_frames` from the VUI's bitstream restriction, when it says.
    pub(crate) max_num_reorder_frames: Option<u32>,
    /// `max_dec_frame_buffering`, when the VUI says.
    pub(crate) max_dec_frame_buffering: Option<u32>,
    /// `video_full_range_flag` and `matrix_coefficients`, when the VUI says.
    pub(crate) full_range: bool,
    pub(crate) matrix_coefficients: Option<u8>,
    /// `num_units_in_tick` and `time_scale`, when the VUI has timing.
    pub(crate) timing: Option<(u32, u32)>,
}

impl Sps {
    /// `MaxFrameNum`.
    pub(crate) fn max_frame_num(&self) -> u32 {
        1 << (u32::from(self.log2_max_frame_num_minus4) + 4)
    }

    /// `MaxPicOrderCntLsb`.
    pub(crate) fn max_poc_lsb(&self) -> u32 {
        1 << (u32::from(self.log2_max_pic_order_cnt_lsb_minus4) + 4)
    }

    /// The decoded size in pixels, before cropping.
    pub(crate) fn coded_size(&self) -> (u32, u32) {
        (self.width_in_mbs * 16, self.height_in_mbs * 16)
    }

    /// The visible size in pixels, after cropping.
    pub(crate) fn visible_size(&self) -> (u32, u32) {
        let (w, h) = self.coded_size();
        let [left, right, top, bottom] = self.crop;
        (
            w.saturating_sub(left + right),
            h.saturating_sub(top + bottom),
        )
    }

    /// The most frames the decoded picture buffer holds (§A.3.1 item h, Table A-1),
    /// from `max_dec_frame_buffering` where the VUI says.
    pub(crate) fn dpb_frames(&self) -> u32 {
        if let Some(frames) = self.max_dec_frame_buffering {
            return frames.clamp(u32::from(self.max_num_ref_frames).max(1), 16);
        }
        let max_dpb_mbs: u32 = match self.level_idc {
            9 => 396,
            10 => 396,
            11 if self.constraint_set3 && self.profile_idc != 100 => 396,
            11 => 900,
            12 | 13 | 20 => 2_376,
            21 => 4_752,
            22 | 30 => 8_100,
            31 => 18_000,
            32 => 20_480,
            40 | 41 => 32_768,
            42 => 34_816,
            50 => 110_400,
            51 | 52 => 184_320,
            _ => 696_320,
        };
        let frame_mbs = (self.width_in_mbs * self.height_in_mbs).max(1);
        (max_dpb_mbs / frame_mbs).clamp(u32::from(self.max_num_ref_frames).max(1), 16)
    }

    /// How many frames output waits behind decoding: the VUI's `max_num_reorder_frames`,
    /// else the whole buffer (intra-only and baseline streams don't reorder).
    pub(crate) fn reorder_frames(&self) -> u32 {
        if let Some(frames) = self.max_num_reorder_frames {
            return frames.min(16);
        }
        if matches!(self.profile_idc, 66 | 83)
            || self.constraint_set3 && intra_profile(self.profile_idc)
        {
            return 0;
        }
        self.dpb_frames()
    }

    /// Parses an SPS NAL unit's RBSP (after the one-byte NAL header).
    pub(crate) fn parse(rbsp: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(rbsp);
        let profile_idc = u8::try_from(r.bits(8)?).unwrap_or(0);
        let constraints = r.bits(8)?;
        let constraint_set3 = constraints & 0x10 != 0;
        let level_idc = u8::try_from(r.bits(8)?).unwrap_or(0);
        let id = u8::try_from(r.ue_max(31, "seq_parameter_set_id")?).unwrap_or(0);
        let mut chroma_format_idc = 1;
        let mut bit_depth_luma_minus8 = 0;
        let mut bit_depth_chroma_minus8 = 0;
        let mut scaling = None;
        if high_profile(profile_idc) {
            chroma_format_idc = u8::try_from(r.ue_max(3, "chroma_format_idc")?).unwrap_or(1);
            if chroma_format_idc == 3 && r.flag()? {
                return Err(ParseError::Unsupported("separate colour planes"));
            }
            bit_depth_luma_minus8 =
                u8::try_from(r.ue_max(6, "bit_depth_luma_minus8")?).unwrap_or(0);
            bit_depth_chroma_minus8 =
                u8::try_from(r.ue_max(6, "bit_depth_chroma_minus8")?).unwrap_or(0);
            let _qpprime_y_zero_transform_bypass = r.flag()?;
            if r.flag()? {
                let count = if chroma_format_idc == 3 { 12 } else { 8 };
                scaling = Some(ScalingLists::read(&mut r, count, None)?);
            }
        }
        let log2_max_frame_num_minus4 =
            u8::try_from(r.ue_max(12, "log2_max_frame_num_minus4")?).unwrap_or(0);
        let pic_order_cnt_type = u8::try_from(r.ue_max(2, "pic_order_cnt_type")?).unwrap_or(0);
        let mut log2_max_pic_order_cnt_lsb_minus4 = 0;
        let mut delta_pic_order_always_zero = false;
        let mut offset_for_non_ref_pic = 0;
        let mut offset_for_top_to_bottom_field = 0;
        let mut offset_for_ref_frame = Vec::new();
        match pic_order_cnt_type {
            0 => {
                log2_max_pic_order_cnt_lsb_minus4 =
                    u8::try_from(r.ue_max(12, "log2_max_pic_order_cnt_lsb_minus4")?).unwrap_or(0);
            }
            1 => {
                delta_pic_order_always_zero = r.flag()?;
                offset_for_non_ref_pic = r.se()?;
                offset_for_top_to_bottom_field = r.se()?;
                let cycle = r.ue_max(255, "num_ref_frames_in_pic_order_cnt_cycle")?;
                for _ in 0..cycle {
                    offset_for_ref_frame.push(r.se()?);
                }
            }
            _ => {}
        }
        let max_num_ref_frames = u8::try_from(r.ue_max(16, "max_num_ref_frames")?).unwrap_or(16);
        let gaps_in_frame_num_allowed = r.flag()?;
        let width_in_mbs = r.ue_max(1_023, "pic_width_in_mbs_minus1")? + 1;
        let height_in_map_units = r.ue_max(1_023, "pic_height_in_map_units_minus1")? + 1;
        let frame_mbs_only = r.flag()?;
        let mb_adaptive_frame_field = if frame_mbs_only { false } else { r.flag()? };
        let direct_8x8_inference = r.flag()?;
        let height_in_mbs = if frame_mbs_only {
            height_in_map_units
        } else {
            height_in_map_units * 2
        };
        let mut crop = [0; 4];
        if r.flag()? {
            let (crop_x, crop_y) = match chroma_format_idc {
                1 => (2, 2),
                2 => (2, 1),
                _ => (1, 1),
            };
            let crop_y = crop_y * if frame_mbs_only { 1 } else { 2 };
            crop = [
                r.ue()?.saturating_mul(crop_x),
                r.ue()?.saturating_mul(crop_x),
                r.ue()?.saturating_mul(crop_y),
                r.ue()?.saturating_mul(crop_y),
            ];
        }
        let mut sps = Self {
            profile_idc,
            constraint_set3,
            level_idc,
            id,
            chroma_format_idc,
            bit_depth_luma_minus8,
            bit_depth_chroma_minus8,
            scaling,
            log2_max_frame_num_minus4,
            pic_order_cnt_type,
            log2_max_pic_order_cnt_lsb_minus4,
            delta_pic_order_always_zero,
            offset_for_non_ref_pic,
            offset_for_top_to_bottom_field,
            offset_for_ref_frame,
            max_num_ref_frames,
            gaps_in_frame_num_allowed,
            width_in_mbs,
            height_in_mbs,
            frame_mbs_only,
            mb_adaptive_frame_field,
            direct_8x8_inference,
            crop,
            max_num_reorder_frames: None,
            max_dec_frame_buffering: None,
            full_range: false,
            matrix_coefficients: None,
            timing: None,
        };
        if r.flag()? {
            // A damaged VUI doesn't stop decoding: what it holds is a hint.
            let _ = read_vui(&mut r, &mut sps);
        }
        Ok(sps)
    }
}

/// The profiles whose SPS carries `chroma_format_idc` and the fields after it.
fn high_profile(profile_idc: u8) -> bool {
    matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    )
}

/// The profiles that are intra-only when `constraint_set3_flag` is set.
fn intra_profile(profile_idc: u8) -> bool {
    matches!(profile_idc, 100 | 110 | 122 | 244 | 44)
}

/// `vui_parameters()` (Annex E.1.1), keeping what playback uses.
fn read_vui(r: &mut BitReader<'_>, sps: &mut Sps) -> Result<()> {
    if r.flag()? {
        // aspect_ratio_info_present_flag
        if r.bits(8)? == 255 {
            r.skip(32)?;
        }
    }
    if r.flag()? {
        // overscan_info_present_flag
        r.skip(1)?;
    }
    if r.flag()? {
        // video_signal_type_present_flag
        r.skip(3)?;
        sps.full_range = r.flag()?;
        if r.flag()? {
            r.skip(16)?;
            sps.matrix_coefficients = u8::try_from(r.bits(8)?).ok();
        }
    }
    if r.flag()? {
        // chroma_loc_info_present_flag
        r.ue()?;
        r.ue()?;
    }
    if r.flag()? {
        // timing_info_present_flag
        let units = r.bits(32)?;
        let scale = r.bits(32)?;
        r.skip(1)?;
        if units > 0 && scale > 0 {
            sps.timing = Some((units, scale));
        }
    }
    let nal_hrd = r.flag()?;
    if nal_hrd {
        read_hrd(r)?;
    }
    let vcl_hrd = r.flag()?;
    if vcl_hrd {
        read_hrd(r)?;
    }
    if nal_hrd || vcl_hrd {
        r.skip(1)?;
    }
    r.skip(1)?;
    if r.flag()? {
        // bitstream_restriction_flag
        r.skip(1)?;
        r.ue()?;
        r.ue()?;
        r.ue()?;
        r.ue()?;
        sps.max_num_reorder_frames = Some(r.ue()?);
        sps.max_dec_frame_buffering = Some(r.ue()?);
    }
    Ok(())
}

/// `hrd_parameters()` (Annex E.1.2), skipped.
fn read_hrd(r: &mut BitReader<'_>) -> Result<()> {
    let count = r.ue_max(31, "cpb_cnt_minus1")? + 1;
    r.skip(8)?;
    for _ in 0..count {
        r.ue()?;
        r.ue()?;
        r.skip(1)?;
    }
    r.skip(20)?;
    Ok(())
}

/// A picture parameter set: what decoding needs of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pps {
    pub(crate) id: u8,
    pub(crate) sps_id: u8,
    pub(crate) entropy_coding_mode: bool,
    pub(crate) bottom_field_pic_order_in_frame_present: bool,
    pub(crate) num_ref_idx_l0_default_active_minus1: u8,
    pub(crate) num_ref_idx_l1_default_active_minus1: u8,
    pub(crate) weighted_pred: bool,
    pub(crate) weighted_bipred_idc: u8,
    pub(crate) pic_init_qp_minus26: i8,
    pub(crate) pic_init_qs_minus26: i8,
    pub(crate) chroma_qp_index_offset: i8,
    pub(crate) deblocking_filter_control_present: bool,
    pub(crate) constrained_intra_pred: bool,
    pub(crate) redundant_pic_cnt_present: bool,
    pub(crate) transform_8x8_mode: bool,
    /// The picture's lists, when the PPS carries them (rule B applied against the
    /// sequence's).
    pub(crate) scaling: Option<ScalingLists>,
    pub(crate) second_chroma_qp_index_offset: i8,
}

impl Pps {
    /// Parses a PPS NAL unit's RBSP. Its sequence's set must be known, for the fall-back
    /// scaling lists and the 4:4:4 list count.
    pub(crate) fn parse(rbsp: &[u8], sps_of: impl Fn(u8) -> Option<Sps>) -> Result<Self> {
        let mut r = BitReader::new(rbsp);
        let id = u8::try_from(r.ue_max(255, "pic_parameter_set_id")?).unwrap_or(0);
        let sps_id = u8::try_from(r.ue_max(31, "seq_parameter_set_id")?).unwrap_or(0);
        let sps = sps_of(sps_id).ok_or(ParseError::Invalid("a PPS naming an unknown SPS"))?;
        let entropy_coding_mode = r.flag()?;
        let bottom_field_pic_order_in_frame_present = r.flag()?;
        if r.ue_max(7, "num_slice_groups_minus1")? > 0 {
            return Err(ParseError::Unsupported("slice groups (FMO)"));
        }
        let num_ref_idx_l0_default_active_minus1 =
            u8::try_from(r.ue_max(31, "num_ref_idx_l0_default_active_minus1")?).unwrap_or(0);
        let num_ref_idx_l1_default_active_minus1 =
            u8::try_from(r.ue_max(31, "num_ref_idx_l1_default_active_minus1")?).unwrap_or(0);
        let weighted_pred = r.flag()?;
        let weighted_bipred_idc = u8::try_from(r.bits(2)?).unwrap_or(0);
        let pic_init_qp_minus26 =
            i8::try_from(r.se_range(-26, 25, "pic_init_qp_minus26")?).unwrap_or(0);
        let pic_init_qs_minus26 =
            i8::try_from(r.se_range(-26, 25, "pic_init_qs_minus26")?).unwrap_or(0);
        let chroma_qp_index_offset =
            i8::try_from(r.se_range(-12, 12, "chroma_qp_index_offset")?).unwrap_or(0);
        let deblocking_filter_control_present = r.flag()?;
        let constrained_intra_pred = r.flag()?;
        let redundant_pic_cnt_present = r.flag()?;
        let mut transform_8x8_mode = false;
        let mut scaling = None;
        let mut second_chroma_qp_index_offset = chroma_qp_index_offset;
        if r.more_rbsp_data() {
            transform_8x8_mode = r.flag()?;
            if r.flag()? {
                let count = 6 + if transform_8x8_mode {
                    if sps.chroma_format_idc == 3 { 6 } else { 2 }
                } else {
                    0
                };
                let sequence = sps.scaling.clone().unwrap_or_else(ScalingLists::flat);
                // Rule B falls back to the sequence's lists when it has them, else rule A.
                let fallback = sps.scaling.as_ref().map(|_| &sequence);
                scaling = Some(ScalingLists::read(&mut r, count, fallback)?);
            }
            second_chroma_qp_index_offset =
                i8::try_from(r.se_range(-12, 12, "second_chroma_qp_index_offset")?).unwrap_or(0);
        }
        Ok(Self {
            id,
            sps_id,
            entropy_coding_mode,
            bottom_field_pic_order_in_frame_present,
            num_ref_idx_l0_default_active_minus1,
            num_ref_idx_l1_default_active_minus1,
            weighted_pred,
            weighted_bipred_idc,
            pic_init_qp_minus26,
            pic_init_qs_minus26,
            chroma_qp_index_offset,
            deblocking_filter_control_present,
            constrained_intra_pred,
            redundant_pic_cnt_present,
            transform_8x8_mode,
            scaling,
            second_chroma_qp_index_offset,
        })
    }

    /// The scaling lists a picture using this PPS decodes with.
    pub(crate) fn scaling_lists(&self, sps: &Sps) -> ScalingLists {
        self.scaling
            .clone()
            .or_else(|| sps.scaling.clone())
            .unwrap_or_else(ScalingLists::flat)
    }
}
