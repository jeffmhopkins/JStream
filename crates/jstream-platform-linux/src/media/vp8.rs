//! VP8's frame headers (RFC 6386), for decoding through VA-API (ADR-0072 (b)): each
//! frame's tag, a key frame's size, and the first partition's header read through the
//! boolean decoder, with the state that carries from frame to frame (the probabilities,
//! the segmentation and the loop filter's deltas).
//!
//! VA-API wants all of the header from the application: the probabilities as they
//! stand for this frame, each segment's quantizer indices and filter levels, the
//! partitions' sizes, and where the boolean decoder stood when the header ended, so the
//! GPU carries on from there into the macroblocks. That last part is what FFmpeg's
//! hwaccel hands its drivers, so the boolean decoder here is the same one FFmpeg's is
//! ([`BoolDecoder`]), and its state is read out the same way.

use super::h264::bits::{ParseError, Result};

mod tables;

use tables::{COEFF_UPDATE_PROBS, DEFAULT_COEFF_PROBS};

/// The coefficient probabilities: by block type, coefficient band, context and token.
pub(crate) type CoeffProbs = [[[[u8; 11]; 3]; 8]; 4];

/// The segments.
pub(crate) const MAX_SEGMENTS: usize = 4;
/// The most token partitions a frame has.
pub(crate) const MAX_PARTITIONS: usize = 8;

/// A key frame's intra mode probabilities (RFC 6386 §11.2), fixed.
pub(crate) const KEY_FRAME_Y_MODE_PROBS: [u8; 4] = [145, 156, 163, 128];
pub(crate) const KEY_FRAME_UV_MODE_PROBS: [u8; 3] = [142, 114, 183];
/// The inter frames' intra mode probabilities a key frame resets to (§16.1).
const Y_MODE_PROBS: [u8; 4] = [112, 86, 140, 37];
const UV_MODE_PROBS: [u8; 3] = [162, 101, 204];

/// The motion vector probabilities a key frame resets to (§17.2).
const MV_PROBS: [[u8; 19]; 2] = [
    [
        162, 128, 225, 146, 172, 147, 214, 39, 156, 128, 129, 132, 75, 145, 178, 206, 239, 254, 254,
    ],
    [
        164, 128, 204, 170, 119, 235, 140, 230, 228, 128, 130, 130, 74, 148, 180, 203, 236, 254,
        254,
    ],
];
/// The probability that each motion vector probability is updated (§17.2).
const MV_UPDATE_PROBS: [[u8; 19]; 2] = [
    [
        237, 246, 253, 253, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 250, 250, 252, 254,
        254,
    ],
    [
        231, 243, 245, 253, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 251, 251, 254, 254,
        254,
    ],
];

/// How many bits the boolean decoder shifts its range left to bring it back to 128 or
/// more: the leading zeros of the range in a byte.
fn norm_shift(range: u32) -> u32 {
    if range == 0 {
        8
    } else {
        range.leading_zeros().saturating_sub(24)
    }
}

/// The boolean decoder (RFC 6386 §7), as FFmpeg's `VPXRangeCoder` keeps it: a code word
/// with 16 bits below the byte being decoded, refilled two bytes at a time, so its
/// state where the header ends reads out exactly as FFmpeg's hwaccel reads it.
#[derive(Debug)]
pub(crate) struct BoolDecoder<'a> {
    /// From the partition's start to the end of the frame: a refill near the
    /// partition's end reads on into what follows, as FFmpeg's does.
    data: &'a [u8],
    /// The partition's length.
    end: usize,
    /// The next byte to read.
    pos: usize,
    high: u32,
    /// Bits left before a refill, negated.
    bits: i32,
    code_word: u32,
}

/// Where the boolean decoder stood when the header ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CoderState {
    pub(crate) range: u8,
    pub(crate) value: u8,
    pub(crate) count: u8,
    /// The macroblocks' first bit, counted from the partition's start, as VA-API's
    /// `macroblock_offset` has it.
    pub(crate) macroblock_offset: u32,
}

impl<'a> BoolDecoder<'a> {
    /// A decoder over a partition `end` bytes long, starting `data`.
    pub(crate) fn new(data: &'a [u8], end: usize) -> Self {
        let byte = |i: usize| u32::from(data.get(i).copied().unwrap_or(0));
        Self {
            data,
            end,
            pos: 3,
            high: 255,
            bits: -16,
            code_word: byte(0) << 16 | byte(1) << 8 | byte(2),
        }
    }

    fn byte(&self, i: usize) -> u32 {
        u32::from(self.data.get(i).copied().unwrap_or(0))
    }

    /// Brings the range back to 128 or more, refilling the code word.
    fn renorm(&mut self) -> u32 {
        let shift = norm_shift(self.high);
        self.high <<= shift;
        let mut code_word = self.code_word << shift;
        self.bits += i32::try_from(shift).unwrap_or(8);
        if self.bits >= 0 && self.pos < self.end {
            let pair = self.byte(self.pos) << 8 | self.byte(self.pos + 1);
            code_word |= pair << self.bits.unsigned_abs();
            self.pos += 2;
            self.bits -= 16;
        }
        code_word
    }

    /// One bool whose probability of being 0 is `prob` / 256.
    pub(crate) fn get(&mut self, prob: u8) -> bool {
        let code_word = self.renorm();
        let low = 1 + (((self.high - 1) * u32::from(prob)) >> 8);
        let low_shift = low << 16;
        let bit = code_word >= low_shift;
        if bit {
            self.high -= low;
            self.code_word = code_word - low_shift;
        } else {
            self.high = low;
            self.code_word = code_word;
        }
        bit
    }

    /// A flag (`L(1)`).
    pub(crate) fn flag(&mut self) -> bool {
        self.get(128)
    }

    /// An `n`-bit unsigned literal, most significant bit first (`L(n)`).
    pub(crate) fn literal(&mut self, n: u32) -> u8 {
        let mut v = 0_u8;
        for _ in 0..n {
            v = v << 1 | u8::from(self.flag());
        }
        v
    }

    /// An optional signed value: a flag, then `n` bits of magnitude and a sign; 0 when
    /// the flag is clear.
    fn signed(&mut self, n: u32) -> i8 {
        if !self.flag() {
            return 0;
        }
        let magnitude = i8::try_from(self.literal(n)).unwrap_or(i8::MAX);
        if self.flag() { -magnitude } else { magnitude }
    }

    /// Where the decoder stands, as FFmpeg's `coder_state_at_header_end` records it.
    pub(crate) fn state(&mut self) -> CoderState {
        self.code_word = self.renorm();
        let behind = (-self.bits) / 8;
        let input = i64::try_from(self.pos).unwrap_or(i64::MAX) - i64::from(behind);
        let count = (-self.bits) % 8;
        let offset = 8 * input - i64::from(count) - 8;
        CoderState {
            range: u8::try_from(self.high).unwrap_or(u8::MAX),
            value: u8::try_from(self.code_word >> 16 & 0xFF).unwrap_or(0),
            count: u8::try_from(count).unwrap_or(0),
            macroblock_offset: u32::try_from(offset).unwrap_or(0),
        }
    }
}

/// Segmentation (§9.3), kept from frame to frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Segmentation {
    pub(crate) enabled: bool,
    pub(crate) update_map: bool,
    pub(crate) update_data: bool,
    /// The segments' values replace the frame's, rather than adding to them.
    pub(crate) absolute: bool,
    pub(crate) quantizer: [i8; MAX_SEGMENTS],
    pub(crate) filter_level: [i8; MAX_SEGMENTS],
}

/// The loop filter's deltas (§9.6), kept from frame to frame: by reference frame
/// (intra, last, golden, alternate) and by mode (B_PRED, ZEROMV, NEWMV and the rest,
/// SPLITMV).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LoopFilterDeltas {
    pub(crate) reference: [i8; 4],
    pub(crate) mode: [i8; 4],
}

/// The probabilities, kept from frame to frame unless a frame says not to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Probs {
    pub(crate) segment_tree: [u8; 3],
    pub(crate) coeff: CoeffProbs,
    pub(crate) y_mode: [u8; 4],
    pub(crate) uv_mode: [u8; 3],
    pub(crate) mv: [[u8; 19]; 2],
}

impl Default for Probs {
    fn default() -> Self {
        Self {
            segment_tree: [255; 3],
            coeff: DEFAULT_COEFF_PROBS,
            y_mode: Y_MODE_PROBS,
            uv_mode: UV_MODE_PROBS,
            mv: MV_PROBS,
        }
    }
}

/// What the golden or alternate frame holds after this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Update {
    /// What it held.
    Keep,
    /// This frame.
    Current,
    /// The last frame, as it was before this one.
    Last,
    /// The other of golden and alternate, as it was before this one.
    Other,
}

/// One frame's header, with the state it decodes against.
#[derive(Clone, Debug)]
pub(crate) struct Header {
    pub(crate) key_frame: bool,
    /// The version (0 to 3): which reconstruction and loop filters it uses.
    pub(crate) version: u8,
    pub(crate) show_frame: bool,
    /// The size, from the last key frame.
    pub(crate) width: u16,
    pub(crate) height: u16,
    /// The bytes before the first partition: the tag, and a key frame's start code
    /// and size.
    pub(crate) header_size: usize,
    /// The first partition's size.
    pub(crate) first_partition_size: u32,
    pub(crate) segmentation: Segmentation,
    pub(crate) filter_simple: bool,
    pub(crate) filter_level: u8,
    pub(crate) sharpness: u8,
    pub(crate) lf_adjust: bool,
    pub(crate) lf_delta_update: bool,
    pub(crate) lf_deltas: LoopFilterDeltas,
    /// The token partitions' sizes; `partitions` of them are used.
    pub(crate) partitions: usize,
    pub(crate) partition_sizes: [u32; MAX_PARTITIONS],
    /// The base quantizer index (Y AC).
    pub(crate) base_q: u8,
    /// The deltas for Y DC, Y2 DC, Y2 AC, UV DC and UV AC.
    pub(crate) q_deltas: [i8; 5],
    pub(crate) golden: Update,
    pub(crate) alternate: Update,
    pub(crate) sign_bias_golden: bool,
    pub(crate) sign_bias_alternate: bool,
    pub(crate) refresh_last: bool,
    pub(crate) mb_no_coeff_skip: bool,
    pub(crate) prob_skip_false: u8,
    pub(crate) prob_intra: u8,
    pub(crate) prob_last: u8,
    pub(crate) prob_golden: u8,
    /// The probabilities this frame decodes with.
    pub(crate) probs: Probs,
    pub(crate) coder: CoderState,
}

impl Header {
    /// Each segment's loop filter level (§9.6), 0 to 63.
    pub(crate) fn filter_levels(&self) -> [u8; MAX_SEGMENTS] {
        let seg = &self.segmentation;
        std::array::from_fn(|i| {
            let level = if seg.enabled {
                let own = i32::from(seg.filter_level[i]);
                if seg.absolute {
                    own
                } else {
                    own + i32::from(self.filter_level)
                }
            } else {
                i32::from(self.filter_level)
            };
            u8::try_from(level.clamp(0, 63)).unwrap_or(0)
        })
    }

    /// Each segment's quantizer indices (§9.6, §14.1): Y AC, Y DC, Y2 DC, Y2 AC, UV DC
    /// and UV AC, each 0 to 127.
    pub(crate) fn quantizer_indices(&self) -> [[u16; 6]; MAX_SEGMENTS] {
        let seg = &self.segmentation;
        std::array::from_fn(|i| {
            let base = i32::from(self.base_q);
            let base = if seg.enabled {
                let own = i32::from(seg.quantizer[i]);
                if seg.absolute { own } else { own + base }
            } else {
                base
            };
            let index = |delta: i8| u16::try_from((base + i32::from(delta)).clamp(0, 127));
            let d = self.q_deltas;
            [0, d[0], d[1], d[2], d[3], d[4]].map(|delta| index(delta).unwrap_or(0))
        })
    }
}

/// Reads frame headers, keeping what carries from one to the next.
#[derive(Debug, Default)]
pub(crate) struct Parser {
    /// The size, from the last key frame.
    size: Option<(u16, u16)>,
    probs: Probs,
    segmentation: Segmentation,
    lf_deltas: LoopFilterDeltas,
}

/// Whether `frame` is a key frame: its tag's lowest bit is clear.
pub(crate) fn is_key_frame(frame: &[u8]) -> bool {
    frame.first().is_some_and(|b| b & 1 == 0)
}

impl Parser {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Forgets everything, for a seek.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Reads `frame`'s header and updates what carries to the next frame.
    ///
    /// # Errors
    /// [`ParseError::Truncated`] for a frame shorter than its header says,
    /// [`ParseError::Invalid`] for a bad start code, size or partition table, or an
    /// inter frame before any key frame, and [`ParseError::Unsupported`] for a version
    /// above 3.
    pub(crate) fn parse(&mut self, frame: &[u8]) -> Result<Header> {
        let [t0, t1, t2, ..] = *frame else {
            return Err(ParseError::Truncated);
        };
        let tag = u32::from(t0) | u32::from(t1) << 8 | u32::from(t2) << 16;
        let key_frame = tag & 1 == 0;
        let version = u8::try_from(tag >> 1 & 7).unwrap_or(0);
        let show_frame = tag >> 4 & 1 == 1;
        let first_partition_size = tag >> 5;
        if version > 3 {
            return Err(ParseError::Unsupported("a VP8 version above 3"));
        }
        let header_size = if key_frame { 10 } else { 3 };
        let first = usize::try_from(first_partition_size).unwrap_or(usize::MAX);
        let rest = frame.get(header_size..).ok_or(ParseError::Truncated)?;
        if first > rest.len() {
            return Err(ParseError::Truncated);
        }
        if key_frame {
            if frame[3..6] != [0x9D, 0x01, 0x2A] {
                return Err(ParseError::Invalid("a VP8 start code"));
            }
            let width = u16::from_le_bytes([frame[6], frame[7]]) & 0x3FFF;
            let height = u16::from_le_bytes([frame[8], frame[9]]) & 0x3FFF;
            if width == 0 || height == 0 {
                return Err(ParseError::Invalid("a VP8 frame size of 0"));
            }
            self.size = Some((width, height));
            self.probs = Probs::default();
            self.segmentation = Segmentation::default();
            self.lf_deltas = LoopFilterDeltas::default();
        }
        let (width, height) = self.size.ok_or(ParseError::Invalid(
            "a VP8 inter frame before any key frame",
        ))?;

        let mut bd = BoolDecoder::new(rest, first);
        if key_frame {
            // color_space (BT.601 is the only one defined) and clamping_type (the
            // decoder clamps either way).
            bd.flag();
            bd.flag();
        }
        self.segmentation.enabled = bd.flag();
        if self.segmentation.enabled {
            self.read_segmentation(&mut bd);
        } else {
            self.segmentation.update_map = false;
            self.segmentation.update_data = false;
        }
        let filter_simple = bd.flag();
        let filter_level = bd.literal(6);
        let sharpness = bd.literal(3);
        let lf_adjust = bd.flag();
        let lf_delta_update = lf_adjust && bd.flag();
        if lf_delta_update {
            for i in 0..8 {
                if bd.flag() {
                    let magnitude = i8::try_from(bd.literal(6)).unwrap_or(0);
                    let value = if bd.flag() { -magnitude } else { magnitude };
                    if i < 4 {
                        self.lf_deltas.reference[i] = value;
                    } else {
                        self.lf_deltas.mode[i - 4] = value;
                    }
                }
            }
        }

        let partitions = 1_usize << bd.literal(2);
        let partition_sizes = Self::partition_sizes(&rest[first..], partitions)?;

        let base_q = bd.literal(7);
        let q_deltas = std::array::from_fn(|_| bd.signed(4));

        let mut golden = Update::Current;
        let mut alternate = Update::Current;
        let (mut sign_bias_golden, mut sign_bias_alternate) = (false, false);
        if !key_frame {
            let refresh_golden = bd.flag();
            let refresh_alternate = bd.flag();
            golden = Self::update(&mut bd, refresh_golden);
            alternate = Self::update(&mut bd, refresh_alternate);
            sign_bias_golden = bd.flag();
            sign_bias_alternate = bd.flag();
        }
        let refresh_probs = bd.flag();
        let saved = (!refresh_probs).then_some(self.probs);
        let refresh_last = key_frame || bd.flag();

        for (i, by_band) in COEFF_UPDATE_PROBS.iter().enumerate() {
            for (j, by_context) in by_band.iter().enumerate() {
                for (k, by_token) in by_context.iter().enumerate() {
                    for (l, &update) in by_token.iter().enumerate() {
                        if bd.get(update) {
                            self.probs.coeff[i][j][k][l] = bd.literal(8);
                        }
                    }
                }
            }
        }
        let mb_no_coeff_skip = bd.flag();
        let prob_skip_false = if mb_no_coeff_skip { bd.literal(8) } else { 0 };
        let (mut prob_intra, mut prob_last, mut prob_golden) = (0, 0, 0);
        if !key_frame {
            prob_intra = bd.literal(8);
            prob_last = bd.literal(8);
            prob_golden = bd.literal(8);
            if bd.flag() {
                self.probs.y_mode = std::array::from_fn(|_| bd.literal(8));
            }
            if bd.flag() {
                self.probs.uv_mode = std::array::from_fn(|_| bd.literal(8));
            }
            for (probs, updates) in self.probs.mv.iter_mut().zip(&MV_UPDATE_PROBS) {
                for (prob, &update) in probs.iter_mut().zip(updates) {
                    if bd.get(update) {
                        let v = bd.literal(7) << 1;
                        *prob = if v == 0 { 1 } else { v };
                    }
                }
            }
        }
        let coder = bd.state();

        let header = Header {
            key_frame,
            version,
            show_frame,
            width,
            height,
            header_size,
            first_partition_size,
            segmentation: self.segmentation,
            filter_simple,
            filter_level,
            sharpness,
            lf_adjust,
            lf_delta_update,
            lf_deltas: self.lf_deltas,
            partitions,
            partition_sizes,
            base_q,
            q_deltas,
            golden,
            alternate,
            sign_bias_golden,
            sign_bias_alternate,
            refresh_last,
            mb_no_coeff_skip,
            prob_skip_false,
            prob_intra,
            prob_last,
            prob_golden,
            probs: self.probs,
            coder,
        };
        if let Some(saved) = saved {
            self.probs = saved;
        }
        Ok(header)
    }

    /// Segmentation's update (§9.3): features left out of an update become 0, and
    /// tree probabilities left out of a map update 255.
    fn read_segmentation(&mut self, bd: &mut BoolDecoder<'_>) {
        let seg = &mut self.segmentation;
        seg.update_map = bd.flag();
        seg.update_data = bd.flag();
        if seg.update_data {
            seg.absolute = bd.flag();
            seg.quantizer = std::array::from_fn(|_| bd.signed(7));
            seg.filter_level = std::array::from_fn(|_| bd.signed(6));
        }
        if seg.update_map {
            self.probs.segment_tree =
                std::array::from_fn(|_| if bd.flag() { bd.literal(8) } else { 255 });
        }
    }

    /// What golden or alternate holds next (§9.7): this frame when `refresh` is set,
    /// otherwise what the 2-bit copy field says.
    fn update(bd: &mut BoolDecoder<'_>, refresh: bool) -> Update {
        if refresh {
            return Update::Current;
        }
        match bd.literal(2) {
            1 => Update::Last,
            2 => Update::Other,
            _ => Update::Keep,
        }
    }

    /// The token partitions' sizes (§9.5): all but the last are listed, 3 bytes each,
    /// before the first; the last takes the rest.
    fn partition_sizes(after: &[u8], count: usize) -> Result<[u32; MAX_PARTITIONS]> {
        let table = 3 * (count - 1);
        let mut left = after
            .len()
            .checked_sub(table)
            .ok_or(ParseError::Invalid("a VP8 partition table"))?;
        let mut sizes = [0_u32; MAX_PARTITIONS];
        for (i, size) in sizes.iter_mut().enumerate().take(count - 1) {
            let b = &after[3 * i..3 * i + 3];
            let n = usize::from(b[0]) | usize::from(b[1]) << 8 | usize::from(b[2]) << 16;
            left = left
                .checked_sub(n)
                .ok_or(ParseError::Invalid("a VP8 partition size"))?;
            *size = u32::try_from(n).unwrap_or(0);
        }
        sizes[count - 1] = u32::try_from(left).unwrap_or(u32::MAX);
        Ok(sizes)
    }
}

#[cfg(test)]
mod tests;
