//! The shell's panels: the scene list and the sources list on the left, the inspector's
//! pages (properties, then outputs) on the right, and the audio mixer along the bottom.
//! They read the shared state from [`Studio`](crate::store::Studio) context and call the
//! engine through the open session from [`SessionContext`](crate::app::SessionContext).
//!
//! Docs: crates/jstream-ui/README.md#design

mod confirm;
pub mod outputs;
mod properties;
mod scenes;
mod sources;

pub use outputs::Outputs;
pub use properties::Properties;
pub use scenes::Scenes;
pub use sources::Sources;

use leptos::prelude::*;

/// The audio mixer, with meters.
#[component]
pub fn Mixer() -> impl IntoView {
    view! { <h2>"Mixer"</h2><p class="placeholder">"Not built yet."</p> }
}
