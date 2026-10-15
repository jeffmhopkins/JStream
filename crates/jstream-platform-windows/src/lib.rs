//! Windows platform backends: the implementations of the `jstream-platform` traits on
//! Windows. It has the QPC media clock (`QpcClock`), WASAPI audio I/O (`WasapiAudio`),
//! Windows.Graphics.Capture display and window capture (`WgcCapture`), Media Foundation
//! codecs (`MfCodecs`), GPU interop (`WindowsGpu`) and the Credential Manager store for
//! stream keys (`WindowsCredentials`); video decoders follow. Layer 2,
//! and one of the few crates allowed `unsafe` (R-UNSAFE-001): every OS call is wrapped
//! here behind safe APIs.
//!
//! The backends exist only on Windows. On other platforms the crate builds empty except
//! for its platform-independent helpers, which are tested everywhere.
//!
//! Docs: crates/jstream-platform-windows/README.md#purpose, docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#gpu-pipeline, docs/adr/0004-wgpu-with-native-interop.md, docs/adr/0031-source-types-and-decoding-scope.md, docs/adr/0014-media-clock.md

mod audio;
mod capture;
mod clock;
mod codec;
#[cfg(windows)]
mod com;
mod credential;
#[cfg(windows)]
mod gpu;

#[cfg(windows)]
pub use audio::WasapiAudio;
#[cfg(windows)]
pub use capture::WgcCapture;
#[cfg(windows)]
pub use clock::QpcClock;
pub use clock::timebase;
#[cfg(windows)]
pub use codec::MfCodecs;
#[cfg(windows)]
pub use credential::WindowsCredentials;
pub use credential::{CredentialTarget, Secret};
#[cfg(windows)]
pub use gpu::WindowsGpu;
