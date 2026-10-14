//! Windows platform backends: the implementations of the `jstream-platform` traits on
//! Windows. It has the QPC media clock (`QpcClock`), WASAPI audio I/O (`WasapiAudio`),
//! Media Foundation codecs (`MfCodecs`) and GPU interop (`WindowsGpu`);
//! Windows.Graphics.Capture and video decoders follow. Layer 2, and one of the
//! few crates allowed `unsafe` (R-UNSAFE-001): every OS call is wrapped here behind the
//! safe trait APIs.
//!
//! The backends exist only on Windows. On other platforms the crate builds empty except
//! for its platform-independent helpers, which are tested everywhere.
//!
//! Docs: crates/jstream-platform-windows/README.md#purpose, docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#gpu-pipeline, docs/adr/0004-wgpu-with-native-interop.md, docs/adr/0003-media-stack-and-codecs.md, docs/adr/0014-media-clock.md

mod audio;
mod clock;
mod codec;
#[cfg(windows)]
mod com;
#[cfg(windows)]
mod gpu;

#[cfg(windows)]
pub use audio::WasapiAudio;
#[cfg(windows)]
pub use clock::QpcClock;
pub use clock::timebase;
#[cfg(windows)]
pub use codec::MfCodecs;
#[cfg(windows)]
pub use gpu::WindowsGpu;
