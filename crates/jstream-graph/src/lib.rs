//! The JStream scene graph: the canvas, sources, scenes and the items that place sources
//! in scenes, and the outputs' configuration.
//! [`Graph::apply`] applies a batch of protocol edits atomically, increments the revision
//! by one, and returns the inverse batch that undoes it. Layer 2: plain data, no GPU, no
//! I/O.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#data-model, crates/jstream-graph/README.md#purpose, docs/adr/0013-command-model.md, docs/adr/0022-item-transform-and-canvas.md

#![forbid(unsafe_code)]

mod apply;
mod deps;
mod model;
mod plan;
mod read;
mod target;

pub use apply::Applied;
pub use model::{Graph, Item, MAX_CANVAS_EDGE, Output, Placement, Scene, Source, State};

#[cfg(test)]
mod tests;
