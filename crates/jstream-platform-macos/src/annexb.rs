//! The platform-independent half of the VideoToolbox encoder (ADR-0063 §6): converting
//! what a `VTCompressionSession` emits into the engine's packets, and its inputs from the
//! engine's frames.
//!
//! - VideoToolbox emits length-prefixed NAL units (ISO/IEC 14496-15, the "AVCC" and
//!   "HVCC" layouts), with the parameter sets in the sample's format description rather
//!   than in the stream. The engine's framing is Annex B (`jstream-media`), with the
//!   parameter sets in front of every keyframe, as Media Foundation emits them, so a
//!   muxer can build its decoder configuration from the first keyframe.
//!   [`annexb_access_unit`] does that conversion.
//! - Core Media times are a 64-bit value over a 32-bit timescale; the engine's are ticks
//!   of a rational time base. [`cm_time_parts`] and [`media_time_from_cm`] map between
//!   them without rounding for the time bases the engine uses.
//! - [`copy_rows`] copies one plane of a frame into a pixel buffer whose rows may be
//!   padded differently.

use jstream_platform::PlatformError;
use jstream_platform::media::{MediaTime, Rounding, Timebase};

/// The Annex B start code written in front of every NAL unit.
const START_CODE: [u8; 4] = [0, 0, 0, 1];

fn malformed(message: String) -> PlatformError {
    PlatformError::Os {
        call: "VTCompressionOutputCallback",
        code: None,
        message,
    }
}

/// Appends one access unit to `out` as Annex B: each of `parameter_sets` (pass them for a
/// keyframe, and none otherwise), then each NAL unit of `data`, every one behind a
/// four-byte start code. `data` is a sequence of NAL units, each behind a big-endian
/// length of `nal_length_size` bytes (1, 2 or 4), as the format description says.
///
/// # Errors
/// [`PlatformError::Os`] for a length size other than 1, 2 or 4, a length that runs past
/// the end of `data`, an empty NAL unit or parameter set, or an access unit with no NAL
/// units. Nothing is appended then.
pub fn annexb_access_unit(
    data: &[u8],
    nal_length_size: usize,
    parameter_sets: &[&[u8]],
    out: &mut Vec<u8>,
) -> Result<(), PlatformError> {
    if !matches!(nal_length_size, 1 | 2 | 4) {
        return Err(malformed(format!(
            "NAL unit length size {nal_length_size} (1, 2 or 4)"
        )));
    }
    if parameter_sets.iter().any(|set| set.is_empty()) {
        return Err(malformed("an empty parameter set".into()));
    }
    let start = out.len();
    for set in parameter_sets {
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(set);
    }
    let mut rest = data;
    let mut units = 0usize;
    while !rest.is_empty() {
        let Some((prefix, tail)) = rest.split_at_checked(nal_length_size) else {
            out.truncate(start);
            return Err(malformed(format!(
                "{} trailing byte(s) after the last NAL unit",
                rest.len()
            )));
        };
        let len = prefix
            .iter()
            .fold(0usize, |acc, &b| (acc << 8) | usize::from(b));
        let Some((unit, tail)) = tail.split_at_checked(len).filter(|_| len > 0) else {
            out.truncate(start);
            return Err(malformed(format!(
                "a NAL unit of {len} byte(s) with {} left in the sample",
                tail.len()
            )));
        };
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(unit);
        units += 1;
        rest = tail;
    }
    if units == 0 {
        out.truncate(start);
        return Err(malformed("an encoded frame with no NAL units".into()));
    }
    Ok(())
}

/// `time` as a Core Media value and timescale: `ticks × num` over `den`, the same instant
/// exactly. `None` when the time base's denominator doesn't fit a 32-bit timescale or the
/// value overflows 64 bits.
pub fn cm_time_parts(time: MediaTime) -> Option<(i64, i32)> {
    let tb = time.timebase();
    let timescale = i32::try_from(tb.den()).ok()?;
    let value = time.ticks().checked_mul(i64::from(tb.num()))?;
    Some((value, timescale))
}

/// A Core Media value over `timescale` as a [`MediaTime`] in `to`, rounded to the nearest
/// tick. Exact for a time [`cm_time_parts`] made from a time in `to`.
///
/// # Errors
/// [`PlatformError::Os`] for a timescale that isn't positive, and
/// [`PlatformError::Media`] when the result doesn't fit `to`.
pub fn media_time_from_cm(
    value: i64,
    timescale: i32,
    to: Timebase,
) -> Result<MediaTime, PlatformError> {
    let den = u32::try_from(timescale)
        .ok()
        .filter(|&d| d > 0)
        .ok_or_else(|| malformed(format!("a Core Media timescale of {timescale}")))?;
    let from = Timebase::new(1, den)?;
    Ok(MediaTime::new(value, from).rescale(to, Rounding::Nearest)?)
}

/// Copies `rows` rows of `row_bytes` bytes from `src` (rows `src_stride` apart) to `dst`
/// (rows `dst_stride` apart), leaving `dst`'s row padding as it was.
///
/// # Errors
/// [`PlatformError::Unsupported`] when a stride is shorter than a row, or either buffer
/// is too short for `rows` rows. Nothing is copied then.
pub fn copy_rows(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    row_bytes: usize,
    rows: usize,
) -> Result<(), PlatformError> {
    let needed = |stride: usize| {
        rows.checked_sub(1)
            .map_or(Some(0), |r| r.checked_mul(stride)?.checked_add(row_bytes))
    };
    let fits =
        |len: usize, stride: usize| stride >= row_bytes && needed(stride).is_some_and(|n| n <= len);
    if !fits(src.len(), src_stride) || !fits(dst.len(), dst_stride) {
        return Err(PlatformError::Unsupported {
            what: format!(
                "copying {rows} row(s) of {row_bytes} bytes from {} bytes {src_stride} apart \
                 to {} bytes {dst_stride} apart",
                src.len(),
                dst.len()
            ),
        });
    }
    for row in 0..rows {
        let (s, d) = (row * src_stride, row * dst_stride);
        dst[d..d + row_bytes].copy_from_slice(&src[s..s + row_bytes]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use jstream_platform::PlatformError;
    use jstream_platform::media::{MediaTime, Timebase};

    use super::{annexb_access_unit, cm_time_parts, copy_rows, media_time_from_cm};

    const SPS: &[u8] = &[0x67, 0x64, 0x00, 0x28];
    const PPS: &[u8] = &[0x68, 0xEE, 0x3C, 0x80];

    #[test]
    fn a_keyframe_gets_its_parameter_sets_and_start_codes() {
        // An IDR slice of 3 bytes and an SEI of 2, behind 4-byte lengths.
        let data = [0, 0, 0, 3, 0x65, 0x88, 0x84, 0, 0, 0, 2, 0x06, 0x05];
        let mut out = vec![0xAA];
        annexb_access_unit(&data, 4, &[SPS, PPS], &mut out).expect("converts");
        let mut want = vec![0xAA];
        for unit in [SPS, PPS, &[0x65, 0x88, 0x84], &[0x06, 0x05]] {
            want.extend_from_slice(&[0, 0, 0, 1]);
            want.extend_from_slice(unit);
        }
        assert_eq!(out, want);
    }

    #[test]
    fn short_length_prefixes_are_read_big_endian() {
        // A 258-byte unit behind a 2-byte length, then a 1-byte one behind it.
        let mut data = vec![0x01, 0x02, 0x41];
        data.extend(std::iter::repeat_n(0x7F, 257));
        data.extend_from_slice(&[0x00, 0x01, 0x01]);
        let mut out = Vec::new();
        annexb_access_unit(&data, 2, &[], &mut out).expect("converts");
        assert_eq!(out.len(), 4 + 258 + 4 + 1);
        assert_eq!(&out[..5], &[0, 0, 0, 1, 0x41]);
        assert_eq!(&out[262..], &[0, 0, 0, 1, 0x01]);

        let mut out = Vec::new();
        annexb_access_unit(&[1, 0x41, 1, 0x01], 1, &[], &mut out).expect("converts");
        assert_eq!(out, [0, 0, 0, 1, 0x41, 0, 0, 0, 1, 0x01]);
    }

    #[test]
    fn malformed_samples_are_refused_and_append_nothing() {
        for (data, size, sets) in [
            (&[0u8, 0, 0, 1, 0x41][..], 3, &[][..]),
            (&[0, 0, 0, 9, 0x41], 4, &[]),
            (&[0, 0, 0, 1, 0x41, 0, 0], 4, &[]),
            (&[0, 0, 0, 0], 4, &[]),
            (&[], 4, &[]),
            (&[0, 0, 0, 1, 0x41], 4, &[&[][..]]),
        ] {
            let mut out = vec![0xAA];
            assert!(
                matches!(
                    annexb_access_unit(data, size, sets, &mut out),
                    Err(PlatformError::Os { .. })
                ),
                "{data:?} {size}"
            );
            assert_eq!(out, [0xAA], "{data:?} {size}");
        }
    }

    #[test]
    fn media_clock_times_round_trip_through_core_media() {
        let hns = Timebase::new(1, 10_000_000).expect("valid");
        let t = MediaTime::new(123_456_789_012, hns);
        let (value, timescale) = cm_time_parts(t).expect("fits");
        assert_eq!((value, timescale), (123_456_789_012, 10_000_000));
        assert_eq!(
            media_time_from_cm(value, timescale, hns)
                .expect("fits")
                .ticks(),
            123_456_789_012
        );

        let ntsc = Timebase::new(1001, 30_000).expect("valid");
        let frame = MediaTime::new(7, ntsc);
        let (value, timescale) = cm_time_parts(frame).expect("fits");
        assert_eq!((value, timescale), (7007, 30_000));
        assert_eq!(
            media_time_from_cm(value, timescale, ntsc)
                .expect("fits")
                .ticks(),
            7
        );
    }

    #[test]
    fn times_core_media_cant_hold_are_refused() {
        let wide = Timebase::new(1, u32::MAX).expect("valid");
        assert_eq!(cm_time_parts(MediaTime::new(1, wide)), None);
        let coarse = Timebase::new(u32::MAX, 1).expect("valid");
        assert_eq!(cm_time_parts(MediaTime::new(i64::MAX / 2, coarse)), None);
        let hns = Timebase::new(1, 10_000_000).expect("valid");
        assert!(media_time_from_cm(1, 0, hns).is_err());
        assert!(media_time_from_cm(1, -1, hns).is_err());
    }

    #[test]
    fn rows_copy_between_strides() {
        // Two rows of 3 bytes, 4 apart, into rows 5 apart.
        let src = [1, 2, 3, 0, 4, 5, 6];
        let mut dst = [9u8; 8];
        copy_rows(&src, 4, &mut dst, 5, 3, 2).expect("fits");
        assert_eq!(dst, [1, 2, 3, 9, 9, 4, 5, 6]);
        copy_rows(&[], 4, &mut [], 4, 3, 0).expect("no rows copy nothing");
    }

    #[test]
    fn rows_that_dont_fit_are_refused() {
        let mut dst = [0u8; 8];
        for (src_len, src_stride, dst_stride, row_bytes, rows) in [
            (7, 4, 5, 3, 3),
            (7, 2, 5, 3, 2),
            (7, 4, 2, 3, 2),
            (11, 4, 6, 3, 3),
        ] {
            let src = vec![0u8; src_len];
            assert!(
                matches!(
                    copy_rows(&src, src_stride, &mut dst, dst_stride, row_bytes, rows),
                    Err(PlatformError::Unsupported { .. })
                ),
                "{src_len} {src_stride} {dst_stride} {row_bytes} {rows}"
            );
        }
        assert_eq!(dst, [0; 8]);
    }
}
