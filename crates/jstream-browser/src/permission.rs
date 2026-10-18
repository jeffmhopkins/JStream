//! What a page gets when it asks for a permission: nothing (#347). Camera, microphone,
//! screen capture, geolocation, notifications, clipboard, MIDI and every other permission
//! Chromium asks about are refused by policy, so the answer doesn't depend on what CEF's
//! windowless default happens to be in a given version. The helper's permission handler
//! (`helper/handlers.rs`) asks [`answer`] for every request and acts on it.

/// How a page's permission request is answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    /// Refused: a media request is cancelled, a permission prompt is answered `DENY`.
    Deny,
}

/// The answer to a page asking for `requested`, CEF's bit mask of permissions (media
/// access or a prompt's request types): always [`Answer::Deny`], whatever it names.
pub(crate) fn answer(requested: u32) -> Answer {
    let _ = requested;
    Answer::Deny
}

#[cfg(test)]
mod tests {
    use super::{Answer, answer};

    /// #347: every permission is refused, each one alone, none, and all at once,
    /// including bits CEF hasn't defined yet.
    #[test]
    fn every_permission_is_refused() {
        let masks = (0..u32::BITS).map(|bit| 1u32 << bit).chain([0, u32::MAX]);
        for requested in masks {
            assert_eq!(answer(requested), Answer::Deny, "{requested:#x}");
        }
    }
}
