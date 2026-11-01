//! The shell's panels: the scene list and the sources list on the left, the inspector's
//! pages (properties, then outputs) on the right, and the audio mixer (`mixer.rs`) along
//! the bottom. The scene list and the sources list share their search, filter, star and
//! drag reordering (`tree.rs`). The Filters panel shows the selected source's filter chains
//! (`filters.rs`). The Agent activity panel lists what agents did (`agent.rs`). The Add source, Properties, Add filter and Filter settings dialogs open
//! over them. They read the
//! shared state from [`Studio`](crate::store::Studio) context and call the engine through the open session from
//! [`SessionContext`](crate::app::SessionContext); the mixer reads its own state.
//!
//! Docs: crates/jstream-ui/README.md#design

pub mod add_source;
pub mod agent;
mod confirm;
pub mod filters;
pub mod inspector;
pub mod labels;
pub mod mixer;
pub mod number;
pub mod outputs;
pub mod properties;
pub mod scene_properties;
mod scenes;
pub mod source_properties;
mod sources;
pub mod studio;
pub mod transform;
mod tree;

pub use add_source::AddSource;
pub use agent::AgentActivity;
pub use filters::Filters;
pub use inspector::Inspector;
pub use mixer::Mixer;
pub use outputs::Outputs;
pub use properties::Properties;
pub use scenes::Scenes;
pub use sources::Sources;
pub use studio::StudioMode;
pub use transform::Transform;
