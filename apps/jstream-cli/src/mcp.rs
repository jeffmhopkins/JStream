//! `jstream-cli mcp`: the stdio MCP proxy. It opens one session through the CLI's
//! connector (`--url` and the token file, or `--mock`), builds a `jstream_mcp::Server`
//! over it, and serves MCP on the given reader and writer until the reader ends.
//!
//! The writer is the process's stdout, which belongs to MCP: only JSON-RPC lines are
//! written to it, by `jstream_mcp::serve_stdio`. Errors before serving are reported on
//! stderr by `run`, like any other command's.
//!
//! Docs: apps/jstream-cli/README.md#usage, apps/jstream-cli/README.md#design, docs/ARCHITECTURE.md#mcp, docs/adr/0006-mcp-full-access.md

use crate::args::Connection;
use crate::exit::CliError;
use crate::run::Connector;
use jstream_control::Control;
use jstream_mcp::{McpError, Server};
use std::io::{self, BufRead, Write};
use std::sync::Arc;

/// Connects, then serves MCP on `input` and `output` until `input` ends.
///
/// A failure to connect is returned before anything is written to `output`, so it exits
/// with the CLI's usual code (2 usage, 3 disconnected). The server sends `hello` itself,
/// before the first tool call that needs the engine, with the MCP client's name. Once
/// serving, a lost engine connection does not stop the proxy: `jstream-mcp` answers each
/// tool call that needs the engine with its `disconnected` tool error, and the proxy
/// keeps serving until `input` ends, then returns `Ok`. Only failing to read `input` or
/// write `output` stops it early, as [`CliError::Io`].
pub fn serve(
    connection: &Connection,
    connector: Connector<'_>,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<(), CliError> {
    let control: Arc<dyn Control> = Arc::from(connector(connection)?);
    let server = Server::new(control);
    jstream_mcp::serve_stdio(&server, input, output).map_err(|error| match error {
        McpError::Io(error) => CliError::Io(error),
        other => CliError::Io(io::Error::other(other)),
    })
}
