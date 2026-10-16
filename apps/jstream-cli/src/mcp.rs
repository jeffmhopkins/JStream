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
use std::io::{self, BufRead, Read, Write};
use std::sync::Arc;

/// Connects, then serves MCP on `input` and `output` until `input` ends.
///
/// A failure to connect is returned before anything is written to `output`, so it exits
/// with the CLI's usual code (2 usage, 3 disconnected). The server sends `hello` itself,
/// at `initialize`, with the MCP client's name, so an idle agent keeps its session past
/// `jstream-server`'s handshake timeout. Once
/// serving, a lost engine connection does not stop the proxy: `jstream-mcp` answers each
/// tool call that needs the engine with its `disconnected` tool error, and the proxy
/// keeps serving until `input` ends, then returns `Ok`. Only failing to read `input`
/// ([`CliError::Read`]) or write `output` ([`CliError::Io`]) stops it early.
pub fn serve(
    connection: &Connection,
    connector: Connector<'_>,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<(), CliError> {
    let control: Arc<dyn Control> = Arc::from(connector(connection)?);
    let server = Server::new(control);
    // `jstream-mcp` reports read and write failures alike, so the input remembers its own.
    let mut input = Input {
        inner: input,
        failed: false,
    };
    let result = jstream_mcp::serve_stdio(&server, &mut input, output);
    let read_failed = input.failed;
    result.map_err(|error| {
        let error = match error {
            McpError::Io(error) => error,
            other => io::Error::other(other),
        };
        if read_failed {
            CliError::Read(error)
        } else {
            CliError::Io(error)
        }
    })
}

/// The proxy's input, noting whether reading it failed: serving stops at the first
/// failure, so a failed read means the error came from the input, not the output.
struct Input<'a> {
    inner: &'a mut dyn BufRead,
    failed: bool,
}

impl Input<'_> {
    fn note<T>(&mut self, result: io::Result<T>) -> io::Result<T> {
        // An interrupted read is retried by the reader, so it isn't a failure.
        if let Err(error) = &result
            && error.kind() != io::ErrorKind::Interrupted
        {
            self.failed = true;
        }
        result
    }
}

impl Read for Input<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let result = self.inner.read(buf);
        self.note(result)
    }
}

impl BufRead for Input<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        // Borrowing `self.inner` for the buffer rules out `note`, so its check is inlined.
        match self.inner.fill_buf() {
            Ok(buf) => Ok(buf),
            Err(error) => {
                if error.kind() != io::ErrorKind::Interrupted {
                    self.failed = true;
                }
                Err(error)
            }
        }
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
    }
}
