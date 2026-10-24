//! The platform-independent half of the VideoToolbox decoder (ADR-0063 §6): turning the
//! engine's packets into what a `VTDecompressionSession` takes, and putting its frames
//! back in presentation order.
//!
//! - The engine's packets are Annex B (`jstream-media`), with the parameter sets in front
//!   of every keyframe. VideoToolbox takes length-prefixed NAL units (the "AVCC" and
//!   "HVCC" layouts) and the parameter sets in a format description instead.
//!   [`split_access_unit`] separates the two; the backend makes a format description from
//!   the sets, and a new session whenever they change.
//! - VideoToolbox emits frames in decode order. [`Reorder`] holds each until no later
//!   packet can present earlier, the rule the fake decoder follows: a packet's `dts`
//!   never decreases and is at most its `pts`, so once a packet with `dts` at or past a
//!   frame's `pts` is decoded, every earlier frame is known.

use jstream_platform::PlatformError;
use jstream_platform::media::{Codec, MediaTime};

fn malformed(message: String) -> PlatformError {
    PlatformError::Os {
        call: "VTDecompressionSessionDecodeFrame",
        code: None,
        message,
    }
}

/// The NAL units of an Annex B access unit: the bytes between start codes (`00 00 01`,
/// or `00 00 00 01`), without them. Bytes before the first start code are ignored, and
/// trailing zero bytes (`trailing_zero_8bits`) are not part of a unit.
pub fn annexb_units(data: &[u8]) -> Vec<&[u8]> {
    // Where each start code's payload begins.
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut units = Vec::with_capacity(starts.len());
    for (n, &start) in starts.iter().enumerate() {
        // The next start code begins 3 bytes before its payload; trailing zeros (and the
        // leading zero of a 4-byte start code) belong to no unit.
        let end = starts.get(n + 1).map_or(data.len(), |&next| next - 3);
        let mut unit = &data[start..end];
        while let [rest @ .., 0] = unit {
            unit = rest;
        }
        if !unit.is_empty() {
            units.push(unit);
        }
    }
    units
}

/// What one NAL unit is, for the decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    /// A parameter set, with its place in the order Core Media wants them in.
    ParameterSet(u8),
    /// An access unit delimiter, which the sample doesn't need.
    Delimiter,
    /// Anything else: slices, SEI.
    Data,
}

fn classify(codec: Codec, unit: &[u8]) -> Unit {
    let Some(&header) = unit.first() else {
        return Unit::Data;
    };
    match codec {
        Codec::Hevc => match (header >> 1) & 0x3f {
            32 => Unit::ParameterSet(0), // VPS
            33 => Unit::ParameterSet(1), // SPS
            34 => Unit::ParameterSet(2), // PPS
            35 => Unit::Delimiter,
            _ => Unit::Data,
        },
        _ => match header & 0x1f {
            7 => Unit::ParameterSet(0),  // SPS
            13 => Unit::ParameterSet(1), // SPS extension
            8 => Unit::ParameterSet(2),  // PPS
            9 => Unit::Delimiter,
            _ => Unit::Data,
        },
    }
}

/// One packet, split for VideoToolbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitAccessUnit<'a> {
    /// The parameter sets the packet carries (none for most packets), VPS, SPS then PPS
    /// as Core Media takes them, each in the order it came.
    pub parameter_sets: Vec<&'a [u8]>,
    /// The other NAL units, each behind a 4-byte big-endian length (the length size the
    /// format description is made with). Empty for a packet of parameter sets alone.
    pub sample: Vec<u8>,
}

/// Splits an Annex B packet of `codec` (H.264 or HEVC) into its parameter sets and a
/// length-prefixed sample, dropping access unit delimiters.
///
/// # Errors
/// [`PlatformError::Os`] for a packet with no NAL units, or one over 4 GiB.
pub fn split_access_unit(codec: Codec, data: &[u8]) -> Result<SplitAccessUnit<'_>, PlatformError> {
    let units = annexb_units(data);
    if units.is_empty() {
        return Err(malformed(format!(
            "a {codec:?} packet of {} byte(s) with no Annex B NAL units",
            data.len()
        )));
    }
    let mut sets: Vec<(u8, &[u8])> = Vec::new();
    let mut sample = Vec::with_capacity(data.len() + 4 * units.len());
    for unit in units {
        match classify(codec, unit) {
            Unit::ParameterSet(order) => sets.push((order, unit)),
            Unit::Delimiter => {}
            Unit::Data => {
                let len = u32::try_from(unit.len())
                    .map_err(|_| malformed(format!("a NAL unit of {} bytes", unit.len())))?;
                sample.extend_from_slice(&len.to_be_bytes());
                sample.extend_from_slice(unit);
            }
        }
    }
    // Stable, so sets of one kind keep their order.
    sets.sort_by_key(|&(order, _)| order);
    Ok(SplitAccessUnit {
        parameter_sets: sets.into_iter().map(|(_, s)| s).collect(),
        sample,
    })
}

/// Whether `sets` can make a format description for `codec`: an SPS and a PPS, and a VPS
/// for HEVC.
pub fn sets_complete(codec: Codec, sets: &[&[u8]]) -> bool {
    let has = |order: u8| {
        sets.iter()
            .any(|s| classify(codec, s) == Unit::ParameterSet(order))
    };
    match codec {
        // VPS, SPS and PPS.
        Codec::Hevc => has(0) && has(1) && has(2),
        // SPS and PPS (an SPS extension is optional).
        _ => has(0) && has(2),
    }
}

/// Decoded frames held until they can come out in presentation order.
#[derive(Debug)]
pub struct Reorder<T> {
    /// Held, sorted by `pts`.
    held: Vec<(MediaTime, T)>,
}

impl<T> Default for Reorder<T> {
    fn default() -> Self {
        Self { held: Vec::new() }
    }
}

impl<T> Reorder<T> {
    /// Holds `frame`, presented at `pts`.
    pub fn hold(&mut self, pts: MediaTime, frame: T) {
        let at = self.held.partition_point(|(p, _)| *p <= pts);
        self.held.insert(at, (pts, frame));
    }

    /// Moves every held frame presented at or before `until` to `out`, in presentation
    /// order: `until` is the `dts` of the newest packet decoded. `None` releases them all,
    /// at the end of the stream.
    pub fn release(&mut self, until: Option<MediaTime>, out: &mut Vec<T>) {
        let n = match until {
            Some(until) => self.held.partition_point(|(p, _)| *p <= until),
            None => self.held.len(),
        };
        out.extend(self.held.drain(..n).map(|(_, f)| f));
    }

    /// How many frames are held.
    pub fn len(&self) -> usize {
        self.held.len()
    }

    /// Whether no frame is held.
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::PlatformError;
    use jstream_platform::media::{Codec, MediaTime, Timebase};

    use super::{Reorder, annexb_units, sets_complete, split_access_unit};

    const SPS: &[u8] = &[0x67, 0x64, 0x00, 0x28];
    const PPS: &[u8] = &[0x68, 0xEE, 0x3C, 0x80];
    const IDR: &[u8] = &[0x65, 0x88, 0x84];
    const AUD: &[u8] = &[0x09, 0xF0];

    fn annexb(units: &[&[u8]], four_byte: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for u in units {
            if four_byte {
                out.push(0);
            }
            out.extend_from_slice(&[0, 0, 1]);
            out.extend_from_slice(u);
        }
        out
    }

    #[test]
    fn annexb_units_split_on_both_start_code_lengths() {
        let mut data = annexb(&[SPS, PPS], true);
        data.extend(annexb(&[IDR], false));
        data.extend_from_slice(&[0, 0]); // trailing_zero_8bits
        assert_eq!(annexb_units(&data), vec![SPS, PPS, IDR]);
        assert!(annexb_units(&[0x65, 0x88]).is_empty(), "no start code");
        assert!(annexb_units(&[]).is_empty());
    }

    #[test]
    fn a_keyframe_splits_into_its_sets_and_a_length_prefixed_sample() {
        let sei: &[u8] = &[0x06, 0x05, 0x01];
        let data = annexb(&[AUD, SPS, PPS, sei, IDR], true);
        let au = split_access_unit(Codec::H264, &data).expect("splits");
        assert_eq!(au.parameter_sets, vec![SPS, PPS]);
        let mut want = vec![0, 0, 0, 3];
        want.extend_from_slice(sei);
        want.extend_from_slice(&[0, 0, 0, 3]);
        want.extend_from_slice(IDR);
        assert_eq!(au.sample, want, "the delimiter is dropped");
        assert!(sets_complete(Codec::H264, &au.parameter_sets));

        // A PPS that comes first still goes after the SPS.
        let data = annexb(&[PPS, SPS, IDR], false);
        let au = split_access_unit(Codec::H264, &data).expect("splits");
        assert_eq!(au.parameter_sets, vec![SPS, PPS]);
    }

    #[test]
    fn hevc_sets_are_ordered_vps_sps_pps() {
        // NAL type in bits 1..7 of the first header byte.
        let vps: &[u8] = &[32 << 1, 1, 0xAA];
        let sps: &[u8] = &[33 << 1, 1, 0xBB];
        let pps: &[u8] = &[34 << 1, 1, 0xCC];
        let idr: &[u8] = &[19 << 1, 1, 0xDD];
        let aud: &[u8] = &[35 << 1, 1, 0x50];
        let data = annexb(&[aud, pps, sps, vps, idr], true);
        let au = split_access_unit(Codec::Hevc, &data).expect("splits");
        assert_eq!(au.parameter_sets, vec![vps, sps, pps]);
        assert_eq!(&au.sample[..4], &[0, 0, 0, 3]);
        assert_eq!(&au.sample[4..], idr);
        assert!(sets_complete(Codec::Hevc, &au.parameter_sets));
        assert!(!sets_complete(Codec::Hevc, &[sps, pps]), "no VPS");
    }

    #[test]
    fn incomplete_or_empty_packets_are_told_apart() {
        let data = annexb(&[IDR], true);
        let p = split_access_unit(Codec::H264, &data).expect("splits");
        assert!(p.parameter_sets.is_empty());
        assert!(!sets_complete(Codec::H264, &[SPS]), "no PPS");
        assert!(!sets_complete(Codec::H264, &[PPS]), "no SPS");
        assert!(matches!(
            split_access_unit(Codec::H264, &[0x65, 0x88]),
            Err(PlatformError::Os { .. })
        ));
        let data = annexb(&[SPS, PPS], true);
        let sets_only = split_access_unit(Codec::H264, &data).expect("splits");
        assert!(sets_only.sample.is_empty());
    }

    #[test]
    fn frames_come_out_in_presentation_order() {
        let tb = Timebase::new(1, 30).expect("timebase");
        let t = |n| MediaTime::new(n, tb);
        let mut r = Reorder::default();
        let mut out = Vec::new();
        // I0 P3 B1 B2, decoded in that order with dts 0, 1, 2, 3 less a frame's delay:
        // dts -1, 0, 1, 2.
        r.hold(t(0), "I0");
        r.release(Some(t(-1)), &mut out);
        assert!(out.is_empty(), "a B frame may still present before I0");
        r.hold(t(3), "P3");
        r.release(Some(t(0)), &mut out);
        assert_eq!(out, ["I0"]);
        r.hold(t(1), "B1");
        r.release(Some(t(1)), &mut out);
        r.hold(t(2), "B2");
        r.release(Some(t(2)), &mut out);
        assert_eq!(out, ["I0", "B1", "B2"]);
        assert_eq!(r.len(), 1);
        r.release(None, &mut out);
        assert_eq!(out, ["I0", "B1", "B2", "P3"]);
        assert!(r.is_empty());
    }
}
