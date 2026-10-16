//! Display and window capture through Windows.Graphics.Capture: the
//! [`Capture`](jstream_platform::Capture) backend.
//!
//! This is the CPU path (ARCH › GPU pipeline, step 6): each frame is copied into a
//! staging texture, read back and converted to NV12 (`convert`), then published to the
//! stream's single-slot mailbox (`mailbox`). Capture into shared textures, the GPU path,
//! waits for the decision on how capture reaches the GPU context (README › Open
//! questions). Targets are named as `id` describes.

// Platform-independent, so they are tested on every platform.
// Shared with the H.264 encoder, which converts the compositor's RGBA the same way.
#[cfg(any(windows, test))]
pub(crate) mod convert;
#[cfg(any(windows, test))]
mod id;
// Shared with `video_device`, whose capture streams publish to the same mailbox.
#[cfg(any(windows, test))]
pub(crate) mod mailbox;
#[cfg(any(windows, test))]
pub(crate) mod ring;
#[cfg(windows)]
mod targets;
#[cfg(windows)]
mod wgc;

#[cfg(windows)]
pub use wgc::WgcCapture;
