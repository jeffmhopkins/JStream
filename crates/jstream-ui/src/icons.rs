//! The prototype's stroke icons (`Panel.dc.html`'s `ICONS`): 24 × 24, one path each,
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
        _ => "M3 5h18v14H3z M3 16l5-5 4 4 3-3 6 6 M15.5 7.5a1.5 1.5 0 1 0 0.01 0z",
    }
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
