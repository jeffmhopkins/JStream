//! Display and window capture through Windows.Graphics.Capture: the
//! [`Capture`](jstream_platform::Capture) backend.
//!
//! This is the CPU path (ARCH › GPU pipeline, step 6): each frame is copied into a
//! staging texture, read back and converted to NV12 (`convert`), then published to the
//! stream's single-slot mailbox (`mailbox`). Capture into shared textures, the GPU path,
//! waits for the decision on how capture reaches the GPU context (README › Open
//! questions). Targets are named as `id` describes.

// Platform-independent, so they are tested on every platform.
#[cfg(any(windows, test))]
mod convert;
#[cfg(any(windows, test))]
mod id;
#[cfg(any(windows, test))]
mod mailbox;
#[cfg(windows)]
mod targets;
#[cfg(windows)]
mod wgc;

#[cfg(windows)]
pub use wgc::WgcCapture;
