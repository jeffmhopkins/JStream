//! The preview canvas: the program preview decoded into a `<canvas>`, with selection
//! and transform handles. A placeholder until the preview stream exists.
//!
//! Docs: crates/jstream-ui/README.md#design

use leptos::prelude::*;

/// The preview canvas.
#[component]
pub fn Canvas() -> impl IntoView {
    view! { <div class="preview"><p class="placeholder">"Preview not built yet."</p></div> }
}
