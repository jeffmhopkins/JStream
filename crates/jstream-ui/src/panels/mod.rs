//! The shell's panels: the scene list and the sources list on the left, the inspector's
//! pages (properties, then outputs) on the right, and the audio mixer (`mixer.rs`) along
//! the bottom. They read the shared state from [`Studio`](crate::store::Studio) context
//! and call the engine through the open session from
//! [`SessionContext`](crate::app::SessionContext); the mixer reads its own state.
//!
//! Docs: crates/jstream-ui/README.md#design

mod confirm;
mod inspector;
pub mod mixer;
pub mod number;
pub mod outputs;
mod properties;
mod scenes;
mod sources;
pub mod studio;
pub mod transform;

pub use inspector::Inspector;
pub use mixer::Mixer;
pub use outputs::Outputs;
pub use properties::Properties;
pub use scenes::Scenes;
pub use sources::Sources;
pub use studio::StudioMode;
pub use transform::Transform;
