//! What a page gets when it asks for a permission (#347). Camera, microphone, screen
//! capture, geolocation, notifications, clipboard, MIDI and every other permission
//! Chromium asks about are refused by policy, so the answer doesn't depend on what CEF's
//! windowless default happens to be in a given version. The helper's permission handler
//! (`helper/handlers.rs`) asks [`answer_media`] for every media request and
//! [`answer_prompt`] for every permission prompt, and acts on the answer.
//!
//! One exception: Chromium's Local Network Access checks ask for a permission before a
//! page reaches this computer or the local network from a less private address. That
//! prompt is answered by the page's [`RequestPolicy`](crate::RequestPolicy): granted when
//! the policy lets the page make local requests (its source opted in, or the page is
//! itself local or a file), refused otherwise. So an opted-in widget can still reach an
//! app on `127.0.0.1`, and for every other page from the internet Chromium refuses, at
//! the network layer, what the request policy can't see: a public name that resolves to
//! a local address, and requests that never reach the resource handler.

/// How a page's permission prompt is answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    /// Refused: the prompt is answered `DENY`.
    Deny,
    /// Granted: a permission prompt is answered `ACCEPT`. Only ever a prompt that asks
    /// for local network access alone.
    Accept,
}

/// CEF's permission-prompt bits for local network access
/// (`cef_permission_request_types_t`): `LOCAL_NETWORK_ACCESS_DEPRECATED`,
/// `LOCAL_NETWORK` and `LOOPBACK_NETWORK`. The helper checks these against CEF's own
/// values when it compiles.
pub(crate) const LOCAL_NETWORK_BITS: u32 = 0x0200_0000 | 0x0400_0000 | 0x0800_0000;

/// The answer to a page asking for `requested`, CEF's bit mask of permissions in a
/// prompt. `local_allowed` is whether the page's request policy lets it make local
/// requests. Granted only when the prompt asks for local network access and nothing
/// else, and the page may; every other prompt is refused, whatever it names.
pub(crate) fn answer_prompt(requested: u32, local_allowed: bool) -> Answer {
    let only_local = requested != 0 && requested & !LOCAL_NETWORK_BITS == 0;
    if only_local && local_allowed {
        Answer::Accept
    } else {
        Answer::Deny
    }
}

/// How a page's media access request (camera, microphone, screen capture) is answered:
/// never granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MediaAnswer {
    /// Refused: the request is cancelled.
    Deny,
}

/// The answer to a page asking for media access (`requested`, CEF's media bit mask):
/// always [`MediaAnswer::Deny`], whatever it names.
pub(crate) fn answer_media(requested: u32) -> MediaAnswer {
    let _ = requested;
    MediaAnswer::Deny
}

#[cfg(test)]
mod tests {
    use super::{Answer, LOCAL_NETWORK_BITS, MediaAnswer, answer_media, answer_prompt};

    fn masks() -> impl Iterator<Item = u32> {
        (0..u32::BITS).map(|bit| 1u32 << bit).chain([0, u32::MAX])
    }

    /// #347: every media request and every prompt is refused, each bit alone, none, and
    /// all at once, including bits CEF hasn't defined yet, for a page that may not make
    /// local requests.
    #[test]
    fn every_permission_is_refused() {
        for requested in masks() {
            assert_eq!(answer_media(requested), MediaAnswer::Deny, "{requested:#x}");
            assert_eq!(
                answer_prompt(requested, false),
                Answer::Deny,
                "{requested:#x}"
            );
        }
    }

    /// #347: a page that may make local requests gets local network access, and only
    /// that: a prompt that also asks for anything else is refused, and so is media.
    #[test]
    fn only_local_network_access_is_granted_and_only_when_allowed() {
        for requested in masks() {
            let local_only = requested != 0 && requested & !LOCAL_NETWORK_BITS == 0;
            let expected = if local_only {
                Answer::Accept
            } else {
                Answer::Deny
            };
            assert_eq!(answer_prompt(requested, true), expected, "{requested:#x}");
            assert_eq!(answer_media(requested), MediaAnswer::Deny, "{requested:#x}");
        }
        assert_eq!(answer_prompt(LOCAL_NETWORK_BITS, true), Answer::Accept);
        assert_eq!(answer_prompt(LOCAL_NETWORK_BITS, false), Answer::Deny);
        assert_eq!(
            answer_prompt(LOCAL_NETWORK_BITS | 1 << 12, true),
            Answer::Deny
        );
    }
}
