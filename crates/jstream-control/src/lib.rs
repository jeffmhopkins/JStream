//! The `Control` trait: the one async API through which every client (the CLI, the MCP
//! server, the web UI, the HTTP server) talks to the engine, whether the engine runs in the
//! same process or behind an authenticated WebSocket. Layer 2: depends only on
//! `jstream-protocol`, WASM-safe, no runtime requirement on callers. The `ws` feature adds
//! `WsControl`, the WebSocket client: on native targets it runs its own runtime, and on
//! `wasm32` it wraps the browser's `WebSocket`.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#protocol, crates/jstream-control/README.md#purpose, crates/jstream-control/README.md#design, docs/adr/0001-library-first-engine.md

#![forbid(unsafe_code)]

mod control;
mod error;
#[cfg(any(test, feature = "mock"))]
mod mock;
#[cfg(all(feature = "ws", not(target_arch = "wasm32")))]
mod ws;
#[cfg(feature = "ws")]
mod ws_config;
#[cfg(all(feature = "ws", target_arch = "wasm32"))]
mod ws_web;

pub use control::{BinaryStream, BoxFuture, Control, EventStream, MaybeSendSync};
pub use error::ControlError;
#[cfg(any(test, feature = "mock"))]
pub use mock::{MOCK_DISPLAY, MockControl, MockEngine};
#[cfg(all(feature = "ws", not(target_arch = "wasm32")))]
pub use ws::WsControl;
#[cfg(feature = "ws")]
pub use ws_config::WsConfig;
#[cfg(all(feature = "ws", target_arch = "wasm32"))]
pub use ws_web::WsControl;

/// The path of the engine's WebSocket API, shared by the server that mounts it and the
/// client that connects to it.
pub const WS_PATH: &str = "/api/v1/ws";

/// The subprotocol a browser client offers and the server selects when it accepts the
/// token from `Sec-WebSocket-Protocol`. Browsers can't set `Authorization` on a
/// WebSocket, so the token travels there instead (see [`WS_TOKEN_PROTOCOL_PREFIX`]).
/// Outside the `ws` feature, like [`WS_PATH`], so the server shares it without the client.
pub const WS_PROTOCOL: &str = "jstream";

/// The prefix of the subprotocol that carries the control token: a browser offers
/// `jstream.token.<token>` next to [`WS_PROTOCOL`]. The server never selects it.
pub const WS_TOKEN_PROTOCOL_PREFIX: &str = "jstream.token.";

/// The protocol crate, re-exported so clients can name its types through one dependency.
pub use jstream_protocol as protocol;
