//! The Microphone and Camera permissions (ADR-0063 §4, §10, §11), and the hint for the
//! system audio recording permission. The Camera permission is checked the same way
//! before a webcam opens (`crate::video_device`).
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

/// Which `AVCaptureDevice` permission: the media type it is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Media {
    /// Audio inputs (`AVMediaTypeAudio`).
    Microphone,
    /// Cameras (`AVMediaTypeVideo`).
    Camera,
}

impl Media {
    /// The permission's name, as System Settings shows it.
    fn pane(self) -> &'static str {
        match self {
            Self::Microphone => "Microphone",
            Self::Camera => "Camera",
        }
    }

    /// What the permission lets JStream do.
    fn what(self) -> &'static str {
        match self {
            Self::Microphone => "record audio inputs",
            Self::Camera => "capture cameras",
        }
    }

    /// What to open again once it is granted.
    fn retry(self) -> &'static str {
        match self {
            Self::Microphone => "open the input again",
            Self::Camera => "open the camera again",
        }
    }
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
    denied(Media::Microphone, status, asked_now)
}

/// The error for the `media` permission with `status` that isn't granted. `asked_now`
/// says whether macOS was just asked to show its prompt.
pub(crate) fn denied(media: Media, status: isize, asked_now: bool) -> PlatformError {
    let (pane, what, retry) = (media.pane(), media.what(), media.retry());
    let message = if status == RESTRICTED {
        format!(
            "the {pane} permission is restricted on this Mac by a configuration profile or \
             Screen Time, so JStream can't {what}; whoever manages the Mac has to allow it"
        )
    } else {
        let state = if asked_now {
            format!("the {pane} permission is not granted, and macOS was asked to prompt for it")
        } else {
            format!("the {pane} permission is not granted")
        };
        format!(
            "{state}. Allow JStream, or the terminal app it runs from, in System Settings > \
             Privacy & Security > {pane}, then {retry}"
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
pub(crate) use system::{
    authorization_status, authorization_status_for, request_access, request_access_for,
};

#[cfg(target_os = "macos")]
mod system {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_av_foundation::{AVCaptureDevice, AVMediaType, AVMediaTypeAudio, AVMediaTypeVideo};

    use super::{Media, NOT_DETERMINED};

    /// The media type `media` is asked for.
    fn media_type(media: Media) -> Option<&'static AVMediaType> {
        // SAFETY: constants AVFoundation exports, read only.
        unsafe {
            match media {
                Media::Microphone => AVMediaTypeAudio,
                Media::Camera => AVMediaTypeVideo,
            }
        }
    }

    /// The Microphone authorization status (`AVAuthorizationStatus`), without prompting.
    pub(crate) fn authorization_status() -> isize {
        authorization_status_for(Media::Microphone)
    }

    /// The `media` authorization status (`AVAuthorizationStatus`), without prompting.
    pub(crate) fn authorization_status_for(media: Media) -> isize {
        let Some(kind) = media_type(media) else {
            return NOT_DETERMINED;
        };
        // SAFETY: a class method that only reads TCC's state; the media type is audio or
        // video, the two it accepts, so it doesn't throw.
        unsafe { AVCaptureDevice::authorizationStatusForMediaType(kind) }.0
    }

    /// Asks macOS to prompt for the Microphone permission, without waiting for the answer.
    pub(crate) fn request_access() {
        request_access_for(Media::Microphone);
    }

    /// Asks macOS to prompt for the `media` permission, without waiting for the answer.
    pub(crate) fn request_access_for(media: Media) {
        let Some(kind) = media_type(media) else {
            return;
        };
        // The answer is read again on the next open, so the handler does nothing (and
        // can't panic).
        let handler = RcBlock::new(|_granted: Bool| {});
        // SAFETY: a class method taking the audio or video media type and a block
        // AVFoundation copies and calls once on a queue of its own.
        unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(kind, &handler) };
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
        assert!(message.contains("record audio inputs"), "{message}");
    }

    #[test]
    fn the_camera_errors_name_the_camera_pane() {
        let PlatformError::Os { message, .. } = denied(Media::Camera, DENIED, false) else {
            panic!("not an OS error");
        };
        assert!(message.contains("Privacy & Security > Camera"), "{message}");
        assert!(message.contains("open the camera again"), "{message}");
        let PlatformError::Os { message, .. } = denied(Media::Camera, RESTRICTED, false) else {
            panic!("not an OS error");
        };
        assert!(message.contains("capture cameras"), "{message}");
    }
}
