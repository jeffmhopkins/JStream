//! The JStream server: the control API over an authenticated WebSocket, in front of any
//! `Control` session. It never links the engine (R-ARCH-003): the host app hands it a
//! [`Connector`] that opens one session per connection, in-process or otherwise. Every
//! upgrade request must carry the control token and pass the `Origin` and `Host` checks,
//! and it listens on `127.0.0.1` unless told otherwise (R-SEC-002, ADR-0006). Layer 4.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#protocol, crates/jstream-server/README.md#purpose, crates/jstream-server/README.md#design, docs/adr/0006-mcp-full-access.md

#![forbid(unsafe_code)]

mod config;
mod error;
mod gate;
mod session;
#[cfg(test)]
mod tests;

pub use config::{ServerConfig, Token};
pub use error::ServerError;
pub use jstream_control::WS_PATH;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{WebSocketUpgrade, rejection::WebSocketUpgradeRejection};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use gate::Gate;
use jstream_control::Control;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::watch;

/// Opens one `Control` session for each accepted connection. A connection's session is
/// dropped when it closes.
pub type Connector = Arc<dyn Fn() -> Box<dyn Control> + Send + Sync>;

/// The largest client message accepted, in bytes. Larger messages close the connection.
const MAX_MESSAGE_BYTES: usize = 4 << 20;

/// A bound server, ready to [`serve`](Server::serve).
pub struct Server {
    listener: TcpListener,
    local_addr: SocketAddr,
    app: Arc<App>,
    shutdown: watch::Sender<bool>,
}

/// What every connection handler shares.
struct App {
    connector: Connector,
    gate: Gate,
    shutdown: watch::Receiver<bool>,
}

impl Server {
    /// Opens the listening socket. Nothing is served until [`serve`](Server::serve).
    pub async fn bind(config: ServerConfig, connector: Connector) -> Result<Server, ServerError> {
        let bind_error = |source| ServerError::Bind {
            addr: config.bind,
            source,
        };
        let listener = TcpListener::bind(config.bind).await.map_err(bind_error)?;
        let local_addr = listener.local_addr().map_err(bind_error)?;
        let (shutdown, stopping) = watch::channel(false);
        let app = Arc::new(App {
            connector,
            gate: Gate::new(&config, local_addr),
            shutdown: stopping,
        });
        Ok(Server {
            listener,
            local_addr,
            app,
            shutdown,
        })
    }

    /// The address the server listens on (with the actual port if `0` was asked for).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The WebSocket API's URL on this server, e.g. `ws://127.0.0.1:4455/api/v1/ws`.
    pub fn ws_url(&self) -> String {
        format!("ws://{}{WS_PATH}", self.local_addr)
    }

    /// Serves until the listener fails.
    pub async fn serve(self) -> Result<(), ServerError> {
        self.serve_with_shutdown(std::future::pending()).await
    }

    /// Serves until `signal` completes, then stops accepting connections and closes the
    /// open ones (close code 1001), which makes their clients' calls fail with
    /// `Disconnected`.
    pub async fn serve_with_shutdown<F>(self, signal: F) -> Result<(), ServerError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let Server {
            listener,
            app,
            shutdown,
            ..
        } = self;
        let router = Router::new().route(WS_PATH, get(upgrade)).with_state(app);
        let signal = async move {
            signal.await;
            shutdown.send_replace(true);
        };
        axum::serve(listener, router)
            .with_graceful_shutdown(signal)
            .await
            .map_err(ServerError::Serve)
    }
}

/// `GET /api/v1/ws`: checks the request (R-SEC-002), then upgrades it and runs a session.
async fn upgrade(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if let Err(refusal) = app.gate.check(&headers) {
        return refusal.into_response();
    }
    let upgrade = match upgrade {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };
    let control = (app.connector)();
    let shutdown = app.shutdown.clone();
    upgrade
        .max_message_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| session::run(socket, control, shutdown))
}

// The two tests below sit at the crate root because RULES.md names them by path
// (R-SEC-002): `jstream_server::rejects_missing_token` and
// `jstream_server::rejects_foreign_origin`.

/// R-SEC-002: an upgrade without the control token, or with a wrong one, is refused with
/// HTTP 401 and no session is opened.
#[cfg(test)]
#[test]
fn rejects_missing_token() {
    tests::check_rejects_missing_token();
}

/// R-SEC-002: an upgrade from a foreign `Origin` (or with a foreign `Host`) is refused
/// with HTTP 403 even with the right token.
#[cfg(test)]
#[test]
fn rejects_foreign_origin() {
    tests::check_rejects_foreign_origin();
}
