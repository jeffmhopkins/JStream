//! The browser helper on Linux (ADR-0095): `jstream-browser-helper`, a plain executable
//! with `libcef.so` and CEF's runtime files beside it. The engine's CEF backend starts it;
//! it is CEF's browser process for the engine's pages, and every renderer, GPU and
//! utility process starts as the same executable, sandboxed by Chromium itself. All it
//! does is hand the process to `jstream_browser::run_helper`.
//!
//! On macOS (ADR-0157) it is the same executable in `JStream.app/Contents/MacOS`, with
//! CEF's framework in `Contents/Frameworks` and copies of itself as the helper apps
//! there, which CEF's subprocesses run from, each in CEF's sandbox.
//!
//! On Windows the helper is the DLL CEF's bootstrap exe loads (`lib.rs`), and this
//! executable, built only with the `cef` feature, does nothing.
//!
//! Docs: apps/jstream-browser-helper/README.md#purpose, docs/adr/0095-browser-helper-on-linux.md, docs/adr/0157-browser-sources-on-macos.md

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn main() {
    std::process::exit(jstream_browser::run_helper());
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn main() {}
