//! The crate's error type.

use std::fmt;

use crate::{MediaTime, PixelFormat, Timebase};

/// Why a media value could not be built or computed.
///
/// Every variant carries the values that broke the invariant, so the message is useful
/// without the caller's context.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MediaError {
    /// A time base had a zero numerator or denominator.
    InvalidTimebase {
        /// The numerator given.
        num: u32,
        /// The denominator given.
        den: u32,
    },
    /// Two timestamps were combined without first rescaling them to one time base.
    TimebaseMismatch {
        /// The time base of the left operand.
        left: Timebase,
        /// The time base of the right operand.
        right: Timebase,
    },
    /// An arithmetic result did not fit its type.
    Overflow {
        /// What was being computed.
        what: &'static str,
    },
    /// A frame size had a zero width or height.
    ZeroDimension {
        /// The width given.
        width: u32,
        /// The height given.
        height: u32,
    },
    /// A frame was given the wrong number of planes for its pixel format.
    PlaneCount {
        /// The pixel format.
        format: PixelFormat,
        /// How many planes the format has.
        expected: usize,
        /// How many planes were given.
        actual: usize,
    },
    /// A plane's stride was shorter than one row of its samples.
    StrideTooSmall {
        /// The plane index.
        plane: usize,
        /// The stride given, in bytes.
        stride: usize,
        /// The minimum stride (the plane's row size), in bytes.
        min: usize,
    },
    /// A plane's buffer was too short for its stride and height.
    PlaneTooSmall {
        /// The plane index.
        plane: usize,
        /// The buffer length, in bytes.
        len: usize,
        /// The bytes needed: `stride * (height - 1) + row_bytes`.
        required: usize,
    },
    /// An audio format had a sample rate of zero.
    ZeroSampleRate,
    /// An audio buffer's sample count was not a multiple of its channel count.
    SampleCountMismatch {
        /// The number of samples given.
        samples: usize,
        /// The channel count of the format.
        channels: usize,
    },
    /// An encoded packet had no data.
    EmptyPacket,
    /// An encoded packet's decode timestamp was later than its presentation timestamp.
    DtsAfterPts {
        /// The presentation timestamp.
        pts: MediaTime,
        /// The decode timestamp.
        dts: MediaTime,
    },
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTimebase { num, den } => {
                write!(
                    f,
                    "invalid time base {num}/{den}: both parts must be non-zero"
                )
            }
            Self::TimebaseMismatch { left, right } => {
                write!(f, "time bases differ ({left} vs {right}); rescale first")
            }
            Self::Overflow { what } => write!(f, "arithmetic overflow computing {what}"),
            Self::ZeroDimension { width, height } => {
                write!(f, "frame size {width}x{height} has a zero dimension")
            }
            Self::PlaneCount {
                format,
                expected,
                actual,
            } => write!(f, "{format:?} has {expected} plane(s), got {actual}"),
            Self::StrideTooSmall { plane, stride, min } => write!(
                f,
                "plane {plane}: stride {stride} is shorter than its row size {min}"
            ),
            Self::PlaneTooSmall {
                plane,
                len,
                required,
            } => write!(f, "plane {plane}: buffer is {len} bytes, needs {required}"),
            Self::ZeroSampleRate => write!(f, "sample rate must be non-zero"),
            Self::SampleCountMismatch { samples, channels } => write!(
                f,
                "{samples} samples is not a whole number of frames of {channels} channel(s)"
            ),
            Self::EmptyPacket => write!(f, "encoded packet has no data"),
            Self::DtsAfterPts { pts, dts } => {
                write!(
                    f,
                    "decode timestamp {dts} is after presentation timestamp {pts}"
                )
            }
        }
    }
}

impl std::error::Error for MediaError {}

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
