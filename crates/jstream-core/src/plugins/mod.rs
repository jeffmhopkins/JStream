//! CLAP plugins hosted out of process (ADR-0035): the filter kinds a scan finds, and
//! the supervisor that keeps each filter's host process running. And, with the
//! `wasm-plugins` feature, the sandboxed WASM automation plugins the engine runs in
//! process (ADR-0069, ADR-0075).

#[cfg(feature = "wasm-plugins")]
pub(crate) mod automation;
pub mod kinds;
pub mod supervisor;
