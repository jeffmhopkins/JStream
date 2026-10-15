//! The crate's error type.

/// Why a mixer operation failed. No variant allocates, so the mixer can return one from
/// its real-time loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AudioError {
    /// A [`MixerConfig`](crate::MixerConfig) field, or a meter's sample rate, is out of
    /// range.
    #[error("invalid audio configuration: {what}")]
    InvalidConfig {
        /// Which field, and the range it must be in.
        what: &'static str,
    },
    /// An input's sample rate is too far from the engine rate to resample.
    #[error("unsupported input rate {rate} Hz for a {engine} Hz engine")]
    UnsupportedRate {
        /// The input's rate, Hz.
        rate: u32,
        /// The engine's rate, Hz.
        engine: u32,
    },
    /// An input's samples are not interleaved `f32`, which every platform backend
    /// delivers.
    #[error("unsupported input sample format: inputs are interleaved f32")]
    UnsupportedFormat,
    /// The output buffer passed to the mixer has the wrong length.
    #[error("output buffer holds {got} samples, the block needs {expected}")]
    OutputLength {
        /// `2 * block_frames`.
        expected: usize,
        /// What was passed.
        got: usize,
    },
}
