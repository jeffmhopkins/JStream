//! The browser helper on Linux (ADR-0095): `jstream-browser-helper`, a plain executable
//! with `libcef.so` and CEF's runtime files beside it. The engine's CEF backend starts it;
//! it is CEF's browser process for the engine's pages, and every renderer, GPU and
//! utility process starts as the same executable, sandboxed by Chromium itself. All it
//! does is hand the process to `jstream_browser::run_helper`.
//!
//! On Windows the helper is the DLL CEF's bootstrap exe loads (`lib.rs`), and this
//! executable, built only with the `cef` feature, does nothing.
//!
//! Docs: apps/jstream-browser-helper/README.md#purpose, docs/adr/0095-browser-helper-on-linux.md

#[cfg(target_os = "linux")]
fn main() {
    std::process::exit(jstream_browser::run_helper());
}

#[cfg(not(target_os = "linux"))]
fn main() {}
