//! The prototype's stroke icons (`Panel.dc.html`'s `ICONS`, with its `EYE_ON`, `EYE_OFF`
//! and `OUT_ICON` and the search box's and filter button's): 24 × 24, one path each,
//! drawn with a 1.8 stroke and round caps and joins by the `icon` class.
//!
//! Docs: docs/UI-FEATURES.md#theme

use leptos::prelude::*;

/// An icon's path, by the prototype's name for it. An unknown name draws the image icon.
pub fn path(name: &str) -> &'static str {
    match name {
        "monitor" => "M3 5h18v11H3z M8 20h8 M12 16v4",
        "window" => "M3 5h18v14H3z M3 9h18 M6 7h.01 M9 7h.01",
        "webcam" => {
            "M12 3a6 6 0 1 0 0 12a6 6 0 1 0 0-12z M12 7a2 2 0 1 0 0 4a2 2 0 1 0 0-4z M7 21h10 M12 15v6"
        }
        "mic" => {
            "M12 3a3 3 0 0 0-3 3v5a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3z M5 11a7 7 0 0 0 14 0 M12 18v3 M8 21h8"
        }
        "speaker" => "M4 9h4l5-4v14l-5-4H4z M17 9a4 4 0 0 1 0 6 M19.5 6.5a8 8 0 0 1 0 11",
        "globe" => {
            "M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18z M3 12h18 M12 3c3 3.5 3 14.5 0 18 M12 3c-3 3.5-3 14.5 0 18"
        }
        "text" => "M5 7V4h14v3 M12 4v16 M9 20h6",
        "film" => "M3 5h18v14H3z M7 5v14 M17 5v14 M3 9h4 M3 15h4 M17 9h4 M17 15h4",
        "palette" => {
            "M12 3a9 9 0 1 0 0 18c1.5 0 2-1 2-2s-1-1.5-1-2.5 1-1.5 2-1.5h2a4 4 0 0 0 4-4c0-4.5-4-8-9-8z M7.5 11h.01 M10 7.5h.01 M14.5 7.5h.01"
        }
        "music" => {
            "M9 18V5l11-2v13 M9 18a3 3 0 1 1-6 0a3 3 0 1 1 6 0z M20 16a3 3 0 1 1-6 0a3 3 0 1 1 6 0z"
        }
        "layers" => "M12 3l9 5-9 5-9-5z M3 13l9 5 9-5",
        "spout" => "M3 6h12v12H3z M15 10l6-3v10l-6-3",
        "wave" => "M2 12h2l2-6 3 12 3-14 3 16 3-10 2 4h2",
        "bars" => "M4 20V12 M8 20V6 M12 20V9 M16 20V4 M20 20V14",
        "chat" => "M4 5h16v11H9l-5 4z",
        "tag" => "M3 3h8l10 10-8 8L3 11z M7.5 7.5h.01",
        "keyframe" => "M12 3l7 9-7 9-7-9z",
        "folder" => "M3 6h6l2 2h10v11H3z",
        "play" => "M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18z M10 8.5v7l6-3.5z",
        "coffee" => {
            "M4 9h12v5a5 5 0 0 1-5 5H9a5 5 0 0 1-5-5z M16 11h2a2 2 0 0 1 0 4h-2 M8 3v3 M12 3v3"
        }
        "gamepad" => {
            "M6 8h12a4 4 0 0 1 4 4v3a3 3 0 0 1-5.5 1.6L15 15H9l-1.5 1.6A3 3 0 0 1 2 15v-3a4 4 0 0 1 4-4z M7 10.5v3 M5.5 12h3 M16 11.5h.01 M18 13h.01"
        }
        "flag" => "M5 21V4 M5 4h11l-2 4 2 4H5",
        "clock" => "M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18z M12 7v5l3 2",
        "user" => "M12 4a4 4 0 1 0 0 8a4 4 0 1 0 0-8z M4 21a8 8 0 0 1 16 0",
        "hash" => "M5 9h15 M4 15h15 M10 3L8 21 M16 3l-2 18",
        "star" => "M12 3l2.8 5.8 6.2.9-4.5 4.4 1.1 6.2L12 17.4 6.4 20.3l1.1-6.2L3 9.7l6.2-.9z",
        "heart" => {
            "M12 20s-7-4.4-9-8.5C1.7 8.4 3.6 5 7 5c2 0 3.5 1.2 5 3 1.5-1.8 3-3 5-3 3.4 0 5.3 3.4 4 6.5-2 4.1-9 8.5-9 8.5z"
        }
        "bolt" => "M13 3L5 14h6l-1 7 8-11h-6z",
        "sparkle" => {
            "M12 3v4 M12 17v4 M3 12h4 M17 12h4 M6 6l2.5 2.5 M15.5 15.5L18 18 M6 18l2.5-2.5 M15.5 8.5L18 6"
        }
        "bell" => "M6 16v-5a6 6 0 0 1 12 0v5l2 2H4z M10 21h4",
        "eye" => "M2 12s4-7 10-7 10 7 10 7-4 7-10 7-10-7-10-7z M12 9a3 3 0 1 0 0 6a3 3 0 1 0 0-6z",
        "eye-off" => {
            "M3 3l18 18 M10.6 6.1A9.9 9.9 0 0 1 12 6c6 0 10 6 10 6a17 17 0 0 1-3.2 3.8 M6.6 6.6A17 17 0 0 0 2 12s4 7 10 7a9.6 9.6 0 0 0 4.4-1.1 M9.9 9.9a3 3 0 0 0 4.2 4.2"
        }
        "lock" => "M5 11h14v10H5z M8 11V7a4 4 0 0 1 8 0v4",
        "wrench" => {
            "M14.5 4a4.5 4.5 0 0 0-4.3 5.8L4 16l4 4 6.2-6.2A4.5 4.5 0 0 0 20 9.5l-2.7 2.7-2.8-.7-.7-2.8L16.5 6A4.5 4.5 0 0 0 14.5 4z"
        }
        "out" => "M2 8a10 10 0 0 1 20 0 M5 11a7 7 0 0 1 14 0 M8.5 14a3.5 3.5 0 0 1 7 0 M12 18h.01",
        "search" => "M11 4a7 7 0 1 0 0 14a7 7 0 1 0 0-14z M20 20l-4-4",
        "funnel" => "M4 5h16l-6 7v6l-4 2v-8z",
        "close" => "M6 6l12 12 M18 6L6 18",
        "reset" => "M4 12a8 8 0 1 0 2.3-5.6 M4 4v5h5",
        _ => "M3 5h18v14H3z M3 16l5-5 4 4 3-3 6 6 M15.5 7.5a1.5 1.5 0 1 0 0.01 0z",
    }
}

/// Every icon's name.
const NAMES: &[&str] = &[
    "monitor", "window", "webcam", "mic", "speaker", "globe", "text", "film", "palette", "music",
    "layers", "spout", "wave", "bars", "chat", "tag", "keyframe", "folder", "play", "coffee",
    "gamepad", "flag", "clock", "user", "hash", "star", "heart", "bolt", "sparkle", "bell", "eye",
    "eye-off", "lock", "wrench", "out", "search", "funnel", "close", "reset", "image",
];

/// The icon of this name as a `'static` name, for a name read at run time (a scene's
/// icon); one this set doesn't have is `fallback`.
pub fn named(name: &str, fallback: &'static str) -> &'static str {
    NAMES
        .iter()
        .copied()
        .find(|n| *n == name)
        .unwrap_or(fallback)
}

/// An icon, `size` pixels square.
#[component]
pub fn Icon(
    /// The prototype's name for it.
    name: &'static str,
    /// Its width and height in CSS pixels.
    #[prop(default = 16)]
    size: u32,
) -> impl IntoView {
    view! {
        <svg class="icon" aria-hidden="true" viewBox="0 0 24 24" width=size height=size>
            <path d=path(name) />
        </svg>
    }
}

#[cfg(test)]
mod tests {
    use super::{NAMES, named, path};

    #[test]
    fn every_name_has_its_own_path() {
        let image = path("image");
        for name in NAMES.iter().filter(|n| **n != "image") {
            assert_ne!(path(name), image, "{name} falls back to the image icon");
        }
        assert_eq!(named("coffee", "layers"), "coffee");
        assert_eq!(named("nope", "layers"), "layers");
    }
}
