//! H.264 for a hardware decoder that is told everything (ADR-0072 (b)): the bitstream's
//! parameter sets and slice headers parsed in pure Rust, and the decoding process
//! around the slice data (picture order, reference marking and lists, output order) in
//! [`dpb`]. The slice data itself is the GPU's to decode, through VA-API.
//!
//! Platform-independent, and tested everywhere.

pub(crate) mod bits;
pub(crate) mod dpb;
pub(crate) mod params;
pub(crate) mod slice;

use bits::{ParseError, Result};

/// The `AVCDecoderConfigurationRecord` of an MP4 `avcC` box or a Matroska
/// `CodecPrivate` (ISO/IEC 14496-15 §5.3.3.1): how long each sample's NAL unit lengths
/// are, and the parameter sets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AvcConfig {
    /// Bytes in each NAL unit's length prefix: 1, 2 or 4.
    pub(crate) length_size: usize,
    /// The SPS and PPS NAL units, header byte included.
    pub(crate) parameter_sets: Vec<Vec<u8>>,
}

impl AvcConfig {
    pub(crate) fn parse(record: &[u8]) -> Result<Self> {
        let mut at = 0;
        let mut take = |n: usize| -> Result<&[u8]> {
            let bytes = record.get(at..at + n).ok_or(ParseError::Truncated)?;
            at += n;
            Ok(bytes)
        };
        let head = take(5)?;
        if head[0] != 1 {
            return Err(ParseError::Invalid("an avcC record of another version"));
        }
        let length_size = usize::from(head[4] & 3) + 1;
        if length_size == 3 {
            return Err(ParseError::Invalid("a NAL length of three bytes"));
        }
        let mut parameter_sets = Vec::new();
        let sps_count = take(1)?[0] & 0x1F;
        for _ in 0..sps_count {
            let len = usize::from(u16::from_be_bytes([take(1)?[0], take(1)?[0]]));
            parameter_sets.push(take(len)?.to_vec());
        }
        let pps_count = take(1)?[0];
        for _ in 0..pps_count {
            let len = usize::from(u16::from_be_bytes([take(1)?[0], take(1)?[0]]));
            parameter_sets.push(take(len)?.to_vec());
        }
        Ok(Self {
            length_size,
            parameter_sets,
        })
    }
}

/// The NAL units of a sample whose units each start with a `length_size`-byte length.
/// A length running past the end stops the split there.
pub(crate) fn split_length_prefixed(sample: &[u8], length_size: usize) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + length_size <= sample.len() {
        let len = sample[at..at + length_size]
            .iter()
            .fold(0_usize, |acc, &b| (acc << 8) | usize::from(b));
        at += length_size;
        let Some(unit) = sample.get(at..at + len) else {
            break;
        };
        if !unit.is_empty() {
            out.push(unit);
        }
        at += len;
    }
    out
}
