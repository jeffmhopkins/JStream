//! Spike S2: a Windows virtual camera whose media source is a Rust COM DLL.
//!
//! Built as `s2_vcam.dll` (loaded by the Windows camera Frame Server) and as an rlib the
//! producer exe (`s2-vcam.exe`) links, so both share one definition of the control block,
//! the object names, the security descriptor and the registry layout. Throwaway code that
//! answers ROADMAP M0 / S2; see `README.md` next to `Cargo.toml`.
//!
//! The media source is a Rust port of the design of VCamSample (smourier/VCamSample): an
//! `IMFActivate` whose `ActivateObject` creates an `IMFMediaSourceEx` with one
//! `IMFMediaStream2`. Frames come cross-process from the producer through shared NT-handle
//! textures and a shared fence (see [`proto`]); with no live producer the stream serves a
//! test pattern.
//!
//! Docs: docs/adr/0020-portable-windows-distribution.md

#![warn(clippy::undocumented_unsafe_blocks)]

pub mod evidence;
pub mod pixels;
pub mod proto;

#[cfg(windows)]
pub mod win;
