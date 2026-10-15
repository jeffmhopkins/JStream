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
use crate::block::block_on;
use crate::exit::CliError;
use crate::run::{CLIENT_NAME, Connector};
use jstream_control::Control;
use jstream_control::protocol::{
    Call, CallResult, Hello, PROTOCOL_VERSION, Query, QueryResult, YoutubeAuth,
};
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
    let server = if engine_has_youtube(connection, connector) {
        server.with_youtube()
    } else {
        server
    };
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

/// Whether the engine has YouTube (an OAuth client the user configured), so the proxy
/// lists the `youtube` tool. Asked over a short session of its own: the MCP server's
/// session sends `hello` only at `initialize`, with the agent's name. Any failure, such
/// as an engine that doesn't know `youtube.status`, counts as no: the tool stays hidden.
fn engine_has_youtube(connection: &Connection, connector: Connector<'_>) -> bool {
    let Ok(control) = connector(connection) else {
        return false;
    };
    let hello = Hello {
        protocol: PROTOCOL_VERSION,
        client_name: CLIENT_NAME.into(),
    };
    if block_on(control.hello(hello)).is_err() {
        return false;
    }
    let status = block_on(control.call(Call::Query {
        query: Query::YoutubeStatus,
    }));
    matches!(
        status.map(|response| response.result),
        Ok(CallResult::Query {
            result: QueryResult::YoutubeStatus { auth },
        }) if auth != YoutubeAuth::Unavailable
    )
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

#[cfg(test)]
mod tests {
    use super::engine_has_youtube;
    use crate::args::Connection;
    use crate::exit::CliError;
    use jstream_control::protocol::{
        Call, CallResult, Hello, Query, QueryResult, RequestId, Response, Welcome, YoutubeAuth,
    };
    use jstream_control::{BoxFuture, Control, ControlError, EventStream, MockControl, MockEngine};

    /// A `MockEngine` session that answers `youtube.status` with `auth`, when given.
    struct Youtube {
        inner: MockControl,
        auth: Option<YoutubeAuth>,
    }

    impl Control for Youtube {
        fn hello(&self, hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
            self.inner.hello(hello)
        }

        fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
            match (&call, self.auth) {
                (
                    Call::Query {
                        query: Query::YoutubeStatus,
                    },
                    Some(auth),
                ) => Box::pin(async move {
                    Ok(Response {
                        id: RequestId::new("req_1"),
                        result: CallResult::Query {
                            result: QueryResult::YoutubeStatus { auth },
                        },
                    })
                }),
                _ => self.inner.call(call),
            }
        }

        fn events(&self) -> EventStream {
            self.inner.events()
        }
    }

    fn has_youtube(auth: Option<YoutubeAuth>) -> bool {
        let engine = MockEngine::new();
        let connector = move |_: &Connection| -> Result<Box<dyn Control>, CliError> {
            Ok(Box::new(Youtube {
                inner: engine.connect(),
                auth,
            }))
        };
        let connection = Connection::Remote {
            url: None,
            token_file: None,
        };
        engine_has_youtube(&connection, &connector)
    }

    /// The proxy lists `youtube` only when the engine has a YouTube setup.
    #[test]
    fn youtube_is_listed_only_when_the_engine_has_it() {
        assert!(has_youtube(Some(YoutubeAuth::SignedOut)));
        assert!(has_youtube(Some(YoutubeAuth::SignedIn)));
        assert!(!has_youtube(Some(YoutubeAuth::Unavailable)));
        // An engine that doesn't answer `youtube.status` (the mock's error) hides it.
        assert!(!has_youtube(None));
    }
}
