//! What a VA-API AV1 session is configured with, and the parameter buffers and packed
//! headers it submits for each picture (ADR-0060 §6).
//!
//! The stream is shaped like the H.264 and HEVC ones (`h264`, `hevc`): key frames and
//! inter frames only, each inter frame predicted from the one before, one tile and one
//! tile group per frame, so packets come out in presentation order with `dts == pts`.
//! Every keyframe is a shown key frame, every `keyframe_interval` frames or when one is
//! requested, and carries the sequence header so a viewer can join there. Order hints
//! are the frames since the key frame, modulo 256. The sequence header is profile 0
//! (Main), 8-bit 4:2:0, with BT.709 limited range, 64x64 superblocks, order hints and
//! CDEF on, and the other optional coding tools off.
//!
//! As FFmpeg's VA-API AV1 encoder does, JStream writes the sequence header OBU
//! ([`sequence_header`]) and each frame's frame header OBU ([`frame_header`]) as packed
//! headers, and the driver writes the tile group OBU after them. Its rate controller
//! picks each frame's quantizer, loop filter and CDEF strengths, so the frame header
//! records where those fields sit ([`PackedFrameHeader`]) and the driver rewrites them
//! there. OBU size fields are as long as the driver asks (`obu_size_bytes_minus1`), so
//! the driver can rewrite the frame header's in place. A packet is a temporal unit: a
//! temporal delimiter ([`TEMPORAL_DELIMITER`]), then the driver's output.
//!
//! What a driver codes is read from its `VAConfigAttribEncAV1Ext2` value into [`Tools`].
//!
//! Platform-independent: building the buffers needs no driver, so it is tested
//! everywhere, and the headers are read back with the crate's own AV1 parser
//! (`media::av1`) in the tests.

use jstream_platform::media::{Codec, PixelFormat};
use jstream_platform::{PlatformError, VideoEncoderConfig};

use super::ffi::{
    ATTRIB_NOT_SUPPORTED, FrameRateParam, HrdParam, Id, MiscParam, PicParamAv1, RateControlParam,
    SeqParamAv1, TileGroupAv1,
};
use super::h264::{BitWriter, Rate, frame_rate_buffer, hrd_buffer, rate, rate_control_buffer};

/// The largest key frame period, as for H.264 and HEVC.
const MAX_KEY_PERIOD: u32 = 1 << 30;
/// `OrderHintBits`.
const ORDER_HINT_BITS: u32 = 8;
/// The `base_q_idx` the frame header carries before the driver's rate controller
/// rewrites it. Above 0, so `delta_q_params()` is present, as FFmpeg writes it.
const BASE_Q_IDX: u8 = 128;
/// The rate controller's range for `base_q_idx`, as FFmpeg gives it: never 0, which
/// would make a frame lossless and change the header's layout.
const MIN_BASE_Q_IDX: u8 = 1;
const MAX_BASE_Q_IDX: u8 = 255;
/// `obu_type` values (AV1 §6.2.2).
const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_TEMPORAL_DELIMITER: u8 = 2;
const OBU_FRAME_HEADER: u8 = 3;
/// `frame_type` values.
const KEY_FRAME: u32 = 0;
const INTER_FRAME: u32 = 1;
/// `primary_ref_frame` when a frame loads no state from a reference.
const PRIMARY_REF_NONE: u8 = 7;
/// `REFS_PER_FRAME`.
const REFS_PER_FRAME: usize = 7;
/// `LAST_FRAME`, the reference searched first (`VARefFrameCtrlAV1`).
const LAST_FRAME: u32 = 1;
/// `TxMode` values.
const TX_MODE_LARGEST: u8 = 1;
const TX_MODE_SELECT: u8 = 2;
/// With 64x64 superblocks: `MAX_TILE_WIDTH >> 6` and `MAX_TILE_AREA >> 12` (§A.3,
/// §5.9.15), the most superblocks one tile may span across and in all.
const MAX_TILE_WIDTH_SB: u32 = 4096 >> 6;
const MAX_TILE_AREA_SB: u32 = (4096 * 2304) >> 12;
/// `color_primaries`, `transfer_characteristics` and `matrix_coefficients`: BT.709.
const CICP_BT709: u32 = 1;

/// A temporal delimiter OBU, with an empty size field: the start of every packet.
pub(crate) const TEMPORAL_DELIMITER: [u8; 2] = [OBU_TEMPORAL_DELIMITER << 3 | 0b10, 0];

/// What the driver codes, from its `VAConfigAttribEncAV1Ext2` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tools {
    /// The length of every OBU size field, in bytes: 1 to 4.
    pub(crate) obu_size_bytes: u32,
    /// `TxMode`: select if the driver can, else the largest.
    pub(crate) tx_mode: u8,
}

impl Tools {
    /// What a configuration is checked with before the driver is asked: 4-byte OBU
    /// sizes, as Intel's and AMD's drivers ask for, and transform mode select. The
    /// session checks it again with the driver's own.
    pub(crate) const DEFAULT: Self = Self {
        obu_size_bytes: 4,
        tx_mode: TX_MODE_SELECT,
    };

    /// The tools of a driver whose `VAConfigAttribEncAV1Ext2` value is `ext2`.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when the driver doesn't advertise the attribute,
    /// which says how long OBU size fields are, as FFmpeg refuses it, or names no
    /// transform mode JStream writes (select or largest).
    pub(crate) fn from_attributes(ext2: u32, name: &str) -> Result<Self, PlatformError> {
        if ext2 == ATTRIB_NOT_SUPPORTED {
            return Err(PlatformError::Unsupported {
                what: format!("AV1 encoding on {name}, which doesn't describe its OBUs"),
            });
        }
        // Bits 2-3: obu_size_bytes_minus1; bits 4-6: tx_mode_support (bit 0 ONLY_4X4,
        // bit 1 TX_MODE_LARGEST, bit 2 TX_MODE_SELECT).
        let obu_size_bytes = ((ext2 >> 2) & 3) + 1;
        let tx = (ext2 >> 4) & 7;
        let tx_mode = if tx & 4 != 0 {
            TX_MODE_SELECT
        } else if tx & 2 != 0 {
            TX_MODE_LARGEST
        } else {
            return Err(PlatformError::Unsupported {
                what: format!("AV1 encoding on {name}, which codes only 4x4 transforms"),
            });
        };
        Ok(Self {
            obu_size_bytes,
            tx_mode,
        })
    }
}

/// A checked configuration and everything derived from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    /// The frame size, even in both dimensions.
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// The surfaces' size: whole 64x64 superblocks, as FFmpeg allocates them.
    pub(crate) coded_width: u32,
    pub(crate) coded_height: u32,
    /// Superblocks across and down: the one tile's size.
    pub(crate) sb_cols: u32,
    pub(crate) sb_rows: u32,
    pub(crate) tools: Tools,
    /// `seq_level_idx`: `(major - 2) * 4 + minor`.
    pub(crate) level_idx: u8,
    pub(crate) rate: Rate,
    /// Frames from one key frame to the next.
    pub(crate) key_period: u32,
}

/// One row of AV1 Table A.1, Main tier: `seq_level_idx`, MaxPicSize, MaxHSize, MaxVSize,
/// MaxDisplayRate and MaxBitrate (in units of 1000 bits per second).
const LEVELS: [(u8, u64, u64, u64, u64, u64); 14] = [
    (0, 147_456, 2_048, 1_152, 4_423_680, 1_500),
    (1, 278_784, 2_816, 1_584, 8_363_520, 3_000),
    (4, 665_856, 4_352, 2_448, 19_975_680, 6_000),
    (5, 1_065_024, 5_504, 3_096, 31_950_720, 10_000),
    (8, 2_359_296, 6_144, 3_456, 70_778_880, 12_000),
    (9, 2_359_296, 6_144, 3_456, 141_557_760, 20_000),
    (12, 8_912_896, 8_192, 4_352, 267_386_880, 30_000),
    (13, 8_912_896, 8_192, 4_352, 534_773_760, 40_000),
    (14, 8_912_896, 8_192, 4_352, 1_069_547_520, 60_000),
    (15, 8_912_896, 8_192, 4_352, 1_069_547_520, 60_000),
    (16, 35_651_584, 16_384, 8_704, 1_069_547_520, 60_000),
    (17, 35_651_584, 16_384, 8_704, 2_139_095_040, 100_000),
    (18, 35_651_584, 16_384, 8_704, 4_278_190_080, 160_000),
    (19, 35_651_584, 16_384, 8_704, 4_278_190_080, 160_000),
];

/// The lowest Main-tier level that holds a `width` x `height` frame at `fps` with a
/// peak of `bps`, or `None` above level 6.3. One tile and one reference frame fit every
/// level's limits.
pub(crate) fn level(width: u32, height: u32, fps: (u32, u32), bps: u32) -> Option<u8> {
    let (w, h) = (u64::from(width), u64::from(height));
    let ps = w * h;
    let (num, den) = (u64::from(fps.0), u64::from(fps.1.max(1)));
    let rate = (ps * num).div_ceil(den);
    LEVELS
        .iter()
        .find(|&&(_, max_ps, max_w, max_h, max_rate, max_br)| {
            ps <= max_ps
                && w <= max_w
                && h <= max_h
                && rate <= max_rate
                && u64::from(bps) <= max_br * 1000
        })
        .map(|&(idx, ..)| idx)
}

/// `MiCols` or `MiRows` for a frame `size` samples across (§7.2): 4x4 units, rounded up
/// to whole 8x8 blocks.
fn mi_units(size: u32) -> u32 {
    2 * size.div_ceil(8)
}

/// Checks a configuration and derives the session's settings, for a driver that codes
/// `tools`.
///
/// # Errors
/// [`PlatformError::Unsupported`] for another codec, an input other than NV12, RGBA or
/// BGRA, odd dimensions, a keyframe interval of 0 or above 2^30, rate control other than
/// CBR and VBR, a zero bit rate, a VBR peak below its mean, a frame period with a zero
/// numerator, a frame that one tile can't hold (wider than 4096 or more than 2304
/// superblocks), and a size, frame rate or bit rate above level 6.3.
pub(crate) fn check(config: &VideoEncoderConfig, tools: Tools) -> Result<Settings, PlatformError> {
    let unsupported = |what: String| Err(PlatformError::Unsupported { what });
    if config.codec != Codec::Av1 {
        return unsupported(format!("{:?} from the AV1 encoder", config.codec));
    }
    if !matches!(
        config.input_format,
        PixelFormat::Nv12 | PixelFormat::Rgba8 | PixelFormat::Bgra8
    ) {
        return unsupported(format!(
            "AV1 from {:?} (the encoder takes NV12, RGBA or BGRA)",
            config.input_format
        ));
    }
    let (width, height) = (config.size.width(), config.size.height());
    if width % 2 != 0 || height % 2 != 0 {
        return unsupported(format!("AV1 at an odd size, {width}x{height}"));
    }
    if config.keyframe_interval == 0 || config.keyframe_interval > MAX_KEY_PERIOD {
        return unsupported(format!(
            "a keyframe interval of {}",
            config.keyframe_interval
        ));
    }
    let rate = rate(config)?;
    let sb_cols = mi_units(width).div_ceil(16);
    let sb_rows = mi_units(height).div_ceil(16);
    if sb_cols > MAX_TILE_WIDTH_SB || sb_cols * sb_rows > MAX_TILE_AREA_SB {
        return unsupported(format!(
            "AV1 at {width}x{height} (more than one tile; JStream codes one)"
        ));
    }
    let Some(level_idx) = level(width, height, rate.fps, rate.bits_per_second) else {
        return unsupported(format!(
            "AV1 at {width}x{height}, {}/{} fps and {} kb/s (above level 6.3)",
            rate.fps.0, rate.fps.1, rate.peak_kbps
        ));
    };
    Ok(Settings {
        width,
        height,
        coded_width: width.next_multiple_of(64),
        coded_height: height.next_multiple_of(64),
        sb_cols,
        sb_rows,
        tools,
        level_idx,
        rate,
        key_period: config.keyframe_interval,
    })
}

impl Settings {
    /// The largest coded picture to expect: an uncompressed one at the coded size,
    /// plus room for the headers.
    pub(crate) fn coded_buffer_size(&self) -> u32 {
        (self.coded_width * self.coded_height * 3 / 2).saturating_add(64 * 1024)
    }
}

/// One picture of the stream: what the GOP made of the next frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Picture {
    pub(crate) key: bool,
    /// The frames since the key frame, modulo 256.
    pub(crate) order_hint: u8,
    /// The reference slot the frame is stored in: 0 for a key frame (which fills all
    /// eight), then 1 and 0 in turn, as FFmpeg alternates them.
    pub(crate) slot: u8,
}

impl Picture {
    /// The slot an inter frame predicts from: the one the frame before was stored in.
    pub(crate) fn ref_slot(&self) -> u8 {
        self.slot ^ 1
    }
}

/// Decides each frame's picture type: a key frame every `key_period` frames, or at the
/// next frame after a request, which also restarts the cadence. Inter frames otherwise.
#[derive(Clone, Debug)]
pub(crate) struct Gop {
    key_period: u32,
    /// Frames since the last key frame, which is the next frame's index in its GOP.
    next: u32,
    slot: u8,
    started: bool,
    forced: bool,
}

impl Gop {
    pub(crate) fn new(s: &Settings) -> Self {
        Self {
            key_period: s.key_period,
            next: 0,
            slot: 0,
            started: false,
            forced: false,
        }
    }

    /// Makes the next frame a key frame.
    pub(crate) fn force_key(&mut self) {
        self.forced = true;
    }

    /// The next frame's picture.
    pub(crate) fn next_picture(&mut self) -> Picture {
        let key = self.forced || !self.started || self.next >= self.key_period;
        if key {
            self.started = true;
            self.forced = false;
            self.next = 0;
            self.slot = 0;
        } else {
            self.slot ^= 1;
        }
        let n = self.next;
        self.next += 1;
        Picture {
            key,
            order_hint: (n % 256) as u8,
            slot: self.slot,
        }
    }
}

/// `leb128()` of `value` in exactly `bytes` bytes, padded with continuation bits as
/// §4.10.5 allows.
fn leb128_fixed(value: u32, bytes: u32) -> Vec<u8> {
    (0..bytes)
        .map(|i| {
            let more = if i + 1 < bytes { 0x80 } else { 0 };
            ((value >> (7 * i)) & 0x7F) as u8 | more
        })
        .collect()
}

/// An OBU of `kind`: its header (no extension, with a size field), its size in
/// `size_bytes` bytes, and `payload`.
fn obu(kind: u8, payload: &[u8], size_bytes: u32) -> Vec<u8> {
    let size = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(1 + size_bytes as usize + payload.len());
    out.push(kind << 3 | 0b10);
    out.extend(leb128_fixed(size, size_bytes));
    out.extend_from_slice(payload);
    out
}

/// The sequence header OBU (§5.5), sent as the packed sequence header on key frames.
pub(crate) fn sequence_header(s: &Settings) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.bits(0, 3); // seq_profile: Main
    w.flag(false); // still_picture
    w.flag(false); // reduced_still_picture_header
    w.flag(false); // timing_info_present_flag
    w.flag(false); // initial_display_delay_present_flag
    w.bits(0, 5); // operating_points_cnt_minus_1
    w.bits(0, 12); // operating_point_idc[0]
    w.bits(u32::from(s.level_idx), 5); // seq_level_idx[0]
    if s.level_idx > 7 {
        w.flag(false); // seq_tier[0]: Main
    }
    // As FFmpeg sizes them: enough bits for the width itself.
    let width_bits = u32::BITS - s.width.leading_zeros();
    let height_bits = u32::BITS - s.height.leading_zeros();
    w.bits(width_bits - 1, 4); // frame_width_bits_minus_1
    w.bits(height_bits - 1, 4); // frame_height_bits_minus_1
    w.bits(s.width - 1, width_bits); // max_frame_width_minus_1
    w.bits(s.height - 1, height_bits); // max_frame_height_minus_1
    w.flag(false); // frame_id_numbers_present_flag
    w.flag(false); // use_128x128_superblock
    w.flag(false); // enable_filter_intra
    w.flag(false); // enable_intra_edge_filter
    w.flag(false); // enable_interintra_compound
    w.flag(false); // enable_masked_compound
    w.flag(false); // enable_warped_motion
    w.flag(false); // enable_dual_filter
    w.flag(true); // enable_order_hint
    w.flag(false); // enable_jnt_comp
    w.flag(false); // enable_ref_frame_mvs
    // seq_choose_screen_content_tools 0, seq_force_screen_content_tools 0: no screen
    // content tools, and seq_force_integer_mv isn't written.
    w.flag(false);
    w.flag(false);
    w.bits(ORDER_HINT_BITS - 1, 3); // order_hint_bits_minus_1
    w.flag(false); // enable_superres
    w.flag(true); // enable_cdef
    w.flag(false); // enable_restoration
    // color_config(): 8-bit, not monochrome, BT.709 limited range, 4:2:0.
    w.flag(false); // high_bitdepth
    w.flag(false); // mono_chrome
    w.flag(true); // color_description_present_flag
    w.bits(CICP_BT709, 8); // color_primaries
    w.bits(CICP_BT709, 8); // transfer_characteristics
    w.bits(CICP_BT709, 8); // matrix_coefficients
    w.flag(false); // color_range: limited
    w.bits(0, 2); // chroma_sample_position: unknown
    w.flag(false); // separate_uv_delta_q
    w.flag(false); // film_grain_params_present
    w.trailing();
    obu(OBU_SEQUENCE_HEADER, &w.into_bytes(), s.tools.obu_size_bytes)
}

/// A frame header OBU and where in it the driver rewrites what its rate controller
/// chooses, in bits from the OBU's first byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackedFrameHeader {
    pub(crate) bytes: Vec<u8>,
    /// `base_q_idx`.
    pub(crate) qindex: u32,
    /// `loop_filter_params()`, from `loop_filter_level[0]`.
    pub(crate) loop_filter: u32,
    /// `cdef_params()`, from `cdef_damping_minus_3`, and its length.
    pub(crate) cdef: u32,
    pub(crate) cdef_bits: u32,
}

/// The frame header OBU (§5.9) for `p`, sent as the packed picture header.
pub(crate) fn frame_header(s: &Settings, p: &Picture) -> PackedFrameHeader {
    let mut w = BitWriter::default();
    w.flag(false); // show_existing_frame
    w.bits(if p.key { KEY_FRAME } else { INTER_FRAME }, 2); // frame_type
    w.flag(true); // show_frame
    // A shown key frame is error resilient without saying so; an inter frame isn't.
    if !p.key {
        w.flag(false); // error_resilient_mode
    }
    w.flag(false); // disable_cdf_update
    // allow_screen_content_tools is 0 from the sequence header.
    w.flag(false); // frame_size_override_flag
    w.bits(u32::from(p.order_hint), ORDER_HINT_BITS); // order_hint
    if !p.key {
        // primary_ref_frame: LAST_FRAME's slot, whose state the frame loads.
        w.bits(0, 3);
        // refresh_frame_flags: a key frame refreshes all eight without saying so.
        w.bits(1 << p.slot, 8);
        w.flag(false); // frame_refs_short_signaling
        for _ in 0..REFS_PER_FRAME {
            w.bits(u32::from(p.ref_slot()), 3); // ref_frame_idx[i]
        }
    }
    // frame_size(): no override, so the sequence header's size; superres_params(): off.
    w.flag(false); // render_and_frame_size_different
    if !p.key {
        w.flag(false); // allow_high_precision_mv
        w.flag(false); // is_filter_switchable
        w.bits(0, 2); // interpolation_filter: EIGHTTAP
        w.flag(false); // is_motion_mode_switchable
        // use_ref_frame_mvs is 0 from the sequence header.
    }
    w.flag(false); // disable_frame_end_update_cdf
    tile_info(&mut w, s);
    // quantization_params()
    let qindex = w.position();
    w.bits(u32::from(BASE_Q_IDX), 8); // base_q_idx
    w.flag(false); // DeltaQYDc: delta_coded
    // separate_uv_delta_q is 0, so no diff_uv_delta.
    w.flag(false); // DeltaQUDc: delta_coded
    w.flag(false); // DeltaQUAc: delta_coded
    w.flag(false); // using_qmatrix
    w.flag(false); // segmentation_enabled
    w.flag(false); // delta_q_present (base_q_idx > 0)
    // loop_filter_params(): levels 0, so no chroma levels; no delta updates.
    let loop_filter = w.position();
    w.bits(0, 6); // loop_filter_level[0]
    w.bits(0, 6); // loop_filter_level[1]
    w.bits(0, 3); // loop_filter_sharpness
    w.flag(false); // loop_filter_delta_enabled
    // cdef_params(): one strength.
    let cdef = w.position();
    w.bits(0, 2); // cdef_damping_minus_3
    w.bits(0, 2); // cdef_bits
    w.bits(0, 4); // cdef_y_pri_strength[0]
    w.bits(0, 2); // cdef_y_sec_strength[0]
    w.bits(0, 4); // cdef_uv_pri_strength[0]
    w.bits(0, 2); // cdef_uv_sec_strength[0]
    let cdef_bits = w.position() - cdef;
    // lr_params(): restoration is off.
    w.flag(s.tools.tx_mode == TX_MODE_SELECT); // tx_mode_select
    if !p.key {
        w.flag(false); // reference_select
        // skip_mode_params(): not allowed without reference_select.
        // allow_warped_motion is 0 from the sequence header.
    }
    w.flag(false); // reduced_tx_set
    if !p.key {
        // global_motion_params(): is_global 0 for LAST_FRAME to ALTREF_FRAME.
        w.bits(0, 7);
    }
    // film_grain_params(): absent from the sequence header.
    w.trailing();
    let payload = w.into_bytes();
    // Offsets count from the OBU's first byte: its header, then its size field.
    let lead = 8 * (1 + s.tools.obu_size_bytes);
    PackedFrameHeader {
        bytes: obu(OBU_FRAME_HEADER, &payload, s.tools.obu_size_bytes),
        qindex: lead + qindex,
        loop_filter: lead + loop_filter,
        cdef: lead + cdef,
        cdef_bits,
    }
}

/// `tile_info()` (§5.9.15) for one uniformly spaced tile.
fn tile_info(w: &mut BitWriter, s: &Settings) {
    let tile_log2 = |blk: u32, target: u32| -> u32 {
        let mut k = 0;
        while (blk << k) < target {
            k += 1;
        }
        k
    };
    w.flag(true); // uniform_tile_spacing_flag
    let min_cols_log2 = tile_log2(MAX_TILE_WIDTH_SB, s.sb_cols);
    let max_cols_log2 = tile_log2(1, s.sb_cols.min(64));
    if min_cols_log2 < max_cols_log2 {
        w.flag(false); // increment_tile_cols_log2: stay at the minimum
    }
    let max_rows_log2 = tile_log2(1, s.sb_rows.min(64));
    let min_tiles_log2 = min_cols_log2.max(tile_log2(MAX_TILE_AREA_SB, s.sb_cols * s.sb_rows));
    let min_rows_log2 = min_tiles_log2.saturating_sub(min_cols_log2);
    if min_rows_log2 < max_rows_log2 {
        w.flag(false); // increment_tile_rows_log2: stay at the minimum
    }
    // `check` keeps both logs at 0: one tile, so neither context_update_tile_id nor
    // tile_size_bytes_minus_1 is written.
}

/// The `seq_fields` union of `VAEncSequenceParameterBufferAV1`, matching
/// [`sequence_header`]: order hints and CDEF on, 8-bit 4:2:0.
pub(crate) fn seq_fields() -> u32 {
    let enable_order_hint = 1 << 8;
    let enable_cdef = 1 << 12;
    let subsampling_x = 1 << 17;
    let subsampling_y = 1 << 18;
    enable_order_hint | enable_cdef | subsampling_x | subsampling_y
}

/// `VAEncSequenceParameterBufferAV1`, matching [`sequence_header`].
pub(crate) fn sequence(s: &Settings) -> SeqParamAv1 {
    SeqParamAv1 {
        seq_profile: 0,
        seq_level_idx: s.level_idx,
        seq_tier: 0,
        hierarchical_flag: 0,
        intra_period: s.key_period,
        ip_period: 1,
        bits_per_second: s.rate.target_bits_per_second,
        seq_fields: seq_fields(),
        order_hint_bits_minus_1: (ORDER_HINT_BITS - 1) as u8,
        va_reserved: [0; 16],
    }
}

/// The `picture_flags` union of `VAEncPictureParameterBufferAV1`: the frame type, and
/// error resilience for a key frame, as its header implies.
pub(crate) fn picture_flags(key: bool) -> u32 {
    let frame_type = if key { KEY_FRAME } else { INTER_FRAME }; // bits 0-1
    let error_resilient_mode = u32::from(key) << 2;
    frame_type | error_resilient_mode
}

/// The `mode_control_flags` union of `VAEncPictureParameterBufferAV1`: the transform
/// mode, and single reference prediction (`reference_mode` 0).
pub(crate) fn mode_control_flags(t: &Tools) -> u32 {
    u32::from(t.tx_mode) << 7 // bits 7-8
}

/// `VAEncPictureParameterBufferAV1` for `p`, reconstructed into `recon`, predicted from
/// `reference` (the frame before, for an inter frame), coded into `coded`, with
/// `header` as its packed frame header. `sequence_bytes` is the length of the packed
/// sequence header sent before it, on a key frame.
pub(crate) fn picture(
    s: &Settings,
    p: &Picture,
    recon: Id,
    reference: Option<Id>,
    coded: Id,
    header: &PackedFrameHeader,
    sequence_bytes: usize,
) -> PicParamAv1 {
    let mut pic = PicParamAv1::EMPTY;
    pic.frame_width_minus_1 = u16::try_from(s.width - 1).unwrap_or(u16::MAX);
    pic.frame_height_minus_1 = u16::try_from(s.height - 1).unwrap_or(u16::MAX);
    pic.reconstructed_frame = recon;
    pic.coded_buf = coded;
    pic.order_hint = p.order_hint;
    if p.key {
        pic.primary_ref_frame = PRIMARY_REF_NONE;
        pic.refresh_frame_flags = 0xFF;
    } else {
        let slot = p.ref_slot();
        pic.primary_ref_frame = 0;
        pic.refresh_frame_flags = 1 << p.slot;
        pic.ref_frame_idx = [slot; REFS_PER_FRAME];
        if let Some(reference) = reference {
            pic.reference_frames[usize::from(slot)] = reference;
        }
        pic.ref_frame_ctrl_l0 = LAST_FRAME; // search_idx0
    }
    pic.picture_flags = picture_flags(p.key);
    pic.base_qindex = BASE_Q_IDX;
    pic.min_base_qindex = MIN_BASE_Q_IDX;
    pic.max_base_qindex = MAX_BASE_Q_IDX;
    pic.mode_control_flags = mode_control_flags(&s.tools);
    pic.tile_cols = 1;
    pic.tile_rows = 1;
    pic.width_in_sbs_minus_1[0] = u16::try_from(s.sb_cols - 1).unwrap_or(0);
    pic.height_in_sbs_minus_1[0] = u16::try_from(s.sb_rows - 1).unwrap_or(0);
    pic.bit_offset_qindex = header.qindex;
    pic.bit_offset_loopfilter_params = header.loop_filter;
    pic.bit_offset_cdef_params = header.cdef;
    pic.size_in_bits_cdef_params = header.cdef_bits;
    // The size field follows the frame header OBU's one-byte header, after the
    // sequence header when one is sent.
    pic.byte_offset_frame_hdr_obu_size =
        u32::try_from(if p.key { sequence_bytes } else { 0 } + 1).unwrap_or(u32::MAX);
    pic.size_in_bits_frame_hdr_obu = u32::try_from(header.bytes.len() * 8).unwrap_or(u32::MAX);
    pic.tile_group_obu_hdr_info = 0b10; // obu_has_size_field
    pic
}

/// `VAEncTileGroupBufferAV1`: the one tile group, of the one tile.
pub(crate) fn tile_group() -> TileGroupAv1 {
    TileGroupAv1::default()
}

/// The rate controller's peak and mean.
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

#[cfg(test)]
mod tests {
    use jstream_platform::media::{Codec, FrameSize, PixelFormat, Timebase};
    use jstream_platform::{HardwarePreference, RateControl, VideoEncoderConfig};

    use super::*;
    use crate::media::av1::Parser;

    fn config(width: u32, height: u32) -> VideoEncoderConfig {
        VideoEncoderConfig {
            codec: Codec::Av1,
            size: FrameSize::new(width, height).expect("a size"),
            input_format: PixelFormat::Nv12,
            frame_period: Timebase::new(1, 30).expect("a period"),
            rate_control: RateControl::Cbr { kbps: 6_000 },
            keyframe_interval: 60,
            hardware: HardwarePreference::Require,
        }
    }

    /// Mesa's radeonsi and Intel's iHD: 4-byte OBU sizes (`obu_size_bytes_minus1` 3)
    /// and every transform mode.
    const EXT2: u32 = 3 << 2 | 7 << 4;

    fn settings(width: u32, height: u32) -> Settings {
        let tools = Tools::from_attributes(EXT2, "test").expect("tools");
        check(&config(width, height), tools).expect("a configuration AV1 takes")
    }

    /// A temporal unit as the driver would leave it, with a tile group OBU of one tile
    /// after the packed headers.
    fn unit(s: &Settings, p: &Picture) -> Vec<u8> {
        let mut out = TEMPORAL_DELIMITER.to_vec();
        if p.key {
            out.extend(sequence_header(s));
        }
        out.extend(frame_header(s, p).bytes);
        // OBU_TILE_GROUP (4): one tile, so no tile_start_and_end_present_flag.
        out.extend(obu(4, &[0xA5, 0x5A, 0x11], 1));
        out
    }

    /// The `n` bits at bit `at` of `bytes`, most significant first.
    fn read_bits(bytes: &[u8], at: u32, n: u32) -> u32 {
        (at..at + n).fold(0, |v, i| {
            let bit = (bytes[(i / 8) as usize] >> (7 - i % 8)) & 1;
            v << 1 | u32::from(bit)
        })
    }

    #[test]
    fn tools_come_from_ext2() {
        let t = Tools::from_attributes(EXT2, "x").expect("tools");
        assert_eq!(t.obu_size_bytes, 4);
        assert_eq!(t.tx_mode, TX_MODE_SELECT);
        // obu_size_bytes_minus1 0, TX_MODE_LARGEST only.
        let t = Tools::from_attributes(2 << 4, "x").expect("tools");
        assert_eq!((t.obu_size_bytes, t.tx_mode), (1, TX_MODE_LARGEST));
        for refused in [ATTRIB_NOT_SUPPORTED, 1 << 4] {
            assert!(matches!(
                Tools::from_attributes(refused, "x"),
                Err(PlatformError::Unsupported { .. })
            ));
        }
    }

    #[test]
    fn check_refuses_what_one_tile_or_the_levels_cant_hold() {
        let s = settings(1920, 1080);
        assert_eq!((s.coded_width, s.coded_height), (1920, 1088));
        assert_eq!((s.sb_cols, s.sb_rows), (30, 17));
        // 1080p30 at 6 Mb/s: level 4.0.
        assert_eq!(s.level_idx, 8);
        let tools = s.tools;
        // 4096x2304 fills one tile exactly; wider or larger needs two.
        assert!(check(&config(4096, 2304), tools).is_ok());
        for (w, h) in [(4160, 1080), (4096, 2320)] {
            assert!(matches!(
                check(&config(w, h), tools),
                Err(PlatformError::Unsupported { .. })
            ));
        }
        let mut odd = config(1921, 1080);
        assert!(check(&odd, tools).is_err());
        odd = config(1920, 1080);
        odd.codec = Codec::Hevc;
        assert!(check(&odd, tools).is_err());
        let mut zero = config(1920, 1080);
        zero.keyframe_interval = 0;
        assert!(check(&zero, tools).is_err());
    }

    #[test]
    fn levels_follow_table_a1() {
        assert_eq!(level(1280, 720, (30, 1), 3_000_000), Some(5));
        assert_eq!(level(1920, 1080, (60, 1), 12_000_000), Some(9));
        assert_eq!(level(3840, 2160, (30, 1), 20_000_000), Some(12));
        assert_eq!(level(3840, 2160, (60, 1), 40_000_000), Some(13));
        assert_eq!(level(640, 360, (30, 1), 1_000_000), Some(1));
        assert_eq!(level(16_384, 8_704, (120, 1), 1_000_000), None);
    }

    #[test]
    fn the_gop_alternates_slots_from_each_key_frame() {
        let mut gop = Gop::new(&Settings {
            key_period: 3,
            ..settings(1280, 720)
        });
        let p: Vec<Picture> = (0..5).map(|_| gop.next_picture()).collect();
        assert_eq!(
            p.iter()
                .map(|p| (p.key, p.order_hint, p.slot))
                .collect::<Vec<_>>(),
            [
                (true, 0, 0),
                (false, 1, 1),
                (false, 2, 0),
                (true, 0, 0),
                (false, 1, 1)
            ]
        );
        assert_eq!(p[1].ref_slot(), 0);
        assert_eq!(p[2].ref_slot(), 1);
        gop.force_key();
        assert!(gop.next_picture().key);
    }

    #[test]
    fn order_hints_wrap_at_256() {
        let mut gop = Gop::new(&Settings {
            key_period: 1000,
            ..settings(1280, 720)
        });
        let hints: Vec<u8> = (0..258).map(|_| gop.next_picture().order_hint).collect();
        assert_eq!(&hints[254..], &[254, 255, 0, 1]);
    }

    #[test]
    fn leb128_is_padded_to_the_asked_length() {
        assert_eq!(leb128_fixed(5, 1), [5]);
        assert_eq!(leb128_fixed(5, 4), [0x85, 0x80, 0x80, 0x00]);
        assert_eq!(leb128_fixed(300, 2), [0xAC, 0x02]);
    }

    #[test]
    fn the_headers_read_back_as_written() {
        for (w, h) in [(1920, 1080), (1280, 720), (3840, 2160), (64, 64)] {
            let s = settings(w, h);
            let mut gop = Gop::new(&s);
            let mut parser = Parser::new();
            for n in 0..4 {
                let p = gop.next_picture();
                let frames = parser
                    .parse(&unit(&s, &p))
                    .expect("a stream the parser reads");
                let seq = parser.sequence().expect("a sequence header");
                assert_eq!((seq.max_frame_width, seq.max_frame_height), (w, h));
                assert!(seq.enable_order_hint && seq.enable_cdef);
                assert_eq!(seq.order_hint_bits, ORDER_HINT_BITS);
                assert!(!seq.use_128x128_superblock && !seq.enable_restoration);
                assert_eq!(seq.color.bit_depth, 8);
                assert_eq!(seq.color.matrix_coefficients, 1);
                assert!(!seq.color.color_range);
                assert!(seq.color.subsampling_x && seq.color.subsampling_y);
                assert_eq!(frames.len(), 1, "{w}x{h} frame {n}");
                let f = &frames[0].header;
                assert_eq!(f.frame_type, if p.key { 0 } else { 1 });
                assert!(f.show_frame);
                assert_eq!(f.error_resilient_mode, p.key);
                assert_eq!(f.order_hint, u32::from(p.order_hint));
                assert_eq!((f.frame_width, f.frame_height), (w, h));
                assert_eq!((f.tile_cols, f.tile_rows), (1, 1));
                assert_eq!(f.base_q_idx, BASE_Q_IDX);
                assert_eq!(f.tx_mode, TX_MODE_SELECT);
                assert_eq!(f.cdef_bits, 0);
                if p.key {
                    assert_eq!(f.refresh_frame_flags, 0xFF);
                    assert_eq!(f.primary_ref_frame, PRIMARY_REF_NONE);
                } else {
                    assert_eq!(f.refresh_frame_flags, 1 << p.slot);
                    assert_eq!(f.ref_frame_idx, [p.ref_slot(); REFS_PER_FRAME]);
                    assert_eq!(f.primary_ref_frame, 0);
                    assert!(!f.reference_select);
                }
            }
        }
    }

    #[test]
    fn the_offsets_point_at_the_fields_the_driver_rewrites() {
        let s = settings(1920, 1080);
        for key in [true, false] {
            let p = Picture {
                key,
                order_hint: 7,
                slot: u8::from(!key),
            };
            let h = frame_header(&s, &p);
            assert_eq!(read_bits(&h.bytes, h.qindex, 8), u32::from(BASE_Q_IDX));
            // The two luma levels, sharpness and the delta flag, all 0, then CDEF.
            assert_eq!(h.cdef, h.loop_filter + 16);
            assert_eq!(h.cdef_bits, 16);
            // obu_size, after the one-byte OBU header, counts the rest.
            let size = h.bytes[1..5]
                .iter()
                .enumerate()
                .fold(0, |v, (i, b)| v | u32::from(b & 0x7F) << (7 * i));
            assert_eq!(size as usize, h.bytes.len() - 5);
            let seq = sequence_header(&s);
            let pic = picture(&s, &p, 10, Some(11), 12, &h, seq.len());
            let at = pic.byte_offset_frame_hdr_obu_size as usize;
            let packed = if key {
                [seq.clone(), h.bytes.clone()].concat()
            } else {
                h.bytes.clone()
            };
            // The size field's first byte: the low seven bits, continued.
            assert_eq!(packed[at], h.bytes[1]);
            assert_eq!(packed[at - 1], OBU_FRAME_HEADER << 3 | 0b10);
            assert_eq!(pic.size_in_bits_frame_hdr_obu as usize, h.bytes.len() * 8);
        }
    }

    #[test]
    fn picture_parameters_match_the_header() {
        let s = settings(1920, 1080);
        let key = Picture {
            key: true,
            order_hint: 0,
            slot: 0,
        };
        let h = frame_header(&s, &key);
        let pic = picture(&s, &key, 10, None, 12, &h, 40);
        assert_eq!(
            (pic.frame_width_minus_1, pic.frame_height_minus_1),
            (1919, 1079)
        );
        assert_eq!((pic.reconstructed_frame, pic.coded_buf), (10, 12));
        assert_eq!(pic.reference_frames, [super::super::ffi::INVALID_ID; 8]);
        assert_eq!(pic.refresh_frame_flags, 0xFF);
        assert_eq!(pic.primary_ref_frame, PRIMARY_REF_NONE);
        assert_eq!(pic.picture_flags, 0x4); // KEY_FRAME, error_resilient_mode
        assert_eq!(pic.byte_offset_frame_hdr_obu_size, 41);
        assert_eq!(
            (pic.width_in_sbs_minus_1[0], pic.height_in_sbs_minus_1[0]),
            (29, 16)
        );
        assert_eq!(pic.mode_control_flags, 0x100); // tx_mode TX_MODE_SELECT
        assert_eq!(pic.tile_group_obu_hdr_info, 0x2); // obu_has_size_field
        let inter = Picture {
            key: false,
            order_hint: 1,
            slot: 1,
        };
        let h = frame_header(&s, &inter);
        let pic = picture(&s, &inter, 13, Some(10), 12, &h, 40);
        assert_eq!(pic.reference_frames[0], 10);
        assert_eq!(pic.reference_frames[1], super::super::ffi::INVALID_ID);
        assert_eq!(pic.ref_frame_idx, [0; 7]);
        assert_eq!(pic.refresh_frame_flags, 0b10);
        assert_eq!(pic.picture_flags, 0x1); // INTER_FRAME
        assert_eq!(pic.ref_frame_ctrl_l0, 0x1); // search_idx0: LAST_FRAME
        assert_eq!(pic.byte_offset_frame_hdr_obu_size, 1);
        assert_eq!((pic.min_base_qindex, pic.max_base_qindex), (1, 255));
    }

    #[test]
    fn sequence_parameters_match_the_header() {
        let s = settings(1920, 1080);
        let seq = sequence(&s);
        assert_eq!(seq.seq_level_idx, 8);
        assert_eq!(seq.intra_period, 60);
        assert_eq!(seq.ip_period, 1);
        assert_eq!(seq.bits_per_second, 6_000_000);
        // enable_order_hint, enable_cdef, subsampling_x and _y, as libva lays them out.
        assert_eq!(seq.seq_fields, 0x100 | 0x1000 | 0x2_0000 | 0x4_0000);
        assert_eq!(seq.order_hint_bits_minus_1, 7);
    }

    #[test]
    fn a_unit_starts_with_a_temporal_delimiter_and_keys_carry_the_sequence_header() {
        let s = settings(1280, 720);
        let mut gop = Gop::new(&s);
        let key = unit(&s, &gop.next_picture());
        let inter = unit(&s, &gop.next_picture());
        let parser = Parser::new();
        assert_eq!(&key[..2], &TEMPORAL_DELIMITER);
        assert_eq!(key[2] >> 3, OBU_SEQUENCE_HEADER);
        assert_eq!(inter[2] >> 3, OBU_FRAME_HEADER);
        assert!(parser.is_key_frame(&key));
    }
}
