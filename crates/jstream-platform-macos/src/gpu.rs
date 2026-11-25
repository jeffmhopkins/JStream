//! GPU frames on macOS (ADR-0063 §8, `jstream-platform` README › GPU frames): the
//! engine's wgpu device on Metal, IOSurface-backed pixel buffers wrapped as Metal
//! textures with no copy, and the `MTLSharedEvent` that tells the platform when the GPU
//! is done with one.
//!
//! `retire` holds the part that makes no system call, so it is tested on every platform;
//! `metal` is the interop itself.

#[cfg(target_os = "macos")]
mod metal;
#[cfg(any(target_os = "macos", test))]
mod retire;

#[cfg(target_os = "macos")]
pub use metal::MetalGpu;
#[cfg(target_os = "macos")]
pub(crate) use metal::{EncodeInput, SurfaceGpu};
