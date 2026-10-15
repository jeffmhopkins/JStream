//! CEF's subprocess exe (ADR-0046): Chromium's renderer, GPU, utility and other
//! processes for browser sources start as `jstream-browser-helper.exe`, which CEF finds
//! through its `browser_subprocess_path`, so the engine's exe never runs as one. It
//! does nothing but hand its process to CEF.
//!
//! Docs: docs/ARCHITECTURE.md#shape, docs/ARCHITECTURE.md#crate-map, apps/jstream-browser-helper/README.md#purpose, docs/adr/0046-browser-crate-and-helper.md

#![forbid(unsafe_code)]
// A GUI-subsystem exe, so no console window opens for each of CEF's processes.
#![cfg_attr(all(windows, feature = "cef"), windows_subsystem = "windows")]

#[cfg(all(windows, feature = "cef"))]
fn main() {
    std::process::exit(jstream_browser::run_subprocess());
}

#[cfg(not(all(windows, feature = "cef")))]
fn main() {
    // Nothing starts this exe but CEF, which only exists in a build with the feature.
    std::process::exit(1);
}
