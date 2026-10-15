//! The `Control` trait: the one async API through which every client (the CLI, the MCP
//! server, the web UI, the HTTP server) talks to the engine, whether the engine runs in the
//! same process or behind an authenticated WebSocket. Layer 2: depends only on
//! `jstream-protocol`, WASM-safe, no runtime requirement on callers. The `ws` feature adds
//! `WsControl`, the WebSocket client (native targets), which runs its own.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#protocol, crates/jstream-control/README.md#purpose, crates/jstream-control/README.md#design, docs/adr/0001-library-first-engine.md

#![forbid(unsafe_code)]

mod control;
mod error;
#[cfg(any(test, feature = "mock"))]
mod mock;
#[cfg(all(feature = "ws", not(target_arch = "wasm32")))]
mod ws;

pub use control::{BinaryStream, BoxFuture, Control, EventStream, MaybeSendSync};
pub use error::ControlError;
#[cfg(any(test, feature = "mock"))]
pub use mock::{MockControl, MockEngine};
#[cfg(all(feature = "ws", not(target_arch = "wasm32")))]
pub use ws::{WsConfig, WsControl};

/// The path of the engine's WebSocket API, shared by the server that mounts it and the
/// client that connects to it.
pub const WS_PATH: &str = "/api/v1/ws";

/// The protocol crate, re-exported so clients can name its types through one dependency.
pub use jstream_protocol as protocol;
