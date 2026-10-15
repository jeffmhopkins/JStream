//! The JStream scene graph: the canvas, sources, scenes and the items that place sources
//! in scenes, and the outputs' configuration with the encoder profiles they use.
//! [`Graph::apply`] applies a batch of protocol edits atomically, increments the revision
//! by one, and returns the inverse batch that undoes it. [`Graph::to_collection_json`] and
//! [`Graph::with_collection_json`] save and load the collection as versioned JSON. Layer 2:
//! plain data, no GPU, no I/O.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#data-model, docs/ARCHITECTURE.md#persistence, crates/jstream-graph/README.md#purpose, docs/adr/0013-command-model.md, docs/adr/0022-item-transform-and-canvas.md, docs/adr/0026-source-filter-chains.md

#![forbid(unsafe_code)]

mod apply;
mod deps;
mod encoder;
mod filter;
mod model;
mod persist;
mod plan;
mod read;
mod target;
mod touched;

pub use apply::Applied;
pub use model::{
    Chains, Effect, Encoder, Filter, Graph, Item, MAX_CANVAS_EDGE, MAX_CHAIN_FILTERS, Output,
    Placement, Scene, Source, State,
};
pub use persist::{LoadError, SCHEMA_VERSION};
pub use touched::Touched;

#[cfg(test)]
mod tests;
