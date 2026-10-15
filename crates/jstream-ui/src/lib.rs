//! The JStream web UI: a Leptos app, rendered client-side and compiled to `wasm32`, that
//! drives the engine through `jstream-control`'s WebSocket client (ADR-0045). The daemon
//! serves it over HTTP and the desktop app shows it in a Tauri webview. Like every client
//! it never links the engine (R-ARCH-003).
//!
//! This is the skeleton: connecting, the session, and the shell's regions. The panels
//! (`panels`) and the preview canvas (`canvas`) fill them in.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-ui/README.md#purpose, crates/jstream-ui/README.md#design, crates/jstream-ui/README.md#build, docs/adr/0045-leptos-web-ui.md

#![forbid(unsafe_code)]

pub mod app;
pub mod canvas;
pub mod host;
pub mod panels;
pub mod session;

pub use app::{App, SessionContext};

/// Mounts the app on the page's `<body>`. The `wasm32` binary's entry point.
pub fn start() {
    leptos::mount::mount_to_body(App);
}
