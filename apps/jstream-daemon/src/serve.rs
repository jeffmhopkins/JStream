//! The engine behind the server: one `Engine`, one `InProcessControl` session per
//! WebSocket connection and per MCP session, and the signals that stop it.

use crate::args::Options;
use jstream_core::Engine;
use jstream_server::{Connector, Server, ServerConfig, Token};
use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

/// The engine the daemon serves: an empty collection, plus the placeholder outputs.
pub fn engine(options: &Options) -> Engine {
    options
        .outputs
        .iter()
        .cloned()
        .fold(Engine::new(), Engine::with_output)
}

/// Binds the server on `127.0.0.1:<port>`, serving `engine` behind `token`, with MCP's
/// streamable-HTTP transport on. Each connection and each MCP session gets its own
/// session (its own `client_id`); all of them share the engine.
pub async fn bind(
    engine: Engine,
    port: u16,
    token: Token,
) -> Result<Server, jstream_server::ServerError> {
    let connector: Connector = Arc::new(move || Box::new(engine.connect()));
    let mut config = ServerConfig::new(token);
    config.bind = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    config.mcp = true;
    Server::bind(config, connector).await
}

/// Installs the handlers for the signals that stop the daemon, and returns a future
/// that completes on the first of them: Ctrl-C (SIGINT) or SIGTERM on Unix, Ctrl-C or
/// Ctrl-Break on Windows. Installing them up front makes a failure a startup error,
/// not a daemon that can't be stopped gracefully. Must run inside the tokio runtime.
#[cfg(unix)]
pub fn shutdown_signal() -> io::Result<impl Future<Output = ()> + Send + 'static> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    Ok(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
    })
}

/// See the Unix version.
#[cfg(windows)]
pub fn shutdown_signal() -> io::Result<impl Future<Output = ()> + Send + 'static> {
    use tokio::signal::windows::{ctrl_break, ctrl_c};
    let mut interrupt = ctrl_c()?;
    let mut break_ = ctrl_break()?;
    Ok(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = break_.recv() => {}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{bind, engine};
    use crate::args::Options;
    use futures_util::StreamExt;
    use jstream_control::protocol::{
        Batch, Call, CallResult, Edit, Event, Hello, OutputId, OutputState, PROTOCOL_VERSION,
        Query, QueryResult, Rev, SceneId,
    };
    use jstream_control::{Control, ControlError, WsConfig, WsControl};
    use jstream_server::Token;
    use std::time::Duration;
    use tokio::sync::oneshot;

    const TOKEN: &str = "daemon-unit-token-0123";

    fn hello() -> Hello {
        Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "daemon-test".into(),
        }
    }

    /// Fails the test after 10 s instead of hanging.
    async fn timely<T>(future: impl Future<Output = T>) -> T {
        match tokio::time::timeout(Duration::from_secs(10), future).await {
            Ok(value) => value,
            Err(_) => panic!("timed out"),
        }
    }

    /// The engine the options describe, served on a free port: two clients share it, and
    /// a shutdown closes their connections.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn serves_one_engine_to_every_connection_until_shut_down() {
        let options = Options {
            outputs: vec![OutputId::new("out_1")],
            ..Options::default()
        };
        let token = Token::new(TOKEN).expect("a valid token");
        let server = bind(engine(&options), 0, token).await.expect("binds");
        assert!(server.local_addr().ip().is_loopback());
        assert_ne!(server.local_addr().port(), 0);
        assert_eq!(
            server.mcp_url(),
            Some(format!("http://{}/mcp", server.local_addr())),
            "serves MCP"
        );
        let url = server.ws_url();
        let (stop, stopped) = oneshot::channel::<()>();
        let serving = tokio::spawn(server.serve_with_shutdown(async move {
            let _ = stopped.await;
        }));

        let first = timely(WsControl::connect(WsConfig::new(url.clone(), TOKEN)))
            .await
            .expect("connects");
        let second = timely(WsControl::connect(WsConfig::new(url.clone(), TOKEN)))
            .await
            .expect("connects");
        let one = timely(first.hello(hello())).await.expect("welcome");
        let two = timely(second.hello(hello())).await.expect("welcome");
        assert_ne!(one.client_id, two.client_id, "one session per connection");
        let mut events = second.events();

        let batch = Batch::single(Edit::SceneCreate {
            scene_id: SceneId::new("main"),
            name: "Main".to_owned(),
        });
        let applied = timely(first.call(Call::Batch(batch)))
            .await
            .expect("applied");
        assert_eq!(applied.result, CallResult::Batch { rev: Rev(1) });
        match timely(events.next()).await {
            Some(Event::StateChanged { rev, client_id, .. }) => {
                assert_eq!((rev, client_id), (Rev(1), one.client_id.clone()));
            }
            other => panic!("expected state.changed, got {other:?}"),
        }
        // The first scene went on program.
        assert!(matches!(
            timely(events.next()).await,
            Some(Event::StudioChanged { .. })
        ));
        let status = timely(second.call(Call::Query {
            query: Query::OutputStatus {
                output_id: OutputId::new("out_1"),
            },
        }))
        .await
        .expect("answered");
        match status.result {
            CallResult::Query {
                result: QueryResult::OutputStatus { state, .. },
            } => {
                assert_eq!(state, OutputState::Stopped);
            }
            other => panic!("expected output.status, got {other:?}"),
        }

        let _ = stop.send(());
        timely(serving)
            .await
            .expect("joins")
            .expect("stops cleanly");
        assert_eq!(timely(events.next()).await, None, "the stream ends");
        match timely(first.call(Call::Query {
            query: Query::SceneList,
        }))
        .await
        {
            Err(ControlError::Disconnected { .. }) => {}
            other => panic!("expected Disconnected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_port_in_use_fails_to_bind() {
        let token = Token::new(TOKEN).expect("a valid token");
        let server = bind(engine(&Options::default()), 0, token.clone())
            .await
            .expect("binds");
        let port = server.local_addr().port();
        let error = bind(engine(&Options::default()), port, token)
            .await
            .err()
            .expect("the port is taken");
        assert!(
            error.to_string().contains(&format!("127.0.0.1:{port}")),
            "{error}"
        );
    }
}
