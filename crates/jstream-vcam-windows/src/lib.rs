//! The virtual camera's media source: a COM DLL (`jstream_vcam_windows.dll`) that the
//! Windows camera Frame Server loads, which serves the frames the engine publishes through
//! the [`jstream_vcam_proto`] contract. Layer 2, and one of the crates allowed `unsafe`
//! (R-UNSAFE-001). It depends only on layer-0 crates and Microsoft's `windows` bindings
//! (ADR-0020).
//!
//! Ported from spike S2, a Rust port of VCamSample's design: the registered class is an
//! `IMFActivate` whose `ActivateObject` creates an `IMFMediaSourceEx` with one
//! `IMFMediaStream2`. Each sample is copied from the producer's newest completed ring
//! texture, or is the DLL's own colour-bar pattern when no producer is live. Every COM
//! method and export runs inside a panic guard (R-UNSAFE-005).
//!
//! The COM objects exist only on Windows. On other platforms the crate builds only its
//! platform-independent pixel helpers, which are tested everywhere.
//!
//! Docs: crates/jstream-vcam-windows/README.md#purpose, crates/jstream-vcam-windows/README.md#hardware-check, docs/ARCHITECTURE.md#crate-map, docs/adr/0020-portable-windows-distribution.md

pub mod pixels;

#[cfg(windows)]
mod win;
