//! One-scene GPU compositor. It owns a wgpu device (or borrows the platform's), draws a
//! stack of images in linear light, and can read the result back as straight sRGB RGBA8.
//! Layer 2: no `unsafe`. The scene graph is the same layer, so the engine builds the
//! [`Scene`] snapshot and calls in.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#gpu-pipeline, crates/jstream-render/README.md#purpose, crates/jstream-render/README.md#design, crates/jstream-render/README.md#testing, docs/adr/0004-wgpu-with-native-interop.md

#![forbid(unsafe_code)]

mod cache;
mod color;
mod compositor;
mod error;
mod gpu;
mod image;
mod place;
mod scene;

pub use cache::{Reclaimed, ResourceCache};
pub use compositor::Compositor;
pub use error::RenderError;
pub use gpu::OpenOptions;
pub use image::Image;
pub use scene::{Color, Crop, Item, LegacyTransform, Output, Placement, Scene, Snapshot};

#[cfg(test)]
mod tests;
