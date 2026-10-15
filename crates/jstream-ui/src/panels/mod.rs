//! The shell's panels: the scenes and sources tree, the inspector (properties, outputs
//! and go-live) and the audio mixer. Placeholders until each panel is built; they read
//! the open session from [`SessionContext`](crate::app::SessionContext) context.
//!
//! Docs: crates/jstream-ui/README.md#design

use leptos::prelude::*;

pub mod outputs;
pub mod studio;

pub use outputs::Outputs;
pub use studio::StudioMode;

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

/// The audio mixer, with meters.
#[component]
pub fn Mixer() -> impl IntoView {
    view! { <h2>"Mixer"</h2><p class="placeholder">"Not built yet."</p> }
}
