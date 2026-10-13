//! The crate's error type.

use jstream_media::{Codec, MediaError};

/// Why a stream could not be framed, encoded, decoded or written.
///
/// Every variant carries the values that caused it, so the message is useful without the
/// caller's context.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MuxError {
    /// H.264 data said to be Annex B did not start with a start code.
    #[error("H.264 data is not Annex B: no start code before byte {offset}")]
    NotAnnexB {
        /// Where the first non-zero byte was found.
        offset: usize,
    },
    /// A length prefix in AVCC data ran past the end of the buffer.
    #[error("AVCC NAL unit at byte {offset} claims {len} bytes, but only {available} remain")]
    TruncatedAvcc {
        /// Where the length prefix starts.
        offset: usize,
        /// The length it claims.
        len: usize,
        /// How many bytes follow the prefix.
        available: usize,
    },
    /// A NAL unit, parameter set or AMF0 value is longer than its length field allows.
    #[error("{what} is {len} bytes, more than the {max} its length field allows")]
    TooLong {
        /// What was being written.
        what: &'static str,
        /// Its length.
        len: usize,
        /// The largest length the format can express.
        max: usize,
    },
    /// No SPS or no PPS was available to build an `AVCDecoderConfigurationRecord`.
    #[error(
        "missing H.264 parameter sets: {sps} SPS and {pps} PPS found; need at least one of each"
    )]
    MissingParameterSets {
        /// How many SPS NAL units were found.
        sps: usize,
        /// How many PPS NAL units were found.
        pps: usize,
    },
    /// An SPS was too short or malformed to read the fields the record needs.
    #[error("malformed H.264 SPS: {reason}")]
    InvalidSps {
        /// What was wrong.
        reason: &'static str,
    },
    /// An `AudioSpecificConfig` had a value the format can't express or was too short.
    #[error("invalid AAC AudioSpecificConfig: {reason}")]
    InvalidAudioConfig {
        /// What was wrong.
        reason: &'static str,
    },
    /// A packet's codec has no track in the muxer's configuration.
    #[error("no {codec:?} track is configured")]
    NoTrack {
        /// The packet's codec.
        codec: Codec,
    },
    /// The first video packet carried no SPS/PPS and none were configured.
    #[error(
        "no AVC decoder configuration: configure one or start with a keyframe carrying SPS and PPS"
    )]
    MissingAvcConfig,
    /// A timestamp fell before the muxer's origin.
    #[error("timestamp {ms} ms is before the stream origin")]
    NegativeTimestamp {
        /// The timestamp relative to the origin, in milliseconds.
        ms: i64,
    },
    /// A track's decode timestamps went backwards.
    #[error("{codec:?} decode timestamp went backwards: {previous} ms then {current} ms")]
    NonMonotonicDts {
        /// The track's codec.
        codec: Codec,
        /// The previous packet's dts, in milliseconds from the origin.
        previous: i64,
        /// This packet's dts, in milliseconds from the origin.
        current: i64,
    },
    /// pts − dts did not fit FLV's signed 24-bit composition time.
    #[error("composition time offset {ms} ms does not fit in 24 bits")]
    CompositionOffsetOutOfRange {
        /// pts − dts, in milliseconds.
        ms: i64,
    },
    /// AMF0 input ended in the middle of a value.
    #[error("AMF0 data truncated at byte {offset}")]
    Amf0Truncated {
        /// How many bytes had been read: where the field that didn't fit starts.
        offset: usize,
    },
    /// An AMF0 type marker this decoder doesn't support.
    #[error("unsupported AMF0 type marker {marker:#04x}")]
    Amf0UnsupportedMarker {
        /// The marker byte.
        marker: u8,
    },
    /// An AMF0 string was not valid UTF-8.
    #[error("AMF0 string is not valid UTF-8")]
    Amf0InvalidUtf8,
    /// AMF0 values were nested deeper than the decoder allows.
    #[error("AMF0 values nested deeper than {max}")]
    Amf0TooDeep {
        /// The nesting limit.
        max: usize,
    },
    /// A timestamp could not be rescaled.
    #[error(transparent)]
    Media(#[from] MediaError),
    /// The output failed.
    #[error("write failed: {0}")]
    Io(#[from] std::io::Error),
}
