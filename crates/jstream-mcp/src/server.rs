//! The MCP server: answers one JSON-RPC message at a time, over any [`Control`] session.
//!
//! It is dual-era (crates/jstream-mcp/README.md#design). A request whose
//! `_meta` carries `io.modelcontextprotocol/protocolVersion` is served statelessly
//! under MCP 2026-07-28. A client that opens with `initialize` is served under the
//! handshake-based revision it negotiated (2025-11-25 or 2025-06-18) for the rest of the
//! connection.

use crate::events::{BUFFER_CAPACITY, CursorAhead, Feed};
use crate::jsonrpc::{self, Incoming, RpcError};
use crate::tools::{Invocation, Tools};
use jstream_control::{Control, ControlError};
use jstream_protocol::{Hello, PROTOCOL_VERSION, ProtocolError, WireError, json_schema};
use serde_json::{Map, Value, json};
use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::task::{Poll, Waker};
use std::thread;
use std::time::Duration;
use tracing::Instrument;

/// The MCP revision served statelessly, with per-request metadata.
pub const MODERN_VERSION: &str = "2026-07-28";

/// The handshake-based MCP revisions served after `initialize`, newest first.
pub const LEGACY_VERSIONS: [&str; 2] = ["2025-11-25", "2025-06-18"];

/// The server's name in `serverInfo`.
const SERVER_NAME: &str = "jstream-mcp";

/// The client name sent in the engine `hello` when the MCP client gave none.
const DEFAULT_CLIENT_NAME: &str = "jstream-mcp";

/// How long a modern client may cache `server/discover` and `tools/list`: the tools are
/// generated at build time, so they only change with the binary.
const TOOLS_TTL_MS: u64 = 3_600_000;

/// How long `initialize` and `server/discover` wait for the engine's answer to `hello`
/// before answering anyway. Well under `jstream-server`'s 10 s handshake timeout, so an
/// engine that accepted the connection but never answers doesn't hold the request until
/// that close.
const OPEN_EARLY_TIMEOUT: Duration = Duration::from_secs(3);

const META: &str = "_meta";
const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

/// Locks `mutex`, ignoring poisoning: the guarded values are replaced whole, so they are
/// never left half-written.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What `initialize` settled, for a client on a handshake-based revision.
#[derive(Clone, Debug)]
struct LegacySession {
    client_name: Option<String>,
}

/// Which revision a request is served under.
enum Era {
    Modern,
    Legacy,
}

/// An MCP server that exposes one engine session to agents.
///
/// It owns one [`Control`] session: every tool call acts as that session's client, so
/// its edits form one undo history and carry one name in the audit log (ADR-0013). The
/// session's `hello` is sent as soon as the MCP client introduces itself, with its name:
/// at `initialize` (handshake-based revisions) or at `server/discover` (2026-07-28). So a
/// server that times out a silent connection (`jstream-server`'s handshake timeout)
/// doesn't close the session while the agent is still idle. If that `hello` fails, or the
/// client skipped both, it is sent before the next tool call that needs the engine. Just
/// before the first `hello`, the server subscribes to the session's events and buffers
/// them for the `events` tool.
///
/// [`handle`](Server::handle) answers one message. It needs no particular executor: the
/// future only awaits the [`Control`] futures. [`serve_stdio`](crate::serve_stdio) runs it
/// over a byte stream.
///
/// Logging (R-RS-005): each server has an `mcp_session` span, every request runs in an
/// `mcp_request` span under it naming the method, and each tool call in a `tool_call`
/// span naming the tool, also directly under `mcp_session`. Tool arguments and results are never logged (R-SEC-001), only
/// names, codes and whether the call failed.
pub struct Server {
    control: Arc<dyn Control>,
    span: tracing::Span,
    tools: Tools,
    /// Whether `hello` succeeded on `control`.
    opened: AtomicBool,
    /// The event subscription, opened before the first `hello`.
    feed: OnceLock<Feed>,
    /// Set by `initialize`.
    legacy: Mutex<Option<LegacySession>>,
    /// How long [`Server::open_early`] waits for `hello`.
    open_early_timeout: Duration,
}

impl Server {
    /// A server over `control`, a session that has not sent `hello` yet (or has: a
    /// second `hello` returns the same identity).
    pub fn new(control: Arc<dyn Control>) -> Self {
        Server {
            control,
            span: tracing::info_span!("mcp_session"),
            tools: Tools::generate(),
            opened: AtomicBool::new(false),
            feed: OnceLock::new(),
            legacy: Mutex::new(None),
            open_early_timeout: OPEN_EARLY_TIMEOUT,
        }
    }

    /// The same server with another wait for `hello` at `initialize`, for tests.
    #[cfg(test)]
    pub(crate) fn with_open_early_timeout(mut self, timeout: Duration) -> Self {
        self.open_early_timeout = timeout;
        self
    }

    /// Answers one JSON-RPC message (one line of the stdio transport, without its
    /// newline). Returns the reply, a single line of JSON, or `None` for a notification
    /// or anything else that gets no reply.
    pub async fn handle(&self, message: &str) -> Option<String> {
        let reply = match jsonrpc::parse(message) {
            Err(reply) => reply,
            Ok(Incoming::Notification | Incoming::Response) => return None,
            Ok(Incoming::Request { id, method, params }) => {
                let span =
                    tracing::debug_span!(parent: &self.span, "mcp_request", method = %method);
                match self
                    .request(&method, &params)
                    .instrument(span.clone())
                    .await
                {
                    Ok(result) => jsonrpc::result_reply(&id, result),
                    Err(error) => {
                        span.in_scope(|| tracing::debug!(code = error.code, "request failed"));
                        jsonrpc::error_reply(Some(&id), &error)
                    }
                }
            }
        };
        Some(reply.to_string())
    }

    async fn request(&self, method: &str, params: &Map<String, Value>) -> Result<Value, RpcError> {
        if method == "initialize" {
            let result = self.initialize(params)?;
            let client_name = lock(&self.legacy)
                .as_ref()
                .and_then(|s| s.client_name.clone());
            self.open_early(client_name).await;
            return Ok(result);
        }
        let meta = params.get(META).and_then(Value::as_object);
        let era = match meta.and_then(|m| m.get(META_PROTOCOL_VERSION)) {
            Some(Value::String(version)) if version == MODERN_VERSION => Era::Modern,
            Some(version) => {
                let legacy = version
                    .as_str()
                    .is_some_and(|v| LEGACY_VERSIONS.contains(&v));
                if !(legacy && lock(&self.legacy).is_some()) {
                    return Err(unsupported_version(version));
                }
                Era::Legacy
            }
            // Handshake-based revisions allow `ping` before `initialize`.
            None if method == "ping" || lock(&self.legacy).is_some() => Era::Legacy,
            None => {
                return Err(RpcError::invalid_params(format!(
                    "`params._meta` must carry `{META_PROTOCOL_VERSION}` (MCP {MODERN_VERSION}), \
                     or the client must open with `initialize` (MCP {})",
                    LEGACY_VERSIONS.join(" or ")
                )));
            }
        };
        match era {
            Era::Modern => {
                if !meta.is_some_and(|m| {
                    m.get(META_CLIENT_CAPABILITIES)
                        .is_some_and(Value::is_object)
                }) {
                    return Err(RpcError::invalid_params(format!(
                        "`params._meta` must carry `{META_CLIENT_CAPABILITIES}` (an object)"
                    )));
                }
                let client_name = meta
                    .and_then(|m| m.get(META_CLIENT_INFO))
                    .and_then(|info| info.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let mut result = match method {
                    "server/discover" => {
                        self.open_early(client_name).await;
                        self.discover()
                    }
                    "tools/list" => self.list(params)?,
                    "tools/call" => self.call(params, client_name).await?,
                    _ => return Err(method_not_found(method)),
                };
                if let Some(object) = result.as_object_mut() {
                    object.insert("resultType".into(), json!("complete"));
                    object.insert(META.into(), json!({ META_SERVER_INFO: server_info() }));
                }
                Ok(result)
            }
            Era::Legacy => match method {
                "ping" => Ok(json!({})),
                "tools/list" => {
                    let mut result = self.list(params)?;
                    if let Some(object) = result.as_object_mut() {
                        object.remove("ttlMs");
                        object.remove("cacheScope");
                    }
                    Ok(result)
                }
                "tools/call" => {
                    let client_name = lock(&self.legacy)
                        .as_ref()
                        .and_then(|s| s.client_name.clone());
                    self.call(params, client_name).await
                }
                _ => Err(method_not_found(method)),
            },
        }
    }

    /// `initialize` (handshake-based revisions): settles the revision for this connection.
    fn initialize(&self, params: &Map<String, Value>) -> Result<Value, RpcError> {
        let Some(requested) = params.get("protocolVersion").and_then(Value::as_str) else {
            return Err(RpcError::invalid_params(
                "`protocolVersion` (a string) is required",
            ));
        };
        // The client's version if served, else the newest; the client decides whether
        // it can use that.
        let version = LEGACY_VERSIONS
            .iter()
            .find(|v| **v == requested)
            .unwrap_or(&LEGACY_VERSIONS[0]);
        let client_name = params
            .get("clientInfo")
            .and_then(|info| info.get("name"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        tracing::info!(
            client_name = client_name.as_deref().unwrap_or(""),
            version,
            "initialized"
        );
        *lock(&self.legacy) = Some(LegacySession { client_name });
        Ok(json!({
            "protocolVersion": version,
            "capabilities": {"tools": {}},
            "serverInfo": server_info(),
            "instructions": self.instructions(),
        }))
    }

    /// `server/discover` (2026-07-28).
    fn discover(&self) -> Value {
        json!({
            "supportedVersions": supported_versions(),
            "capabilities": {"tools": {}},
            "instructions": self.instructions(),
            "ttlMs": TOOLS_TTL_MS,
            "cacheScope": "public",
        })
    }

    /// `tools/list`: every tool on one page.
    fn list(&self, params: &Map<String, Value>) -> Result<Value, RpcError> {
        if params.contains_key("cursor") {
            return Err(RpcError::invalid_params(
                "invalid cursor: the tool list has a single page",
            ));
        }
        Ok(json!({
            "tools": self.tools.definitions(),
            "ttlMs": TOOLS_TTL_MS,
            "cacheScope": "public",
        }))
    }

    /// `tools/call`, in a `tool_call` span naming the tool. Logs the outcome: the error
    /// code of a failed call, never its arguments or result.
    async fn call(
        &self,
        params: &Map<String, Value>,
        client_name: Option<String>,
    ) -> Result<Value, RpcError> {
        let tool = params.get("name").and_then(Value::as_str).unwrap_or("");
        // A child of the session, not of `mcp_request`: that span is `debug`, so at the
        // default `info` level it is disabled and would cut `tool_call` off from it.
        let span = tracing::info_span!(parent: &self.span, "tool_call", tool = %tool);
        let result = self
            .call_tool(params, client_name)
            .instrument(span.clone())
            .await;
        span.in_scope(|| match &result {
            Ok(value) if value.get("isError") == Some(&Value::Bool(true)) => {
                let code = value
                    .pointer("/structuredContent/error/code")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                tracing::debug!(code, "tool call failed");
            }
            Ok(_) => tracing::debug!("tool call done"),
            // A JSON-RPC error: `handle` logs it as a failed request.
            Err(_) => {}
        });
        result
    }

    /// `tools/call`. An unknown tool or a request that doesn't fit `CallToolRequest` is a
    /// JSON-RPC error. Everything else, including arguments that don't fit the tool and
    /// every engine error, is a result with `isError`, so the agent can correct itself.
    async fn call_tool(
        &self,
        params: &Map<String, Value>,
        client_name: Option<String>,
    ) -> Result<Value, RpcError> {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return Err(RpcError::invalid_params("`name` (a string) is required"));
        };
        let arguments = match params.get("arguments") {
            None => json!({}),
            Some(arguments @ Value::Object(_)) => arguments.clone(),
            Some(_) => return Err(RpcError::invalid_params("`arguments` must be an object")),
        };
        let Some(invocation) = self.tools.decode(name, arguments) else {
            return Err(RpcError::invalid_params(format!("Unknown tool: {name}")));
        };
        let invocation = match invocation {
            Err(error) => return Ok(protocol_error(error.into())),
            Ok(Invocation::Describe) => return Ok(tool_result(json_schema(), false)),
            Ok(Invocation::Events { after }) if self.feed.get().is_some() => {
                return Ok(self.events(after));
            }
            Ok(invocation) => invocation,
        };
        let client_name = client_name.unwrap_or_else(|| DEFAULT_CLIENT_NAME.to_owned());
        if let Err(error) = self.open(client_name).await {
            tracing::warn!(error = %error, "could not open the engine session");
            return Ok(match error {
                // The call was never sent. Over `WsControl` this is permanent: a closed
                // connection never reopens, so every later `hello` fails the same way.
                ControlError::Disconnected { reason } => disconnected(format!(
                    "the engine session could not be opened: {reason}; this call was not \
                     sent. Restart the MCP server to get a new session. It is a \
                     new client of the engine, with its own undo history: edits made before \
                     can't be undone from it"
                )),
                other => control_error(other),
            });
        }
        let call = match invocation {
            Invocation::Call(call) => call,
            // The first `events` call: the session is open now, and so is the feed.
            Invocation::Events { after } => return Ok(self.events(after)),
            Invocation::Describe => return Ok(tool_result(json_schema(), false)),
        };
        Ok(match self.control.call(call).await {
            Ok(response) => match serde_json::to_value(&response) {
                Ok(value) => tool_result(value, false),
                Err(e) => protocol_error(
                    ProtocolError::Internal {
                        reason: format!("could not encode the response: {e}"),
                    }
                    .into(),
                ),
            },
            Err(ControlError::Disconnected { reason }) => {
                // The session may be gone: say `hello` again before the next call.
                self.opened.store(false, Ordering::Release);
                tracing::warn!(reason = %reason, "engine connection lost during a tool call");
                disconnected(format!(
                    "connection to the engine lost: {reason}; whether the call took effect \
                     is unknown, so re-read the state before retrying. The next call tries \
                     to reopen the session"
                ))
            }
            Err(other) => control_error(other),
        })
    }

    /// The `events` tool: the buffered events after `after`. It sends nothing to the
    /// engine, so it still answers once the session is gone, with `ended` set.
    fn events(&self, after: u64) -> Value {
        let Some(feed) = self.feed.get() else {
            return protocol_error(
                ProtocolError::Internal {
                    reason: "the event subscription is not open".into(),
                }
                .into(),
            );
        };
        match feed.read(after) {
            Ok(page) => match serde_json::to_value(&page) {
                Ok(value) => tool_result(value, false),
                Err(e) => protocol_error(
                    ProtocolError::Internal {
                        reason: format!("could not encode the events: {e}"),
                    }
                    .into(),
                ),
            },
            Err(CursorAhead { after, newest }) => protocol_error(
                ProtocolError::Malformed {
                    reason: format!(
                        "`after` is {after}, but the newest event is {newest}: the cursor \
                         is from another MCP server session. Read again without `after`, \
                         and re-read the state"
                    ),
                }
                .into(),
            ),
        }
    }

    /// Opens the engine session when the MCP client introduces itself (`initialize`,
    /// `server/discover`). A failure, or no answer within the open-early timeout, doesn't
    /// fail the request: the next tool call that needs the engine tries again and reports
    /// it.
    async fn open_early(&self, client_name: Option<String>) {
        let client_name = client_name.unwrap_or_else(|| DEFAULT_CLIENT_NAME.to_owned());
        match with_timeout(self.open(client_name), self.open_early_timeout).await {
            Some(Ok(())) => {}
            Some(Err(error)) => tracing::warn!(
                error = %error,
                "could not open the engine session; the next tool call tries again"
            ),
            None => tracing::warn!(
                "the engine didn't answer hello in time; the next tool call tries again"
            ),
        }
    }

    /// Sends `hello` once, subscribing to the session's events just before the first
    /// one. Concurrent first calls may both send it; a second `hello` returns the same
    /// identity, so that is harmless.
    async fn open(&self, client_name: String) -> Result<(), ControlError> {
        if self.opened.load(Ordering::Acquire) {
            return Ok(());
        }
        self.feed
            .get_or_init(|| Feed::start(self.control.events(), BUFFER_CAPACITY));
        let hello = Hello {
            protocol: PROTOCOL_VERSION,
            client_name,
        };
        let welcome = self.control.hello(hello).await?;
        self.opened.store(true, Ordering::Release);
        tracing::info!(client_id = %welcome.client_id, "engine session opened");
        Ok(())
    }

    /// Guidance for the model, from `initialize` and `server/discover`.
    fn instructions(&self) -> String {
        let groups: Vec<String> = self.tools.groups().map(|g| format!("`{g}`")).collect();
        format!(
            "Controls JStream, a live-streaming and recording app, through its protocol. \
             Each of the {} tools runs one command or query on that noun, chosen by \
             `op.type`. `batch` applies several edits atomically. Edits are undoable per \
             client and each applied batch raises the state `rev` by one; pass \
             `expected_rev` to make a read-modify-write safe. IDs of new objects are \
             chosen by you. `events` returns what changed since your last read, from \
             every client. `describe_api` returns the full protocol schema. \
             `video` with `op.type` `video.snapshot` returns the current program mix as \
             a PNG image content block; the JSON result's `png` field only names that \
             block and its size.",
            groups.join(", ")
        )
    }
}

/// Runs `future` for at most `limit`: its output, or `None` if the time ran out first
/// (the future is then dropped). It needs no async runtime: a timer thread, started only
/// if `future` doesn't finish on its first poll, wakes the task when the time is up, and
/// exits early once the wait is over either way.
async fn with_timeout<F: Future>(future: F, limit: Duration) -> Option<F::Output> {
    let mut future = pin!(future);
    let mut timer: Option<Timer> = None;
    poll_fn(|cx| {
        if let Poll::Ready(output) = future.as_mut().poll(cx) {
            return Poll::Ready(Some(output));
        }
        let timer = timer.get_or_insert_with(|| Timer::start(limit));
        if timer.fired(cx.waker()) {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    })
    .await
}

/// What a [`Timer`] and its thread share.
#[derive(Default)]
struct TimerState {
    fired: bool,
    cancelled: bool,
    waker: Option<Waker>,
}

/// A one-shot timer on its own thread, cancelled when dropped.
struct Timer(Arc<(Mutex<TimerState>, Condvar)>);

impl Timer {
    fn start(limit: Duration) -> Timer {
        let shared = Arc::new((Mutex::new(TimerState::default()), Condvar::new()));
        let theirs = Arc::clone(&shared);
        let spawned = thread::Builder::new()
            .name("jstream-mcp-timer".into())
            .spawn(move || {
                let (state, cancel) = &*theirs;
                let (mut state, _) = cancel
                    .wait_timeout_while(lock(state), limit, |s| !s.cancelled)
                    .unwrap_or_else(PoisonError::into_inner);
                if !state.cancelled {
                    state.fired = true;
                    if let Some(waker) = state.waker.take() {
                        waker.wake();
                    }
                }
            });
        if spawned.is_err() {
            // No thread to time it: stop waiting at once rather than possibly forever.
            lock(&shared.0).fired = true;
        }
        Timer(shared)
    }

    /// Whether the time is up; if not, `waker` is woken when it is.
    fn fired(&self, waker: &Waker) -> bool {
        let mut state = lock(&self.0.0);
        if !state.fired {
            state.waker = Some(waker.clone());
        }
        state.fired
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        lock(&self.0.0).cancelled = true;
        self.0.1.notify_all();
    }
}

fn server_info() -> Value {
    json!({"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")})
}

fn supported_versions() -> Vec<&'static str> {
    std::iter::once(MODERN_VERSION)
        .chain(LEGACY_VERSIONS)
        .collect()
}

fn method_not_found(method: &str) -> RpcError {
    RpcError::new(
        jsonrpc::METHOD_NOT_FOUND,
        format!("method not found: {method}"),
    )
}

fn unsupported_version(requested: &Value) -> RpcError {
    RpcError {
        code: jsonrpc::UNSUPPORTED_PROTOCOL_VERSION,
        message: "Unsupported protocol version".into(),
        data: Some(json!({"supported": supported_versions(), "requested": requested})),
    }
}

/// A `CallToolResult`: `structured` as `structuredContent`, and serialized in a text
/// block for clients that only read `content`.
///
/// A successful `video.snapshot` result also leads with an image block, so an agent can
/// see the program mix. The base64 PNG goes only there: in the text block and in
/// `structuredContent` its `png` field is replaced by [`png_placeholder`], so the frame
/// (up to about 5 MB of base64) isn't repeated into the agent's text context.
fn tool_result(mut structured: Value, is_error: bool) -> Value {
    let mut content = Vec::new();
    if let Some(png) = take_snapshot_png(&mut structured) {
        content.push(json!({"type": "image", "data": png, "mimeType": "image/png"}));
    }
    content.push(json!({"type": "text", "text": structured.to_string()}));
    json!({
        "content": content,
        "structuredContent": structured,
        "isError": is_error,
    })
}

/// If `structured` is a successful `video.snapshot` response, moves its base64 PNG out
/// and leaves [`png_placeholder`] in its place.
fn take_snapshot_png(structured: &mut Value) -> Option<String> {
    let result = structured.pointer_mut("/result/result")?;
    if result.get("type")?.as_str()? != "video.snapshot" {
        return None;
    }
    let png = result.get_mut("png")?;
    let len = png.as_str()?.len();
    match std::mem::replace(png, Value::String(png_placeholder(len))) {
        Value::String(data) => Some(data),
        _ => None,
    }
}

/// What a `video.snapshot` result's `png` field holds over MCP: where the image went and
/// how long its base64 is (crates/jstream-mcp/README.md#design).
fn png_placeholder(base64_len: usize) -> String {
    format!("<in the image content block: {base64_len} bytes of base64>")
}

/// A tool error with the MCP-layer code `disconnected`: the engine session's connection
/// was lost (crates/jstream-mcp/README.md#design).
fn disconnected(message: String) -> Value {
    tool_result(
        json!({"error": {"code": "disconnected", "message": message}}),
        true,
    )
}

/// A tool error for a [`ControlError`] other than `Disconnected`.
fn control_error(error: ControlError) -> Value {
    match error {
        ControlError::Protocol(wire) => protocol_error(wire),
        other => protocol_error(
            ProtocolError::Internal {
                reason: other.to_string(),
            }
            .into(),
        ),
    }
}

/// A tool error carrying a protocol error: `{"error": {"code": …, …, "message": …}}`.
fn protocol_error(error: WireError) -> Value {
    tool_result(json!({ "error": error }), true)
}
