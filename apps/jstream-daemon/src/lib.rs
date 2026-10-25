//! `jstream-daemon`'s library: the engine wiring the daemon binary serves (`live`),
//! exposed so the integration tests build the daemon's own live engine on the platform
//! fakes instead of a copy of it. The binary (`main.rs`) is the daemon itself: the
//! command line, the token, logs, crash reports and the server. Nothing else depends on
//! this library; like every host, the daemon only wires the engine to its interfaces
//! (ADR-0001).
//!
//! Docs: docs/ARCHITECTURE.md#shape, docs/ARCHITECTURE.md#crate-map, apps/jstream-daemon/README.md#purpose, apps/jstream-daemon/README.md#usage, apps/jstream-daemon/README.md#design, docs/adr/0001-library-first-engine.md, docs/adr/0006-mcp-full-access.md, docs/security/threat-model.md#server

#![forbid(unsafe_code)]

pub mod live;
