//! The JStream scene graph: sources, scenes and the items that place sources in scenes.
//! [`Graph::apply`] applies a batch of protocol edits atomically, increments the revision
//! by one, and returns the inverse batch that undoes it. Layer 2: plain data, no GPU, no
//! I/O.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#data-model, crates/jstream-graph/README.md#purpose, docs/adr/0013-command-model.md

#![forbid(unsafe_code)]

mod apply;
mod deps;
mod model;
mod read;

pub use apply::Applied;
pub use model::{Graph, Item, Scene, Source, State};

#[cfg(test)]
mod tests;
