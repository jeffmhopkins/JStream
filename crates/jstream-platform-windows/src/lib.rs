//! Windows platform backends: the implementations of the `jstream-platform` traits on
//! Windows. This first cut has the QPC media clock (`QpcClock`) and WASAPI audio I/O
//! (`WasapiAudio`); Windows.Graphics.Capture, Media Foundation codecs and GPU interop
//! follow. Layer 2, and one of the few crates allowed `unsafe` (R-UNSAFE-001): every
//! OS call is wrapped here behind the safe trait APIs.
//!
//! The backends exist only on Windows. On other platforms the crate builds empty except
//! for its platform-independent helpers, which are tested everywhere.
//!
//! Docs: crates/jstream-platform-windows/README.md#purpose, docs/ARCHITECTURE.md#crate-map, docs/adr/0014-media-clock.md

mod audio;
mod clock;
#[cfg(windows)]
mod com;

#[cfg(windows)]
pub use audio::WasapiAudio;
#[cfg(windows)]
pub use clock::QpcClock;
pub use clock::timebase;
