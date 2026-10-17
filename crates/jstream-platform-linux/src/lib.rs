//! Linux platform backends: the implementations of the `jstream-platform` traits on Linux,
//! as ADR-0060 lays them out. It has the `CLOCK_MONOTONIC` media clock
//! (`MonotonicClock`), session and support-tier detection (`Session`), display and window
//! capture through the ScreenCast portal and a PipeWire video stream (`PortalCapture`, on
//! the crate's own minimal D-Bus client), webcams and capture cards through V4L2
//! (`V4l2Devices`), run-time loading of libpipewire (`PipeWire`,
//! behind `PipeWireAudio`) and libva, VA-API H.264 encoding from CPU frames behind
//! `VaapiCodecs`, Cisco's OpenH264 binary as the software H.264 encoder, downloaded only
//! on the user's request and checked against its pin (`OpenH264Store`, `OpenH264Encoder`,
//! ADR-0061), and AAC encoding on JStream's own AAC-LC encoder (`RustAacEncoder`,
//! feature `aac-encoder`), which `VaapiCodecs` opens. Audio input and output go through
//! ALSA (`AlsaAudio`, libasound loaded at run time), the fallback ADR-0060 §4 names;
//! `PipeWireAudio`'s streams report `Unsupported` until they land. Layer 2, and one of the crates allowed `unsafe`
//! (R-UNSAFE-001): every system call is wrapped here behind safe APIs.
//!
//! System libraries are loaded with `dlopen` when first needed, never linked, so the
//! crate builds and tests on any machine without their headers. The backends exist only
//! on Linux. On other platforms the crate builds empty except for its
//! platform-independent helpers, which are tested everywhere.
//!
//! Docs: crates/jstream-platform-linux/README.md#purpose, docs/ARCHITECTURE.md#crate-map, docs/adr/0060-linux-platform-support.md, docs/adr/0061-source-types-and-decoding-scope-v2.md, docs/adr/0062-aac-lc-encoder-patent-check.md, docs/adr/0014-media-clock.md

#[cfg(feature = "aac-encoder")]
mod aac;
#[cfg(any(target_os = "linux", test))]
mod alsa;
mod clock;
mod codec;
#[cfg(any(target_os = "linux", test))]
mod dbus;
#[cfg(target_os = "linux")]
mod dl;
#[cfg(target_os = "linux")]
mod openh264;
mod pipewire;
mod portal;
mod session;
#[cfg(any(target_os = "linux", test))]
mod spa;
mod v4l2;
mod vaapi;

#[cfg(feature = "aac-encoder")]
pub use aac::{AAC_ENCODER_NAME, RustAacEncoder, aac_config, aac_encoder_info};
#[cfg(target_os = "linux")]
pub use alsa::AlsaAudio;
#[cfg(target_os = "linux")]
pub use clock::MonotonicClock;
pub use clock::{hns_from_timespec, timebase};
#[cfg(target_os = "linux")]
pub use codec::VaapiCodecs;
pub use codec::choose_encoder;
#[cfg(target_os = "linux")]
pub use openh264::{
    Fetch, HttpsFetch, MAX_DOWNLOAD_BYTES, OPENH264_COMPONENT, OPENH264_ENCODER_NAME,
    OPENH264_LICENSE_URL, OPENH264_NOTICE, OPENH264_PIN, OpenH264Encoder, OpenH264Pin,
    OpenH264Store, openh264_encoder_info, wants_software,
};
pub use pipewire::{PIPEWIRE_FLOOR, pipewire_version_supported};
#[cfg(target_os = "linux")]
pub use pipewire::{PipeWire, PipeWireAudio};
#[cfg(target_os = "linux")]
pub use portal::PortalCapture;
pub use portal::{CursorModes, PersistMode, PortalPick, SourceTypes, handle_token, request_path};
pub use session::{Session, SessionKind, SupportTier};
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub use v4l2::V4l2Devices;
#[cfg(target_os = "linux")]
pub use vaapi::probe_encoders;
pub use vaapi::{H264Profile, VaEncoder, VaEntrypoint};
