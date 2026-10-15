//! The JStream scene graph: sources, scenes and the items that place sources in scenes,
//! and the outputs' configuration.
//! [`Graph::apply`] applies a batch of protocol edits atomically, increments the revision
//! by one, and returns the inverse batch that undoes it. [`Graph::to_collection_json`] and
//! [`Graph::with_collection_json`] save and load the collection as versioned JSON. Layer 2:
//! plain data, no GPU, no I/O.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#data-model, docs/ARCHITECTURE.md#persistence, crates/jstream-graph/README.md#purpose, docs/adr/0013-command-model.md

#![forbid(unsafe_code)]

mod apply;
mod deps;
mod model;
mod persist;
mod read;
mod target;

pub use apply::Applied;
pub use model::{Graph, Item, Output, Scene, Source, State};
pub use persist::{LoadError, SCHEMA_VERSION};

#[cfg(test)]
mod tests;
