//! WGSL effect packs (ADR-0025 › Effect packs, ADR-0035): third-party video filters as
//! data. A pack is a folder with an `effect.toml` manifest and the WGSL files it names.
//! Each effect becomes a `video_filter` kind with the ID `<pack>/<effect>`, whose
//! parameters are declared in the manifest in descriptor form and reach the shader as
//! one uniform struct.
//!
//! - [`Pack::load`] parses and checks the manifest, then validates each effect's WGSL
//!   with naga against the host's prelude and the effect contract ([`shader`]).
//! - [`Effect`] holds the kind's descriptor, the WGSL the host compiles, the uniform
//!   [`Layout`], and turns settings into uniform bytes and migrates old settings.
//! - [`ShaderSource`] is a pack's `[[source]]` table (ADR-0093): a `source` kind whose
//!   picture is a program of passes, drawn through render's `ExternalProgram`.
//! - [`PackWatcher`] hot-reloads a folder of packs by polling, and reports kind changes.
//!
//! This crate opens no GPU device. Drawing an effect is the render filter harness's job.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-effects/README.md#purpose, docs/adr/0025-typed-settings-kind-descriptors.md

#![forbid(unsafe_code)]

mod error;
mod manifest;
mod pack;
pub mod shader;
mod source;
mod watch;

pub use error::PackError;
pub use manifest::{MAX_EFFECTS, MAX_ID_LEN, MAX_IMAGES, MAX_PARAMS, SOURCE_SIDE};
pub use pack::{Effect, MANIFEST, MAX_MANIFEST_BYTES, MAX_SHADER_BYTES, Pack};
pub use shader::{FieldType, Layout, MAX_VALUE_BYTES};
pub use source::{AUDIO_BANDS, MAX_IMAGE_BYTES, MAX_IMAGE_SIDE, ShaderSource};
pub use watch::{Changes, DEFAULT_POLL_INTERVAL, KindChange, LOAD_THREAD, PackWatcher};

#[cfg(test)]
mod source_tests;
#[cfg(test)]
mod tests;
