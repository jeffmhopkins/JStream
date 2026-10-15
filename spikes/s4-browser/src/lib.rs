//! Spike S4: CEF browser source on Windows.
//!
//! CEF off-screen rendering with accelerated paint -> the shared D3D11 texture opened on
//! wgpu's D3D12 device -> copied out of CEF's pool and composited over a background, with
//! no CPU copy of frame data; plus CEF's per-browser audio handler. Throwaway code that
//! answers ADR-0021's "prove it first"; see `README.md` next to `Cargo.toml`.
//!
//! This library holds the platform-independent parts (options, the test page, the checks
//! and the report format), so they are unit-tested on every platform. The Windows run is
//! in the `s4-browser` binary.
//!
//! Docs: docs/adr/0021-browser-source.md, docs/adr/0004-wgpu-with-native-interop.md

pub mod analysis;
pub mod cli;
pub mod json;
pub mod page;
pub mod stats;
