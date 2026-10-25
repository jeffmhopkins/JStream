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
    /// An input's or a monitor device's sample rate is too far from the engine rate to
    /// resample.
    #[error("unsupported device rate {rate} Hz for a {engine} Hz engine")]
    UnsupportedRate {
        /// The device's rate, Hz.
        rate: u32,
        /// The engine's rate, Hz.
        engine: u32,
    },
    /// An input's or a monitor device's samples are not interleaved `f32`, which every
    /// platform backend uses.
    #[error("unsupported sample format: devices are interleaved f32")]
    UnsupportedFormat,
    /// The output buffer passed to the mixer has the wrong length.
    #[error("output buffer holds {got} samples, the block needs {expected}")]
    OutputLength {
        /// `2 * block_frames`.
        expected: usize,
        /// What was passed.
        got: usize,
    },
    /// The mixer already has as many inputs as it can install without allocating on the
    /// mixer thread.
    #[error("the mixer already has the maximum number of inputs")]
    TooManyInputs,
    /// The mixer already has as many buses, the master included, as it reserved mix
    /// buffers for.
    #[error("the mixer already has the maximum number of buses")]
    TooManyBuses,
    /// The filter chain already holds as many filters as it reserved.
    #[error("the filter chain already has the maximum number of filters")]
    TooManyFilters,
    /// The mixer already holds as many output taps as it reserved.
    #[error("the mixer already has the maximum number of output taps")]
    TooManyOutputTaps,
    /// The input, bus, output tap or filter was removed.
    #[error("the input, bus, output tap or filter was removed")]
    Removed,
    /// Input, bus or filter changes are waiting for the mixer to apply them, and the
    /// queue is full.
    #[error("the mixer command queue is full")]
    CommandQueueFull,
    /// The thread that frees the mixer's retired rings could not be started.
    #[error("the mixer's collector thread could not be started")]
    CollectorUnavailable,
}
