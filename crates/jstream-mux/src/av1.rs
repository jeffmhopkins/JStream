//! AV1 framing: OBUs in the low-overhead bitstream format, and the
//! `AV1CodecConfigurationRecord` built from the sequence header OBU.
//!
//! An AV1 [`Packet`](jstream_media::Packet) payload is one temporal unit: OBUs, each with
//! its size field except perhaps the last, as encoders emit them. FLV (through enhanced
//! RTMP) and MP4 store the same OBUs without temporal delimiters or padding
//! ([`to_sample`]), and carry the sequence header once more in the configuration record
//! ([`Av1CodecConfig`]).

use crate::MuxError;
use crate::bits::BitReader;

/// OBU type of a sequence header.
pub const OBU_SEQUENCE_HEADER: u8 = 1;
/// OBU type of a temporal delimiter.
pub const OBU_TEMPORAL_DELIMITER: u8 = 2;
/// OBU type of padding.
pub const OBU_PADDING: u8 = 15;

/// One OBU, borrowed from the data it was found in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Obu<'a> {
    /// `obu_type`.
    pub obu_type: u8,
    /// The header: one byte, or two with the extension (temporal and spatial IDs).
    pub header: &'a [u8],
    /// `obu_size` bytes of payload.
    pub payload: &'a [u8],
    /// The whole OBU as it appeared: header, size field (if any) and payload.
    pub raw: &'a [u8],
}

impl Obu<'_> {
    /// The OBU with its size field present, as configuration records require: `raw`
    /// unchanged if it already has one, else rebuilt with `obu_has_size_field` set.
    pub fn with_size_field(&self) -> Vec<u8> {
        if self.header[0] & 0x02 != 0 {
            return self.raw.to_vec();
        }
        let mut out = Vec::with_capacity(self.raw.len() + 8);
        out.push(self.header[0] | 0x02);
        out.extend_from_slice(&self.header[1..]);
        write_leb128(&mut out, self.payload.len() as u64);
        out.extend_from_slice(self.payload);
        out
    }
}

/// Splits low-overhead AV1 data into OBUs. An OBU without a size field runs to the end
/// of `data`, so only the last one may omit it.
///
/// # Errors
/// [`MuxError::InvalidObu`] for a set forbidden bit, a size that runs past the end of
/// `data`, or a size field longer than eight bytes or above 2^32 − 1.
pub fn obus(data: &[u8]) -> Result<Vec<Obu<'_>>, MuxError> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let invalid = |reason| MuxError::InvalidObu {
            offset: pos,
            reason,
        };
        let first = data[pos];
        if first & 0x80 != 0 {
            return Err(invalid("obu_forbidden_bit is set"));
        }
        let header_len = if first & 0x04 != 0 { 2 } else { 1 };
        let header = data
            .get(pos..pos + header_len)
            .ok_or(invalid("header truncated"))?;
        let mut cursor = pos + header_len;
        let size = if first & 0x02 != 0 {
            let (size, len) = read_leb128(&data[cursor..]).ok_or(invalid("bad obu_size"))?;
            cursor += len;
            usize::try_from(size).map_err(|_| invalid("bad obu_size"))?
        } else {
            data.len() - cursor
        };
        let end = cursor
            .checked_add(size)
            .filter(|&end| end <= data.len())
            .ok_or(invalid("obu_size runs past the end"))?;
        out.push(Obu {
            obu_type: (first >> 3) & 0x0F,
            header,
            payload: &data[cursor..end],
            raw: &data[pos..end],
        });
        pos = end;
    }
    Ok(out)
}

/// One temporal unit as FLV and MP4 store it: the OBUs unchanged, without temporal
/// delimiters and padding, which the AV1 ISOBMFF binding says should not be stored.
///
/// # Errors
/// As [`obus`].
pub fn to_sample(data: &[u8]) -> Result<Vec<u8>, MuxError> {
    let mut out = Vec::with_capacity(data.len());
    for obu in obus(data)? {
        if !matches!(obu.obu_type, OBU_TEMPORAL_DELIMITER | OBU_PADDING) {
            out.extend_from_slice(obu.raw);
        }
    }
    Ok(out)
}

/// `leb128()`: the value and how many bytes it took; `None` past the end, beyond eight
/// bytes, or above 2^32 − 1 (the AV1 limit).
fn read_leb128(data: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for (i, &byte) in data.iter().take(8).enumerate() {
        value |= u64::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            return (value <= u64::from(u32::MAX)).then_some((value, i + 1));
        }
    }
    None
}

fn write_leb128(out: &mut Vec<u8>, mut value: u64) {
    loop {
        // The low seven bits.
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// The AV1 decoder configuration that FLV (through enhanced RTMP) and MP4 carry once,
/// ahead of the frames: the `AV1CodecConfigurationRecord` of the AV1 ISOBMFF binding,
/// with the sequence header OBU as its `configOBUs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Av1CodecConfig {
    sequence_header: Vec<u8>,
    info: SequenceInfo,
}

impl Av1CodecConfig {
    /// A configuration from the first sequence header OBU in `data`, typically the first
    /// keyframe's temporal unit. The profile, level, tier, bit depth, monochrome flag and
    /// chroma subsampling are read from it.
    ///
    /// # Errors
    /// As [`obus`], [`MuxError::MissingAv1SequenceHeader`] when there is none, and
    /// [`MuxError::InvalidAv1SequenceHeader`] when it can't be read.
    pub fn from_obus(data: &[u8]) -> Result<Self, MuxError> {
        let obu = obus(data)?
            .into_iter()
            .find(|o| o.obu_type == OBU_SEQUENCE_HEADER)
            .ok_or(MuxError::MissingAv1SequenceHeader)?;
        let info = SequenceInfo::parse(obu.payload)?;
        Ok(Self {
            sequence_header: obu.with_size_field(),
            info,
        })
    }

    /// The sequence header OBU stored in the record, with its size field.
    pub fn sequence_header(&self) -> &[u8] {
        &self.sequence_header
    }

    /// `seq_profile` (0 Main, 1 High, 2 Professional).
    pub fn profile(&self) -> u8 {
        self.info.profile
    }

    /// `seq_level_idx[0]` (e.g. 8 for level 4.0).
    pub fn level_idx(&self) -> u8 {
        self.info.level_idx
    }

    /// The serialized `AV1CodecConfigurationRecord`: four bytes of fields, no initial
    /// presentation delay, and the sequence header OBU.
    pub fn to_bytes(&self) -> Vec<u8> {
        let i = &self.info;
        let mut out = vec![
            0x81, // marker 1, version 1
            (i.profile << 5) | i.level_idx,
            (u8::from(i.tier) << 7)
                | (u8::from(i.high_bitdepth) << 6)
                | (u8::from(i.twelve_bit) << 5)
                | (u8::from(i.monochrome) << 4)
                | (u8::from(i.subsampling_x) << 3)
                | (u8::from(i.subsampling_y) << 2)
                | i.chroma_sample_position,
            0x00, // reserved, initial_presentation_delay_present 0
        ];
        out.extend_from_slice(&self.sequence_header);
        out
    }
}

/// The sequence header fields the configuration record needs.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SequenceInfo {
    profile: u8,
    level_idx: u8,
    tier: bool,
    high_bitdepth: bool,
    twelve_bit: bool,
    monochrome: bool,
    subsampling_x: bool,
    subsampling_y: bool,
    chroma_sample_position: u8,
}

impl SequenceInfo {
    fn parse(payload: &[u8]) -> Result<Self, MuxError> {
        Self::read(&mut BitReader::new(payload)).ok_or(MuxError::InvalidAv1SequenceHeader {
            reason: "truncated",
        })?
    }

    /// Reads `sequence_header_obu()` up to the end of `color_config()`: `None` if it ends
    /// first. The field names follow the AV1 specification, section 5.5.
    fn read(r: &mut BitReader<'_>) -> Option<Result<Self, MuxError>> {
        let profile = r.bits(3)?;
        if profile > 2 {
            return Some(Err(MuxError::InvalidAv1SequenceHeader {
                reason: "seq_profile above 2",
            }));
        }
        r.flag()?; // still_picture
        let reduced_still_picture_header = r.flag()?;
        let level_idx;
        let mut tier = false;
        if reduced_still_picture_header {
            level_idx = r.bits(5)?;
        } else {
            let timing_info_present = r.flag()?;
            let mut decoder_model_info_present = false;
            let mut buffer_delay_length = 0;
            if timing_info_present {
                r.skip(64)?; // num_units_in_display_tick, time_scale
                if r.flag()? {
                    // equal_picture_interval
                    r.uvlc()?; // num_ticks_per_picture_minus_1
                }
                decoder_model_info_present = r.flag()?;
                if decoder_model_info_present {
                    buffer_delay_length = r.bits(5)? as usize + 1;
                    r.skip(32 + 5 + 5)?;
                }
            }
            let initial_display_delay_present = r.flag()?;
            let operating_points = r.bits(5)? + 1;
            let mut first = None;
            for _ in 0..operating_points {
                r.skip(12)?; // operating_point_idc
                let level = r.bits(5)?;
                let op_tier = level > 7 && r.flag()?;
                first.get_or_insert((level, op_tier));
                if decoder_model_info_present && r.flag()? {
                    // operating_parameters_info: two delays and low_delay_mode_flag
                    r.skip(2 * buffer_delay_length + 1)?;
                }
                if initial_display_delay_present && r.flag()? {
                    r.skip(4)?; // initial_display_delay_minus_1
                }
            }
            (level_idx, tier) = first?;
        }

        let width_bits = r.bits(4)? as usize + 1;
        let height_bits = r.bits(4)? as usize + 1;
        r.skip(width_bits + height_bits)?; // max_frame_width/height_minus_1
        if !reduced_still_picture_header && r.flag()? {
            // frame_id_numbers_present_flag: delta and additional frame ID lengths
            r.skip(4 + 3)?;
        }
        // use_128x128_superblock, enable_filter_intra, enable_intra_edge_filter
        r.skip(3)?;
        if !reduced_still_picture_header {
            // enable_interintra_compound, enable_masked_compound, enable_warped_motion,
            // enable_dual_filter
            r.skip(4)?;
            let enable_order_hint = r.flag()?;
            if enable_order_hint {
                r.skip(2)?; // enable_jnt_comp, enable_ref_frame_mvs
            }
            // seq_choose_screen_content_tools, else seq_force_screen_content_tools
            let force_screen_content_tools = if r.flag()? { 2 } else { r.bits(1)? };
            if force_screen_content_tools > 0 && !r.flag()? {
                // seq_choose_integer_mv is 0: seq_force_integer_mv
                r.skip(1)?;
            }
            if enable_order_hint {
                r.skip(3)?; // order_hint_bits_minus_1
            }
        }
        r.skip(3)?; // enable_superres, enable_cdef, enable_restoration

        // color_config()
        let high_bitdepth = r.flag()?;
        let twelve_bit = profile == 2 && high_bitdepth && r.flag()?;
        let monochrome = profile != 1 && r.flag()?;
        let (mut primaries, mut transfer, mut matrix) = (2, 2, 2);
        if r.flag()? {
            // color_description_present_flag
            primaries = r.bits(8)?;
            transfer = r.bits(8)?;
            matrix = r.bits(8)?;
        }
        let (subsampling_x, subsampling_y, mut chroma_sample_position) = if monochrome {
            (true, true, 0)
        } else if (primaries, transfer, matrix) == (1, 13, 0) {
            // BT.709 primaries, sRGB transfer, identity matrix: 4:4:4, no color_range bit
            (false, false, 0)
        } else {
            r.skip(1)?; // color_range
            match profile {
                0 => (true, true, 0),
                1 => (false, false, 0),
                _ if twelve_bit => {
                    let x = r.flag()?;
                    (x, x && r.flag()?, 0)
                }
                _ => (true, false, 0),
            }
        };
        if !monochrome && subsampling_x && subsampling_y {
            chroma_sample_position = r.bits(2)?;
        }
        // Each value was read from at most as many bits as its field holds.
        Some(Ok(Self {
            profile: profile as u8,
            level_idx: level_idx as u8,
            tier,
            high_bitdepth,
            twelve_bit,
            monochrome,
            subsampling_x,
            subsampling_y,
            chroma_sample_position: chroma_sample_position as u8,
        }))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The sequence header OBU libaom wrote for a 64x64 RGB stream: High profile (1),
    /// level 2.0 (0), sRGB, so 4:4:4 with no color_range bit.
    pub(crate) const SEQUENCE_HEADER_444: [u8; 15] = [
        0x0A, 0x0D, 0x20, 0x00, 0x00, 0x02, 0xAF, 0xFF, 0x9B, 0x5F, 0x24, 0x04, 0x34, 0x00, 0x80,
    ];

    /// A keyframe temporal unit: temporal delimiter, sequence header, and a stand-in frame
    /// OBU (type 6) whose last OBU omits its size field.
    pub(crate) fn keyframe_tu() -> Vec<u8> {
        let mut tu = vec![0x12, 0x00];
        tu.extend_from_slice(&SEQUENCE_HEADER_444);
        tu.extend_from_slice(&[0x30, 0xAA, 0xBB]);
        tu
    }

    #[test]
    fn splits_obus() {
        let tu = keyframe_tu();
        let obus = obus(&tu).unwrap();
        let types: Vec<_> = obus.iter().map(|o| o.obu_type).collect();
        assert_eq!(types, [OBU_TEMPORAL_DELIMITER, OBU_SEQUENCE_HEADER, 6]);
        assert_eq!(obus[1].header, &[0x0A]);
        assert_eq!(obus[1].payload, &SEQUENCE_HEADER_444[2..]);
        assert_eq!(obus[1].raw, &SEQUENCE_HEADER_444);
        assert_eq!(
            obus[2].payload,
            &[0xAA, 0xBB],
            "no size field: runs to the end"
        );

        // With the extension byte (temporal and spatial IDs) and a two-byte size
        let mut ext = vec![0x36, 0x28, 0x80, 0x01];
        ext.extend_from_slice(&[7; 128]);
        let o = super::obus(&ext).unwrap();
        assert_eq!(
            (o[0].obu_type, o[0].header.len(), o[0].payload.len()),
            (6, 2, 128)
        );
    }

    #[test]
    fn malformed_obus_are_errors() {
        let err = |data: &[u8]| super::obus(data).unwrap_err();
        assert!(matches!(
            err(&[0x12, 0x00, 0x92, 0x00]),
            MuxError::InvalidObu {
                offset: 2,
                reason: "obu_forbidden_bit is set"
            }
        ));
        assert!(matches!(
            err(&[0x32, 0x05, 0xAA]),
            MuxError::InvalidObu {
                reason: "obu_size runs past the end",
                ..
            }
        ));
        assert!(matches!(
            err(&[0x32, 0x80]),
            MuxError::InvalidObu {
                reason: "bad obu_size",
                ..
            }
        ));
        // 2^32 does not fit
        assert!(matches!(
            err(&[0x32, 0x80, 0x80, 0x80, 0x80, 0x10]),
            MuxError::InvalidObu {
                reason: "bad obu_size",
                ..
            }
        ));
        assert!(matches!(
            err(&[0x36]),
            MuxError::InvalidObu {
                reason: "header truncated",
                ..
            }
        ));
    }

    #[test]
    fn sample_drops_delimiters_and_padding() {
        let mut tu = keyframe_tu();
        tu.splice(2..2, [0x7A, 0x02, 0x00, 0x00]); // padding OBU
        let mut expected = SEQUENCE_HEADER_444.to_vec();
        expected.extend_from_slice(&[0x30, 0xAA, 0xBB]);
        assert_eq!(to_sample(&tu).unwrap(), expected);
    }

    /// The record FFmpeg 6.1's FLV muxer wrote for the same stream, byte for byte.
    #[test]
    fn record_is_byte_exact() {
        let cfg = Av1CodecConfig::from_obus(&keyframe_tu()).unwrap();
        assert_eq!((cfg.profile(), cfg.level_idx()), (1, 0));
        let mut expected = vec![0x81, 0x20, 0x00, 0x00];
        expected.extend_from_slice(&SEQUENCE_HEADER_444);
        assert_eq!(cfg.to_bytes(), expected);
        assert_eq!(cfg.sequence_header(), &SEQUENCE_HEADER_444);

        assert!(matches!(
            Av1CodecConfig::from_obus(&[0x12, 0x00, 0x32, 0x00]),
            Err(MuxError::MissingAv1SequenceHeader)
        ));
    }

    /// Bits for a sequence header, padded with trailing bits.
    fn header_bits(bits: &str) -> Vec<u8> {
        let mut bits: String = bits.chars().filter(|c| !c.is_whitespace()).collect();
        bits.push('1');
        while !bits.len().is_multiple_of(8) {
            bits.push('0');
        }
        bits.as_bytes()
            .chunks(8)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 2).unwrap())
            .collect()
    }

    fn config_from_payload(payload: &[u8]) -> Result<Av1CodecConfig, MuxError> {
        // A sequence header OBU without a size field, to exercise the rebuild too.
        let mut obu = vec![0x08];
        obu.extend_from_slice(payload);
        Av1CodecConfig::from_obus(&obu)
    }

    #[test]
    fn main_profile_10_bit_with_timing_and_decoder_model() {
        let payload = header_bits(
            "000 0 0 \
             1 \
             00000000000000000000000000000001 00000000000000000000000000011110 1 1 \
             1 00100 00000000000000000000000000000001 00100 00100 \
             1 00000 \
             000000000000 01001 1 1 00010 01001 0 1 1111 \
             0101 0101 101010 101010 \
             0 000 \
             0000 1 11 1 0 1 010 000 \
             1 0 0 0 01",
        );
        // seq_profile 0, timing info with equal_picture_interval and a decoder model
        // (buffer delays of 5 bits), initial display delay present, one operating point
        // at level 9 (4.1) with high tier, a decoder model and a display delay; 64x64;
        // 10-bit, colour description absent, color_range 0, chroma_sample_position 1.
        let cfg = config_from_payload(&payload).unwrap();
        assert_eq!((cfg.profile(), cfg.level_idx()), (0, 9));
        let bytes = cfg.to_bytes();
        assert_eq!(&bytes[..4], &[0x81, 0x09, 0b1100_1101, 0x00]);
        // The OBU was rebuilt with a size field.
        assert_eq!(bytes[4], 0x0A);
        assert_eq!(usize::from(bytes[5]), payload.len());
        assert_eq!(&bytes[6..], &payload[..]);
    }

    #[test]
    fn reduced_still_picture_and_professional_twelve_bit() {
        // seq_profile 2, still, reduced header, level 1; 1x1; 12-bit 4:2:2 (x 1, y 0)
        // with a BT.2020 colour description.
        let payload = header_bits(
            "010 1 1 00001 \
             0000 0000 0 0 \
             000 \
             000 \
             1 1 0 1 00001001 00010000 00001001 0 1 0 0",
        );
        let cfg = config_from_payload(&payload).unwrap();
        assert_eq!((cfg.profile(), cfg.level_idx()), (2, 1));
        assert_eq!(&cfg.to_bytes()[..4], &[0x81, 0x41, 0b0110_1000, 0x00]);

        // Monochrome Main: 4:0:0 reports subsampling 1, 1
        let payload = header_bits("000 1 1 00001 0000 0000 0 0 000 000 0 1 0 0 0");
        let cfg = config_from_payload(&payload).unwrap();
        assert_eq!(&cfg.to_bytes()[..4], &[0x81, 0x01, 0b0001_1100, 0x00]);
    }

    #[test]
    fn malformed_sequence_header_is_an_error() {
        assert!(matches!(
            config_from_payload(&[0xE0]),
            Err(MuxError::InvalidAv1SequenceHeader {
                reason: "seq_profile above 2"
            })
        ));
        assert!(matches!(
            config_from_payload(&SEQUENCE_HEADER_444[2..8]),
            Err(MuxError::InvalidAv1SequenceHeader {
                reason: "truncated"
            })
        ));
    }

    #[test]
    fn leb128_round_trips() {
        for v in [0, 1, 127, 128, 300, u64::from(u32::MAX)] {
            let mut out = Vec::new();
            write_leb128(&mut out, v);
            assert_eq!(read_leb128(&out), Some((v, out.len())));
        }
        assert_eq!(read_leb128(&[0x80; 9]), None);
    }
}
