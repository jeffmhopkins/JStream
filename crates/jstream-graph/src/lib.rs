//! The JStream scene graph: sources, scenes and the items that place sources in scenes,
//! and the outputs' configuration with the encoder profiles they use.
//! [`Graph::apply`] applies a batch of protocol edits atomically, increments the revision
//! by one, and returns the inverse batch that undoes it. Layer 2: plain data, no GPU, no
//! I/O.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#data-model, crates/jstream-graph/README.md#purpose, docs/adr/0013-command-model.md

#![forbid(unsafe_code)]

mod apply;
mod deps;
mod encoder;
mod model;
mod read;
mod target;

pub use apply::Applied;
pub use model::{Encoder, Graph, Item, Output, Scene, Source, State};

#[cfg(test)]
mod tests;
