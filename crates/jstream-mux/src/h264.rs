//! H.264 bitstream framing: Annex B (start codes) to AVCC (length prefixes), and the
//! `AVCDecoderConfigurationRecord` built from SPS and PPS.
//!
//! JStream carries H.264 [`Packet`](jstream_media::Packet) payloads as **Annex B**, the
//! form OS encoders such as Media Foundation emit. Containers that need AVCC (FLV, RTMP,
//! MP4) convert at mux time with [`annexb_to_avcc`]. NAL unit bytes are copied unchanged,
//! so emulation-prevention bytes are preserved.

use crate::MuxError;

/// NAL unit type of a sequence parameter set.
pub const NAL_SPS: u8 = 7;
/// NAL unit type of a picture parameter set.
pub const NAL_PPS: u8 = 8;
/// NAL unit type of an IDR slice.
pub const NAL_IDR: u8 = 5;

/// The `nal_unit_type` of a NAL unit (the low five bits of its first byte); `None` for an
/// empty slice.
pub fn nal_type(nalu: &[u8]) -> Option<u8> {
    nalu.first().map(|b| b & 0x1F)
}

/// Splits an Annex B byte stream into NAL units, without their start codes.
///
/// Both 3-byte (`00 00 01`) and 4-byte (`00 00 00 01`) start codes are accepted, and
/// trailing zero bytes before the next start code (`trailing_zero_8bits`) are dropped. A
/// NAL unit never ends in a zero byte, so nothing of the unit itself is lost. Empty units
/// are skipped.
///
/// # Errors
/// [`MuxError::NotAnnexB`] when a non-zero byte comes before the first start code.
pub fn annexb_nalus(data: &[u8]) -> Result<Vec<&[u8]>, MuxError> {
    let Some((first, first_len)) = find_start_code(data, 0) else {
        let offset = data.iter().position(|&b| b != 0).unwrap_or(data.len());
        return if offset == data.len() {
            Ok(Vec::new())
        } else {
            Err(MuxError::NotAnnexB { offset })
        };
    };
    if let Some(offset) = data[..first].iter().position(|&b| b != 0) {
        return Err(MuxError::NotAnnexB { offset });
    }
    let mut nalus = Vec::new();
    let mut start = first + first_len;
    loop {
        let (end, next) = match find_start_code(data, start) {
            Some((pos, len)) => (pos, Some(pos + len)),
            None => (data.len(), None),
        };
        let nalu = trim_trailing_zeros(&data[start..end]);
        if !nalu.is_empty() {
            nalus.push(nalu);
        }
        match next {
            Some(n) => start = n,
            None => return Ok(nalus),
        }
    }
}

/// The position and length (always 3) of the first `00 00 01` at or after `from`. A
/// preceding zero of a 4-byte start code is removed by [`trim_trailing_zeros`].
fn find_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
    data.get(from..)?
        .windows(3)
        .position(|w| w == [0, 0, 1])
        .map(|p| (from + p, 3))
}

fn trim_trailing_zeros(mut nalu: &[u8]) -> &[u8] {
    while let [rest @ .., 0] = nalu {
        nalu = rest;
    }
    nalu
}

/// Converts Annex B to AVCC: each NAL unit prefixed by its length as a 4-byte big-endian
/// integer (`lengthSizeMinusOne = 3` in the decoder configuration record).
///
/// ```
/// use jstream_mux::h264::{annexb_to_avcc, avcc_to_annexb};
///
/// let annexb = [0, 0, 0, 1, 0x65, 0x88, 0, 0, 1, 0x41, 0x9A];
/// let avcc = annexb_to_avcc(&annexb)?;
/// assert_eq!(avcc, [0, 0, 0, 2, 0x65, 0x88, 0, 0, 0, 2, 0x41, 0x9A]);
/// assert_eq!(avcc_to_annexb(&avcc)?, [0, 0, 0, 1, 0x65, 0x88, 0, 0, 0, 1, 0x41, 0x9A]);
/// # Ok::<(), jstream_mux::MuxError>(())
/// ```
///
/// # Errors
/// [`MuxError::NotAnnexB`] as for [`annexb_nalus`], and [`MuxError::TooLong`] for a NAL
/// unit of 4 GiB or more.
pub fn annexb_to_avcc(data: &[u8]) -> Result<Vec<u8>, MuxError> {
    let nalus = annexb_nalus(data)?;
    let mut out = Vec::with_capacity(data.len() + nalus.len());
    for nalu in nalus {
        let len = u32::try_from(nalu.len()).map_err(|_| MuxError::TooLong {
            what: "NAL unit",
            len: nalu.len(),
            max: u32::MAX as usize,
        })?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(nalu);
    }
    Ok(out)
}

/// Splits AVCC data with 4-byte length prefixes into NAL units.
///
/// # Errors
/// [`MuxError::TruncatedAvcc`] when a length prefix, or the unit it announces, runs past
/// the end of `data`.
pub fn avcc_nalus(data: &[u8]) -> Result<Vec<&[u8]>, MuxError> {
    let mut nalus = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let truncated = |len| MuxError::TruncatedAvcc {
            offset: pos,
            len,
            available: data.len().saturating_sub(pos + 4),
        };
        let prefix: [u8; 4] = data
            .get(pos..pos + 4)
            .and_then(|p| p.try_into().ok())
            .ok_or_else(|| truncated(4))?;
        let len = u32::from_be_bytes(prefix) as usize;
        let nalu = data
            .get(pos + 4..)
            .and_then(|rest| rest.get(..len))
            .ok_or_else(|| truncated(len))?;
        nalus.push(nalu);
        pos += 4 + len;
    }
    Ok(nalus)
}

/// Converts AVCC with 4-byte length prefixes back to Annex B with 4-byte start codes, for
/// writing elementary `.h264` streams and for tests.
///
/// # Errors
/// As [`avcc_nalus`].
pub fn avcc_to_annexb(data: &[u8]) -> Result<Vec<u8>, MuxError> {
    let mut out = Vec::with_capacity(data.len());
    for nalu in avcc_nalus(data)? {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nalu);
    }
    Ok(out)
}

/// The H.264 decoder configuration that FLV and MP4 carry once, ahead of the frames: the
/// `AVCDecoderConfigurationRecord` of ISO/IEC 14496-15, built from SPS and PPS NAL units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvcDecoderConfig {
    sps: Vec<Vec<u8>>,
    pps: Vec<Vec<u8>>,
    profile_idc: u8,
    constraint_flags: u8,
    level_idc: u8,
    chroma_format_idc: u8,
    bit_depth_luma_minus8: u8,
    bit_depth_chroma_minus8: u8,
}

impl AvcDecoderConfig {
    /// A configuration from SPS and PPS NAL units (without start codes or length
    /// prefixes). The profile, level and, for high profiles, chroma format and bit depths
    /// are read from the first SPS.
    ///
    /// # Errors
    /// [`MuxError::MissingParameterSets`] when either list is empty,
    /// [`MuxError::InvalidSps`] when the first SPS can't be read, and
    /// [`MuxError::TooLong`] for more than 31 SPS, 255 PPS, or a set of 64 KiB or more.
    pub fn new(sps: Vec<Vec<u8>>, pps: Vec<Vec<u8>>) -> Result<Self, MuxError> {
        let Some(first) = sps.first() else {
            return Err(MuxError::MissingParameterSets {
                sps: 0,
                pps: pps.len(),
            });
        };
        if pps.is_empty() {
            return Err(MuxError::MissingParameterSets {
                sps: sps.len(),
                pps: 0,
            });
        }
        let info = SpsInfo::parse(first)?;
        check_count("SPS list", sps.len(), 31)?;
        check_count("PPS list", pps.len(), 255)?;
        for set in sps.iter().chain(&pps) {
            check_count("parameter set", set.len(), usize::from(u16::MAX))?;
        }
        Ok(Self {
            sps,
            pps,
            profile_idc: info.profile_idc,
            constraint_flags: info.constraint_flags,
            level_idc: info.level_idc,
            chroma_format_idc: info.chroma_format_idc,
            bit_depth_luma_minus8: info.bit_depth_luma_minus8,
            bit_depth_chroma_minus8: info.bit_depth_chroma_minus8,
        })
    }

    /// A configuration from the SPS and PPS NAL units found in an Annex B access unit,
    /// typically the first keyframe an encoder emits.
    ///
    /// # Errors
    /// As [`annexb_nalus`] and [`AvcDecoderConfig::new`].
    pub fn from_annexb(data: &[u8]) -> Result<Self, MuxError> {
        let (sps, pps) = parameter_sets(&annexb_nalus(data)?);
        Self::new(sps, pps)
    }

    /// The SPS NAL units.
    pub fn sps(&self) -> &[Vec<u8>] {
        &self.sps
    }

    /// The PPS NAL units.
    pub fn pps(&self) -> &[Vec<u8>] {
        &self.pps
    }

    /// `profile_idc` from the first SPS (66 Baseline, 77 Main, 100 High, …).
    pub fn profile_idc(&self) -> u8 {
        self.profile_idc
    }

    /// `level_idc` from the first SPS (e.g. 41 for level 4.1).
    pub fn level_idc(&self) -> u8 {
        self.level_idc
    }

    /// The serialized `AVCDecoderConfigurationRecord`, with 4-byte NAL length fields.
    ///
    /// For profiles other than Baseline, Main and Extended (66, 77, 88), the record ends
    /// with the chroma format and bit depths and zero SPS extensions, as ISO/IEC 14496-15
    /// and FFmpeg write it.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![
            1, // configurationVersion
            self.profile_idc,
            self.constraint_flags,
            self.level_idc,
            0xFC | 3, // reserved '111111' + lengthSizeMinusOne
        ];
        // Counts and lengths were checked in `new`.
        out.push(0xE0 | (self.sps.len() as u8));
        for set in &self.sps {
            out.extend_from_slice(&(set.len() as u16).to_be_bytes());
            out.extend_from_slice(set);
        }
        out.push(self.pps.len() as u8);
        for set in &self.pps {
            out.extend_from_slice(&(set.len() as u16).to_be_bytes());
            out.extend_from_slice(set);
        }
        if !matches!(self.profile_idc, 66 | 77 | 88) {
            out.push(0xFC | self.chroma_format_idc);
            out.push(0xF8 | self.bit_depth_luma_minus8);
            out.push(0xF8 | self.bit_depth_chroma_minus8);
            out.push(0); // numOfSequenceParameterSetExt
        }
        out
    }
}

fn check_count(what: &'static str, len: usize, max: usize) -> Result<(), MuxError> {
    if len > max {
        Err(MuxError::TooLong { what, len, max })
    } else {
        Ok(())
    }
}

/// The SPS and PPS among `nalus`, copied, in stream order.
pub fn parameter_sets(nalus: &[&[u8]]) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let pick = |t| {
        nalus
            .iter()
            .filter(|n| nal_type(n) == Some(t))
            .map(|n| n.to_vec())
            .collect::<Vec<_>>()
    };
    (pick(NAL_SPS), pick(NAL_PPS))
}

/// The SPS fields the decoder configuration record needs.
struct SpsInfo {
    profile_idc: u8,
    constraint_flags: u8,
    level_idc: u8,
    chroma_format_idc: u8,
    bit_depth_luma_minus8: u8,
    bit_depth_chroma_minus8: u8,
}

impl SpsInfo {
    fn parse(sps: &[u8]) -> Result<Self, MuxError> {
        if nal_type(sps) != Some(NAL_SPS) {
            return Err(MuxError::InvalidSps {
                reason: "NAL unit type is not 7",
            });
        }
        let rbsp = unescape(&sps[1..]);
        let [profile_idc, constraint_flags, level_idc, ..] = rbsp[..] else {
            return Err(MuxError::InvalidSps {
                reason: "shorter than profile, constraints and level",
            });
        };
        let mut info = Self {
            profile_idc,
            constraint_flags,
            level_idc,
            chroma_format_idc: 1,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
        };
        if matches!(
            profile_idc,
            100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
        ) {
            let mut bits = BitReader::new(&rbsp[3..]);
            bits.ue()?; // seq_parameter_set_id
            let chroma = bits.ue()?;
            if chroma == 3 {
                bits.bit()?; // separate_colour_plane_flag
            }
            let luma = bits.ue()?;
            let chroma_depth = bits.ue()?;
            if chroma > 3 || luma > 6 || chroma_depth > 6 {
                return Err(MuxError::InvalidSps {
                    reason: "chroma format or bit depth out of range",
                });
            }
            // Range-checked just above.
            info.chroma_format_idc = chroma as u8;
            info.bit_depth_luma_minus8 = luma as u8;
            info.bit_depth_chroma_minus8 = chroma_depth as u8;
        }
        Ok(info)
    }
}

/// Removes emulation-prevention bytes (`00 00 03` → `00 00`), giving the RBSP.
fn unescape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0;
    for &b in data {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn bit(&mut self) -> Result<u32, MuxError> {
        let byte = self.data.get(self.pos / 8).ok_or(MuxError::InvalidSps {
            reason: "truncated",
        })?;
        let bit = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Ok(u32::from(bit))
    }

    /// An unsigned Exp-Golomb code.
    fn ue(&mut self) -> Result<u32, MuxError> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return Err(MuxError::InvalidSps {
                    reason: "Exp-Golomb code too long",
                });
            }
        }
        let mut rest = 0u64;
        for _ in 0..zeros {
            rest = (rest << 1) | u64::from(self.bit()?);
        }
        u32::try_from((1u64 << zeros) - 1 + rest).map_err(|_| MuxError::InvalidSps {
            reason: "Exp-Golomb value too large",
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // An SPS for High profile (100), level 3.1, 4:2:0 8-bit, with an emulation-prevention
    // byte in its VUI; and a PPS.
    pub(crate) const SPS: [u8; 25] = [
        0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x50, 0x05, 0xBB, 0x01, 0x10, 0x00, 0x00, 0x03,
        0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0xC0, 0xF1, 0x83, 0x19,
    ];
    pub(crate) const PPS: [u8; 6] = [0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0];

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let data = [
            0, 0, 0, 1, 0x09, 0xF0, // AUD, 4-byte start code
            0, 0, 1, 0x67, 0x42, // 3-byte start code
            0, 0, 0, 0, 1, 0x68, 0xCE, 0, // trailing_zero_8bits, then a unit ending in 00
        ];
        let nalus = annexb_nalus(&data).unwrap();
        assert_eq!(nalus, vec![&[0x09, 0xF0][..], &[0x67, 0x42], &[0x68, 0xCE]]);
        assert_eq!(nal_type(nalus[1]), Some(NAL_SPS));
        assert_eq!(nal_type(&[]), None);
    }

    #[test]
    fn rejects_data_without_a_leading_start_code() {
        assert!(matches!(
            annexb_nalus(&[0, 0x65, 0, 0, 1, 0x41]),
            Err(MuxError::NotAnnexB { offset: 1 })
        ));
        assert!(matches!(
            annexb_nalus(&[0, 0, 0, 4, 0x65, 0x88, 0x84, 0x00]),
            Err(MuxError::NotAnnexB { offset: 3 })
        ));
        assert!(annexb_nalus(&[]).unwrap().is_empty());
        assert!(annexb_nalus(&[0, 0]).unwrap().is_empty());
        assert!(annexb_nalus(&[0, 0, 1]).unwrap().is_empty());
    }

    #[test]
    fn round_trip_preserves_emulation_prevention_bytes() {
        // Slice data containing 00 00 03 01, 00 00 03 00 and 00 00 03 03 sequences, and a
        // cabac_zero_word tail (00 00 03): all must survive unchanged.
        let slice = [
            0x65, 0x88, 0x00, 0x00, 0x03, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00, 0x22, 0x00, 0x00,
            0x03, 0x03, 0x7F, 0x00, 0x00, 0x03,
        ];
        let mut annexb = vec![0, 0, 0, 1];
        annexb.extend_from_slice(&SPS);
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(&PPS);
        annexb.extend_from_slice(&[0, 0, 1]);
        annexb.extend_from_slice(&slice);

        let avcc = annexb_to_avcc(&annexb).unwrap();
        let nalus = avcc_nalus(&avcc).unwrap();
        assert_eq!(nalus, vec![&SPS[..], &PPS[..], &slice[..]]);
        assert_eq!(&avcc[avcc.len() - slice.len() - 4..][..4], &[0, 0, 0, 20]);

        let back = avcc_to_annexb(&avcc).unwrap();
        assert_eq!(annexb_nalus(&back).unwrap(), annexb_nalus(&annexb).unwrap());
        assert_eq!(annexb_to_avcc(&back).unwrap(), avcc);
    }

    #[test]
    fn truncated_avcc_is_an_error() {
        assert!(matches!(
            avcc_nalus(&[0, 0, 0, 5, 0x65]),
            Err(MuxError::TruncatedAvcc {
                offset: 0,
                len: 5,
                available: 1
            })
        ));
        assert!(matches!(
            avcc_nalus(&[0, 0, 0, 1, 0x65, 0, 0]),
            Err(MuxError::TruncatedAvcc { offset: 5, .. })
        ));
        assert!(avcc_nalus(&[]).unwrap().is_empty());
    }

    #[test]
    fn unescape_removes_emulation_prevention() {
        assert_eq!(
            unescape(&[0, 0, 3, 1, 0, 0, 3, 0, 0, 3]),
            [0, 0, 1, 0, 0, 0, 0]
        );
        assert_eq!(unescape(&[0, 3, 0, 0, 0, 3]), [0, 3, 0, 0, 0]);
    }

    #[test]
    fn high_profile_record_is_byte_exact() {
        let cfg = AvcDecoderConfig::new(vec![SPS.to_vec()], vec![PPS.to_vec()]).unwrap();
        assert_eq!((cfg.profile_idc(), cfg.level_idc()), (100, 31));
        let mut expected = vec![0x01, 0x64, 0x00, 0x1F, 0xFF, 0xE1, 0x00, 25];
        expected.extend_from_slice(&SPS);
        expected.extend_from_slice(&[0x01, 0x00, 0x06]);
        expected.extend_from_slice(&PPS);
        // 4:2:0, 8-bit luma and chroma, no SPS extensions
        expected.extend_from_slice(&[0xFD, 0xF8, 0xF8, 0x00]);
        assert_eq!(cfg.to_bytes(), expected);
    }

    #[test]
    fn baseline_record_has_no_extension() {
        // Baseline (66) level 3.0; the rest of the SPS is not read for this profile.
        let sps = [0x67, 0x42, 0xC0, 0x1E, 0xDA];
        let pps = [0x68, 0xCE, 0x3C, 0x80];
        let cfg = AvcDecoderConfig::new(vec![sps.to_vec()], vec![pps.to_vec()]).unwrap();
        assert_eq!(
            cfg.to_bytes(),
            [
                0x01, 0x42, 0xC0, 0x1E, 0xFF, 0xE1, 0x00, 0x05, 0x67, 0x42, 0xC0, 0x1E, 0xDA, 0x01,
                0x00, 0x04, 0x68, 0xCE, 0x3C, 0x80
            ]
        );
    }

    #[test]
    fn high_10_bit_depth_is_read() {
        // profile 110 (High 10): sps_id ue(0)='1', chroma ue(1)='010', luma ue(2)='011',
        // chroma depth ue(2)='011' -> 1010 0110 11xx xxxx
        let sps = [0x67, 110, 0x00, 0x28, 0b1010_0110, 0b1110_0000];
        let cfg = AvcDecoderConfig::new(vec![sps.to_vec()], vec![PPS.to_vec()]).unwrap();
        let bytes = cfg.to_bytes();
        assert_eq!(&bytes[bytes.len() - 4..], &[0xFD, 0xFA, 0xFA, 0x00]);
    }

    #[test]
    fn from_annexb_extracts_parameter_sets() {
        let mut au = vec![0, 0, 0, 1, 0x09, 0x10];
        for nalu in [&SPS[..], &PPS[..], &[0x65, 0x88, 0x84]] {
            au.extend_from_slice(&[0, 0, 0, 1]);
            au.extend_from_slice(nalu);
        }
        let cfg = AvcDecoderConfig::from_annexb(&au).unwrap();
        assert_eq!(cfg.sps(), &[SPS.to_vec()]);
        assert_eq!(cfg.pps(), &[PPS.to_vec()]);

        assert!(matches!(
            AvcDecoderConfig::from_annexb(&[0, 0, 1, 0x65, 0x88]),
            Err(MuxError::MissingParameterSets { sps: 0, pps: 0 })
        ));
        assert!(matches!(
            AvcDecoderConfig::new(vec![SPS.to_vec()], vec![]),
            Err(MuxError::MissingParameterSets { sps: 1, pps: 0 })
        ));
    }

    #[test]
    fn malformed_sps_is_an_error() {
        let bad = |sps: &[u8]| AvcDecoderConfig::new(vec![sps.to_vec()], vec![PPS.to_vec()]);
        assert!(matches!(
            bad(&[0x67, 0x64]),
            Err(MuxError::InvalidSps { .. })
        ));
        assert!(matches!(
            bad(&[0x68, 0x42, 0, 0x1E]),
            Err(MuxError::InvalidSps { .. })
        ));
        // High profile with the bit fields cut off
        assert!(matches!(
            bad(&[0x67, 0x64, 0, 0x1F]),
            Err(MuxError::InvalidSps { .. })
        ));
        // An all-zero Exp-Golomb prefix never terminates within 32 bits
        assert!(matches!(
            bad(&[0x67, 0x64, 0, 0x1F, 0, 0, 0, 0, 0]),
            Err(MuxError::InvalidSps { .. })
        ));
        assert!(matches!(
            AvcDecoderConfig::new(vec![SPS.to_vec(); 32], vec![PPS.to_vec()]),
            Err(MuxError::TooLong { .. })
        ));
    }
}
