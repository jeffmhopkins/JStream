//! VP9's frame headers (the VP9 bitstream specification, v0.6), for decoding through
//! VA-API (ADR-0072 (b)): a superframe split into its frames, each frame's uncompressed
//! header read, and the state that carries from frame to frame (the reference slots'
//! sizes, the loop filter's deltas and the segmentation features).
//!
//! The GPU's driver reads the compressed header and the tiles itself, and keeps the
//! probability contexts. What VA-API wants from the application is the uncompressed
//! header's fields, and from them each segment's loop filter levels and dequantizers
//! ([`Header::segments`]).

use super::h264::bits::{BitReader, ParseError, Result};

/// The reference slots.
pub(crate) const NUM_REF_FRAMES: usize = 8;
/// The segments.
pub(crate) const MAX_SEGMENTS: usize = 8;

/// The segment features (§7.2.10): an alternate quantizer, an alternate loop filter
/// level, a fixed reference frame, and skip.
const SEG_LVL_ALT_Q: usize = 0;
const SEG_LVL_ALT_L: usize = 1;
const SEG_LVL_REF_FRAME: usize = 2;
const SEG_LVL_SKIP: usize = 3;
const FEATURE_BITS: [u32; 4] = [8, 6, 2, 0];
const FEATURE_SIGNED: [bool; 4] = [true, true, false, false];

/// The frames of one sample: a superframe's (§B.3), or the sample itself.
pub(crate) fn frames(sample: &[u8]) -> Vec<&[u8]> {
    let whole = || vec![sample];
    let Some(&marker) = sample.last() else {
        return Vec::new();
    };
    if marker & 0xE0 != 0xC0 {
        return whole();
    }
    let count = usize::from(marker & 7) + 1;
    let size_bytes = usize::from((marker >> 3) & 3) + 1;
    let index_len = 2 + size_bytes * count;
    let Some(index_at) = sample.len().checked_sub(index_len) else {
        return whole();
    };
    if sample[index_at] != marker {
        return whole();
    }
    let mut out = Vec::with_capacity(count);
    let mut at = 0_usize;
    for i in 0..count {
        let field = &sample[index_at + 1 + i * size_bytes..index_at + 1 + (i + 1) * size_bytes];
        let size = field
            .iter()
            .rev()
            .fold(0_usize, |v, b| v << 8 | usize::from(*b));
        let Some(frame) = at
            .checked_add(size)
            .filter(|end| *end <= index_at)
            .map(|end| &sample[at..end])
        else {
            // A damaged index: what fits is kept.
            break;
        };
        // A zero-size frame is padding.
        if !frame.is_empty() {
            out.push(frame);
        }
        at += size;
    }
    out
}

/// Whether `frame` is a key frame, from its first bits alone: where decoding can start.
pub(crate) fn is_key_frame(frame: &[u8]) -> bool {
    fn key(r: &mut BitReader<'_>) -> Result<bool> {
        if r.bits(2)? != 2 {
            return Ok(false);
        }
        if r.bits(2)? == 3 {
            r.flag()?;
        }
        // show_existing_frame, then frame_type (0: a key frame).
        Ok(!r.flag()? && !r.flag()?)
    }
    key(&mut BitReader::new(frame)).unwrap_or(false)
}

/// The loop filter's reference and mode deltas, which carry from frame to frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LoopFilterDeltas {
    pub(crate) enabled: bool,
    /// Intra, last, golden and alternate reference.
    pub(crate) reference: [i8; 4],
    /// Zero motion, and the other inter modes.
    pub(crate) mode: [i8; 2],
}

impl LoopFilterDeltas {
    /// `setup_past_independence`'s values.
    const DEFAULT: Self = Self {
        enabled: true,
        reference: [1, 0, -1, -1],
        mode: [0, 0],
    };
}

/// The segmentation parameters, which carry from frame to frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Segmentation {
    pub(crate) enabled: bool,
    pub(crate) update_map: bool,
    pub(crate) temporal_update: bool,
    pub(crate) tree_probs: [u8; 7],
    pub(crate) pred_probs: [u8; 3],
    /// Feature values are absolute, not deltas from the frame's.
    pub(crate) abs_delta: bool,
    pub(crate) feature_enabled: [[bool; 4]; MAX_SEGMENTS],
    pub(crate) feature_data: [[i16; 4]; MAX_SEGMENTS],
}

impl Segmentation {
    const DEFAULT: Self = Self {
        enabled: false,
        update_map: false,
        temporal_update: false,
        tree_probs: [255; 7],
        pred_probs: [255; 3],
        abs_delta: false,
        feature_enabled: [[false; 4]; MAX_SEGMENTS],
        feature_data: [[0; 4]; MAX_SEGMENTS],
    };

    /// Whether `segment` uses `feature` on this frame.
    fn active(&self, segment: usize, feature: usize) -> bool {
        self.enabled && self.feature_enabled[segment][feature]
    }
}

/// One segment's parameters, as VA-API takes them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SegmentParams {
    pub(crate) reference_enabled: bool,
    pub(crate) reference: u8,
    pub(crate) skip: bool,
    /// `[reference][mode]`: intra, last, golden, alternate; zero motion, other.
    pub(crate) filter_level: [[u8; 2]; 4],
    pub(crate) luma_ac: i16,
    pub(crate) luma_dc: i16,
    pub(crate) chroma_ac: i16,
    pub(crate) chroma_dc: i16,
}

/// A frame's uncompressed header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) profile: u8,
    /// `show_existing_frame`: the slot to show again, and nothing to decode.
    pub(crate) show_existing: Option<usize>,
    pub(crate) key_frame: bool,
    pub(crate) show_frame: bool,
    pub(crate) error_resilient: bool,
    pub(crate) intra_only: bool,
    pub(crate) reset_frame_context: u8,
    pub(crate) refresh_frame_flags: u8,
    /// The last, golden and alternate references' slots.
    pub(crate) ref_frame_idx: [u8; 3],
    pub(crate) ref_frame_sign_bias: [bool; 3],
    pub(crate) allow_high_precision_mv: bool,
    /// libvpx's order, which VA-API takes: regular, smooth, sharp, bilinear,
    /// switchable.
    pub(crate) interp_filter: u8,
    pub(crate) refresh_frame_context: bool,
    pub(crate) frame_parallel_decoding_mode: bool,
    pub(crate) frame_context_idx: u8,
    pub(crate) bit_depth: u8,
    pub(crate) subsampling: (bool, bool),
    /// `color_space`: 0 unknown, 1 BT.601, 2 BT.709, 3 SMPTE 170, 4 SMPTE 240,
    /// 5 BT.2020, 7 RGB.
    pub(crate) color_space: u8,
    pub(crate) full_range: bool,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) filter_level: u8,
    pub(crate) sharpness: u8,
    pub(crate) deltas: LoopFilterDeltas,
    pub(crate) base_q_idx: u8,
    pub(crate) delta_q_y_dc: i8,
    pub(crate) delta_q_uv_dc: i8,
    pub(crate) delta_q_uv_ac: i8,
    pub(crate) segmentation: Segmentation,
    pub(crate) tile_cols_log2: u8,
    pub(crate) tile_rows_log2: u8,
    /// The uncompressed header's length, in bytes.
    pub(crate) uncompressed_size: usize,
    /// The compressed header's length, in bytes.
    pub(crate) compressed_size: u16,
}

impl Header {
    /// Whether the frame is coded without loss.
    pub(crate) fn lossless(&self) -> bool {
        self.base_q_idx == 0
            && self.delta_q_y_dc == 0
            && self.delta_q_uv_dc == 0
            && self.delta_q_uv_ac == 0
    }

    /// Each segment's parameters (§8.6.1's quantizers, §8.8.1's filter levels).
    pub(crate) fn segments(&self) -> [SegmentParams; MAX_SEGMENTS] {
        let seg = &self.segmentation;
        let mut out = [SegmentParams::default(); MAX_SEGMENTS];
        for (i, params) in out.iter_mut().enumerate() {
            // The quantizer index.
            let q = if seg.active(i, SEG_LVL_ALT_Q) {
                let data = i32::from(seg.feature_data[i][SEG_LVL_ALT_Q]);
                let q = if seg.abs_delta {
                    data
                } else {
                    i32::from(self.base_q_idx) + data
                };
                q.clamp(0, 255)
            } else {
                i32::from(self.base_q_idx)
            };
            params.luma_dc = dc_q(q + i32::from(self.delta_q_y_dc));
            params.luma_ac = ac_q(q);
            params.chroma_dc = dc_q(q + i32::from(self.delta_q_uv_dc));
            params.chroma_ac = ac_q(q + i32::from(self.delta_q_uv_ac));

            // The loop filter levels.
            let mut level = i32::from(self.filter_level);
            if seg.active(i, SEG_LVL_ALT_L) {
                let data = i32::from(seg.feature_data[i][SEG_LVL_ALT_L]);
                level = if seg.abs_delta { data } else { level + data };
                level = level.clamp(0, 63);
            }
            let clamp = |v: i32| u8::try_from(v.clamp(0, 63)).unwrap_or(0);
            if self.deltas.enabled {
                let scale = 1 << (level >> 5);
                let intra = clamp(level + i32::from(self.deltas.reference[0]) * scale);
                params.filter_level[0] = [intra, intra];
                for r in 1..4 {
                    for m in 0..2 {
                        params.filter_level[r][m] = clamp(
                            level
                                + i32::from(self.deltas.reference[r]) * scale
                                + i32::from(self.deltas.mode[m]) * scale,
                        );
                    }
                }
            } else {
                params.filter_level = [[clamp(level); 2]; 4];
            }

            params.reference_enabled = seg.active(i, SEG_LVL_REF_FRAME);
            params.reference =
                u8::try_from(seg.feature_data[i][SEG_LVL_REF_FRAME] & 3).unwrap_or(0);
            params.skip = seg.active(i, SEG_LVL_SKIP);
        }
        out
    }
}

/// The state that carries from one frame's header to the next.
#[derive(Clone, Debug)]
pub(crate) struct Parser {
    /// Each reference slot's frame size; `None` before a frame fills it.
    sizes: [Option<(u16, u16)>; NUM_REF_FRAMES],
    deltas: LoopFilterDeltas,
    segmentation: Segmentation,
    /// The last colour configuration, for an intra-only frame of profile 0, which
    /// doesn't repeat it.
    colour: (u8, bool),
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub(crate) fn new() -> Self {
        Self {
            sizes: [None; NUM_REF_FRAMES],
            deltas: LoopFilterDeltas::DEFAULT,
            segmentation: Segmentation::DEFAULT,
            colour: (0, false),
        }
    }

    /// Forgets the reference slots, after a seek: decoding starts again at a key
    /// frame.
    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }

    /// Whether `slot` holds a frame.
    pub(crate) fn filled(&self, slot: usize) -> bool {
        self.sizes.get(slot).is_some_and(Option::is_some)
    }

    /// Reads `frame`'s uncompressed header, and updates the state it carries.
    ///
    /// # Errors
    /// [`ParseError::Truncated`] for a frame that ends inside its header,
    /// [`ParseError::Invalid`] for a field the specification forbids, and
    /// [`ParseError::Unsupported`] for a frame that refers to an empty slot.
    pub(crate) fn parse(&mut self, frame: &[u8]) -> Result<Header> {
        let mut r = BitReader::new(frame);
        if r.bits(2)? != 2 {
            return Err(ParseError::Invalid("a VP9 frame marker"));
        }
        let low = r.bits(1)?;
        let high = r.bits(1)?;
        let profile = u8::try_from(high << 1 | low).unwrap_or(0);
        if profile == 3 && r.flag()? {
            return Err(ParseError::Invalid("a VP9 reserved bit"));
        }
        let mut h = Header {
            profile,
            show_existing: None,
            key_frame: false,
            show_frame: true,
            error_resilient: false,
            intra_only: false,
            reset_frame_context: 0,
            refresh_frame_flags: 0,
            ref_frame_idx: [0; 3],
            ref_frame_sign_bias: [false; 3],
            allow_high_precision_mv: false,
            interp_filter: 0,
            refresh_frame_context: false,
            frame_parallel_decoding_mode: true,
            frame_context_idx: 0,
            bit_depth: 8,
            subsampling: (true, true),
            color_space: self.colour.0,
            full_range: self.colour.1,
            width: 0,
            height: 0,
            filter_level: 0,
            sharpness: 0,
            deltas: self.deltas,
            base_q_idx: 0,
            delta_q_y_dc: 0,
            delta_q_uv_dc: 0,
            delta_q_uv_ac: 0,
            segmentation: self.segmentation,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
            uncompressed_size: 0,
            compressed_size: 0,
        };
        if r.flag()? {
            let slot = r.bits(3)? as usize;
            if !self.filled(slot) {
                return Err(ParseError::Unsupported(
                    "a VP9 frame shown from an empty slot",
                ));
            }
            h.show_existing = Some(slot);
            h.uncompressed_size = r.position().div_ceil(8);
            return Ok(h);
        }
        h.key_frame = !r.flag()?;
        h.show_frame = r.flag()?;
        h.error_resilient = r.flag()?;
        if h.key_frame {
            sync_code(&mut r)?;
            self.colour_config(&mut r, &mut h)?;
            (h.width, h.height) = frame_size(&mut r)?;
            render_size(&mut r)?;
            h.refresh_frame_flags = 0xFF;
        } else {
            h.intra_only = if h.show_frame { false } else { r.flag()? };
            h.reset_frame_context = if h.error_resilient {
                0
            } else {
                u8::try_from(r.bits(2)?).unwrap_or(0)
            };
            if h.intra_only {
                sync_code(&mut r)?;
                if profile > 0 {
                    self.colour_config(&mut r, &mut h)?;
                } else {
                    // Profile 0's intra-only frames are 8-bit 4:2:0 BT.601.
                    h.color_space = 1;
                    h.full_range = false;
                }
                h.refresh_frame_flags = u8::try_from(r.bits(8)?).unwrap_or(0);
                (h.width, h.height) = frame_size(&mut r)?;
                render_size(&mut r)?;
            } else {
                h.refresh_frame_flags = u8::try_from(r.bits(8)?).unwrap_or(0);
                for i in 0..3 {
                    let slot = u8::try_from(r.bits(3)?).unwrap_or(0);
                    if !self.filled(usize::from(slot)) {
                        return Err(ParseError::Unsupported(
                            "a VP9 frame that refers to an empty slot",
                        ));
                    }
                    h.ref_frame_idx[i] = slot;
                    h.ref_frame_sign_bias[i] = r.flag()?;
                }
                // frame_size_with_refs.
                let mut found = None;
                for i in 0..3 {
                    if r.flag()? {
                        found = self.sizes[usize::from(h.ref_frame_idx[i])];
                        break;
                    }
                }
                (h.width, h.height) = match found {
                    Some(size) => size,
                    None => frame_size(&mut r)?,
                };
                render_size(&mut r)?;
                h.allow_high_precision_mv = r.flag()?;
                h.interp_filter = if r.flag()? {
                    4
                } else {
                    // literal_to_type, in libvpx's order.
                    [1, 0, 2, 3][r.bits(2)? as usize]
                };
            }
        }
        if h.error_resilient {
            h.refresh_frame_context = false;
            h.frame_parallel_decoding_mode = true;
        } else {
            h.refresh_frame_context = r.flag()?;
            h.frame_parallel_decoding_mode = r.flag()?;
        }
        h.frame_context_idx = u8::try_from(r.bits(2)?).unwrap_or(0);
        if h.key_frame || h.intra_only || h.error_resilient {
            // setup_past_independence.
            self.deltas = LoopFilterDeltas::DEFAULT;
            self.segmentation.feature_enabled = [[false; 4]; MAX_SEGMENTS];
            self.segmentation.feature_data = [[0; 4]; MAX_SEGMENTS];
            self.segmentation.abs_delta = false;
            h.frame_context_idx = 0;
        }

        // loop_filter_params.
        h.filter_level = u8::try_from(r.bits(6)?).unwrap_or(0);
        h.sharpness = u8::try_from(r.bits(3)?).unwrap_or(0);
        self.deltas.enabled = r.flag()?;
        if self.deltas.enabled && r.flag()? {
            for d in &mut self.deltas.reference {
                if r.flag()? {
                    *d = signed(&mut r, 6)?;
                }
            }
            for d in &mut self.deltas.mode {
                if r.flag()? {
                    *d = signed(&mut r, 6)?;
                }
            }
        }
        h.deltas = self.deltas;

        // quantization_params.
        h.base_q_idx = u8::try_from(r.bits(8)?).unwrap_or(0);
        h.delta_q_y_dc = delta_q(&mut r)?;
        h.delta_q_uv_dc = delta_q(&mut r)?;
        h.delta_q_uv_ac = delta_q(&mut r)?;

        // segmentation_params.
        let seg = &mut self.segmentation;
        seg.enabled = r.flag()?;
        seg.update_map = false;
        seg.temporal_update = false;
        if seg.enabled {
            seg.update_map = r.flag()?;
            if seg.update_map {
                for p in &mut seg.tree_probs {
                    *p = prob(&mut r)?;
                }
                seg.temporal_update = r.flag()?;
                for p in &mut seg.pred_probs {
                    *p = if seg.temporal_update {
                        prob(&mut r)?
                    } else {
                        255
                    };
                }
            }
            if r.flag()? {
                seg.abs_delta = r.flag()?;
                for i in 0..MAX_SEGMENTS {
                    for j in 0..4 {
                        let enabled = r.flag()?;
                        let mut value = 0_i16;
                        if enabled {
                            value = i16::try_from(r.bits(FEATURE_BITS[j])?).unwrap_or(0);
                            if FEATURE_SIGNED[j] && r.flag()? {
                                value = -value;
                            }
                        }
                        seg.feature_enabled[i][j] = enabled;
                        seg.feature_data[i][j] = value;
                    }
                }
            }
        }
        h.segmentation = *seg;

        // tile_info.
        let mi_cols = (u32::from(h.width) + 7) >> 3;
        let sb64_cols = (mi_cols + 7) >> 3;
        let mut min_log2 = 0;
        while (64 << min_log2) < sb64_cols {
            min_log2 += 1;
        }
        let mut max_log2 = 1;
        while (sb64_cols >> max_log2) >= 4 {
            max_log2 += 1;
        }
        max_log2 -= 1;
        let mut cols = min_log2;
        while cols < max_log2 && r.flag()? {
            cols += 1;
        }
        h.tile_cols_log2 = u8::try_from(cols).unwrap_or(0);
        h.tile_rows_log2 = u8::from(r.flag()?);
        if h.tile_rows_log2 == 1 && r.flag()? {
            h.tile_rows_log2 = 2;
        }

        h.compressed_size = u16::try_from(r.bits(16)?).unwrap_or(0);
        if h.compressed_size == 0 {
            return Err(ParseError::Invalid(
                "a VP9 frame without a compressed header",
            ));
        }
        h.uncompressed_size = r.position().div_ceil(8);
        if h.uncompressed_size + usize::from(h.compressed_size) > frame.len() {
            return Err(ParseError::Truncated);
        }

        for (slot, size) in self.sizes.iter_mut().enumerate() {
            if h.refresh_frame_flags >> slot & 1 == 1 {
                *size = Some((h.width, h.height));
            }
        }
        Ok(h)
    }

    /// `color_config` (§6.2.2), for a key frame or a later profile's intra-only one.
    fn colour_config(&mut self, r: &mut BitReader<'_>, h: &mut Header) -> Result<()> {
        h.bit_depth = if h.profile >= 2 {
            if r.flag()? { 12 } else { 10 }
        } else {
            8
        };
        h.color_space = u8::try_from(r.bits(3)?).unwrap_or(0);
        if h.color_space == 7 {
            // RGB: full range, and 4:4:4 in profiles 1 and 3.
            h.full_range = true;
            if h.profile == 1 || h.profile == 3 {
                h.subsampling = (false, false);
                r.flag()?;
            }
        } else {
            h.full_range = r.flag()?;
            if h.profile == 1 || h.profile == 3 {
                h.subsampling = (r.flag()?, r.flag()?);
                r.flag()?;
            }
        }
        self.colour = (h.color_space, h.full_range);
        Ok(())
    }
}

fn sync_code(r: &mut BitReader<'_>) -> Result<()> {
    if r.bits(24)? == 0x49_8342 {
        Ok(())
    } else {
        Err(ParseError::Invalid("a VP9 sync code"))
    }
}

fn frame_size(r: &mut BitReader<'_>) -> Result<(u16, u16)> {
    let w = u16::try_from(r.bits(16)? + 1).unwrap_or(u16::MAX);
    let h = u16::try_from(r.bits(16)? + 1).unwrap_or(u16::MAX);
    Ok((w, h))
}

/// `render_size`: only a hint for display, so it is read and left.
fn render_size(r: &mut BitReader<'_>) -> Result<()> {
    if r.flag()? {
        r.bits(32)?;
    }
    Ok(())
}

/// `su(n)`: a magnitude, then its sign.
fn signed(r: &mut BitReader<'_>, n: u32) -> Result<i8> {
    let value = i8::try_from(r.bits(n)?).unwrap_or(0);
    Ok(if r.flag()? { -value } else { value })
}

fn delta_q(r: &mut BitReader<'_>) -> Result<i8> {
    if r.flag()? { signed(r, 4) } else { Ok(0) }
}

fn prob(r: &mut BitReader<'_>) -> Result<u8> {
    Ok(if r.flag()? {
        u8::try_from(r.bits(8)?).unwrap_or(255)
    } else {
        255
    })
}

/// The DC dequantizer for 8-bit video at `q`, clamped into the table.
fn dc_q(q: i32) -> i16 {
    DC_QLOOKUP[usize::try_from(q.clamp(0, 255)).unwrap_or(0)]
}

/// The AC dequantizer for 8-bit video at `q`, clamped into the table.
fn ac_q(q: i32) -> i16 {
    AC_QLOOKUP[usize::try_from(q.clamp(0, 255)).unwrap_or(0)]
}

/// §8.6.1's `dc_qlookup[0]`: the 8-bit DC dequantizers.
const DC_QLOOKUP: [i16; 256] = [
    4, 8, 8, 9, 10, 11, 12, 12, 13, 14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 23, 24, 25, 26, 26, 27,
    28, 29, 30, 31, 32, 32, 33, 34, 35, 36, 37, 38, 38, 39, 40, 41, 42, 43, 43, 44, 45, 46, 47, 48,
    48, 49, 50, 51, 52, 53, 53, 54, 55, 56, 57, 57, 58, 59, 60, 61, 62, 62, 63, 64, 65, 66, 66, 67,
    68, 69, 70, 70, 71, 72, 73, 74, 74, 75, 76, 77, 78, 78, 79, 80, 81, 81, 82, 83, 84, 85, 85, 87,
    88, 90, 92, 93, 95, 96, 98, 99, 101, 102, 104, 105, 107, 108, 110, 111, 113, 114, 116, 117,
    118, 120, 121, 123, 125, 127, 129, 131, 134, 136, 138, 140, 142, 144, 146, 148, 150, 152, 154,
    156, 158, 161, 164, 166, 169, 172, 174, 177, 180, 182, 185, 187, 190, 192, 195, 199, 202, 205,
    208, 211, 214, 217, 220, 223, 226, 230, 233, 237, 240, 243, 247, 250, 253, 257, 261, 265, 269,
    272, 276, 280, 284, 288, 292, 296, 300, 304, 309, 313, 317, 322, 326, 330, 335, 340, 344, 349,
    354, 359, 364, 369, 374, 379, 384, 389, 395, 400, 406, 411, 417, 423, 429, 435, 441, 447, 454,
    461, 467, 475, 482, 489, 497, 505, 513, 522, 530, 539, 549, 559, 569, 579, 590, 602, 614, 626,
    640, 654, 668, 684, 700, 717, 736, 755, 775, 796, 819, 843, 869, 896, 925, 955, 988, 1022,
    1058, 1098, 1139, 1184, 1232, 1282, 1336,
];

/// §8.6.1's `ac_qlookup[0]`: the 8-bit AC dequantizers.
const AC_QLOOKUP: [i16; 256] = [
    4, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30,
    31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54,
    55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78,
    79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101,
    102, 104, 106, 108, 110, 112, 114, 116, 118, 120, 122, 124, 126, 128, 130, 132, 134, 136, 138,
    140, 142, 144, 146, 148, 150, 152, 155, 158, 161, 164, 167, 170, 173, 176, 179, 182, 185, 188,
    191, 194, 197, 200, 203, 207, 211, 215, 219, 223, 227, 231, 235, 239, 243, 247, 251, 255, 260,
    265, 270, 275, 280, 285, 290, 295, 300, 305, 311, 317, 323, 329, 335, 341, 347, 353, 359, 366,
    373, 380, 387, 394, 401, 408, 416, 424, 432, 440, 448, 456, 465, 474, 483, 492, 501, 510, 520,
    530, 540, 550, 560, 571, 582, 593, 604, 615, 627, 639, 651, 663, 676, 689, 702, 715, 729, 743,
    757, 771, 786, 801, 816, 832, 848, 864, 881, 898, 915, 933, 951, 969, 988, 1007, 1026, 1046,
    1066, 1087, 1108, 1129, 1151, 1173, 1196, 1219, 1243, 1267, 1292, 1317, 1343, 1369, 1396, 1423,
    1451, 1479, 1508, 1537, 1567, 1597, 1628, 1660, 1692, 1725, 1759, 1793, 1828,
];

#[cfg(test)]
mod tests;
