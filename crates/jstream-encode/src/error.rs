//! Why a session failed, why a push was refused, and how a packet stream ended.

use jstream_platform::PlatformError;
use jstream_platform::media::{Codec, MediaKind};

/// Why an encoder session could not start, or stopped.
///
/// `Clone`, because every subscriber of a failed session receives the same error.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EncodeError {
    /// The configured codec is for the other media kind (an audio codec for a video
    /// session, or the reverse).
    #[error("{codec:?} is not a {expected:?} codec")]
    WrongKind {
        /// The configured codec.
        codec: Codec,
        /// The kind the session needs.
        expected: MediaKind,
    },
    /// The platform could not open the encoder.
    #[error("opening the encoder failed: {0}")]
    Open(PlatformError),
    /// The encoder failed while encoding, polling or draining. The session has stopped.
    #[error("the encoder failed: {0}")]
    Encoder(PlatformError),
    /// The OS refused to start the encode thread.
    #[error("the encode thread could not be started: {message}")]
    Spawn {
        /// The OS's message.
        message: String,
    },
    /// The encode thread panicked (in the encoder or in this crate). The panic was
    /// caught at the thread boundary and the session stopped.
    #[error("the encode thread panicked: {message}")]
    Panicked {
        /// The panic's message, when it was a string.
        message: String,
    },
}

/// How a subscriber's packet stream ended. Every packet queued before the end is
/// delivered first.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StreamEnd {
    /// The session was finished: the encoder drained, and every packet it produced was
    /// offered to the subscriber.
    #[error("the encoder session finished")]
    Finished,
    /// The session stopped on an error.
    #[error(transparent)]
    Failed(EncodeError),
    /// The session was dropped without being finished, so the encoder was not drained.
    #[error("the encoder session was closed without finishing")]
    Closed,
}

/// Why [`VideoInput::push`](crate::VideoInput::push) or
/// [`AudioInput::push`](crate::AudioInput::push) did not queue its input. Either way the
/// input is dropped, and the call never blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, thiserror::Error)]
pub enum PushError {
    /// The input queue is full. The input was dropped and counted in
    /// [`SessionStats::dropped_queue_full`](crate::SessionStats::dropped_queue_full)
    /// (R-RT-004).
    #[error("the encoder's input queue is full; the input was dropped")]
    Full,
    /// The session is finishing, or has ended.
    #[error("the encoder session is finishing or has ended")]
    Closed,
}
