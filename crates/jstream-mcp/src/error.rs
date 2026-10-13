//! What serving MCP can fail with.

/// Why a transport stopped.
///
/// Problems with single messages never end up here: malformed JSON-RPC, unknown tools and
/// engine errors are all answered on the wire. Only the stream itself failing stops a
/// transport.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpError {
    /// Reading a message or writing a reply failed.
    #[error("MCP transport I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
