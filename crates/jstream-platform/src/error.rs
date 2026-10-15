//! The crate's error type, shared by every backend trait.

use jstream_media::MediaError;

/// Why a backend call failed.
///
/// One enum for every trait, so the engine handles platform failures in one place and
/// errors never cross a crate boundary as bare strings (R-RS-003). Backends map their OS
/// errors into it: an `HRESULT`, `OSStatus` or `errno` goes in [`PlatformError::Os`],
/// with the call that returned it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PlatformError {
    /// The backend does not support what was asked: a codec, a pixel or sample format, a
    /// capture target kind, or a whole capability (such as a virtual camera).
    #[error("not supported by this backend: {what}")]
    Unsupported {
        /// What was asked for.
        what: String,
    },
    /// No device or capture target has this ID (any more).
    #[error("no {kind} with ID {id:?}")]
    NotFound {
        /// The kind of thing looked up, such as "audio device" or "display".
        kind: &'static str,
        /// The ID that was not found.
        id: String,
    },
    /// A queue is full: the caller drops the input and counts it (R-RT-004). Raised, for
    /// example, by an encoder that has not asked for more input.
    #[error("{what} is busy")]
    Busy {
        /// What was busy.
        what: &'static str,
    },
    /// The call is not valid in the object's current state, such as encoding after
    /// `finish` or publishing to a virtual camera that is not started.
    #[error("{what}: {state}")]
    InvalidState {
        /// The operation that was refused.
        what: &'static str,
        /// The state that refused it.
        state: &'static str,
    },
    /// A device or capture target stopped working: unplugged, disabled, or its window
    /// closed. The stream that reported it delivers nothing more.
    #[error("{what} was lost: {reason}")]
    DeviceLost {
        /// The device or target.
        what: String,
        /// Why, as the backend reports it.
        reason: String,
    },
    /// An OS component the backend needs is not installed, such as Media Foundation on a
    /// Windows N edition without the Media Feature Pack. Retrying won't help; the user has
    /// to install it, as `remedy` says.
    #[error("{component} is not installed: {remedy}")]
    MissingComponent {
        /// The component, such as "Media Foundation".
        component: &'static str,
        /// What the user can do about it, as a sentence.
        remedy: String,
    },
    /// An OS or driver call failed.
    #[error("{call} failed{}: {message}", .code.map(|c| format!(" (0x{c:08X})")).unwrap_or_default())]
    Os {
        /// The API that failed, such as "IMFTransform::ProcessInput".
        call: &'static str,
        /// The raw status code (`HRESULT`, `OSStatus`, `errno`), when there is one.
        code: Option<u32>,
        /// The OS's message for it.
        message: String,
    },
    /// A media value could not be built, such as a frame whose planes don't match its
    /// format.
    #[error(transparent)]
    Media(#[from] MediaError),
}

#[cfg(test)]
mod tests {
    use super::PlatformError;
    use jstream_media::MediaError;

    #[test]
    fn messages_carry_context() {
        let e = PlatformError::Os {
            call: "IMFTransform::ProcessInput",
            code: Some(0xC00D_6D72),
            message: "The transform cannot take more input".into(),
        };
        assert_eq!(
            e.to_string(),
            "IMFTransform::ProcessInput failed (0xC00D6D72): The transform cannot take more input"
        );
        let e = PlatformError::Os {
            call: "open",
            code: None,
            message: "denied".into(),
        };
        assert_eq!(e.to_string(), "open failed: denied");
        let e = PlatformError::NotFound {
            kind: "display",
            id: "DISPLAY2".into(),
        };
        assert_eq!(e.to_string(), "no display with ID \"DISPLAY2\"");
        let e = PlatformError::MissingComponent {
            component: "Media Foundation",
            remedy: "install the Media Feature Pack.".into(),
        };
        assert_eq!(
            e.to_string(),
            "Media Foundation is not installed: install the Media Feature Pack."
        );
    }

    #[test]
    fn media_errors_convert() {
        let e: PlatformError = MediaError::EmptyPacket.into();
        assert_eq!(e, PlatformError::Media(MediaError::EmptyPacket));
        assert_eq!(e.to_string(), MediaError::EmptyPacket.to_string());
    }
}
