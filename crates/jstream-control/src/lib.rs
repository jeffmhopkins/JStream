//! The `Control` trait: the one async API through which every client (the CLI, the MCP
//! server, the web UI, the HTTP server) talks to the engine, whether the engine runs in the
//! same process or behind an authenticated WebSocket. Layer 2: depends only on
//! `jstream-protocol`, WASM-safe, no runtime requirement.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#protocol, crates/jstream-control/README.md#purpose, crates/jstream-control/README.md#design, docs/adr/0001-library-first-engine.md

#![forbid(unsafe_code)]

mod control;
mod error;
#[cfg(any(test, feature = "mock"))]
mod mock;

pub use control::{BoxFuture, Control, EventStream, MaybeSendSync};
pub use error::ControlError;
#[cfg(any(test, feature = "mock"))]
pub use mock::{MockControl, MockEngine};

/// The protocol crate, re-exported so clients can name its types through one dependency.
pub use jstream_protocol as protocol;
