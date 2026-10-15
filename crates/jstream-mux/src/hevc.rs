//! HEVC (H.265) framing: NAL unit types, and the `HEVCDecoderConfigurationRecord` built
//! from VPS, SPS and PPS.
//!
//! HEVC [`Packet`](jstream_media::Packet) payloads are **Annex B**, like H.264, and the
//! start-code framing is the same, so [`h264::annexb_nalus`](crate::h264::annexb_nalus)
//! splits them and [`h264::annexb_to_avcc`](crate::h264::annexb_to_avcc) converts them
//! to 4-byte length prefixes. Only the NAL unit header (two bytes) and the parameter sets
//! differ, and those are here.

use crate::MuxError;
use crate::bits::{BitReader, unescape};
use crate::h264::annexb_nalus;

/// NAL unit type of a video parameter set.
pub const NAL_VPS: u8 = 32;
/// NAL unit type of a sequence parameter set.
pub const NAL_SPS: u8 = 33;
/// NAL unit type of a picture parameter set.
pub const NAL_PPS: u8 = 34;

/// The `nal_unit_type` of an HEVC NAL unit (bits 1 to 6 of its first byte); `None` for
/// an empty slice.
pub fn nal_type(nalu: &[u8]) -> Option<u8> {
    nalu.first().map(|b| (b >> 1) & 0x3F)
}

/// The HEVC decoder configuration that FLV (through enhanced RTMP) and MP4 carry once,
/// ahead of the frames: the `HEVCDecoderConfigurationRecord` of ISO/IEC 14496-15, built
/// from VPS, SPS and PPS NAL units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HevcDecoderConfig {
    vps: Vec<Vec<u8>>,
    sps: Vec<Vec<u8>>,
    pps: Vec<Vec<u8>>,
    info: SpsInfo,
}

impl HevcDecoderConfig {
    /// A configuration from VPS, SPS and PPS NAL units (without start codes or length
    /// prefixes). The profile, tier, level, chroma format, bit depths and temporal
    /// layering are read from the first SPS.
    ///
    /// # Errors
    /// [`MuxError::MissingHevcParameterSets`] when any list is empty,
    /// [`MuxError::InvalidHevcSps`] when the first SPS can't be read, and
    /// [`MuxError::TooLong`] for more than 65535 sets of one type, or a set of 64 KiB or
    /// more.
    pub fn new(vps: Vec<Vec<u8>>, sps: Vec<Vec<u8>>, pps: Vec<Vec<u8>>) -> Result<Self, MuxError> {
        let Some(first) = sps.first().filter(|_| !vps.is_empty() && !pps.is_empty()) else {
            return Err(MuxError::MissingHevcParameterSets {
                vps: vps.len(),
                sps: sps.len(),
                pps: pps.len(),
            });
        };
        let info = SpsInfo::parse(first)?;
        for list in [&vps, &sps, &pps] {
            check_len("parameter set list", list.len())?;
            for set in list {
                check_len("parameter set", set.len())?;
            }
        }
        Ok(Self {
            vps,
            sps,
            pps,
            info,
        })
    }

    /// A configuration from the VPS, SPS and PPS NAL units found in an Annex B access
    /// unit, typically the first keyframe an encoder emits.
    ///
    /// # Errors
    /// As [`annexb_nalus`] and [`HevcDecoderConfig::new`].
    pub fn from_annexb(data: &[u8]) -> Result<Self, MuxError> {
        let nalus = annexb_nalus(data)?;
        let pick = |t| {
            nalus
                .iter()
                .filter(|n| nal_type(n) == Some(t))
                .map(|n| n.to_vec())
                .collect::<Vec<_>>()
        };
        Self::new(pick(NAL_VPS), pick(NAL_SPS), pick(NAL_PPS))
    }

    /// The VPS NAL units.
    pub fn vps(&self) -> &[Vec<u8>] {
        &self.vps
    }

    /// The SPS NAL units.
    pub fn sps(&self) -> &[Vec<u8>] {
        &self.sps
    }

    /// The PPS NAL units.
    pub fn pps(&self) -> &[Vec<u8>] {
        &self.pps
    }

    /// `general_profile_idc` from the first SPS (1 Main, 2 Main 10, …).
    pub fn profile_idc(&self) -> u8 {
        self.info.profile_byte & 0x1F
    }

    /// `general_level_idc` from the first SPS: 30 times the level (e.g. 123 for 4.1).
    pub fn level_idc(&self) -> u8 {
        self.info.level_idc
    }

    /// The serialized `HEVCDecoderConfigurationRecord`, with 4-byte NAL length fields and
    /// one array each for the VPS, SPS and PPS.
    ///
    /// The arrays are marked incomplete (`array_completeness` 0), because the frames keep
    /// their in-band parameter sets, as FFmpeg writes FLV. `min_spatial_segmentation_idc`,
    /// `parallelismType`, `avgFrameRate` and `constantFrameRate` are written as 0
    /// (unknown), which the record allows.
    pub fn to_bytes(&self) -> Vec<u8> {
        let info = &self.info;
        let mut out = vec![1, info.profile_byte]; // configurationVersion
        out.extend_from_slice(&info.compatibility_flags.to_be_bytes());
        out.extend_from_slice(&info.constraint_flags);
        out.extend_from_slice(&[
            info.level_idc,
            0xF0, // reserved '1111' + min_spatial_segmentation_idc (12 bits)
            0x00,
            0xFC, // reserved '111111' + parallelismType
            0xFC | info.chroma_format_idc,
            0xF8 | info.bit_depth_luma_minus8,
            0xF8 | info.bit_depth_chroma_minus8,
            0x00, // avgFrameRate (16 bits)
            0x00,
            // constantFrameRate 0, numTemporalLayers, temporalIdNested, lengthSizeMinusOne 3
            (info.temporal_layers << 3) | (u8::from(info.temporal_id_nested) << 2) | 3,
            3, // numOfArrays
        ]);
        for (nal_type, list) in [
            (NAL_VPS, &self.vps),
            (NAL_SPS, &self.sps),
            (NAL_PPS, &self.pps),
        ] {
            // array_completeness 0, reserved 0, NAL_unit_type
            out.push(nal_type);
            // Counts and lengths were checked in `new`.
            out.extend_from_slice(&(list.len() as u16).to_be_bytes());
            for set in list {
                out.extend_from_slice(&(set.len() as u16).to_be_bytes());
                out.extend_from_slice(set);
            }
        }
        out
    }
}

fn check_len(what: &'static str, len: usize) -> Result<(), MuxError> {
    let max = usize::from(u16::MAX);
    if len > max {
        Err(MuxError::TooLong { what, len, max })
    } else {
        Ok(())
    }
}

/// The SPS fields the decoder configuration record needs.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SpsInfo {
    /// `general_profile_space` (2 bits), `general_tier_flag` and `general_profile_idc`
    /// (5 bits), as the record stores them.
    profile_byte: u8,
    compatibility_flags: u32,
    /// The 48 bits after the compatibility flags: progressive, interlaced, non-packed and
    /// frame-only flags, then 44 constraint bits.
    constraint_flags: [u8; 6],
    level_idc: u8,
    chroma_format_idc: u8,
    bit_depth_luma_minus8: u8,
    bit_depth_chroma_minus8: u8,
    /// `sps_max_sub_layers_minus1 + 1`.
    temporal_layers: u8,
    temporal_id_nested: bool,
}

impl SpsInfo {
    fn parse(sps: &[u8]) -> Result<Self, MuxError> {
        let invalid = |reason| MuxError::InvalidHevcSps { reason };
        if nal_type(sps) != Some(NAL_SPS) {
            return Err(invalid("NAL unit type is not 33"));
        }
        // The NAL unit header is two bytes.
        let rbsp = unescape(sps.get(2..).unwrap_or_default());
        Self::read(&mut BitReader::new(&rbsp)).ok_or(invalid("truncated"))?
    }

    /// Reads the SPS up to the bit depths: `None` if it ends first.
    fn read(r: &mut BitReader<'_>) -> Option<Result<Self, MuxError>> {
        let invalid = |reason| Some(Err(MuxError::InvalidHevcSps { reason }));
        r.bits(4)?; // sps_video_parameter_set_id
        let max_sub_layers_minus1 = r.bits(3)?;
        if max_sub_layers_minus1 > 6 {
            return invalid("sps_max_sub_layers_minus1 above 6");
        }
        let temporal_id_nested = r.flag()?;

        // profile_tier_level(1, sps_max_sub_layers_minus1): the general part, 12 bytes.
        let mut general = [0u8; 12];
        for byte in &mut general {
            // 8 bits fit in u8.
            *byte = r.bits(8)? as u8;
        }
        let mut sub_profile = [false; 6];
        let mut sub_level = [false; 6];
        for i in 0..max_sub_layers_minus1 as usize {
            sub_profile[i] = r.flag()?;
            sub_level[i] = r.flag()?;
        }
        if max_sub_layers_minus1 > 0 {
            r.skip(2 * (8 - max_sub_layers_minus1 as usize))?; // reserved_zero_2bits
        }
        for i in 0..max_sub_layers_minus1 as usize {
            if sub_profile[i] {
                r.skip(88)?;
            }
            if sub_level[i] {
                r.skip(8)?;
            }
        }

        r.ue()?; // sps_seq_parameter_set_id
        let chroma_format_idc = r.ue()?;
        if chroma_format_idc > 3 {
            return invalid("chroma_format_idc above 3");
        }
        if chroma_format_idc == 3 {
            r.flag()?; // separate_colour_plane_flag
        }
        r.ue()?; // pic_width_in_luma_samples
        r.ue()?; // pic_height_in_luma_samples
        if r.flag()? {
            // conformance_window_flag: four offsets
            for _ in 0..4 {
                r.ue()?;
            }
        }
        let luma = r.ue()?;
        let chroma = r.ue()?;
        if luma > 7 || chroma > 7 {
            // The record has three bits for each.
            return invalid("bit depth above 15");
        }
        let [
            profile_byte,
            c0,
            c1,
            c2,
            c3,
            k0,
            k1,
            k2,
            k3,
            k4,
            k5,
            level_idc,
        ] = general;
        // Range-checked above.
        Some(Ok(Self {
            profile_byte,
            compatibility_flags: u32::from_be_bytes([c0, c1, c2, c3]),
            constraint_flags: [k0, k1, k2, k3, k4, k5],
            level_idc,
            chroma_format_idc: chroma_format_idc as u8,
            bit_depth_luma_minus8: luma as u8,
            bit_depth_chroma_minus8: chroma as u8,
            temporal_layers: max_sub_layers_minus1 as u8 + 1,
            temporal_id_nested,
        }))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::h264::annexb_to_avcc;

    // Parameter sets libx265 wrote for a 64x64 Main-profile 4:2:0 8-bit stream, level 1.
    pub(crate) const VPS: [u8; 24] = [
        0x40, 0x01, 0x0C, 0x01, 0xFF, 0xFF, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00,
        0x03, 0x00, 0x00, 0x03, 0x00, 0x1E, 0x91, 0x30, 0x24,
    ];
    pub(crate) const SPS: [u8; 41] = [
        0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x03, 0x00, 0x1E, 0xA0, 0x20, 0x81, 0x05, 0x96, 0x44, 0xE4, 0x93, 0x2B, 0xC0, 0x5A, 0x02,
        0x00, 0x00, 0x03, 0x00, 0x02, 0x00, 0x00, 0x03, 0x00, 0x3C, 0x10,
    ];
    pub(crate) const PPS: [u8; 7] = [0x44, 0x01, 0xC1, 0x72, 0xB4, 0x22, 0x40];

    /// An IDR access unit (IDR_N_LP, type 20) carrying its parameter sets in-band, as
    /// encoders emit keyframes.
    pub(crate) fn idr_annexb() -> Vec<u8> {
        let mut au = Vec::new();
        for nalu in [&VPS[..], &SPS[..], &PPS[..], &[0x28, 0x01, 0xAF, 0x08]] {
            au.extend_from_slice(&[0, 0, 0, 1]);
            au.extend_from_slice(nalu);
        }
        au
    }

    fn config() -> HevcDecoderConfig {
        HevcDecoderConfig::new(vec![VPS.to_vec()], vec![SPS.to_vec()], vec![PPS.to_vec()]).unwrap()
    }

    #[test]
    fn nal_types() {
        assert_eq!(nal_type(&VPS), Some(NAL_VPS));
        assert_eq!(nal_type(&SPS), Some(NAL_SPS));
        assert_eq!(nal_type(&PPS), Some(NAL_PPS));
        assert_eq!(nal_type(&[0x28, 0x01]), Some(20));
        assert_eq!(nal_type(&[]), None);
    }

    /// The record FFmpeg 6.1's FLV muxer wrote for the same stream, byte for byte.
    #[test]
    fn record_is_byte_exact() {
        let cfg = config();
        assert_eq!((cfg.profile_idc(), cfg.level_idc()), (1, 30));
        let mut expected = vec![
            0x01, // configurationVersion
            0x01, // profile space 0, main tier, Main
            0x60, 0x00, 0x00, 0x00, // compatibility: Main, Main 10
            0x90, 0x00, 0x00, 0x00, 0x00, 0x00, // progressive, frame only
            0x1E, // level 1
            0xF0, 0x00, 0xFC, // min_spatial_segmentation_idc, parallelismType
            0xFD, 0xF8, 0xF8, // 4:2:0, 8-bit luma and chroma
            0x00, 0x00, // avgFrameRate
            0x0F, // one temporal layer, nested, 4-byte lengths
            0x03, // three arrays
        ];
        for (t, set) in [(0x20, &VPS[..]), (0x21, &SPS[..]), (0x22, &PPS[..])] {
            expected.extend_from_slice(&[t, 0x00, 0x01, 0x00, set.len() as u8]);
            expected.extend_from_slice(set);
        }
        assert_eq!(cfg.to_bytes(), expected);
    }

    #[test]
    fn from_annexb_extracts_parameter_sets() {
        let cfg = HevcDecoderConfig::from_annexb(&idr_annexb()).unwrap();
        assert_eq!(cfg, config());
        assert_eq!(cfg.vps(), &[VPS.to_vec()]);
        assert_eq!(cfg.sps(), &[SPS.to_vec()]);
        assert_eq!(cfg.pps(), &[PPS.to_vec()]);

        // The frame keeps its in-band parameter sets when converted to length prefixes.
        let avcc = annexb_to_avcc(&idr_annexb()).unwrap();
        assert_eq!(&avcc[..6], &[0, 0, 0, 24, 0x40, 0x01]);

        assert!(matches!(
            HevcDecoderConfig::from_annexb(&[0, 0, 1, 0x28, 0x01, 0xAF]),
            Err(MuxError::MissingHevcParameterSets {
                vps: 0,
                sps: 0,
                pps: 0
            })
        ));
        assert!(matches!(
            HevcDecoderConfig::new(vec![VPS.to_vec()], vec![SPS.to_vec()], vec![]),
            Err(MuxError::MissingHevcParameterSets {
                vps: 1,
                sps: 1,
                pps: 0
            })
        ));
    }

    #[test]
    fn main_10_with_sub_layers_and_conformance_window() {
        // A hand-built SPS: vps id 0, max_sub_layers_minus1 1, nesting 1; general PTL for
        // Main 10 (profile 2), high tier, level 4.1 (123); sub-layer 0 with only a level
        // present; then sps id ue(0), chroma ue(1), width ue(0), height ue(0), a
        // conformance window of four ue(0), luma ue(2), chroma ue(2).
        let mut bits = String::new();
        bits += "0000"; // sps_video_parameter_set_id
        bits += "001"; // sps_max_sub_layers_minus1
        bits += "1"; // sps_temporal_id_nesting_flag
        bits += "00100010"; // profile space 0, high tier, profile 2
        bits += &format!("{:032b}", 0x2000_0000u32); // compatible with Main 10
        bits += &format!("{:048b}", 0xB000_0000_0000u64); // progressive, non-packed, frame only
        bits += &format!("{:08b}", 123); // level 4.1
        bits += "01"; // sub-layer 0: no profile, level present
        bits += "00000000000000"; // reserved_zero_2bits for 1..8
        bits += "01111011"; // sub_layer_level_idc
        bits += "1"; // sps_seq_parameter_set_id ue(0)
        bits += "010"; // chroma_format_idc ue(1)
        bits += "1"; // width ue(0)
        bits += "1"; // height ue(0)
        bits += "1"; // conformance_window_flag
        bits += "1111"; // four offsets ue(0)
        bits += "011"; // bit_depth_luma_minus8 ue(2)
        bits += "011"; // bit_depth_chroma_minus8 ue(2)
        while !bits.len().is_multiple_of(8) {
            bits.push('1');
        }
        let mut sps = vec![0x42, 0x01];
        for chunk in bits.as_bytes().chunks(8) {
            sps.push(u8::from_str_radix(std::str::from_utf8(chunk).unwrap(), 2).unwrap());
        }
        let cfg =
            HevcDecoderConfig::new(vec![VPS.to_vec()], vec![sps], vec![PPS.to_vec()]).unwrap();
        assert_eq!((cfg.profile_idc(), cfg.level_idc()), (2, 123));
        let bytes = cfg.to_bytes();
        assert_eq!(
            &bytes[..23],
            &[
                0x01, 0x22, 0x20, 0x00, 0x00, 0x00, 0xB0, 0x00, 0x00, 0x00, 0x00, 0x00, 123, 0xF0,
                0x00, 0xFC, 0xFD, 0xFA, 0xFA, 0x00, 0x00,
                0x17, // two temporal layers, nested, 4-byte lengths
                0x03
            ]
        );
    }

    #[test]
    fn malformed_sps_is_an_error() {
        let bad = |sps: &[u8]| {
            HevcDecoderConfig::new(vec![VPS.to_vec()], vec![sps.to_vec()], vec![PPS.to_vec()])
        };
        assert!(matches!(bad(&[0x42]), Err(MuxError::InvalidHevcSps { .. })));
        assert!(matches!(
            bad(&PPS),
            Err(MuxError::InvalidHevcSps {
                reason: "NAL unit type is not 33"
            })
        ));
        assert!(matches!(
            bad(&SPS[..20]),
            Err(MuxError::InvalidHevcSps {
                reason: "truncated"
            })
        ));
        // sps_max_sub_layers_minus1 = 7
        assert!(matches!(
            bad(&[0x42, 0x01, 0x0E, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            Err(MuxError::InvalidHevcSps { .. })
        ));
        assert!(matches!(
            HevcDecoderConfig::new(
                vec![VPS.to_vec()],
                vec![SPS.to_vec()],
                vec![PPS.to_vec(); 65_536]
            ),
            Err(MuxError::TooLong { .. })
        ));
    }
}
