//! The wasmtime host for sandboxed WASM automation plugins (ADR-0035). A plugin is a
//! core WASM module that reaches the engine only through [`Control`]: it gets no WASI, no
//! filesystem, no network and no clock, and every call it makes must name commands its
//! user-granted [`Capabilities`] list, which is empty by default. Each invocation runs on
//! a fixed fuel budget and a wall-time deadline, the plugin's memory and tables are
//! capped, and so are the commands it sends to `Control` a second ([`Limits`]).
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-plugin-wasm/README.md#purpose, crates/jstream-plugin-wasm/README.md#abi, docs/adr/0035-clap-plugins-out-of-process.md

#![forbid(unsafe_code)]

pub mod abi;
mod block_on;
mod capability;
mod error;
mod events;
mod limits;
#[cfg(test)]
mod no_ambient_capabilities;
mod plugin;
mod rate;

pub use capability::{Capabilities, command_names};
pub use error::Error;
pub use events::{EventSender, MAX_QUEUED_EVENTS};
pub use limits::Limits;
pub use plugin::{EPOCH_TICK, Plugin, PluginConfig, PluginHost};

/// The control crate, re-exported so a host names `Control` and the protocol through one
/// dependency.
pub use jstream_control as control;

#[cfg(doc)]
use jstream_control::Control;
