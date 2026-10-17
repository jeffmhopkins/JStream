//! Linux platform backends: the implementations of the `jstream-platform` traits on Linux,
//! as ADR-0060 lays them out. The first part: it has the `CLOCK_MONOTONIC` media
//! clock (`MonotonicClock`), session and support-tier detection (`Session`), the
//! ScreenCast portal's capture targets (`PortalCapture`), run-time loading of libpipewire
//! (`PipeWire`, behind `PipeWireAudio`) and libva, and VA-API H.264 encoding from CPU
//! frames behind `VaapiCodecs`. Capture and audio sessions report `Unsupported` until
//! their follow-ups land. Layer 2, and one of the crates allowed `unsafe`
//! (R-UNSAFE-001): every system call is wrapped here behind safe APIs.
//!
//! System libraries are loaded with `dlopen` when first needed, never linked, so the
//! crate builds and tests on any machine without their headers. The backends exist only
//! on Linux. On other platforms the crate builds empty except for its
//! platform-independent helpers, which are tested everywhere.
//!
//! Docs: crates/jstream-platform-linux/README.md#purpose, docs/ARCHITECTURE.md#crate-map, docs/adr/0060-linux-platform-support.md, docs/adr/0031-source-types-and-decoding-scope.md, docs/adr/0014-media-clock.md

mod clock;
mod codec;
#[cfg(target_os = "linux")]
mod dl;
mod pipewire;
mod portal;
mod session;
mod vaapi;

#[cfg(target_os = "linux")]
pub use clock::MonotonicClock;
pub use clock::{hns_from_timespec, timebase};
#[cfg(target_os = "linux")]
pub use codec::VaapiCodecs;
pub use codec::choose_encoder;
pub use pipewire::{PIPEWIRE_FLOOR, pipewire_version_supported};
#[cfg(target_os = "linux")]
pub use pipewire::{PipeWire, PipeWireAudio};
#[cfg(target_os = "linux")]
pub use portal::PortalCapture;
pub use portal::{CursorModes, PersistMode, PortalPick, SourceTypes, handle_token, request_path};
pub use session::{Session, SessionKind, SupportTier};
#[cfg(target_os = "linux")]
pub use vaapi::probe_encoders;
pub use vaapi::{H264Profile, VaEncoder, VaEntrypoint};
