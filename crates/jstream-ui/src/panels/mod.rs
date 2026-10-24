//! The shell's panels: the scene list and the sources list on the left, the inspector's
//! pages (properties, then outputs) on the right, and the audio mixer (`mixer.rs`) along
//! the bottom. The Add source and Properties dialogs open over them. They read the shared state from [`Studio`](crate::store::Studio) context
//! and call the engine through the open session from
//! [`SessionContext`](crate::app::SessionContext); the mixer reads its own state.
//!
//! Docs: crates/jstream-ui/README.md#design

pub mod add_source;
mod confirm;
pub mod mixer;
pub mod outputs;
pub mod properties;
mod scenes;
pub mod source_properties;
mod sources;
pub mod studio;

pub use add_source::AddSource;
pub use mixer::Mixer;
pub use outputs::Outputs;
pub use properties::Properties;
pub use scenes::Scenes;
pub use sources::Sources;
pub use studio::StudioMode;
