//! The crate's error type.

use crate::{MediaTime, PixelFormat, Timebase};

/// Why a media value could not be built or computed.
///
/// Every variant carries the values that broke the invariant, so the message is useful
/// without the caller's context.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MediaError {
    /// A time base had a zero numerator or denominator.
    #[error("invalid time base {num}/{den}: both parts must be non-zero")]
    InvalidTimebase {
        /// The numerator given.
        num: u32,
        /// The denominator given.
        den: u32,
    },
    /// Two timestamps were combined without first rescaling them to one time base.
    #[error("time bases differ ({left} vs {right}); rescale first")]
    TimebaseMismatch {
        /// The time base of the left operand.
        left: Timebase,
        /// The time base of the right operand.
        right: Timebase,
    },
    /// An arithmetic result did not fit its type.
    #[error("arithmetic overflow computing {what}")]
    Overflow {
        /// What was being computed.
        what: &'static str,
    },
    /// A frame size had a zero width or height.
    #[error("frame size {width}x{height} has a zero dimension")]
    ZeroDimension {
        /// The width given.
        width: u32,
        /// The height given.
        height: u32,
    },
    /// A frame was given the wrong number of planes for its pixel format.
    #[error("{format:?} has {expected} plane(s), got {actual}")]
    PlaneCount {
        /// The pixel format.
        format: PixelFormat,
        /// How many planes the format has.
        expected: usize,
        /// How many planes were given.
        actual: usize,
    },
    /// A plane's stride was shorter than one row of its samples.
    #[error("plane {plane}: stride {stride} is shorter than its row size {min}")]
    StrideTooSmall {
        /// The plane index.
        plane: usize,
        /// The stride given, in bytes.
        stride: usize,
        /// The minimum stride (the plane's row size), in bytes.
        min: usize,
    },
    /// A plane's buffer was too short for its stride and height.
    #[error("plane {plane}: buffer is {len} bytes, needs {required}")]
    PlaneTooSmall {
        /// The plane index.
        plane: usize,
        /// The buffer length, in bytes.
        len: usize,
        /// The bytes needed: `stride * (height - 1) + row_bytes`.
        required: usize,
    },
    /// An audio format had a sample rate of zero.
    #[error("sample rate must be non-zero")]
    ZeroSampleRate,
    /// An audio buffer's sample count was not a multiple of its channel count.
    #[error("{samples} samples is not a whole number of frames of {channels} channel(s)")]
    SampleCountMismatch {
        /// The number of samples given.
        samples: usize,
        /// The channel count of the format.
        channels: usize,
    },
    /// An encoded packet had no data.
    #[error("encoded packet has no data")]
    EmptyPacket,
    /// An encoded packet's decode timestamp was later than its presentation timestamp.
    #[error("decode timestamp {dts} is after presentation timestamp {pts}")]
    DtsAfterPts {
        /// The presentation timestamp.
        pts: MediaTime,
        /// The decode timestamp.
        dts: MediaTime,
    },
}

#[cfg(test)]
mod tests {
    use super::MediaError;
    use crate::PixelFormat;

    #[test]
    fn messages_carry_context() {
        let e = MediaError::StrideTooSmall {
            plane: 1,
            stride: 10,
            min: 12,
        };
        assert_eq!(
            e.to_string(),
            "plane 1: stride 10 is shorter than its row size 12"
        );
        let e = MediaError::PlaneCount {
            format: PixelFormat::Nv12,
            expected: 2,
            actual: 1,
        };
        assert_eq!(e.to_string(), "Nv12 has 2 plane(s), got 1");
    }

    #[test]
    fn is_a_std_error() {
        fn takes(_: &dyn std::error::Error) {}
        takes(&MediaError::EmptyPacket);
    }
}
