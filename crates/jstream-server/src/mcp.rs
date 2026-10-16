//! MCP's streamable-HTTP transport at [`MCP_PATH`], on the control port, behind the same
//! `Host`, `Origin` and token checks as the WebSocket API (R-SEC-002, ADR-0006). Each
//! request body is one JSON-RPC message, answered by a `jstream_mcp::Server`
//! (crates/jstream-server/README.md#design).
//!
//! - **`POST`** carries one message. A request is answered with `200` and one JSON body; a
//!   notification or a response gets `202` and no body. The server never answers with an
//!   SSE stream: every MCP request it serves completes on its own.
//! - **Sessions.** An `initialize` (the handshake-based revisions) opens a new session: a
//!   new `jstream_mcp::Server` over a new `Control` session from the [`Connector`], whose
//!   ID comes back in `Mcp-Session-Id`. Later requests carrying that header go to it; an
//!   `initialize` carrying it ends that session and opens a new one. A
//!   request without the header (MCP 2026-07-28 is stateless) goes to one shared
//!   stateless server, opened at its first request. An unknown or expired ID is `404`, so
//!   the client initializes again.
//! - **`DELETE`** with `Mcp-Session-Id` ends that session. **`GET`** (an SSE stream for
//!   server-initiated messages) is `405`: the server sends none.
//! - **Credentials.** A session belongs to the credential that opened it, the control
//!   token or one paired device's token (ADR-0044), and a request presenting another one
//!   doesn't find it (`404`). Requests without a session ID share one stateless server
//!   per credential. A session or stateless server a device opened ends once that device
//!   is revoked or lapses, checked whenever a request arrives; its token is refused at
//!   once anyway.
//! - **Limits.** At most `ServerConfig::max_mcp_sessions` sessions are open; an
//!   `initialize` beyond that is `503`. A session unused for
//!   `ServerConfig::mcp_idle_timeout` is ended, checked whenever a request arrives.
//!   Bodies over 4 MiB are `413`, refused from `Content-Length` before they are read.

use crate::devices::Now;
use crate::gate::{Credential, Refusal};
use crate::listener::Accepted;
use crate::{App, MAX_MESSAGE_BYTES, ServerConfig};
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use jstream_mcp::{LEGACY_VERSIONS, MODERN_VERSION, Server as McpServer};
use jstream_protocol::DeviceId;
use serde_json::Value;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::time::Instant;

/// The path MCP's streamable-HTTP transport is served at.
pub const MCP_PATH: &str = "/mcp";

/// The header carrying a session's ID (MCP streamable HTTP, *Session Management*).
const SESSION_ID: HeaderName = HeaderName::from_static("mcp-session-id");

/// The header naming the client's MCP revision on every request after `initialize`.
const PROTOCOL_VERSION: HeaderName = HeaderName::from_static("mcp-protocol-version");

/// The random bytes in a session ID, written as twice as many hex digits.
const SESSION_ID_BYTES: usize = 16;

/// Locks `mutex`, ignoring poisoning: the session map is never left half-changed.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The MCP sessions of one server.
pub(crate) struct Mcp {
    max_sessions: usize,
    idle_timeout: Duration,
    sessions: Mutex<HashMap<String, Session>>,
    /// The servers for requests without `Mcp-Session-Id`, one per credential (`None` for
    /// the control token), each opened at its credential's first such request.
    stateless: Mutex<HashMap<Option<DeviceId>, Arc<McpServer>>>,
    /// Numbers the sessions, for their spans and log lines.
    numbers: AtomicU64,
}

/// One session opened by `initialize`.
struct Session {
    server: Arc<McpServer>,
    number: u64,
    last_used: Instant,
    /// The device whose token opened it; `None` for the control token.
    device: Option<DeviceId>,
}

/// Why a session couldn't be opened.
enum StartError {
    /// `max_mcp_sessions` are open.
    Full,
    /// The OS's random source failed.
    Random(getrandom::Error),
}

impl Mcp {
    pub(crate) fn new(config: &ServerConfig) -> Mcp {
        Mcp {
            max_sessions: config.max_mcp_sessions,
            idle_timeout: config.mcp_idle_timeout,
            sessions: Mutex::new(HashMap::new()),
            stateless: Mutex::new(HashMap::new()),
            numbers: AtomicU64::new(0),
        }
    }

    /// A new MCP server over a new `Control` session admitted by `credential`, created in
    /// an `mcp_client` span so the engine's session span and the MCP server's own nest
    /// under it.
    fn open(
        &self,
        app: &App,
        kind: &'static str,
        peer: SocketAddr,
        credential: Credential,
    ) -> (u64, Arc<McpServer>) {
        let number = self.numbers.fetch_add(1, Ordering::Relaxed) + 1;
        let span = tracing::info_span!("mcp_client", id = number, kind);
        let server = span.in_scope(|| {
            match credential.device() {
                Some(device_id) => {
                    tracing::info!(peer = %peer, device_id = %device_id, "mcp session opened");
                }
                None => tracing::info!(peer = %peer, "mcp session opened"),
            }
            Arc::new(McpServer::new(Arc::from(app.open(credential))))
        });
        (number, server)
    }

    /// The shared server for `credential`'s requests without a session ID.
    fn stateless(&self, app: &App, peer: SocketAddr, credential: Credential) -> Arc<McpServer> {
        let mut stateless = lock(&self.stateless);
        let now = Now::current().unix;
        stateless.retain(|device, _| {
            device
                .as_ref()
                .is_none_or(|id| app.devices.contains(id, now))
        });
        let key = credential.device().cloned();
        let server = stateless
            .entry(key)
            .or_insert_with(|| self.open(app, "stateless", peer, credential).1);
        Arc::clone(server)
    }

    /// Ends the sessions unused for `idle_timeout`, and those whose device is no longer
    /// paired.
    fn expire(&self, app: &App, sessions: &mut HashMap<String, Session>) {
        let now = Instant::now();
        let unix = Now::current().unix;
        sessions.retain(|_, session| {
            if session
                .device
                .as_ref()
                .is_some_and(|id| !app.devices.contains(id, unix))
            {
                tracing::info!(id = session.number, "mcp session ended: device revoked");
                return false;
            }
            let live = now.duration_since(session.last_used) < self.idle_timeout;
            if !live {
                tracing::info!(id = session.number, "mcp session ended: idle");
            }
            live
        });
    }

    /// Opens a session for an `initialize`, returning its ID and server.
    fn start(
        &self,
        app: &App,
        peer: SocketAddr,
        credential: Credential,
    ) -> Result<(String, Arc<McpServer>), StartError> {
        let mut bytes = [0u8; SESSION_ID_BYTES];
        getrandom::fill(&mut bytes).map_err(StartError::Random)?;
        let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let mut sessions = lock(&self.sessions);
        self.expire(app, &mut sessions);
        if sessions.len() >= self.max_sessions {
            return Err(StartError::Full);
        }
        let device = credential.device().cloned();
        let (number, server) = self.open(app, "session", peer, credential);
        sessions.insert(
            id.clone(),
            Session {
                server: Arc::clone(&server),
                number,
                last_used: Instant::now(),
                device,
            },
        );
        Ok((id, server))
    }

    /// The session with this ID, if `credential` opened it, marked as used now.
    fn find(&self, app: &App, id: &str, credential: &Credential) -> Option<Arc<McpServer>> {
        let mut sessions = lock(&self.sessions);
        self.expire(app, &mut sessions);
        let session = sessions.get_mut(id)?;
        if session.device.as_ref() != credential.device() {
            return None;
        }
        session.last_used = Instant::now();
        Some(Arc::clone(&session.server))
    }

    /// Ends the session with this ID, if it is open. Its `Control` session is dropped once
    /// its requests in flight finish.
    fn end(&self, app: &App, id: &str, reason: &'static str) -> bool {
        let mut sessions = lock(&self.sessions);
        self.expire(app, &mut sessions);
        match sessions.remove(id) {
            Some(session) => {
                tracing::info!(id = session.number, reason, "mcp session ended");
                true
            }
            None => false,
        }
    }
}

/// `MCP_PATH`, any method: checks the request (R-SEC-002), then serves it.
///
/// A request that passes the checks is an authenticated client, so the listener's
/// header-read deadline no longer applies to its connection, which may then carry more
/// requests (HTTP keep-alive) and calls that take longer than the deadline.
pub(crate) async fn handle(
    State(app): State<Arc<App>>,
    ConnectInfo(accepted): ConnectInfo<Accepted>,
    method: Method,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let peer = accepted.peer;
    let credential = match app.gate.admit(&headers, peer) {
        Ok(credential) => credential,
        Err(refusal) => return refusal.into_response(),
    };
    let Some(mcp) = &app.mcp else {
        return StatusCode::NOT_FOUND.into_response();
    };
    accepted.request_read();
    match method {
        Method::POST => post(&app, mcp, &headers, body, peer, credential).await,
        Method::DELETE => delete(&app, mcp, &headers, &credential),
        _ => (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "POST, DELETE")],
            "this server sends no server-initiated messages: POST requests\n",
        )
            .into_response(),
    }
}

/// `POST`: one JSON-RPC message.
async fn post(
    app: &App,
    mcp: &Mcp,
    headers: &HeaderMap,
    body: Body,
    peer: SocketAddr,
    credential: Credential,
) -> Response {
    if !is_json(headers) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "the body must be one JSON-RPC message (`Content-Type: application/json`)\n",
        )
            .into_response();
    }
    if let Some(version) = headers.get(&PROTOCOL_VERSION) {
        let served = version
            .to_str()
            .is_ok_and(|v| v == MODERN_VERSION || LEGACY_VERSIONS.contains(&v));
        if !served {
            return bad_request(format!(
                "unsupported `MCP-Protocol-Version`; this server speaks {MODERN_VERSION}, {}",
                LEGACY_VERSIONS.join(", ")
            ));
        }
    }
    let session_id = match single(headers, &SESSION_ID) {
        Ok(id) => id,
        Err(()) => return bad_request("`Mcp-Session-Id` must appear at most once".into()),
    };
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    // A body declared too long is refused before any of it is read.
    if declared.is_some_and(|length| length > MAX_MESSAGE_BYTES as u64) {
        return too_large();
    }
    let bytes = match axum::body::to_bytes(body, MAX_MESSAGE_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => return too_large(),
    };
    let Ok(message) = std::str::from_utf8(&bytes) else {
        return bad_request("the body is not UTF-8".into());
    };
    if is_initialize(message) {
        // A client that handshakes again on its session replaces it, rather than
        // leaving it to the idle timeout.
        if let Some(id) = session_id
            && mcp.find(app, id, &credential).is_some()
        {
            mcp.end(app, id, "replaced by a new initialize");
        }
        return initialize(app, mcp, message, peer, credential).await;
    }
    let server = match session_id {
        None => mcp.stateless(app, peer, credential),
        Some(id) => match mcp.find(app, id, &credential) {
            Some(server) => server,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    "unknown or expired MCP session: send `initialize` again\n",
                )
                    .into_response();
            }
        },
    };
    reply(server.handle(message).await)
}

/// An `initialize`: a new session, kept only if the handshake succeeds.
async fn initialize(
    app: &App,
    mcp: &Mcp,
    message: &str,
    peer: SocketAddr,
    credential: Credential,
) -> Response {
    let (id, server) = match mcp.start(app, peer, credential) {
        Ok(started) => started,
        Err(StartError::Full) => {
            Refusal::McpSessions.log(peer);
            return Refusal::McpSessions.into_response();
        }
        Err(StartError::Random(error)) => {
            tracing::warn!(error = %error, "could not draw a session ID");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not open a session\n",
            )
                .into_response();
        }
    };
    let answer = server.handle(message).await;
    let succeeded = answer.as_deref().is_some_and(|text| {
        serde_json::from_str::<Value>(text).is_ok_and(|value| value.get("result").is_some())
    });
    let mut response = reply(answer);
    if succeeded {
        if let Ok(value) = HeaderValue::from_str(&id) {
            response.headers_mut().insert(SESSION_ID, value);
        }
    } else {
        mcp.end(app, &id, "initialize failed");
    }
    response
}

/// `DELETE`: ends the session the client names, if its credential opened it.
fn delete(app: &App, mcp: &Mcp, headers: &HeaderMap, credential: &Credential) -> Response {
    match single(headers, &SESSION_ID) {
        Ok(Some(id))
            if mcp.find(app, id, credential).is_some()
                && mcp.end(app, id, "deleted by the client") =>
        {
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Some(_)) => (StatusCode::NOT_FOUND, "unknown or expired MCP session\n").into_response(),
        _ => bad_request("`DELETE` needs one `Mcp-Session-Id`".into()),
    }
}

/// The server's answer: `200` with the JSON reply, or `202` when the message gets none.
fn reply(answer: Option<String>) -> Response {
    match answer {
        Some(text) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            Bytes::from(text),
        )
            .into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

fn too_large() -> Response {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        "the body is over 4 MiB, or could not be read\n",
    )
        .into_response()
}

fn bad_request(reason: String) -> Response {
    (StatusCode::BAD_REQUEST, format!("{reason}\n")).into_response()
}

/// Whether the body is declared JSON (`application/json`, with any parameters).
fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
}

/// Whether `message` is an `initialize` request.
fn is_initialize(message: &str) -> bool {
    serde_json::from_str::<Value>(message)
        .is_ok_and(|value| value.get("method").and_then(Value::as_str) == Some("initialize"))
}

/// The header's value if it appears at most once as visible ASCII; `Err` if it is
/// repeated or unreadable.
fn single<'h>(headers: &'h HeaderMap, name: &HeaderName) -> Result<Option<&'h str>, ()> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(());
    }
    value.to_str().map(Some).map_err(|_| ())
}
