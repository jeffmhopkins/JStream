//! The shell's panels: the scenes and sources tree, the inspector (properties, outputs
//! and go-live) and the audio mixer (`mixer.rs`). Placeholders until each panel is
//! built; they read the open session from [`SessionContext`](crate::app::SessionContext)
//! context.
//!
//! Docs: crates/jstream-ui/README.md#design

use leptos::prelude::*;

pub mod mixer;
pub mod outputs;

pub use mixer::Mixer;
pub use outputs::Outputs;

/// The scenes and sources tree.
#[component]
pub fn SceneTree() -> impl IntoView {
    view! { <h2>"Scenes"</h2><p class="placeholder">"Not built yet."</p> }
}

/// The inspector: the selected item's or source's properties, and the outputs.
#[component]
pub fn Inspector() -> impl IntoView {
    view! { <h2>"Properties"</h2><p class="placeholder">"Not built yet."</p> }
}
