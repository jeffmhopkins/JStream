//! AV1's headers (the AV1 bitstream specification, v1.0.0 with errata), for decoding
//! through VA-API (ADR-0072 (b)): a sample's OBUs, the sequence header, each frame's
//! uncompressed header, and where each tile group's tiles lie. It keeps what carries
//! from frame to frame in the eight reference slots: sizes, order hints, loop filter
//! deltas, segmentation features, global motion and film grain parameters.
//!
//! The GPU's driver keeps the probability contexts and reads the tiles. What VA-API
//! wants from the application is the headers' fields, with the global motion
//! parameters decoded against the primary reference frame's (§7.20) and the film
//! grain parameters loaded from the slot a frame names (§7.21). FFmpeg's CBS reader
//! and AV1 decoder hand their VA-API hwaccel the same, and the tests check this parser
//! against FFmpeg's `trace_headers` field for field.

use super::h264::bits::{BitReader, ParseError, Result};

mod tables;

use tables::DIV_LUT;

/// The reference slots.
pub(crate) const NUM_REF_FRAMES: usize = 8;
/// The references an inter frame names: LAST, LAST2, LAST3, GOLDEN, BWDREF, ALTREF2,
/// ALTREF.
pub(crate) const REFS_PER_FRAME: usize = 7;
/// The segments.
pub(crate) const MAX_SEGMENTS: usize = 8;
/// The segment features.
pub(crate) const SEG_LVL_MAX: usize = 8;
/// The most tile columns or rows.
pub(crate) const MAX_TILE_COLS: usize = 64;
const MAX_TILE_ROWS: usize = 64;
const MAX_TILE_WIDTH: u32 = 4096;
const MAX_TILE_AREA: u32 = 4096 * 2304;
/// `primary_ref_frame` when there is none.
pub(crate) const PRIMARY_REF_NONE: u8 = 7;

/// `frame_type`.
pub(crate) const KEY_FRAME: u8 = 0;
pub(crate) const INTER_FRAME: u8 = 1;
pub(crate) const INTRA_ONLY_FRAME: u8 = 2;
pub(crate) const SWITCH_FRAME: u8 = 3;

/// `seq_force_screen_content_tools` and `seq_force_integer_mv`: chosen per frame.
const SELECT: u8 = 2;

/// The OBU types this reads.
const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_FRAME_HEADER: u8 = 3;
const OBU_TILE_GROUP: u8 = 4;
const OBU_FRAME: u8 = 6;
const OBU_TILE_LIST: u8 = 8;

/// Global motion (§5.9.24, §7.20).
const WARPEDMODEL_PREC_BITS: u32 = 16;
const GM_ABS_ALPHA_BITS: u32 = 12;
const GM_ALPHA_PREC_BITS: u32 = 15;
const GM_ABS_TRANS_ONLY_BITS: u32 = 9;
const GM_TRANS_ONLY_PREC_BITS: u32 = 3;
const GM_ABS_TRANS_BITS: u32 = 12;
const GM_TRANS_PREC_BITS: u32 = 6;
const WARP_PARAM_REDUCE_BITS: u32 = 6;
const DIV_LUT_BITS: u32 = 8;
const DIV_LUT_PREC_BITS: u32 = 14;
/// The warp model types.
pub(crate) const IDENTITY: u8 = 0;
pub(crate) const TRANSLATION: u8 = 1;
pub(crate) const ROTZOOM: u8 = 2;
pub(crate) const AFFINE: u8 = 3;
/// The identity model's parameters.
const IDENTITY_PARAMS: [i32; 6] = [
    0,
    0,
    1 << WARPEDMODEL_PREC_BITS,
    0,
    0,
    1 << WARPEDMODEL_PREC_BITS,
];

/// The loop filter's default deltas (§7.20 `setup_past_independence`).
const DEFAULT_REF_DELTAS: [i8; 8] = [1, 0, 0, 0, -1, 0, -1, -1];

/// The bits and signedness of each segment feature's value (§5.9.14).
const FEATURE_BITS: [u32; SEG_LVL_MAX] = [8, 6, 6, 6, 6, 3, 0, 0];
const FEATURE_SIGNED: [bool; SEG_LVL_MAX] = [true, true, true, true, true, false, false, false];
const SEG_LVL_ALT_Q: usize = 0;

/// Reads AV1's syntax elements (§4.10).
struct Reader<'a> {
    bits: BitReader<'a>,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            bits: BitReader::new(data),
        }
    }

    /// `f(n)`.
    fn f(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        self.bits.bits(n)
    }

    /// `f(n)` that fits a byte.
    fn f8(&mut self, n: u32) -> Result<u8> {
        Ok(u8::try_from(self.f(n)?).unwrap_or(u8::MAX))
    }

    fn flag(&mut self) -> Result<bool> {
        Ok(self.f(1)? == 1)
    }

    /// `su(n)`.
    fn su(&mut self, n: u32) -> Result<i32> {
        let v = self.f(n)?;
        let sign = 1_u32 << (n - 1);
        let v = i64::from(v) - if v & sign == 0 { 0 } else { 1_i64 << n };
        Ok(i32::try_from(v).unwrap_or(0))
    }

    /// `ns(n)`, for `n` of 1 or more.
    fn ns(&mut self, n: u32) -> Result<u32> {
        let w = 32 - n.leading_zeros();
        let m = (1_u32 << w) - n;
        let v = self.f(w - 1)?;
        if v < m {
            return Ok(v);
        }
        let extra = self.f(1)?;
        Ok((v << 1) - m + extra)
    }

    /// A value from `min` that each 1 bit raises, up to `max` (CBS's `increment`).
    fn increment(&mut self, min: u32, max: u32) -> Result<u32> {
        let mut v = min;
        while v < max {
            if self.f(1)? == 1 {
                v += 1;
            } else {
                break;
            }
        }
        Ok(v)
    }

    /// `uvlc()`.
    fn uvlc(&mut self) -> Result<u32> {
        let mut zeros = 0;
        while self.f(1)? == 0 {
            zeros += 1;
            if zeros >= 32 {
                return Err(ParseError::Invalid("an AV1 uvlc() of 32 zeros"));
            }
        }
        Ok(self.f(zeros)? + ((1_u32 << zeros) - 1))
    }

    /// `decode_subexp(numSyms)` (§5.9.27).
    fn subexp(&mut self, num_syms: u32) -> Result<u32> {
        let max_len = (31 - (num_syms - 1).leading_zeros()) - 3;
        let len = self.increment(0, max_len)?;
        let (bits, offset) = if len > 0 {
            (2 + len, 1_u32 << (2 + len))
        } else {
            (3, 0)
        };
        let v = if len < max_len {
            self.f(bits)?
        } else {
            self.ns(num_syms - offset)?
        };
        Ok(v + offset)
    }

    /// `delta_q`: a flag, then `su(7)`.
    fn delta_q(&mut self) -> Result<i8> {
        if self.flag()? {
            Ok(i8::try_from(self.su(7)?).unwrap_or(0))
        } else {
            Ok(0)
        }
    }

    fn byte_alignment(&mut self) -> Result<()> {
        while !self.bits.position().is_multiple_of(8) {
            self.f(1)?;
        }
        Ok(())
    }
}

/// `leb128()` at `data[at..]`: the value and the bytes it took.
fn leb128(data: &[u8], at: usize) -> Result<(u64, usize)> {
    let mut v = 0_u64;
    for i in 0..8 {
        let byte = *data.get(at + i).ok_or(ParseError::Truncated)?;
        v |= u64::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok((v, i + 1));
        }
    }
    Err(ParseError::Invalid("an AV1 leb128() over 8 bytes"))
}

/// The configuration OBUs of an `av1C` record (AV1-ISOBMFF §2.3, which Matroska's
/// `CodecPrivate` holds too): what follows its four bytes, or nothing for a record of
/// another version.
pub(crate) fn config_obus(av1c: &[u8]) -> Vec<u8> {
    match av1c {
        // marker 1, version 1.
        [0x81, _, _, _, obus @ ..] => obus.to_vec(),
        _ => Vec::new(),
    }
}

/// One OBU of a sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Obu {
    pub(crate) kind: u8,
    pub(crate) spatial_id: u8,
    /// The payload's range in the sample.
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// A sample's OBUs (§5.3), each with a size field, as Matroska and MP4 carry them.
///
/// # Errors
/// [`ParseError::Truncated`] when an OBU runs past the sample, and
/// [`ParseError::Invalid`] for a forbidden bit or an OBU without its size.
pub(crate) fn obus(sample: &[u8]) -> Result<Vec<Obu>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < sample.len() {
        let header = sample[at];
        if header & 0x80 != 0 {
            return Err(ParseError::Invalid("an AV1 OBU's forbidden bit"));
        }
        let kind = header >> 3 & 0xF;
        let extension = header & 4 != 0;
        if header & 2 == 0 {
            return Err(ParseError::Invalid("an AV1 OBU without its size"));
        }
        let mut pos = at + 1;
        let mut spatial_id = 0;
        if extension {
            let ext = *sample.get(pos).ok_or(ParseError::Truncated)?;
            spatial_id = ext >> 3 & 3;
            pos += 1;
        }
        let (size, len) = leb128(sample, pos)?;
        pos += len;
        let size = usize::try_from(size).map_err(|_| ParseError::Truncated)?;
        let end = pos.checked_add(size).ok_or(ParseError::Truncated)?;
        if end > sample.len() {
            return Err(ParseError::Truncated);
        }
        out.push(Obu {
            kind,
            spatial_id,
            start: pos,
            end,
        });
        at = end;
    }
    Ok(out)
}

/// `color_config()` (§5.5.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ColorConfig {
    pub(crate) bit_depth: u8,
    pub(crate) mono_chrome: bool,
    pub(crate) color_primaries: u8,
    pub(crate) transfer_characteristics: u8,
    pub(crate) matrix_coefficients: u8,
    pub(crate) color_range: bool,
    pub(crate) subsampling_x: bool,
    pub(crate) subsampling_y: bool,
    pub(crate) chroma_sample_position: u8,
    pub(crate) separate_uv_delta_q: bool,
}

/// The sequence header (§5.5).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SequenceHeader {
    pub(crate) profile: u8,
    pub(crate) still_picture: bool,
    pub(crate) reduced_still_picture_header: bool,
    decoder_model_info_present: bool,
    equal_picture_interval: bool,
    frame_presentation_time_length: u32,
    buffer_removal_time_length: u32,
    /// Each operating point's `operating_point_idc`, and whether it has a decoder
    /// model.
    operating_points: Vec<(u16, bool)>,
    frame_width_bits: u32,
    frame_height_bits: u32,
    pub(crate) max_frame_width: u32,
    pub(crate) max_frame_height: u32,
    frame_id_numbers_present: bool,
    delta_frame_id_length: u32,
    frame_id_length: u32,
    pub(crate) use_128x128_superblock: bool,
    pub(crate) enable_filter_intra: bool,
    pub(crate) enable_intra_edge_filter: bool,
    pub(crate) enable_interintra_compound: bool,
    pub(crate) enable_masked_compound: bool,
    pub(crate) enable_warped_motion: bool,
    pub(crate) enable_dual_filter: bool,
    pub(crate) enable_order_hint: bool,
    pub(crate) enable_jnt_comp: bool,
    pub(crate) enable_ref_frame_mvs: bool,
    seq_force_screen_content_tools: u8,
    seq_force_integer_mv: u8,
    /// `OrderHintBits`: 0 without order hints.
    pub(crate) order_hint_bits: u32,
    pub(crate) enable_superres: bool,
    pub(crate) enable_cdef: bool,
    pub(crate) enable_restoration: bool,
    pub(crate) color: ColorConfig,
    pub(crate) film_grain_params_present: bool,
}

impl SequenceHeader {
    /// Reads a sequence header OBU's payload.
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        let mut s = Self {
            profile: r.f8(3)?,
            still_picture: r.flag()?,
            reduced_still_picture_header: r.flag()?,
            ..Self::default()
        };
        if s.profile > 2 {
            return Err(ParseError::Invalid("an AV1 seq_profile above 2"));
        }
        if s.reduced_still_picture_header {
            r.f(5)?;
            s.operating_points.push((0, false));
        } else {
            let mut buffer_delay_length = 0;
            if r.flag()? {
                // timing_info()
                r.f(32)?;
                r.f(32)?;
                s.equal_picture_interval = r.flag()?;
                if s.equal_picture_interval {
                    r.uvlc()?;
                }
                s.decoder_model_info_present = r.flag()?;
                if s.decoder_model_info_present {
                    buffer_delay_length = r.f(5)? + 1;
                    r.f(32)?;
                    s.buffer_removal_time_length = r.f(5)? + 1;
                    s.frame_presentation_time_length = r.f(5)? + 1;
                }
            }
            let initial_display_delay_present = r.flag()?;
            let count = r.f(5)? + 1;
            for _ in 0..count {
                let idc = u16::try_from(r.f(12)?).unwrap_or(0);
                let level = r.f(5)?;
                if level > 7 {
                    r.f(1)?;
                }
                let mut model = false;
                if s.decoder_model_info_present {
                    model = r.flag()?;
                    if model {
                        r.f(buffer_delay_length)?;
                        r.f(buffer_delay_length)?;
                        r.f(1)?;
                    }
                }
                if initial_display_delay_present && r.flag()? {
                    r.f(4)?;
                }
                s.operating_points.push((idc, model));
            }
        }
        s.frame_width_bits = r.f(4)? + 1;
        s.frame_height_bits = r.f(4)? + 1;
        s.max_frame_width = r.f(s.frame_width_bits)? + 1;
        s.max_frame_height = r.f(s.frame_height_bits)? + 1;
        s.frame_id_numbers_present = !s.reduced_still_picture_header && r.flag()?;
        if s.frame_id_numbers_present {
            s.delta_frame_id_length = r.f(4)? + 2;
            s.frame_id_length = r.f(3)? + 1 + s.delta_frame_id_length;
        }
        s.use_128x128_superblock = r.flag()?;
        s.enable_filter_intra = r.flag()?;
        s.enable_intra_edge_filter = r.flag()?;
        if s.reduced_still_picture_header {
            s.seq_force_screen_content_tools = SELECT;
            s.seq_force_integer_mv = SELECT;
        } else {
            s.enable_interintra_compound = r.flag()?;
            s.enable_masked_compound = r.flag()?;
            s.enable_warped_motion = r.flag()?;
            s.enable_dual_filter = r.flag()?;
            s.enable_order_hint = r.flag()?;
            if s.enable_order_hint {
                s.enable_jnt_comp = r.flag()?;
                s.enable_ref_frame_mvs = r.flag()?;
            }
            s.seq_force_screen_content_tools = if r.flag()? { SELECT } else { r.f8(1)? };
            s.seq_force_integer_mv = if s.seq_force_screen_content_tools > 0 {
                if r.flag()? { SELECT } else { r.f8(1)? }
            } else {
                SELECT
            };
            if s.enable_order_hint {
                s.order_hint_bits = r.f(3)? + 1;
            }
        }
        s.enable_superres = r.flag()?;
        s.enable_cdef = r.flag()?;
        s.enable_restoration = r.flag()?;
        s.color = Self::color_config(r, s.profile)?;
        s.film_grain_params_present = r.flag()?;
        Ok(s)
    }

    fn color_config(r: &mut Reader<'_>, profile: u8) -> Result<ColorConfig> {
        let mut c = ColorConfig {
            bit_depth: 8,
            color_primaries: 2,
            transfer_characteristics: 2,
            matrix_coefficients: 2,
            ..ColorConfig::default()
        };
        let high = r.flag()?;
        c.bit_depth = if profile == 2 && high {
            if r.flag()? { 12 } else { 10 }
        } else if high {
            10
        } else {
            8
        };
        c.mono_chrome = profile != 1 && r.flag()?;
        if r.flag()? {
            c.color_primaries = r.f8(8)?;
            c.transfer_characteristics = r.f8(8)?;
            c.matrix_coefficients = r.f8(8)?;
        }
        if c.mono_chrome {
            c.color_range = r.flag()?;
            c.subsampling_x = true;
            c.subsampling_y = true;
        } else if c.color_primaries == 1
            && c.transfer_characteristics == 13
            && c.matrix_coefficients == 0
        {
            c.color_range = true;
            c.separate_uv_delta_q = r.flag()?;
        } else {
            c.color_range = r.flag()?;
            match profile {
                0 => (c.subsampling_x, c.subsampling_y) = (true, true),
                1 => {}
                _ if c.bit_depth == 12 => {
                    c.subsampling_x = r.flag()?;
                    c.subsampling_y = c.subsampling_x && r.flag()?;
                }
                _ => c.subsampling_x = true,
            }
            if c.subsampling_x && c.subsampling_y {
                c.chroma_sample_position = r.f8(2)?;
            }
            c.separate_uv_delta_q = r.flag()?;
        }
        Ok(c)
    }

    /// `get_relative_dist(a, b)` (§7.12.2).
    fn relative_dist(&self, a: u32, b: u32) -> i32 {
        if !self.enable_order_hint {
            return 0;
        }
        let diff = a.wrapping_sub(b);
        let m = 1_u32 << (self.order_hint_bits - 1);
        let v = i64::from(diff & (m - 1)) - i64::from(diff & m);
        i32::try_from(v).unwrap_or(0)
    }
}

/// Film grain parameters (§5.9.30), as a frame applies them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FilmGrain {
    pub(crate) apply_grain: bool,
    pub(crate) grain_seed: u16,
    pub(crate) update_grain: bool,
    pub(crate) num_y_points: u8,
    pub(crate) point_y_value: [u8; 14],
    pub(crate) point_y_scaling: [u8; 14],
    pub(crate) chroma_scaling_from_luma: bool,
    pub(crate) num_cb_points: u8,
    pub(crate) point_cb_value: [u8; 10],
    pub(crate) point_cb_scaling: [u8; 10],
    pub(crate) num_cr_points: u8,
    pub(crate) point_cr_value: [u8; 10],
    pub(crate) point_cr_scaling: [u8; 10],
    pub(crate) grain_scaling_minus_8: u8,
    pub(crate) ar_coeff_lag: u8,
    /// Less 128.
    pub(crate) ar_coeffs_y: [i8; 24],
    pub(crate) ar_coeffs_cb: [i8; 25],
    pub(crate) ar_coeffs_cr: [i8; 25],
    pub(crate) ar_coeff_shift_minus_6: u8,
    pub(crate) grain_scale_shift: u8,
    pub(crate) cb_mult: u8,
    pub(crate) cb_luma_mult: u8,
    pub(crate) cb_offset: u16,
    pub(crate) cr_mult: u8,
    pub(crate) cr_luma_mult: u8,
    pub(crate) cr_offset: u16,
    pub(crate) overlap_flag: bool,
    pub(crate) clip_to_restricted_range: bool,
}

/// What a reference slot keeps (§7.20).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RefSlot {
    valid: bool,
    frame_id: u32,
    upscaled_width: u32,
    frame_width: u32,
    frame_height: u32,
    render_width: u32,
    render_height: u32,
    frame_type: u8,
    order_hint: u32,
    loop_filter_ref_deltas: [i8; 8],
    loop_filter_mode_deltas: [i8; 2],
    feature_enabled: [[bool; SEG_LVL_MAX]; MAX_SEGMENTS],
    feature_value: [[i16; SEG_LVL_MAX]; MAX_SEGMENTS],
    gm_params: [[i32; 6]; 8],
    film_grain: FilmGrain,
}

impl Default for RefSlot {
    fn default() -> Self {
        Self {
            valid: false,
            frame_id: 0,
            upscaled_width: 0,
            frame_width: 0,
            frame_height: 0,
            render_width: 0,
            render_height: 0,
            frame_type: KEY_FRAME,
            order_hint: 0,
            loop_filter_ref_deltas: DEFAULT_REF_DELTAS,
            loop_filter_mode_deltas: [0; 2],
            feature_enabled: [[false; SEG_LVL_MAX]; MAX_SEGMENTS],
            feature_value: [[0; SEG_LVL_MAX]; MAX_SEGMENTS],
            gm_params: [IDENTITY_PARAMS; 8],
            film_grain: FilmGrain::default(),
        }
    }
}

/// One frame's uncompressed header (§5.9), with what it loads from its references.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FrameHeader {
    /// `Some(slot)` for `show_existing_frame`: the rest describes that slot's frame.
    pub(crate) show_existing: Option<usize>,
    pub(crate) frame_type: u8,
    pub(crate) show_frame: bool,
    pub(crate) showable_frame: bool,
    pub(crate) error_resilient_mode: bool,
    pub(crate) disable_cdf_update: bool,
    pub(crate) allow_screen_content_tools: bool,
    pub(crate) force_integer_mv: bool,
    pub(crate) order_hint: u32,
    pub(crate) primary_ref_frame: u8,
    pub(crate) refresh_frame_flags: u8,
    /// `FrameWidth` before superres scales it down: what the frame shows.
    pub(crate) upscaled_width: u32,
    pub(crate) frame_width: u32,
    pub(crate) frame_height: u32,
    pub(crate) use_superres: bool,
    /// `SuperresDenom`: 8 without superres.
    pub(crate) superres_denom: u8,
    pub(crate) allow_intrabc: bool,
    pub(crate) ref_frame_idx: [u8; REFS_PER_FRAME],
    pub(crate) allow_high_precision_mv: bool,
    /// 4 is switchable.
    pub(crate) interpolation_filter: u8,
    pub(crate) is_motion_mode_switchable: bool,
    pub(crate) use_ref_frame_mvs: bool,
    pub(crate) disable_frame_end_update_cdf: bool,
    // tile_info()
    pub(crate) uniform_tile_spacing: bool,
    pub(crate) tile_cols: usize,
    pub(crate) tile_rows: usize,
    pub(crate) tile_cols_log2: u32,
    pub(crate) tile_rows_log2: u32,
    pub(crate) width_in_sbs: Vec<u16>,
    pub(crate) height_in_sbs: Vec<u16>,
    pub(crate) context_update_tile_id: u16,
    pub(crate) tile_size_bytes: usize,
    // quantization_params()
    pub(crate) base_q_idx: u8,
    pub(crate) delta_q_y_dc: i8,
    pub(crate) delta_q_u_dc: i8,
    pub(crate) delta_q_u_ac: i8,
    pub(crate) delta_q_v_dc: i8,
    pub(crate) delta_q_v_ac: i8,
    pub(crate) using_qmatrix: bool,
    pub(crate) qm_y: u8,
    pub(crate) qm_u: u8,
    pub(crate) qm_v: u8,
    // segmentation_params()
    pub(crate) segmentation_enabled: bool,
    pub(crate) segmentation_update_map: bool,
    pub(crate) segmentation_temporal_update: bool,
    pub(crate) segmentation_update_data: bool,
    pub(crate) feature_enabled: [[bool; SEG_LVL_MAX]; MAX_SEGMENTS],
    pub(crate) feature_value: [[i16; SEG_LVL_MAX]; MAX_SEGMENTS],
    // delta_q_params(), delta_lf_params()
    pub(crate) delta_q_present: bool,
    pub(crate) delta_q_res: u8,
    pub(crate) delta_lf_present: bool,
    pub(crate) delta_lf_res: u8,
    pub(crate) delta_lf_multi: bool,
    pub(crate) coded_lossless: bool,
    // loop_filter_params()
    pub(crate) loop_filter_level: [u8; 4],
    pub(crate) loop_filter_sharpness: u8,
    pub(crate) loop_filter_delta_enabled: bool,
    pub(crate) loop_filter_delta_update: bool,
    pub(crate) loop_filter_ref_deltas: [i8; 8],
    pub(crate) loop_filter_mode_deltas: [i8; 2],
    // cdef_params()
    pub(crate) cdef_damping_minus_3: u8,
    pub(crate) cdef_bits: u8,
    pub(crate) cdef_y_strengths: [u8; 8],
    pub(crate) cdef_uv_strengths: [u8; 8],
    // lr_params(): FrameRestorationType, and the shifts.
    pub(crate) lr_type: [u8; 3],
    pub(crate) lr_unit_shift: u8,
    pub(crate) lr_uv_shift: u8,
    pub(crate) tx_mode: u8,
    pub(crate) reference_select: bool,
    pub(crate) skip_mode_present: bool,
    pub(crate) allow_warped_motion: bool,
    pub(crate) reduced_tx_set: bool,
    /// By reference frame, LAST (1) to ALTREF (7); 0 unused.
    pub(crate) gm_type: [u8; 8],
    pub(crate) gm_params: [[i32; 6]; 8],
    pub(crate) gm_invalid: [bool; 8],
    pub(crate) film_grain: FilmGrain,
}

impl Default for FrameHeader {
    fn default() -> Self {
        Self {
            show_existing: None,
            frame_type: KEY_FRAME,
            show_frame: true,
            showable_frame: false,
            error_resilient_mode: false,
            disable_cdf_update: false,
            allow_screen_content_tools: false,
            force_integer_mv: false,
            order_hint: 0,
            primary_ref_frame: PRIMARY_REF_NONE,
            refresh_frame_flags: 0,
            upscaled_width: 0,
            frame_width: 0,
            frame_height: 0,
            use_superres: false,
            superres_denom: 8,
            allow_intrabc: false,
            ref_frame_idx: [0; REFS_PER_FRAME],
            allow_high_precision_mv: false,
            interpolation_filter: 0,
            is_motion_mode_switchable: false,
            use_ref_frame_mvs: false,
            disable_frame_end_update_cdf: true,
            uniform_tile_spacing: true,
            tile_cols: 1,
            tile_rows: 1,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
            width_in_sbs: Vec::new(),
            height_in_sbs: Vec::new(),
            context_update_tile_id: 0,
            tile_size_bytes: 4,
            base_q_idx: 0,
            delta_q_y_dc: 0,
            delta_q_u_dc: 0,
            delta_q_u_ac: 0,
            delta_q_v_dc: 0,
            delta_q_v_ac: 0,
            using_qmatrix: false,
            qm_y: 0,
            qm_u: 0,
            qm_v: 0,
            segmentation_enabled: false,
            segmentation_update_map: false,
            segmentation_temporal_update: false,
            segmentation_update_data: false,
            feature_enabled: [[false; SEG_LVL_MAX]; MAX_SEGMENTS],
            feature_value: [[0; SEG_LVL_MAX]; MAX_SEGMENTS],
            delta_q_present: false,
            delta_q_res: 0,
            delta_lf_present: false,
            delta_lf_res: 0,
            delta_lf_multi: false,
            coded_lossless: false,
            loop_filter_level: [0; 4],
            loop_filter_sharpness: 0,
            loop_filter_delta_enabled: false,
            loop_filter_delta_update: false,
            loop_filter_ref_deltas: DEFAULT_REF_DELTAS,
            loop_filter_mode_deltas: [0; 2],
            cdef_damping_minus_3: 0,
            cdef_bits: 0,
            cdef_y_strengths: [0; 8],
            cdef_uv_strengths: [0; 8],
            lr_type: [0; 3],
            lr_unit_shift: 0,
            lr_uv_shift: 0,
            tx_mode: 0,
            reference_select: false,
            skip_mode_present: false,
            allow_warped_motion: false,
            reduced_tx_set: false,
            gm_type: [IDENTITY; 8],
            gm_params: [IDENTITY_PARAMS; 8],
            gm_invalid: [false; 8],
            film_grain: FilmGrain::default(),
        }
    }
}

impl FrameHeader {
    /// Whether the frame is intra only (a key or intra-only frame).
    pub(crate) fn is_intra(&self) -> bool {
        self.frame_type == KEY_FRAME || self.frame_type == INTRA_ONLY_FRAME
    }
}

/// One tile: where it lies in its tile group's data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tile {
    pub(crate) offset: u32,
    pub(crate) size: u32,
    pub(crate) row: u16,
    pub(crate) column: u16,
}

/// A tile group: its tile data's range in the sample, and its tiles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TileGroup {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) tg_start: u16,
    pub(crate) tg_end: u16,
    pub(crate) tiles: Vec<Tile>,
}

/// What a sample holds, frame by frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub(crate) header: FrameHeader,
    /// Empty for `show_existing_frame`.
    pub(crate) tile_groups: Vec<TileGroup>,
}

/// Reads a stream's headers, keeping what carries from frame to frame.
#[derive(Debug, Default)]
pub(crate) struct Parser {
    sequence: Option<SequenceHeader>,
    slots: [RefSlot; NUM_REF_FRAMES],
}

/// `tile_log2(blkSize, target)` (§5.9.15).
fn tile_log2(blk: u32, target: u32) -> u32 {
    let mut k = 0;
    while (blk << k) < target {
        k += 1;
    }
    k
}

impl Parser {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The sequence header, once one is read.
    pub(crate) fn sequence(&self) -> Option<&SequenceHeader> {
        self.sequence.as_ref()
    }

    /// Whether `sample` starts with a shown key frame, which decoding can start at. It
    /// reads only each frame header's first bits, with the sequence header the sample
    /// carries, or else the last one read.
    pub(crate) fn is_key_frame(&self, sample: &[u8]) -> bool {
        let Ok(obus) = obus(sample) else {
            return false;
        };
        let mut reduced = self
            .sequence
            .as_ref()
            .map(|s| s.reduced_still_picture_header);
        for obu in obus {
            let first = sample.get(obu.start).copied().unwrap_or(0);
            match obu.kind {
                // seq_profile (3 bits), still_picture, reduced_still_picture_header.
                OBU_SEQUENCE_HEADER => reduced = Some(first >> 3 & 1 == 1),
                OBU_FRAME_HEADER | OBU_FRAME => {
                    return match reduced {
                        Some(true) => true,
                        // show_existing_frame 0, frame_type KEY_FRAME (0), show_frame 1.
                        Some(false) => first & 0xF0 == 0x10,
                        None => false,
                    };
                }
                _ => {}
            }
        }
        false
    }

    /// Forgets the reference slots, for a seek; the sequence header stays.
    pub(crate) fn reset(&mut self) {
        self.slots = [RefSlot::default(); NUM_REF_FRAMES];
    }

    /// Reads a sample's OBUs: its sequence header if it has one, and its frames.
    ///
    /// # Errors
    /// [`ParseError::Truncated`] and [`ParseError::Invalid`] for a damaged sample,
    /// [`ParseError::Unsupported`] for large-scale tiles or a spatial layer, and
    /// [`ParseError::Invalid`] for a frame before any sequence header or one that
    /// references an empty slot.
    pub(crate) fn parse(&mut self, sample: &[u8]) -> Result<Vec<Frame>> {
        let mut frames: Vec<Frame> = Vec::new();
        // The frame whose tile groups are still to come.
        let mut open: Option<Frame> = None;
        for obu in obus(sample)? {
            let payload = &sample[obu.start..obu.end];
            if obu.spatial_id > 0 {
                return Err(ParseError::Unsupported("an AV1 stream with spatial layers"));
            }
            match obu.kind {
                OBU_SEQUENCE_HEADER => {
                    let mut r = Reader::new(payload);
                    let sequence = SequenceHeader::read(&mut r)?;
                    if self.sequence.as_ref().is_some_and(|s| *s != sequence) {
                        self.reset();
                    }
                    self.sequence = Some(sequence);
                }
                OBU_FRAME_HEADER | OBU_FRAME => {
                    let mut r = Reader::new(payload);
                    let header = self.frame_header(&mut r)?;
                    if header.show_existing.is_some() {
                        frames.push(Frame {
                            header,
                            tile_groups: Vec::new(),
                        });
                        continue;
                    }
                    let mut frame = Frame {
                        header,
                        tile_groups: Vec::new(),
                    };
                    if obu.kind == OBU_FRAME {
                        r.byte_alignment()?;
                        let done =
                            self.tile_group(&mut r, &mut frame, sample, obu.start, obu.end)?;
                        if done {
                            frames.push(frame);
                        } else {
                            open = Some(frame);
                        }
                    } else {
                        open = Some(frame);
                    }
                }
                OBU_TILE_GROUP => {
                    let Some(mut frame) = open.take() else {
                        return Err(ParseError::Invalid("an AV1 tile group without its frame"));
                    };
                    let mut r = Reader::new(payload);
                    let done = self.tile_group(&mut r, &mut frame, sample, obu.start, obu.end)?;
                    if done {
                        frames.push(frame);
                    } else {
                        open = Some(frame);
                    }
                }
                OBU_TILE_LIST => {
                    return Err(ParseError::Unsupported("AV1 large-scale tiles"));
                }
                // Temporal delimiters, redundant frame headers (copies of the one
                // read), metadata, padding.
                _ => {}
            }
        }
        if open.is_some() {
            return Err(ParseError::Truncated);
        }
        Ok(frames)
    }

    /// Reads a tile group's header at `r` (its tile data starting at `sample[at..]`
    /// once aligned), and adds its tiles to `frame`. True when it holds the frame's
    /// last tile.
    fn tile_group(
        &self,
        r: &mut Reader<'_>,
        frame: &mut Frame,
        sample: &[u8],
        obu_start: usize,
        end: usize,
    ) -> Result<bool> {
        let h = &frame.header;
        let num_tiles = h.tile_cols * h.tile_rows;
        let mut tg_start = 0;
        let mut tg_end = num_tiles - 1;
        if num_tiles > 1 && r.flag()? {
            let bits = h.tile_cols_log2 + h.tile_rows_log2;
            tg_start = usize::try_from(r.f(bits)?).unwrap_or(usize::MAX);
            tg_end = usize::try_from(r.f(bits)?).unwrap_or(usize::MAX);
        }
        let expected = frame
            .tile_groups
            .last()
            .map_or(0, |g| usize::from(g.tg_end) + 1);
        if tg_start != expected || tg_end < tg_start || tg_end >= num_tiles {
            return Err(ParseError::Invalid("an AV1 tile group's tiles"));
        }
        r.byte_alignment()?;
        let start = obu_start + r.bits.position() / 8;
        let data = sample.get(start..end).ok_or(ParseError::Truncated)?;
        let mut tiles = Vec::with_capacity(tg_end - tg_start + 1);
        let mut at = 0_usize;
        for n in tg_start..=tg_end {
            let row = u16::try_from(n / h.tile_cols).unwrap_or(0);
            let column = u16::try_from(n % h.tile_cols).unwrap_or(0);
            let size = if n == tg_end {
                data.len() - at
            } else {
                let bytes = data
                    .get(at..at + h.tile_size_bytes)
                    .ok_or(ParseError::Truncated)?;
                let mut size = 0_usize;
                for (i, b) in bytes.iter().enumerate() {
                    size |= usize::from(*b) << (8 * i);
                }
                at += h.tile_size_bytes;
                if size >= data.len() - at {
                    return Err(ParseError::Invalid("an AV1 tile size"));
                }
                size + 1
            };
            tiles.push(Tile {
                offset: u32::try_from(at).unwrap_or(u32::MAX),
                size: u32::try_from(size).unwrap_or(u32::MAX),
                row,
                column,
            });
            at += size;
        }
        frame.tile_groups.push(TileGroup {
            start,
            end,
            tg_start: u16::try_from(tg_start).unwrap_or(0),
            tg_end: u16::try_from(tg_end).unwrap_or(0),
            tiles,
        });
        Ok(tg_end == num_tiles - 1)
    }

    /// `uncompressed_header()` (§5.9.2), and the reference update after it (§7.20).
    #[allow(
        clippy::too_many_lines,
        reason = "one syntax structure, in the spec's order"
    )]
    fn frame_header(&mut self, r: &mut Reader<'_>) -> Result<FrameHeader> {
        let seq = self.sequence.clone().ok_or(ParseError::Invalid(
            "an AV1 frame before any sequence header",
        ))?;
        let mut h = FrameHeader::default();
        let all_frames = u8::MAX;
        if seq.reduced_still_picture_header {
            h.frame_type = KEY_FRAME;
            h.show_frame = true;
        } else {
            if r.flag()? {
                let slot = usize::try_from(r.f(3)?).unwrap_or(0);
                let saved = self.slots[slot];
                if !saved.valid {
                    return Err(ParseError::Invalid("an AV1 frame shown from an empty slot"));
                }
                if seq.decoder_model_info_present && !seq.equal_picture_interval {
                    r.f(seq.frame_presentation_time_length)?;
                }
                if seq.frame_id_numbers_present {
                    r.f(seq.frame_id_length)?;
                }
                h.show_existing = Some(slot);
                h.frame_type = saved.frame_type;
                h.upscaled_width = saved.upscaled_width;
                h.frame_width = saved.frame_width;
                h.frame_height = saved.frame_height;
                h.order_hint = saved.order_hint;
                h.film_grain = saved.film_grain;
                if h.frame_type == KEY_FRAME {
                    // §7.21: the frame is loaded, and refreshes every slot.
                    h.refresh_frame_flags = all_frames;
                    for s in &mut self.slots {
                        *s = saved;
                    }
                }
                return Ok(h);
            }
            h.frame_type = r.f8(2)?;
            h.show_frame = r.flag()?;
            if h.show_frame && seq.decoder_model_info_present && !seq.equal_picture_interval {
                r.f(seq.frame_presentation_time_length)?;
            }
            h.showable_frame = if h.show_frame {
                h.frame_type != KEY_FRAME
            } else {
                r.flag()?
            };
            h.error_resilient_mode = h.frame_type == SWITCH_FRAME
                || (h.frame_type == KEY_FRAME && h.show_frame)
                || r.flag()?;
        }
        let intra = h.is_intra();
        if h.frame_type == KEY_FRAME && h.show_frame {
            for s in &mut self.slots {
                s.valid = false;
                s.order_hint = 0;
            }
        }
        h.disable_cdf_update = r.flag()?;
        h.allow_screen_content_tools = if seq.seq_force_screen_content_tools == SELECT {
            r.flag()?
        } else {
            seq.seq_force_screen_content_tools == 1
        };
        h.force_integer_mv = if h.allow_screen_content_tools {
            if seq.seq_force_integer_mv == SELECT {
                r.flag()?
            } else {
                seq.seq_force_integer_mv == 1
            }
        } else {
            false
        };
        if intra {
            h.force_integer_mv = true;
        }
        let mut current_frame_id = 0;
        if seq.frame_id_numbers_present {
            current_frame_id = r.f(seq.frame_id_length)?;
            let diff = 1_u32 << seq.delta_frame_id_length;
            for s in &mut self.slots {
                let stale = if current_frame_id > diff {
                    s.frame_id > current_frame_id || s.frame_id < current_frame_id - diff
                } else {
                    s.frame_id > current_frame_id
                        && s.frame_id < (1 << seq.frame_id_length) + current_frame_id - diff
                };
                if stale {
                    s.valid = false;
                }
            }
        }
        let frame_size_override = if h.frame_type == SWITCH_FRAME {
            true
        } else if seq.reduced_still_picture_header {
            false
        } else {
            r.flag()?
        };
        h.order_hint = r.f(seq.order_hint_bits)?;
        h.primary_ref_frame = if intra || h.error_resilient_mode {
            PRIMARY_REF_NONE
        } else {
            r.f8(3)?
        };
        if seq.decoder_model_info_present && r.flag()? {
            for &(idc, model) in &seq.operating_points {
                if model {
                    // Temporal and spatial layer 0 (layers aren't decoded here).
                    let in_temporal = idc & 1 == 1;
                    let in_spatial = idc >> 8 & 1 == 1;
                    if idc == 0 || (in_temporal && in_spatial) {
                        r.f(seq.buffer_removal_time_length)?;
                    }
                }
            }
        }
        h.refresh_frame_flags =
            if h.frame_type == SWITCH_FRAME || (h.frame_type == KEY_FRAME && h.show_frame) {
                all_frames
            } else {
                r.f8(8)?
            };
        if (!intra || h.refresh_frame_flags != all_frames) && seq.enable_order_hint {
            for s in &mut self.slots {
                let hint = if h.error_resilient_mode {
                    r.f(seq.order_hint_bits)?
                } else {
                    s.order_hint
                };
                if hint != s.order_hint {
                    // §7.20: a slot whose frame the encoder no longer has.
                    *s = RefSlot {
                        order_hint: hint,
                        ..RefSlot::default()
                    };
                }
            }
        }
        // The reference this frame loads from.
        let render;
        if intra {
            render = self.frame_size(r, &seq, &mut h, frame_size_override)?;
            h.allow_intrabc =
                h.allow_screen_content_tools && h.upscaled_width == h.frame_width && r.flag()?;
        } else {
            let short = seq.enable_order_hint && r.flag()?;
            if short {
                let last = r.f8(3)?;
                let golden = r.f8(3)?;
                h.ref_frame_idx = self.set_frame_refs(&seq, h.order_hint, last, golden);
            }
            for i in 0..REFS_PER_FRAME {
                if !short {
                    h.ref_frame_idx[i] = r.f8(3)?;
                }
                if seq.frame_id_numbers_present {
                    r.f(seq.delta_frame_id_length)?;
                }
                if !self.slots[usize::from(h.ref_frame_idx[i])].valid {
                    return Err(ParseError::Invalid(
                        "an AV1 frame referencing an empty slot",
                    ));
                }
            }
            if frame_size_override && !h.error_resilient_mode {
                let mut found = None;
                for i in 0..REFS_PER_FRAME {
                    if r.flag()? {
                        found = Some(self.slots[usize::from(h.ref_frame_idx[i])]);
                        break;
                    }
                }
                match found {
                    Some(s) => {
                        h.upscaled_width = s.upscaled_width;
                        h.frame_width = s.upscaled_width;
                        h.frame_height = s.frame_height;
                        render = (s.render_width, s.render_height);
                        Self::superres(r, &seq, &mut h)?;
                    }
                    None => render = self.frame_size(r, &seq, &mut h, frame_size_override)?,
                }
            } else {
                render = self.frame_size(r, &seq, &mut h, frame_size_override)?;
            }
            h.allow_high_precision_mv = !h.force_integer_mv && r.flag()?;
            h.interpolation_filter = if r.flag()? { 4 } else { r.f8(2)? };
            h.is_motion_mode_switchable = r.flag()?;
            h.use_ref_frame_mvs =
                !h.error_resilient_mode && seq.enable_ref_frame_mvs && r.flag()?;
        }
        h.disable_frame_end_update_cdf =
            seq.reduced_still_picture_header || h.disable_cdf_update || r.flag()?;

        // load_previous() or setup_past_independence().
        let primary = (h.primary_ref_frame != PRIMARY_REF_NONE)
            .then(|| self.slots[usize::from(h.ref_frame_idx[usize::from(h.primary_ref_frame)])]);
        let previous = primary.unwrap_or_default();
        h.loop_filter_ref_deltas = previous.loop_filter_ref_deltas;
        h.loop_filter_mode_deltas = previous.loop_filter_mode_deltas;

        self.tile_info(r, &seq, &mut h)?;
        Self::quantization_params(r, &seq, &mut h)?;
        Self::segmentation_params(r, &mut h, &previous)?;
        h.delta_q_present = h.base_q_idx > 0 && r.flag()?;
        if h.delta_q_present {
            h.delta_q_res = r.f8(2)?;
            h.delta_lf_present = !h.allow_intrabc && r.flag()?;
            if h.delta_lf_present {
                h.delta_lf_res = r.f8(2)?;
                h.delta_lf_multi = r.flag()?;
            }
        }
        h.coded_lossless = (0..MAX_SEGMENTS).all(|i| {
            let q = if h.feature_enabled[i][SEG_LVL_ALT_Q] {
                i32::from(h.base_q_idx) + i32::from(h.feature_value[i][SEG_LVL_ALT_Q])
            } else {
                i32::from(h.base_q_idx)
            };
            q.clamp(0, 255) == 0
                && h.delta_q_y_dc == 0
                && h.delta_q_u_ac == 0
                && h.delta_q_u_dc == 0
                && h.delta_q_v_ac == 0
                && h.delta_q_v_dc == 0
        });
        let all_lossless = h.coded_lossless && h.frame_width == h.upscaled_width;
        let planes = if seq.color.mono_chrome { 1 } else { 3 };
        Self::loop_filter_params(r, planes, &mut h)?;
        Self::cdef_params(r, &seq, planes, &mut h)?;
        Self::lr_params(r, &seq, planes, all_lossless, &mut h)?;
        h.tx_mode = if h.coded_lossless {
            0
        } else if r.increment(1, 2)? == 2 {
            2
        } else {
            1
        };
        h.reference_select = !intra && r.flag()?;
        h.skip_mode_present = self.skip_mode_allowed(&seq, &h) && r.flag()?;
        h.allow_warped_motion =
            !intra && !h.error_resilient_mode && seq.enable_warped_motion && r.flag()?;
        h.reduced_tx_set = r.flag()?;
        self.global_motion_params(r, &mut h, &previous)?;
        self.film_grain_params(r, &seq, &mut h)?;

        // §7.20: the slots this frame refreshes.
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if h.refresh_frame_flags >> i & 1 == 1 {
                *slot = RefSlot {
                    valid: true,
                    frame_id: current_frame_id,
                    upscaled_width: h.upscaled_width,
                    frame_width: h.frame_width,
                    frame_height: h.frame_height,
                    render_width: render.0,
                    render_height: render.1,
                    frame_type: h.frame_type,
                    order_hint: h.order_hint,
                    loop_filter_ref_deltas: h.loop_filter_ref_deltas,
                    loop_filter_mode_deltas: h.loop_filter_mode_deltas,
                    feature_enabled: h.feature_enabled,
                    feature_value: h.feature_value,
                    gm_params: h.gm_params,
                    film_grain: h.film_grain,
                };
            }
        }
        Ok(h)
    }

    /// `frame_size()` and `render_size()`: the render size.
    fn frame_size(
        &self,
        r: &mut Reader<'_>,
        seq: &SequenceHeader,
        h: &mut FrameHeader,
        override_flag: bool,
    ) -> Result<(u32, u32)> {
        if override_flag {
            h.upscaled_width = r.f(seq.frame_width_bits)? + 1;
            h.frame_height = r.f(seq.frame_height_bits)? + 1;
        } else {
            h.upscaled_width = seq.max_frame_width;
            h.frame_height = seq.max_frame_height;
        }
        h.frame_width = h.upscaled_width;
        Self::superres(r, seq, h)?;
        if r.flag()? {
            let w = r.f(16)? + 1;
            let hh = r.f(16)? + 1;
            Ok((w, hh))
        } else {
            Ok((h.upscaled_width, h.frame_height))
        }
    }

    /// `superres_params()`.
    fn superres(r: &mut Reader<'_>, seq: &SequenceHeader, h: &mut FrameHeader) -> Result<()> {
        h.use_superres = seq.enable_superres && r.flag()?;
        h.superres_denom = if h.use_superres { r.f8(3)? + 9 } else { 8 };
        let denom = u32::from(h.superres_denom);
        h.frame_width = (h.upscaled_width * 8 + denom / 2) / denom;
        Ok(())
    }

    /// `set_frame_refs()` (§7.8): the references a frame that signals only LAST and
    /// GOLDEN uses, chosen by order hint.
    fn set_frame_refs(
        &self,
        seq: &SequenceHeader,
        order_hint: u32,
        last: u8,
        golden: u8,
    ) -> [u8; REFS_PER_FRAME] {
        let mut idx: [Option<usize>; REFS_PER_FRAME] = [None; REFS_PER_FRAME];
        idx[0] = Some(usize::from(last));
        idx[3] = Some(usize::from(golden));
        let mut used = [false; NUM_REF_FRAMES];
        used[usize::from(last)] = true;
        used[usize::from(golden)] = true;
        let cur = 1_i32 << (seq.order_hint_bits - 1);
        let shifted: [i32; NUM_REF_FRAMES] =
            std::array::from_fn(|i| cur + seq.relative_dist(self.slots[i].order_hint, order_hint));
        let mut latest = shifted[usize::from(last)];
        let mut earliest = shifted[usize::from(golden)];
        // The unused slot whose hint `wanted` accepts, the last one that `better`
        // prefers over the hint kept in `keep`.
        let find = |used: &[bool; NUM_REF_FRAMES],
                    keep: &mut i32,
                    backward: bool,
                    better: fn(i32, i32) -> bool| {
            let mut found = None;
            for (i, &hint) in shifted.iter().enumerate() {
                if !used[i] && (hint >= cur) == backward && (found.is_none() || better(hint, *keep))
                {
                    found = Some(i);
                    *keep = hint;
                }
            }
            found
        };
        let later = |hint: i32, kept: i32| hint >= kept;
        let earlier = |hint: i32, kept: i32| hint < kept;
        // ALTREF: the latest backward reference; BWDREF and ALTREF2: the earliest.
        for (at, latest_wins) in [(6, true), (4, false), (5, false)] {
            let found = if latest_wins {
                find(&used, &mut latest, true, later)
            } else {
                find(&used, &mut earliest, true, earlier)
            };
            if let Some(i) = found {
                idx[at] = Some(i);
                used[i] = true;
            }
        }
        // LAST2, LAST3, BWDREF, ALTREF2, ALTREF still unset: the latest forward ones.
        for at in [1, 2, 4, 5, 6] {
            if idx[at].is_none()
                && let Some(i) = find(&used, &mut latest, false, later)
            {
                idx[at] = Some(i);
                used[i] = true;
            }
        }
        // The rest: the earliest of all.
        let mut rest = 0;
        for (i, &hint) in shifted.iter().enumerate() {
            if i == 0 || hint < earliest {
                rest = i;
                earliest = hint;
            }
        }
        idx.map(|i| u8::try_from(i.unwrap_or(rest)).unwrap_or(0))
    }

    /// `tile_info()` (§5.9.15).
    fn tile_info(
        &self,
        r: &mut Reader<'_>,
        seq: &SequenceHeader,
        h: &mut FrameHeader,
    ) -> Result<()> {
        let mi_cols = 2 * ((h.frame_width + 7) >> 3);
        let mi_rows = 2 * ((h.frame_height + 7) >> 3);
        let (sb_cols, sb_rows, sb_shift) = if seq.use_128x128_superblock {
            ((mi_cols + 31) >> 5, (mi_rows + 31) >> 5, 5)
        } else {
            ((mi_cols + 15) >> 4, (mi_rows + 15) >> 4, 4)
        };
        let sb_size = sb_shift + 2;
        let max_tile_width_sb = MAX_TILE_WIDTH >> sb_size;
        let mut max_tile_area_sb = MAX_TILE_AREA >> (2 * sb_size);
        let min_log2_tile_cols = tile_log2(max_tile_width_sb, sb_cols);
        let max_log2_tile_cols = tile_log2(1, sb_cols.min(64));
        let max_log2_tile_rows = tile_log2(1, sb_rows.min(64));
        let min_log2_tiles = min_log2_tile_cols.max(tile_log2(max_tile_area_sb, sb_rows * sb_cols));
        h.uniform_tile_spacing = r.flag()?;
        h.width_in_sbs.clear();
        h.height_in_sbs.clear();
        let to16 = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
        if h.uniform_tile_spacing {
            h.tile_cols_log2 = r.increment(min_log2_tile_cols, max_log2_tile_cols)?;
            let tile_width = (sb_cols + (1 << h.tile_cols_log2) - 1) >> h.tile_cols_log2;
            let cols = sb_cols.div_ceil(tile_width);
            for i in 0..cols {
                h.width_in_sbs.push(to16(if i + 1 < cols {
                    tile_width
                } else {
                    sb_cols - (cols - 1) * tile_width
                }));
            }
            let min_log2_tile_rows = min_log2_tiles.saturating_sub(h.tile_cols_log2);
            h.tile_rows_log2 = r.increment(min_log2_tile_rows, max_log2_tile_rows)?;
            let tile_height = (sb_rows + (1 << h.tile_rows_log2) - 1) >> h.tile_rows_log2;
            let rows = sb_rows.div_ceil(tile_height);
            for i in 0..rows {
                h.height_in_sbs.push(to16(if i + 1 < rows {
                    tile_height
                } else {
                    sb_rows - (rows - 1) * tile_height
                }));
            }
        } else {
            let mut widest = 0;
            let mut start = 0;
            while start < sb_cols && h.width_in_sbs.len() < MAX_TILE_COLS {
                let max_width = (sb_cols - start).min(max_tile_width_sb);
                let size = r.ns(max_width)? + 1;
                widest = widest.max(size);
                start += size;
                h.width_in_sbs.push(to16(size));
            }
            h.tile_cols_log2 = tile_log2(1, u32::try_from(h.width_in_sbs.len()).unwrap_or(0));
            max_tile_area_sb = if min_log2_tiles > 0 {
                (sb_rows * sb_cols) >> (min_log2_tiles + 1)
            } else {
                sb_rows * sb_cols
            };
            let max_tile_height_sb = (max_tile_area_sb / widest.max(1)).max(1);
            start = 0;
            while start < sb_rows && h.height_in_sbs.len() < MAX_TILE_ROWS {
                let max_height = (sb_rows - start).min(max_tile_height_sb);
                let size = r.ns(max_height)? + 1;
                start += size;
                h.height_in_sbs.push(to16(size));
            }
            h.tile_rows_log2 = tile_log2(1, u32::try_from(h.height_in_sbs.len()).unwrap_or(0));
        }
        h.tile_cols = h.width_in_sbs.len();
        h.tile_rows = h.height_in_sbs.len();
        if h.tile_cols == 0 || h.tile_rows == 0 {
            return Err(ParseError::Invalid("an AV1 frame with no tiles"));
        }
        if h.tile_cols_log2 > 0 || h.tile_rows_log2 > 0 {
            h.context_update_tile_id =
                u16::try_from(r.f(h.tile_cols_log2 + h.tile_rows_log2)?).unwrap_or(0);
            h.tile_size_bytes = usize::try_from(r.f(2)?).unwrap_or(0) + 1;
            if usize::from(h.context_update_tile_id) >= h.tile_cols * h.tile_rows {
                return Err(ParseError::Invalid("an AV1 context_update_tile_id"));
            }
        }
        Ok(())
    }

    /// `quantization_params()` (§5.9.12).
    fn quantization_params(
        r: &mut Reader<'_>,
        seq: &SequenceHeader,
        h: &mut FrameHeader,
    ) -> Result<()> {
        h.base_q_idx = r.f8(8)?;
        h.delta_q_y_dc = r.delta_q()?;
        if !seq.color.mono_chrome {
            let diff_uv_delta = seq.color.separate_uv_delta_q && r.flag()?;
            h.delta_q_u_dc = r.delta_q()?;
            h.delta_q_u_ac = r.delta_q()?;
            if diff_uv_delta {
                h.delta_q_v_dc = r.delta_q()?;
                h.delta_q_v_ac = r.delta_q()?;
            } else {
                h.delta_q_v_dc = h.delta_q_u_dc;
                h.delta_q_v_ac = h.delta_q_u_ac;
            }
        }
        h.using_qmatrix = r.flag()?;
        if h.using_qmatrix {
            h.qm_y = r.f8(4)?;
            h.qm_u = r.f8(4)?;
            h.qm_v = if seq.color.separate_uv_delta_q {
                r.f8(4)?
            } else {
                h.qm_u
            };
        }
        Ok(())
    }

    /// `segmentation_params()` (§5.9.14).
    fn segmentation_params(
        r: &mut Reader<'_>,
        h: &mut FrameHeader,
        previous: &RefSlot,
    ) -> Result<()> {
        h.segmentation_enabled = r.flag()?;
        if !h.segmentation_enabled {
            return Ok(());
        }
        if h.primary_ref_frame == PRIMARY_REF_NONE {
            h.segmentation_update_map = true;
            h.segmentation_update_data = true;
        } else {
            h.segmentation_update_map = r.flag()?;
            h.segmentation_temporal_update = h.segmentation_update_map && r.flag()?;
            h.segmentation_update_data = r.flag()?;
        }
        if !h.segmentation_update_data {
            h.feature_enabled = previous.feature_enabled;
            h.feature_value = previous.feature_value;
            return Ok(());
        }
        for i in 0..MAX_SEGMENTS {
            for j in 0..SEG_LVL_MAX {
                let enabled = r.flag()?;
                h.feature_enabled[i][j] = enabled;
                h.feature_value[i][j] = if enabled && FEATURE_BITS[j] > 0 {
                    let v = if FEATURE_SIGNED[j] {
                        r.su(1 + FEATURE_BITS[j])?
                    } else {
                        i32::try_from(r.f(FEATURE_BITS[j])?).unwrap_or(0)
                    };
                    i16::try_from(v).unwrap_or(0)
                } else {
                    0
                };
            }
        }
        Ok(())
    }

    /// `loop_filter_params()` (§5.9.11).
    fn loop_filter_params(r: &mut Reader<'_>, planes: u32, h: &mut FrameHeader) -> Result<()> {
        if h.coded_lossless || h.allow_intrabc {
            h.loop_filter_ref_deltas = DEFAULT_REF_DELTAS;
            h.loop_filter_mode_deltas = [0; 2];
            return Ok(());
        }
        h.loop_filter_level[0] = r.f8(6)?;
        h.loop_filter_level[1] = r.f8(6)?;
        if planes > 1 && (h.loop_filter_level[0] != 0 || h.loop_filter_level[1] != 0) {
            h.loop_filter_level[2] = r.f8(6)?;
            h.loop_filter_level[3] = r.f8(6)?;
        }
        h.loop_filter_sharpness = r.f8(3)?;
        h.loop_filter_delta_enabled = r.flag()?;
        if h.loop_filter_delta_enabled {
            h.loop_filter_delta_update = r.flag()?;
            if h.loop_filter_delta_update {
                for d in &mut h.loop_filter_ref_deltas {
                    if r.flag()? {
                        *d = i8::try_from(r.su(7)?).unwrap_or(0);
                    }
                }
                for d in &mut h.loop_filter_mode_deltas {
                    if r.flag()? {
                        *d = i8::try_from(r.su(7)?).unwrap_or(0);
                    }
                }
            }
        }
        Ok(())
    }

    /// `cdef_params()` (§5.9.19): each strength as VA-API packs it, primary × 4 +
    /// secondary.
    fn cdef_params(
        r: &mut Reader<'_>,
        seq: &SequenceHeader,
        planes: u32,
        h: &mut FrameHeader,
    ) -> Result<()> {
        if h.coded_lossless || h.allow_intrabc || !seq.enable_cdef {
            return Ok(());
        }
        h.cdef_damping_minus_3 = r.f8(2)?;
        h.cdef_bits = r.f8(2)?;
        for i in 0..1_usize << h.cdef_bits {
            let pri = r.f8(4)?;
            let sec = r.f8(2)?;
            h.cdef_y_strengths[i] = pri << 2 | sec;
            if planes > 1 {
                let pri = r.f8(4)?;
                let sec = r.f8(2)?;
                h.cdef_uv_strengths[i] = pri << 2 | sec;
            }
        }
        Ok(())
    }

    /// `lr_params()` (§5.9.20): `lr_type` as coded (0 none, 1 Wiener, 2 self-guided, 3
    /// switchable).
    fn lr_params(
        r: &mut Reader<'_>,
        seq: &SequenceHeader,
        planes: u32,
        all_lossless: bool,
        h: &mut FrameHeader,
    ) -> Result<()> {
        if all_lossless || h.allow_intrabc || !seq.enable_restoration {
            return Ok(());
        }
        let mut uses_lr = false;
        let mut uses_chroma_lr = false;
        for i in 0..usize::try_from(planes).unwrap_or(3) {
            h.lr_type[i] = r.f8(2)?;
            if h.lr_type[i] != 0 {
                uses_lr = true;
                uses_chroma_lr |= i > 0;
            }
        }
        if uses_lr {
            let min = u32::from(seq.use_128x128_superblock);
            h.lr_unit_shift = u8::try_from(r.increment(min, 2)?).unwrap_or(0);
            if seq.color.subsampling_x && seq.color.subsampling_y && uses_chroma_lr {
                h.lr_uv_shift = r.f8(1)?;
            }
        }
        Ok(())
    }

    /// Whether `skip_mode_present` is coded (§5.9.22).
    fn skip_mode_allowed(&self, seq: &SequenceHeader, h: &FrameHeader) -> bool {
        if h.is_intra() || !h.reference_select || !seq.enable_order_hint {
            return false;
        }
        let mut forward: Option<u32> = None;
        let mut backward: Option<u32> = None;
        for &i in &h.ref_frame_idx {
            let hint = self.slots[usize::from(i)].order_hint;
            let dist = seq.relative_dist(hint, h.order_hint);
            if dist < 0 {
                if forward.is_none_or(|f| seq.relative_dist(hint, f) > 0) {
                    forward = Some(hint);
                }
            } else if dist > 0 && backward.is_none_or(|b| seq.relative_dist(hint, b) < 0) {
                backward = Some(hint);
            }
        }
        let Some(forward) = forward else {
            return false;
        };
        if backward.is_some() {
            return true;
        }
        h.ref_frame_idx
            .iter()
            .any(|&i| seq.relative_dist(self.slots[usize::from(i)].order_hint, forward) < 0)
    }

    /// `global_motion_params()` (§5.9.24), decoded against the previous frame's
    /// parameters (§7.20), with each model's shear check (§7.11.3.6).
    fn global_motion_params(
        &self,
        r: &mut Reader<'_>,
        h: &mut FrameHeader,
        previous: &RefSlot,
    ) -> Result<()> {
        if h.is_intra() {
            return Ok(());
        }
        for reference in 1..=REFS_PER_FRAME {
            let kind = if r.flag()? {
                if r.flag()? {
                    ROTZOOM
                } else if r.flag()? {
                    TRANSLATION
                } else {
                    AFFINE
                }
            } else {
                IDENTITY
            };
            h.gm_type[reference] = kind;
            let mut read = |idx: usize, h: &mut FrameHeader| -> Result<()> {
                let (abs_bits, prec_bits) = if idx < 2 {
                    if kind == TRANSLATION {
                        let hp = u32::from(!h.allow_high_precision_mv);
                        (GM_ABS_TRANS_ONLY_BITS - hp, GM_TRANS_ONLY_PREC_BITS - hp)
                    } else {
                        (GM_ABS_TRANS_BITS, GM_TRANS_PREC_BITS)
                    }
                } else {
                    (GM_ABS_ALPHA_BITS, GM_ALPHA_PREC_BITS)
                };
                let prec_diff = WARPEDMODEL_PREC_BITS - prec_bits;
                let round = if idx % 3 == 2 {
                    1_i32 << WARPEDMODEL_PREC_BITS
                } else {
                    0
                };
                let sub = if idx % 3 == 2 { 1_i32 << prec_bits } else { 0 };
                let mx = 1_i32 << abs_bits;
                let reference_value = (previous.gm_params[reference][idx] >> prec_diff) - sub;
                let coded = r.subexp(u32::try_from(2 * mx + 1).unwrap_or(1))?;
                let v = decode_signed_subexp_with_ref(coded, -mx, mx + 1, reference_value);
                h.gm_params[reference][idx] = (v << prec_diff) + round;
                Ok(())
            };
            if kind >= ROTZOOM {
                read(2, h)?;
                read(3, h)?;
                if kind == AFFINE {
                    read(4, h)?;
                    read(5, h)?;
                } else {
                    h.gm_params[reference][4] = -h.gm_params[reference][3];
                    h.gm_params[reference][5] = h.gm_params[reference][2];
                }
            }
            if kind >= TRANSLATION {
                read(0, h)?;
                read(1, h)?;
            }
            h.gm_invalid[reference] = !shear_valid(&h.gm_params[reference]);
        }
        Ok(())
    }

    /// `film_grain_params()` (§5.9.30), with `load_grain_params()` for a frame that
    /// reuses another's.
    fn film_grain_params(
        &self,
        r: &mut Reader<'_>,
        seq: &SequenceHeader,
        h: &mut FrameHeader,
    ) -> Result<()> {
        if !seq.film_grain_params_present || (!h.show_frame && !h.showable_frame) {
            return Ok(());
        }
        let mut g = FilmGrain {
            apply_grain: r.flag()?,
            ..FilmGrain::default()
        };
        if !g.apply_grain {
            return Ok(());
        }
        g.grain_seed = u16::try_from(r.f(16)?).unwrap_or(0);
        g.update_grain = h.frame_type != INTER_FRAME || r.flag()?;
        if !g.update_grain {
            let slot = usize::try_from(r.f(3)?).unwrap_or(0);
            if !h
                .ref_frame_idx
                .contains(&u8::try_from(slot).unwrap_or(u8::MAX))
            {
                return Err(ParseError::Invalid("an AV1 film_grain_params_ref_idx"));
            }
            let seed = g.grain_seed;
            g = self.slots[slot].film_grain;
            g.grain_seed = seed;
            g.update_grain = false;
            h.film_grain = g;
            return Ok(());
        }
        g.num_y_points = r.f8(4)?;
        if g.num_y_points > 14 {
            return Err(ParseError::Invalid("an AV1 num_y_points"));
        }
        for i in 0..usize::from(g.num_y_points) {
            g.point_y_value[i] = r.f8(8)?;
            g.point_y_scaling[i] = r.f8(8)?;
        }
        g.chroma_scaling_from_luma = !seq.color.mono_chrome && r.flag()?;
        if !(seq.color.mono_chrome
            || g.chroma_scaling_from_luma
            || (seq.color.subsampling_x && seq.color.subsampling_y && g.num_y_points == 0))
        {
            g.num_cb_points = r.f8(4)?;
            if g.num_cb_points > 10 {
                return Err(ParseError::Invalid("an AV1 num_cb_points"));
            }
            for i in 0..usize::from(g.num_cb_points) {
                g.point_cb_value[i] = r.f8(8)?;
                g.point_cb_scaling[i] = r.f8(8)?;
            }
            g.num_cr_points = r.f8(4)?;
            if g.num_cr_points > 10 {
                return Err(ParseError::Invalid("an AV1 num_cr_points"));
            }
            for i in 0..usize::from(g.num_cr_points) {
                g.point_cr_value[i] = r.f8(8)?;
                g.point_cr_scaling[i] = r.f8(8)?;
            }
        }
        g.grain_scaling_minus_8 = r.f8(2)?;
        g.ar_coeff_lag = r.f8(2)?;
        let num_pos_luma = 2 * usize::from(g.ar_coeff_lag) * (usize::from(g.ar_coeff_lag) + 1);
        let num_pos_chroma = if g.num_y_points > 0 {
            for i in 0..num_pos_luma {
                g.ar_coeffs_y[i] = minus_128(r.f8(8)?);
            }
            num_pos_luma + 1
        } else {
            num_pos_luma
        };
        if g.chroma_scaling_from_luma || g.num_cb_points > 0 {
            for i in 0..num_pos_chroma {
                g.ar_coeffs_cb[i] = minus_128(r.f8(8)?);
            }
        }
        if g.chroma_scaling_from_luma || g.num_cr_points > 0 {
            for i in 0..num_pos_chroma {
                g.ar_coeffs_cr[i] = minus_128(r.f8(8)?);
            }
        }
        g.ar_coeff_shift_minus_6 = r.f8(2)?;
        g.grain_scale_shift = r.f8(2)?;
        if g.num_cb_points > 0 {
            g.cb_mult = r.f8(8)?;
            g.cb_luma_mult = r.f8(8)?;
            g.cb_offset = u16::try_from(r.f(9)?).unwrap_or(0);
        }
        if g.num_cr_points > 0 {
            g.cr_mult = r.f8(8)?;
            g.cr_luma_mult = r.f8(8)?;
            g.cr_offset = u16::try_from(r.f(9)?).unwrap_or(0);
        }
        g.overlap_flag = r.flag()?;
        g.clip_to_restricted_range = r.flag()?;
        h.film_grain = g;
        Ok(())
    }
}

/// A coded `ar_coeffs_*_plus_128`, less 128.
fn minus_128(v: u8) -> i8 {
    i8::try_from(i16::from(v) - 128).unwrap_or(0)
}

/// `inverse_recenter(r, v)` (§5.9.28).
fn inverse_recenter(r: i32, v: i32) -> i32 {
    if v > 2 * r {
        v
    } else if v & 1 == 1 {
        r - ((v + 1) >> 1)
    } else {
        r + (v >> 1)
    }
}

/// `decode_signed_subexp_with_ref` (§5.9.26), from the value `decode_subexp` read.
fn decode_signed_subexp_with_ref(coded: u32, low: i32, high: i32, r: i32) -> i32 {
    let mx = high - low;
    let r = r - low;
    let v = i32::try_from(coded).unwrap_or(0);
    let x = if (r << 1) <= mx {
        inverse_recenter(r, v)
    } else {
        mx - 1 - inverse_recenter(mx - 1 - r, v)
    };
    x + low
}

/// `Round2Signed`.
fn round2_signed(x: i64, n: u32) -> i64 {
    if n == 0 {
        return x;
    }
    let round = |x: i64| (x + (1_i64 << (n - 1))) >> n;
    if x < 0 { -round(-x) } else { round(x) }
}

/// `resolve_divisor` (§7.11.3.7): the divisor's reciprocal and its shift.
fn resolve_divisor(d: i64) -> (i64, u32) {
    let n = 63 - d.leading_zeros();
    let e = d - (1_i64 << n);
    let f = if n > DIV_LUT_BITS {
        round2_signed(e, n - DIV_LUT_BITS)
    } else {
        e << (DIV_LUT_BITS - n)
    };
    let f = usize::try_from(f).unwrap_or(0).min(DIV_LUT.len() - 1);
    (i64::from(DIV_LUT[f]), n + DIV_LUT_PREC_BITS)
}

/// The shear process's validity check (§7.11.3.6), as FFmpeg's hwaccel computes it.
fn shear_valid(p: &[i32; 6]) -> bool {
    if p[2] <= 0 {
        return false;
    }
    let clip16 = |v: i64| v.clamp(i64::from(i16::MIN), i64::from(i16::MAX));
    let alpha = clip16(i64::from(p[2]) - (1 << WARPEDMODEL_PREC_BITS));
    let beta = clip16(i64::from(p[3]));
    let (div_factor, div_shift) = resolve_divisor(i64::from(p[2]));
    let v = i64::from(p[4]) << WARPEDMODEL_PREC_BITS;
    let w = i64::from(p[3]) * i64::from(p[4]);
    let gamma = clip16(round2_signed(v.saturating_mul(div_factor), div_shift));
    let delta = clip16(
        i64::from(p[5])
            - round2_signed(w.saturating_mul(div_factor), div_shift)
            - (1 << WARPEDMODEL_PREC_BITS),
    );
    let reduce =
        |v: i64| clip16(round2_signed(v, WARP_PARAM_REDUCE_BITS) << WARP_PARAM_REDUCE_BITS);
    let (alpha, beta, gamma, delta) = (reduce(alpha), reduce(beta), reduce(gamma), reduce(delta));
    4 * alpha.abs() + 7 * beta.abs() < (1 << WARPEDMODEL_PREC_BITS)
        && 4 * gamma.abs() + 4 * delta.abs() < (1 << WARPEDMODEL_PREC_BITS)
}

#[cfg(test)]
mod tests;
