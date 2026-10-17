//! The Microphone permission (ADR-0063 §4, §11), and the hint for the system audio
//! recording permission.
//!
//! Without it macOS doesn't refuse a Core Audio input: it records silence. So recording a
//! microphone or line input checks it first, through `AVCaptureDevice`'s authorization
//! status for audio, as the Screen Recording permission is checked before capture. When
//! macOS hasn't asked yet, the backend asks once (`requestAccessForMediaType:`, which
//! shows the prompt) and reports an error naming the System Settings pane; later calls
//! report it without asking again, and nothing retries on its own. A grant takes effect
//! for the next open.
//!
//! Desktop and application audio need the system audio recording permission instead,
//! which has no public API to read: macOS prompts when a tap first runs, and without the
//! grant the tap records silence. JStream accepts that silence. It doesn't guess from
//! the samples or read the private TCC database; it logs [`SYSTEM_AUDIO_HINT`] when a tap
//! opens.
//!
//! The steps, errors and hint are platform independent, so they are tested on every
//! platform.

use jstream_platform::PlatformError;

/// `AVAuthorizationStatusNotDetermined`: the user hasn't been asked.
pub(crate) const NOT_DETERMINED: isize = 0;
/// `AVAuthorizationStatusRestricted`: a configuration profile or parental controls deny
/// it, and the user can't change that.
pub(crate) const RESTRICTED: isize = 1;
/// `AVAuthorizationStatusDenied`: the user refused it.
pub(crate) const DENIED: isize = 2;
/// `AVAuthorizationStatusAuthorized`.
pub(crate) const AUTHORIZED: isize = 3;

/// The call the permission errors name.
const CALL: &str = "AVCaptureDevice authorizationStatusForMediaType";

/// What opening an input does about the Microphone permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MicStep {
    /// Granted: go ahead.
    Proceed,
    /// Not asked yet, and this backend hasn't asked: ask once, then report
    /// [`microphone_denied`] with `asked_now`.
    Ask,
    /// Refused, restricted, or asked already: report the error without asking.
    Refuse,
}

/// The step for an authorization `status`, when this backend has `asked` before. A status
/// added after this mapping proceeds, so a new kind of grant doesn't lock recording out.
pub(crate) fn mic_step(status: isize, asked: bool) -> MicStep {
    match status {
        AUTHORIZED => MicStep::Proceed,
        NOT_DETERMINED if !asked => MicStep::Ask,
        NOT_DETERMINED | RESTRICTED | DENIED => MicStep::Refuse,
        _ => MicStep::Proceed,
    }
}

/// The error for a Microphone permission with `status` that isn't granted. `asked_now`
/// says whether macOS was just asked to show its prompt.
pub(crate) fn microphone_denied(status: isize, asked_now: bool) -> PlatformError {
    let message = if status == RESTRICTED {
        "the Microphone permission is restricted on this Mac by a configuration profile or \
         Screen Time, so JStream can't record audio inputs; whoever manages the Mac has to \
         allow it"
            .to_owned()
    } else {
        let state = if asked_now {
            "the Microphone permission is not granted, and macOS was asked to prompt for it"
        } else {
            "the Microphone permission is not granted"
        };
        format!(
            "{state}. Allow JStream, or the terminal app it runs from, in System Settings > \
             Privacy & Security > Microphone, then open the input again"
        )
    };
    PlatformError::Os {
        call: CALL,
        code: None,
        message,
    }
}

/// Logged when a process tap opens: where the system audio recording permission is
/// granted, since a tap without it records silence and nothing reports a refusal.
pub(crate) const SYSTEM_AUDIO_HINT: &str = "desktop and application audio are silent \
    unless JStream, or the terminal app it runs from, is allowed to record system audio: \
    macOS asks the first time a tap runs, and the setting is in System Settings > \
    Privacy & Security > Screen & System Audio Recording, under System Audio Recording \
    Only";

#[cfg(target_os = "macos")]
pub(crate) use system::{authorization_status, request_access};

#[cfg(target_os = "macos")]
mod system {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeAudio};

    use super::NOT_DETERMINED;

    /// The Microphone authorization status (`AVAuthorizationStatus`), without prompting.
    pub(crate) fn authorization_status() -> isize {
        // SAFETY: `AVMediaTypeAudio` is a constant AVFoundation exports, read only.
        let Some(audio) = (unsafe { AVMediaTypeAudio }) else {
            return NOT_DETERMINED;
        };
        // SAFETY: a class method that only reads TCC's state; the media type is audio, one
        // of the two it accepts, so it doesn't throw.
        unsafe { AVCaptureDevice::authorizationStatusForMediaType(audio) }.0
    }

    /// Asks macOS to prompt for the Microphone permission, without waiting for the answer.
    pub(crate) fn request_access() {
        // SAFETY: as above.
        let Some(audio) = (unsafe { AVMediaTypeAudio }) else {
            return;
        };
        // The answer is read again on the next open, so the handler does nothing (and
        // can't panic).
        let handler = RcBlock::new(|_granted: Bool| {});
        // SAFETY: a class method taking the audio media type and a block AVFoundation
        // copies and calls once on a queue of its own.
        unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(audio, &handler) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_unasked_status_is_asked_once() {
        assert_eq!(mic_step(AUTHORIZED, false), MicStep::Proceed);
        assert_eq!(mic_step(AUTHORIZED, true), MicStep::Proceed);
        assert_eq!(mic_step(NOT_DETERMINED, false), MicStep::Ask);
        assert_eq!(mic_step(NOT_DETERMINED, true), MicStep::Refuse);
        for status in [DENIED, RESTRICTED] {
            assert_eq!(mic_step(status, false), MicStep::Refuse, "{status}");
            assert_eq!(mic_step(status, true), MicStep::Refuse, "{status}");
        }
        assert_eq!(mic_step(7, false), MicStep::Proceed, "a status added later");
    }

    #[test]
    fn the_system_audio_hint_names_the_settings_pane() {
        assert!(
            SYSTEM_AUDIO_HINT.contains("Privacy & Security > Screen & System Audio Recording"),
            "{SYSTEM_AUDIO_HINT}"
        );
        assert!(!SYSTEM_AUDIO_HINT.contains("  "), "{SYSTEM_AUDIO_HINT}");
    }

    #[test]
    fn errors_name_the_settings_pane_or_the_restriction() {
        let PlatformError::Os { call, message, .. } = microphone_denied(DENIED, false) else {
            panic!("not an OS error");
        };
        assert_eq!(call, CALL);
        assert!(
            message.contains("Privacy & Security > Microphone"),
            "{message}"
        );
        assert!(!message.contains("asked to prompt"), "{message}");
        let PlatformError::Os { message, .. } = microphone_denied(NOT_DETERMINED, true) else {
            panic!("not an OS error");
        };
        assert!(message.contains("asked to prompt"), "{message}");
        let PlatformError::Os { message, .. } = microphone_denied(RESTRICTED, false) else {
            panic!("not an OS error");
        };
        assert!(message.contains("restricted"), "{message}");
    }
}
