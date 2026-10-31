//! One-scene GPU compositor. It owns a wgpu device (or borrows the platform's), draws a
//! stack of images in linear light, and can read the result back as straight sRGB RGBA8
//! or convert it to NV12 for the encoders.
//! Layer 2: no `unsafe`. The scene graph is the same layer, so the engine builds the
//! [`Scene`] snapshot and calls in.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#gpu-pipeline, crates/jstream-render/README.md#purpose, crates/jstream-render/README.md#design, crates/jstream-render/README.md#filters, crates/jstream-render/README.md#testing, docs/adr/0004-wgpu-with-native-interop.md

#![forbid(unsafe_code)]

mod blend;
mod cache;
mod color;
mod compositor;
mod convert;
mod error;
mod filter;
mod gpu;
mod image;
mod mips;
mod place;
mod scene;
pub mod transition;

pub use cache::{Reclaimed, ResourceCache};
pub use compositor::Compositor;
pub use convert::{Nv12Converter, Nv12Pass, Nv12Planes, Nv12Target};
pub use error::RenderError;
pub use filter::{
    ExternalFilter, FilterClock, Lut, LutError, PRELUDE as FILTER_PRELUDE, VideoFilter,
    descriptors as video_filter_descriptors, lut_image,
};
pub use gpu::OpenOptions;
pub use image::Image;
pub use jstream_protocol::BlendMode;
pub use scene::{Color, Crop, Item, LegacyTransform, Output, Placement, Scene, Snapshot};

#[cfg(test)]
mod tests;
