//! The JStream server: the control API over an authenticated WebSocket, in front of any
//! `Control` session. It never links the engine (R-ARCH-003): the host app hands it a
//! [`Connector`] that opens one session per connection, in-process or otherwise. Every
//! upgrade request must carry the control token and pass the `Origin` and `Host` checks,
//! and it listens on `127.0.0.1` unless told otherwise (R-SEC-002, ADR-0006). It bounds
//! how many connections are open at once and how long a connection has to send its
//! upgrade request and complete its handshake. When the host turns it on
//! (`ServerConfig::mcp`), it also serves MCP's streamable-HTTP transport at [`MCP_PATH`],
//! behind the same checks, with one MCP session per `initialize`. When the host names a
//! directory (`ServerConfig::ui_dir`), it serves the web UI's files from it at every
//! other path, behind the same checks, with a browser's `Basic` credentials carrying the
//! token. It pairs devices (ADR-0044): `pairing.start` opens a one-time code, a device
//! trades it for its own device token at [`PAIR_PATH`], the one request answered without
//! a token, and a device token then gets in wherever the control token does, while the
//! server listens on loopback. The server answers `pairing.start`, `device.list` and
//! `device.revoke` itself, on every connection and MCP session. Layer 4.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#protocol, crates/jstream-server/README.md#purpose, crates/jstream-server/README.md#design, docs/adr/0006-mcp-full-access.md

#![forbid(unsafe_code)]

mod admitted;
mod config;
mod devices;
mod error;
mod gate;
mod listener;
mod mcp;
mod pair;
mod session;
#[cfg(test)]
mod tests;
mod ui;

pub use config::{ServerConfig, Token};
pub use error::ServerError;
pub use jstream_control::WS_PATH;
pub use mcp::MCP_PATH;
pub use pair::PAIR_PATH;

use admitted::Admitted;
use axum::Router;
use axum::extract::ws::{WebSocketUpgrade, rejection::WebSocketUpgradeRejection};
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use devices::Devices;
use gate::{Credential, Gate, Refusal};
use jstream_control::{Control, WS_PROTOCOL};
use listener::{Accepted, Limited};
use mcp::Mcp;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tracing::Instrument;
use ui::Ui;

/// Opens one `Control` session for each accepted connection. A connection's session is
/// dropped when it closes.
pub type Connector = Arc<dyn Fn() -> Box<dyn Control> + Send + Sync>;

/// The largest client message accepted, in bytes. Larger messages close the connection.
const MAX_MESSAGE_BYTES: usize = 4 << 20;

/// A bound server, ready to [`serve`](Server::serve).
pub struct Server {
    listener: TcpListener,
    local_addr: SocketAddr,
    header_read_timeout: Duration,
    app: Arc<App>,
    shutdown: watch::Sender<bool>,
}

/// What every connection handler shares.
struct App {
    connector: Connector,
    gate: Gate,
    shutdown: watch::Receiver<bool>,
    /// Numbers the accepted connections, for their `connection` spans.
    connections: AtomicU64,
    /// One permit per open connection, up to `ServerConfig::max_connections`.
    slots: Arc<Semaphore>,
    /// `ServerConfig::handshake_timeout`.
    handshake_timeout: Duration,
    /// The MCP sessions, when `ServerConfig::mcp` is on.
    mcp: Option<Mcp>,
    /// The web UI's files, when `ServerConfig::ui_dir` is set.
    ui: Option<Ui>,
    /// The paired devices and the open pairing code (ADR-0044).
    devices: Arc<Devices>,
    /// Whether the server listens on a loopback address, the only place pairing is
    /// offered and device tokens are accepted until it has TLS.
    loopback: bool,
}

impl App {
    /// A new session from the connector, admitted by `credential`.
    fn open(&self, credential: Credential) -> Box<dyn Control> {
        Box::new(Admitted::new(
            (self.connector)(),
            credential,
            Arc::clone(&self.devices),
            self.loopback,
        ))
    }
}

impl Server {
    /// Opens the listening socket. Nothing is served until [`serve`](Server::serve).
    ///
    /// An extra host without a port (or with an invalid one) is refused with
    /// [`ServerError::InvalidHost`] before anything is opened, since it could never match.
    pub async fn bind(config: ServerConfig, connector: Connector) -> Result<Server, ServerError> {
        gate::check_extra_hosts(&config.extra_hosts)?;
        let ui = config.ui_dir.as_deref().map(Ui::new).transpose()?;
        let devices = Arc::new(Devices::load(config.device_file.clone())?);
        let bind_error = |source| ServerError::Bind {
            addr: config.bind,
            source,
        };
        let listener = TcpListener::bind(config.bind).await.map_err(bind_error)?;
        let local_addr = listener.local_addr().map_err(bind_error)?;
        let (shutdown, stopping) = watch::channel(false);
        let mcp = config.mcp.then(|| Mcp::new(&config));
        let app = Arc::new(App {
            connector,
            gate: Gate::new(&config, local_addr, Arc::clone(&devices)),
            shutdown: stopping,
            connections: AtomicU64::new(0),
            slots: Arc::new(Semaphore::new(
                config.max_connections.min(Semaphore::MAX_PERMITS),
            )),
            handshake_timeout: config.handshake_timeout,
            mcp,
            ui,
            devices,
            loopback: local_addr.ip().is_loopback(),
        });
        Ok(Server {
            listener,
            local_addr,
            header_read_timeout: config.header_read_timeout,
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

    /// The MCP endpoint's URL on this server, e.g. `http://127.0.0.1:4455/mcp`, when
    /// [`ServerConfig::mcp`] is on.
    pub fn mcp_url(&self) -> Option<String> {
        self.app
            .mcp
            .as_ref()
            .map(|_| format!("http://{}{MCP_PATH}", self.local_addr))
    }

    /// The web UI's URL on this server, e.g. `http://127.0.0.1:4455/`, when
    /// [`ServerConfig::ui_dir`] is set.
    pub fn ui_url(&self) -> Option<String> {
        self.app
            .ui
            .as_ref()
            .map(|_| format!("http://{}/", self.local_addr))
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
            header_read_timeout,
            app,
            shutdown,
            ..
        } = self;
        let mut router = Router::new()
            .route(WS_PATH, get(upgrade))
            .route(PAIR_PATH, any(pair::handle));
        if app.mcp.is_some() {
            router = router.route(MCP_PATH, any(mcp::handle));
        }
        if app.ui.is_some() {
            router = router.fallback(ui::handle);
        }
        let router = router.with_state(app);
        let signal = async move {
            signal.await;
            shutdown.send_replace(true);
        };
        // The peer address is only logged, never trusted: the gate decides on the headers.
        let service = router.into_make_service_with_connect_info::<Accepted>();
        axum::serve(Limited::new(listener, header_read_timeout), service)
            .with_graceful_shutdown(signal)
            .await
            .map_err(ServerError::Serve)
    }
}

/// `GET /api/v1/ws`: checks the request (R-SEC-002) and that a connection slot is free,
/// then upgrades it and runs a session.
///
/// A refusal is logged with its reason and the peer address ([`Gate::admit`],
/// [`Refusal::log`]). An accepted connection runs in a `connection` span carrying its
/// number and peer address, which every log line of the session nests under. Request
/// headers are never logged, so the token can't be (R-SEC-001).
async fn upgrade(
    State(app): State<Arc<App>>,
    ConnectInfo(accepted): ConnectInfo<Accepted>,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let peer = accepted.peer;
    let credential = match app.gate.admit(&headers, peer) {
        Ok(credential) => credential,
        Err(refusal) => return refusal.into_response(),
    };
    // A device's connection closes when the device is revoked. One revoked since the
    // gate admitted it is refused as its token would be now.
    let revoked = match credential.device() {
        Some(id) => match app.devices.watch(id) {
            Some(watch) => Some(watch),
            None => {
                Refusal::Token.log(peer);
                return Refusal::Token.into_response();
            }
        },
        None => None,
    };
    let upgrade = match upgrade {
        Ok(upgrade) => upgrade,
        Err(rejection) => {
            tracing::debug!(peer = %peer, rejection = %rejection, "not a WebSocket upgrade");
            return rejection.into_response();
        }
    };
    // Held until the session ends; released at once if the upgrade fails.
    let Ok(slot) = Arc::clone(&app.slots).try_acquire_owned() else {
        Refusal::Full.log(peer);
        return Refusal::Full.into_response();
    };
    // The request is accepted: the header-read deadline no longer applies, the
    // handshake deadline does.
    accepted.request_read();
    let hello_by = accepted.at + app.handshake_timeout;
    let number = app.connections.fetch_add(1, Ordering::Relaxed) + 1;
    let span = tracing::info_span!("connection", id = number, peer = %peer);
    // Opened inside the span, so a session span the connector creates nests under it.
    let control = span.in_scope(|| {
        match credential.device() {
            Some(device_id) => tracing::info!(device_id = %device_id, "connection accepted"),
            None => tracing::info!("connection accepted"),
        }
        app.open(credential)
    });
    let shutdown = app.shutdown.clone();
    // A browser client offers `jstream` beside the token's subprotocol and fails the
    // connection unless one is selected; the token's is never echoed.
    upgrade
        .protocols([WS_PROTOCOL])
        .max_message_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| {
            async move {
                session::run(socket, control, shutdown, revoked, hello_by).await;
                drop(slot);
            }
            .instrument(span)
        })
}

// The two tests below sit at the crate root because RULES.md names them by path
// (R-SEC-002): `jstream_server::rejects_missing_token` and
// `jstream_server::rejects_foreign_origin`.

/// R-SEC-002: an upgrade or an MCP request without the control token, or with a wrong
/// one, is refused with HTTP 401 and no session is opened.
#[cfg(test)]
#[test]
fn rejects_missing_token() {
    tests::check_rejects_missing_token();
    tests::check_mcp_rejects_missing_token();
}

/// R-SEC-002: an upgrade or an MCP request from a foreign `Origin` (or with a foreign
/// `Host`) is refused with HTTP 403 even with the right token.
#[cfg(test)]
#[test]
fn rejects_foreign_origin() {
    tests::check_rejects_foreign_origin();
    tests::check_mcp_rejects_foreign_origin();
}
