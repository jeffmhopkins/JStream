//! The JStream desktop app: a Tauri window shows the web UI (`jstream-ui`), and the
//! engine runs in the same process, served to the window over the authenticated
//! localhost WebSocket API, the one a remote client uses (ADR-0045, ADR-0066). Windows
//! first: the window (`shell`) needs WebView2, and its two commands open the Browse
//! dialogs (`pick`) and save the network setting (`network`). The rest builds and is tested everywhere.
//!
//! Docs: docs/ARCHITECTURE.md#shape, docs/ARCHITECTURE.md#crate-map, apps/jstream-desktop/README.md#purpose, apps/jstream-desktop/README.md#usage, apps/jstream-desktop/README.md#design, docs/adr/0045-leptos-web-ui.md, docs/adr/0066-portable-windows-distribution-v2.md

#![forbid(unsafe_code)]

pub mod app;
pub mod bootstrap;
#[cfg(all(windows, feature = "cef"))]
pub mod browser;
pub mod host;
pub mod hotkeys;
pub mod links;
pub mod logs;
pub mod network;
#[cfg(desktop_window)]
pub mod pick;
#[cfg(desktop_window)]
pub mod shell;
pub mod ui;
pub mod updates;
