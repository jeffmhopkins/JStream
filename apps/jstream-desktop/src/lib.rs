//! The JStream desktop app: a Tauri window shows the web UI (`jstream-ui`), and the
//! engine runs in the same process, served to the window over the authenticated
//! localhost WebSocket API, the one a remote client uses (ADR-0045, ADR-0020). Windows
//! first: the window (`shell`) needs WebView2. The rest builds and is tested everywhere.
//!
//! Docs: docs/ARCHITECTURE.md#shape, docs/ARCHITECTURE.md#crate-map, apps/jstream-desktop/README.md#purpose, apps/jstream-desktop/README.md#usage, apps/jstream-desktop/README.md#design, docs/adr/0045-leptos-web-ui.md, docs/adr/0020-portable-windows-distribution.md

#![forbid(unsafe_code)]

pub mod app;
pub mod bootstrap;
#[cfg(all(windows, feature = "cef"))]
pub mod browser;
pub mod data;
pub mod host;
pub mod logs;
#[cfg(windows)]
pub mod shell;
pub mod ui;
