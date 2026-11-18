//! H.264 slice headers (ITU-T H.264 §7.3.3): what VA-API's slice parameters and the
//! decoded picture buffer need, and where the slice data starts.

use super::bits::{BitReader, ParseError, Result};
use super::params::{Pps, Sps};

/// `slice_type % 5`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SliceType {
    P = 0,
    B = 1,
    I = 2,
    Sp = 3,
    Si = 4,
}

impl SliceType {
    fn from_code(code: u32) -> Self {
        match code % 5 {
            0 => Self::P,
            1 => Self::B,
            2 => Self::I,
            3 => Self::Sp,
            _ => Self::Si,
        }
    }

    pub(crate) fn is_intra(self) -> bool {
        matches!(self, Self::I | Self::Si)
    }
}

/// One `modification_of_pic_nums_idc` and its value (§7.3.3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ListModification {
    /// 0: `abs_diff_pic_num_minus1`, subtracted.
    ShortTermSub(u32),
    /// 1: `abs_diff_pic_num_minus1`, added.
    ShortTermAdd(u32),
    /// 2: `long_term_pic_num`.
    LongTerm(u32),
}

/// One memory management control operation (§7.3.3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mmco {
    /// 1: `difference_of_pic_nums_minus1`.
    ForgetShortTerm(u32),
    /// 2: `long_term_pic_num`.
    ForgetLongTerm(u32),
    /// 3: `difference_of_pic_nums_minus1`, `long_term_frame_idx`.
    ShortToLong(u32, u32),
    /// 4: `max_long_term_frame_idx_plus1`.
    MaxLongTermIdx(u32),
    /// 5: everything stops being a reference.
    ForgetAll,
    /// 6: `long_term_frame_idx` for the current picture.
    CurrentToLong(u32),
}

/// `dec_ref_pic_marking()`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Marking {
    /// IDR: `no_output_of_prior_pics_flag`.
    pub(crate) no_output_of_prior_pics: bool,
    /// IDR: `long_term_reference_flag`.
    pub(crate) long_term_reference: bool,
    /// Non-IDR: `adaptive_ref_pic_marking_mode_flag`'s operations, `None` for the
    /// sliding window.
    pub(crate) mmco: Option<Vec<Mmco>>,
}

/// One list's explicit weights: `(luma weight, luma offset, [chroma weight, offset; 2])`
/// for each reference index.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Weights {
    pub(crate) luma_flag: bool,
    pub(crate) chroma_flag: bool,
    pub(crate) luma: Vec<(i16, i16)>,
    pub(crate) chroma: Vec<[(i16, i16); 2]>,
}

/// `pred_weight_table()`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PredWeights {
    pub(crate) luma_log2_denom: u8,
    pub(crate) chroma_log2_denom: u8,
    pub(crate) l0: Weights,
    pub(crate) l1: Weights,
}

/// A parsed slice header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SliceHeader {
    pub(crate) nal_ref_idc: u8,
    pub(crate) idr: bool,
    pub(crate) first_mb: u32,
    pub(crate) slice_type: SliceType,
    pub(crate) pps_id: u8,
    pub(crate) frame_num: u32,
    pub(crate) field_pic: bool,
    pub(crate) bottom_field: bool,
    pub(crate) idr_pic_id: u32,
    pub(crate) pic_order_cnt_lsb: u32,
    pub(crate) delta_pic_order_cnt_bottom: i32,
    pub(crate) delta_pic_order_cnt: [i32; 2],
    pub(crate) redundant_pic_cnt: u32,
    pub(crate) direct_spatial_mv_pred: bool,
    pub(crate) num_ref_idx_l0_active_minus1: u8,
    pub(crate) num_ref_idx_l1_active_minus1: u8,
    pub(crate) modifications_l0: Vec<ListModification>,
    pub(crate) modifications_l1: Vec<ListModification>,
    pub(crate) weights: Option<PredWeights>,
    pub(crate) marking: Marking,
    pub(crate) cabac_init_idc: u8,
    pub(crate) slice_qp_delta: i8,
    pub(crate) disable_deblocking_filter_idc: u8,
    pub(crate) slice_alpha_c0_offset_div2: i8,
    pub(crate) slice_beta_offset_div2: i8,
    /// Where the slice data starts, in bits from the start of the unescaped NAL unit,
    /// its header byte included: VA-API's `slice_data_bit_offset`.
    pub(crate) header_bits: usize,
}

impl SliceHeader {
    /// Whether the slice is part of a reference picture.
    pub(crate) fn is_reference(&self) -> bool {
        self.nal_ref_idc != 0
    }

    /// Parses the header of a slice NAL unit (types 1 and 5). `nal` is the unescaped
    /// unit, header byte included. `pps_of` and `sps_of` look up the parameter sets.
    pub(crate) fn parse(
        nal: &[u8],
        pps_of: impl Fn(u8) -> Option<Pps>,
        sps_of: impl Fn(u8) -> Option<Sps>,
    ) -> Result<(Self, Pps, Sps)> {
        let mut r = BitReader::new(nal);
        r.skip(1)?;
        let nal_ref_idc = u8::try_from(r.bits(2)?).unwrap_or(0);
        let nal_unit_type = r.bits(5)?;
        let idr = nal_unit_type == 5;
        let first_mb = r.ue()?;
        let slice_type = SliceType::from_code(r.ue_max(9, "slice_type")?);
        let pps_id = u8::try_from(r.ue_max(255, "pic_parameter_set_id")?).unwrap_or(0);
        let pps = pps_of(pps_id).ok_or(ParseError::Invalid("a slice naming an unknown PPS"))?;
        let sps = sps_of(pps.sps_id).ok_or(ParseError::Invalid("a PPS naming an unknown SPS"))?;
        let frame_num = r.bits(u32::from(sps.log2_max_frame_num_minus4) + 4)?;
        let (mut field_pic, mut bottom_field) = (false, false);
        if !sps.frame_mbs_only {
            field_pic = r.flag()?;
            if field_pic {
                bottom_field = r.flag()?;
            }
        }
        let idr_pic_id = if idr {
            r.ue_max(65_535, "idr_pic_id")?
        } else {
            0
        };
        let mut pic_order_cnt_lsb = 0;
        let mut delta_pic_order_cnt_bottom = 0;
        let mut delta_pic_order_cnt = [0; 2];
        if sps.pic_order_cnt_type == 0 {
            pic_order_cnt_lsb = r.bits(u32::from(sps.log2_max_pic_order_cnt_lsb_minus4) + 4)?;
            if pps.bottom_field_pic_order_in_frame_present && !field_pic {
                delta_pic_order_cnt_bottom = r.se()?;
            }
        }
        if sps.pic_order_cnt_type == 1 && !sps.delta_pic_order_always_zero {
            delta_pic_order_cnt[0] = r.se()?;
            if pps.bottom_field_pic_order_in_frame_present && !field_pic {
                delta_pic_order_cnt[1] = r.se()?;
            }
        }
        let redundant_pic_cnt = if pps.redundant_pic_cnt_present {
            r.ue_max(127, "redundant_pic_cnt")?
        } else {
            0
        };
        let direct_spatial_mv_pred = if slice_type == SliceType::B {
            r.flag()?
        } else {
            false
        };
        let mut l0 = pps.num_ref_idx_l0_default_active_minus1;
        let mut l1 = pps.num_ref_idx_l1_default_active_minus1;
        if matches!(slice_type, SliceType::P | SliceType::Sp | SliceType::B) && r.flag()? {
            let max = if field_pic { 31 } else { 15 };
            l0 = u8::try_from(r.ue_max(max, "num_ref_idx_l0_active_minus1")?).unwrap_or(0);
            if slice_type == SliceType::B {
                l1 = u8::try_from(r.ue_max(max, "num_ref_idx_l1_active_minus1")?).unwrap_or(0);
            }
        }
        let mut modifications_l0 = Vec::new();
        let mut modifications_l1 = Vec::new();
        if !slice_type.is_intra() {
            modifications_l0 = read_modifications(&mut r)?;
            if slice_type == SliceType::B {
                modifications_l1 = read_modifications(&mut r)?;
            }
        }
        let weights = if (pps.weighted_pred && matches!(slice_type, SliceType::P | SliceType::Sp))
            || (pps.weighted_bipred_idc == 1 && slice_type == SliceType::B)
        {
            Some(read_weights(&mut r, &sps, slice_type, l0, l1)?)
        } else {
            None
        };
        let mut marking = Marking::default();
        if nal_ref_idc != 0 {
            if idr {
                marking.no_output_of_prior_pics = r.flag()?;
                marking.long_term_reference = r.flag()?;
            } else if r.flag()? {
                marking.mmco = Some(read_mmco(&mut r)?);
            }
        }
        let cabac_init_idc = if pps.entropy_coding_mode && !slice_type.is_intra() {
            u8::try_from(r.ue_max(2, "cabac_init_idc")?).unwrap_or(0)
        } else {
            0
        };
        let slice_qp_delta = i8::try_from(r.se_range(-87, 77, "slice_qp_delta")?).unwrap_or(0);
        if matches!(slice_type, SliceType::Sp | SliceType::Si) {
            if slice_type == SliceType::Sp {
                r.skip(1)?;
            }
            r.se()?;
        }
        let mut disable_deblocking_filter_idc = 0;
        let mut slice_alpha_c0_offset_div2 = 0;
        let mut slice_beta_offset_div2 = 0;
        if pps.deblocking_filter_control_present {
            disable_deblocking_filter_idc =
                u8::try_from(r.ue_max(2, "disable_deblocking_filter_idc")?).unwrap_or(0);
            if disable_deblocking_filter_idc != 1 {
                slice_alpha_c0_offset_div2 =
                    i8::try_from(r.se_range(-6, 6, "slice_alpha_c0_offset_div2")?).unwrap_or(0);
                slice_beta_offset_div2 =
                    i8::try_from(r.se_range(-6, 6, "slice_beta_offset_div2")?).unwrap_or(0);
            }
        }
        let header = Self {
            nal_ref_idc,
            idr,
            first_mb,
            slice_type,
            pps_id,
            frame_num,
            field_pic,
            bottom_field,
            idr_pic_id,
            pic_order_cnt_lsb,
            delta_pic_order_cnt_bottom,
            delta_pic_order_cnt,
            redundant_pic_cnt,
            direct_spatial_mv_pred,
            num_ref_idx_l0_active_minus1: l0,
            num_ref_idx_l1_active_minus1: l1,
            modifications_l0,
            modifications_l1,
            weights,
            marking,
            cabac_init_idc,
            slice_qp_delta,
            disable_deblocking_filter_idc,
            slice_alpha_c0_offset_div2,
            slice_beta_offset_div2,
            header_bits: r.position(),
        };
        Ok((header, pps, sps))
    }
}

/// `ref_pic_list_modification()` for one list.
fn read_modifications(r: &mut BitReader<'_>) -> Result<Vec<ListModification>> {
    let mut out = Vec::new();
    if !r.flag()? {
        return Ok(out);
    }
    loop {
        let op = match r.ue_max(5, "modification_of_pic_nums_idc")? {
            0 => ListModification::ShortTermSub(r.ue()?),
            1 => ListModification::ShortTermAdd(r.ue()?),
            2 => ListModification::LongTerm(r.ue()?),
            3 => return Ok(out),
            _ => return Err(ParseError::Unsupported("multiview list modifications")),
        };
        out.push(op);
        if out.len() > 32 {
            return Err(ParseError::Invalid("more than 32 list modifications"));
        }
    }
}

/// `pred_weight_table()`.
fn read_weights(
    r: &mut BitReader<'_>,
    sps: &Sps,
    slice_type: SliceType,
    l0: u8,
    l1: u8,
) -> Result<PredWeights> {
    let luma_log2_denom = u8::try_from(r.ue_max(7, "luma_log2_weight_denom")?).unwrap_or(0);
    let chroma_log2_denom = if sps.chroma_format_idc != 0 {
        u8::try_from(r.ue_max(7, "chroma_log2_weight_denom")?).unwrap_or(0)
    } else {
        0
    };
    let mut table = PredWeights {
        luma_log2_denom,
        chroma_log2_denom,
        ..PredWeights::default()
    };
    let lists = if slice_type == SliceType::B { 2 } else { 1 };
    for list in 0..lists {
        let count = usize::from(if list == 0 { l0 } else { l1 }) + 1;
        let weights = if list == 0 {
            &mut table.l0
        } else {
            &mut table.l1
        };
        let luma_default = 1_i16 << luma_log2_denom;
        let chroma_default = 1_i16 << chroma_log2_denom;
        for _ in 0..count {
            if r.flag()? {
                weights.luma_flag = true;
                let w = i16::try_from(r.se_range(-128, 127, "luma_weight")?).unwrap_or(0);
                let o = i16::try_from(r.se_range(-128, 127, "luma_offset")?).unwrap_or(0);
                weights.luma.push((w, o));
            } else {
                weights.luma.push((luma_default, 0));
            }
            if sps.chroma_format_idc != 0 && r.flag()? {
                weights.chroma_flag = true;
                let mut pair = [(0, 0); 2];
                for entry in &mut pair {
                    let w = i16::try_from(r.se_range(-128, 127, "chroma_weight")?).unwrap_or(0);
                    let o = i16::try_from(r.se_range(-128, 127, "chroma_offset")?).unwrap_or(0);
                    *entry = (w, o);
                }
                weights.chroma.push(pair);
            } else {
                weights.chroma.push([(chroma_default, 0); 2]);
            }
        }
    }
    Ok(table)
}

/// The operations of `dec_ref_pic_marking()` with its adaptive flag set.
fn read_mmco(r: &mut BitReader<'_>) -> Result<Vec<Mmco>> {
    let mut out = Vec::new();
    loop {
        let op = match r.ue_max(6, "memory_management_control_operation")? {
            0 => return Ok(out),
            1 => Mmco::ForgetShortTerm(r.ue()?),
            2 => Mmco::ForgetLongTerm(r.ue()?),
            3 => {
                let diff = r.ue()?;
                Mmco::ShortToLong(diff, r.ue()?)
            }
            4 => Mmco::MaxLongTermIdx(r.ue()?),
            5 => Mmco::ForgetAll,
            _ => Mmco::CurrentToLong(r.ue()?),
        };
        out.push(op);
        if out.len() > 66 {
            return Err(ParseError::Invalid("too many marking operations"));
        }
    }
}
