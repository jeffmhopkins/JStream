//! The JStream MCP server: gives AI agents full control of JStream through the Model
//! Context Protocol. Its tools are generated from the protocol's JSON Schema, grouped by
//! noun, and every call goes through one `dyn Control` session, so the server never links
//! the engine. Layer 3: depends on `jstream-control` and `jstream-protocol` only, and
//! needs no async runtime.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#mcp, crates/jstream-mcp/README.md#purpose, crates/jstream-mcp/README.md#design, docs/adr/0006-mcp-full-access.md, docs/adr/0001-library-first-engine.md, docs/adr/0036-mcp-events-polling-tool.md

#![forbid(unsafe_code)]

mod error;
mod events;
mod jsonrpc;
mod server;
mod stdio;
#[cfg(test)]
mod tests;
mod tools;

pub use error::McpError;
pub use server::{LEGACY_VERSIONS, MODERN_VERSION, Server};
pub use stdio::serve_stdio;

// The test below sits at the crate root because RULES.md names it by path:
// `jstream_mcp::tool_names_are_valid` (R-API-002).

/// R-API-002: every tool name matches `^[a-zA-Z0-9_-]{1,128}$` and is unique, every op is
/// `noun.verb` under the tool named by its noun, and every protocol command and query
/// reaches exactly one tool.
#[cfg(test)]
#[test]
fn tool_names_are_valid() {
    tools::check_tool_names_are_valid();
}
