//! One-scene GPU compositor. It owns a wgpu device (or borrows the platform's), draws a
//! stack of images in linear light, and can read the result back as straight sRGB RGBA8
//! or convert it to NV12 for the encoders.
//! Layer 2: no `unsafe`. The scene graph is the same layer, so the engine builds the
//! [`Scene`] snapshot and calls in.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#gpu-pipeline, crates/jstream-render/README.md#purpose, crates/jstream-render/README.md#design, crates/jstream-render/README.md#filters, crates/jstream-render/README.md#testing, docs/adr/0004-wgpu-with-native-interop.md, docs/adr/0107-3d-models.md

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
mod ingest;
mod mips;
mod model;
mod place;
mod readback;
mod scene;
mod timer;
pub mod transition;

pub use cache::{Reclaimed, ResourceCache};
pub use compositor::Compositor;
pub use convert::{Nv12Converter, Nv12Pass, Nv12Planes, Nv12Target};
pub use error::{NO_ADAPTER_HINT, RenderError};
pub use filter::{
    ExternalFilter, ExternalProgram, FIRST_INPUT_BINDING, FilterClock, Lut, LutError,
    MAX_PROGRAM_BYTES, PRELUDE as FILTER_PRELUDE, PROGRAM_AUDIO_BANDS, PROGRAM_MAX_INPUTS,
    PROGRAM_MAX_PASSES, PROGRAM_PARAMS_BINDING, PROGRAM_PRELUDE, ProgramAudio, ProgramImage,
    ProgramInput, ProgramPass, VideoFilter, descriptors as video_filter_descriptors, lut_image,
    outset as video_filter_outset,
};
pub use gpu::{OpenOptions, adapter_available, context_lost};
pub use image::Image;
pub use jstream_protocol::BlendMode;
pub use model::{GpuModel, Lighting, ModelDraw};
pub use readback::{Copier, READBACK_DEPTH, ReadbackRing};
pub use scene::{Color, Crop, Item, LegacyTransform, Output, Placement, Scene, Snapshot};

#[cfg(test)]
mod tests;
