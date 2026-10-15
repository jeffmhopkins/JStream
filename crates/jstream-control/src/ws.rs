//! [`WsControl`]: the [`Control`] trait over the engine's WebSocket API (feature `ws`,
//! native targets).
//!
//! One connection is one session. [`WsControl::connect`] opens it on a dedicated thread
//! that runs a single-threaded tokio runtime, so callers need no runtime of their own:
//! every future and stream it returns is completed through channels, whatever executor
//! (or `block_on`) polls it. That thread's task writes the requests, routes each
//! `response` or `error` to its waiting call by request ID, and fans `event`s out to the
//! subscriptions, and binary frames to the binary channel ([`Control::binary`]), which
//! holds at most [`BINARY_BUFFER`] of them. When the socket closes, pending and later
//! calls fail with [`ControlError::Disconnected`] and every event and binary stream
//! ends.
//!
//! The control token travels in the upgrade request's `Authorization` header, never in
//! the URL, and the request carries an `Origin` the server accepts (ADR-0006). Plain
//! `ws://` only: TLS is not supported yet.
//!
//! Docs: crates/jstream-control/README.md#design, docs/adr/0006-mcp-full-access.md

use crate::WS_PATH;
use crate::control::{BinaryStream, BoxFuture, Control, EventStream};
use crate::error::ControlError;
use futures_core::Stream;
use futures_util::{SinkExt, StreamExt};
use jstream_protocol::{
    BinaryFrame, Call, ClientMessage, Event, Hello, KeyframeGate, Request, RequestId, Response,
    ServerMessage, Welcome,
};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode, Uri, header};
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Where and how to connect.
#[derive(Clone)]
pub struct WsConfig {
    /// The engine's WebSocket URL, `ws://<host>:<port>[/path]`. Without a path (or with
    /// `/`) the API path `/api/v1/ws` is used.
    pub url: String,
    /// The control token, sent as `Authorization: Bearer <token>`.
    pub token: String,
    /// The `Origin` to send. `None` sends the engine's own origin (`http://<host>:<port>`
    /// of the URL), which the server always accepts.
    pub origin: Option<String>,
}

impl WsConfig {
    /// A configuration for `url` with `token` and the default `Origin`.
    pub fn new(url: impl Into<String>, token: impl Into<String>) -> Self {
        WsConfig {
            url: url.into(),
            token: token.into(),
            origin: None,
        }
    }
}

impl fmt::Debug for WsConfig {
    /// Never prints the token.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WsConfig")
            .field("url", &self.url)
            .field("token", &"***")
            .field("origin", &self.origin)
            .finish()
    }
}

/// One client session with an engine over its WebSocket API.
///
/// Dropping it closes the connection. Request IDs are `req_1`, `req_2`, … per session,
/// and the server reports them in the `state.changed` events the calls cause.
pub struct WsControl {
    commands: mpsc::UnboundedSender<Command>,
    shared: Arc<Shared>,
    next_request: AtomicU64,
}

impl fmt::Debug for WsControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WsControl")
            .field("closed", &*lock(&self.shared.closed))
            .finish_non_exhaustive()
    }
}

/// What the session's thread and the [`WsControl`] handle share.
#[derive(Default)]
struct Shared {
    /// The live event subscriptions; `None` once the connection has closed.
    subscribers: Mutex<Option<Vec<mpsc::UnboundedSender<Event>>>>,
    /// The binary channel, once [`Control::binary`] has opened it.
    binary: Mutex<Option<Arc<Mutex<BinaryQueue>>>>,
    /// Why the connection closed, once it has.
    closed: Mutex<Option<String>>,
}

impl Shared {
    /// `Disconnected`, with the reason the connection closed if it is known.
    fn disconnected(&self) -> ControlError {
        let reason = lock(&self.closed)
            .clone()
            .unwrap_or_else(|| "the connection closed".into());
        ControlError::Disconnected { reason }
    }
}

/// Locks `mutex`, ignoring poisoning: every critical section here is a plain push, take
/// or replace that leaves the data consistent.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A message from the handle to the session's thread.
enum Command {
    Hello(Hello, oneshot::Sender<Result<Welcome, ControlError>>),
    Call(Request, oneshot::Sender<Result<Response, ControlError>>),
}

fn disconnected(reason: impl Into<String>) -> ControlError {
    ControlError::Disconnected {
        reason: reason.into(),
    }
}

impl WsControl {
    /// Connects to the engine and opens a session. `hello` is still the caller's first
    /// call.
    ///
    /// Fails with [`ControlError::Disconnected`] if the URL is not a `ws://` URL, the
    /// engine can't be reached, or it refuses the upgrade (a wrong token is HTTP 401, a
    /// refused `Origin` or `Host` 403; the reason says which).
    pub fn connect(config: WsConfig) -> BoxFuture<'static, Result<WsControl, ControlError>> {
        let request = match upgrade_request(&config) {
            Ok(request) => request,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        let shared = Arc::new(Shared::default());
        *lock(&shared.subscribers) = Some(Vec::new());
        let (commands, receiver) = mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let thread_shared = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("jstream-ws".into())
            .spawn(move || session_thread(request, receiver, thread_shared, ready_tx));
        if let Err(error) = spawned {
            return Box::pin(std::future::ready(Err(disconnected(format!(
                "could not start the connection thread: {error}"
            )))));
        }
        Box::pin(async move {
            ready_rx
                .await
                .unwrap_or_else(|_| Err(disconnected("the connection thread stopped")))?;
            Ok(WsControl {
                commands,
                shared,
                next_request: AtomicU64::new(1),
            })
        })
    }
}

impl Control for WsControl {
    fn hello(&self, hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
        let (tx, rx) = oneshot::channel();
        let sent = self.commands.send(Command::Hello(hello, tx)).is_ok();
        let shared = Arc::clone(&self.shared);
        Box::pin(async move {
            if !sent {
                return Err(shared.disconnected());
            }
            rx.await.unwrap_or_else(|_| Err(shared.disconnected()))
        })
    }

    fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
        let id = RequestId::new(format!(
            "req_{}",
            self.next_request.fetch_add(1, Ordering::Relaxed)
        ));
        let (tx, rx) = oneshot::channel();
        let sent = self
            .commands
            .send(Command::Call(Request { id, call }, tx))
            .is_ok();
        let shared = Arc::clone(&self.shared);
        Box::pin(async move {
            if !sent {
                return Err(shared.disconnected());
            }
            rx.await.unwrap_or_else(|_| Err(shared.disconnected()))
        })
    }

    fn events(&self) -> EventStream {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Some(subscribers) = lock(&self.shared.subscribers).as_mut() {
            subscribers.push(tx);
        }
        // Once closed, `tx` is dropped here and the stream ends at once.
        Box::pin(Subscription(rx))
    }

    fn binary(&self) -> BinaryStream {
        let queue = Arc::new(Mutex::new(BinaryQueue::default()));
        let old = {
            let mut binary = lock(&self.shared.binary);
            if lock(&self.shared.closed).is_some() {
                lock(&queue).closed = true;
                None
            } else {
                binary.replace(Arc::clone(&queue))
            }
        };
        if let Some(old) = old {
            let waker = lock(&old).close();
            wake(waker);
        }
        Box::pin(BinaryChannel(queue))
    }
}

/// How many binary frames the binary channel holds for a reader that falls behind. At
/// a preview's 15 to 30 frames a second that is about half a second to a second.
pub(crate) const BINARY_BUFFER: usize = 16;

/// The frames waiting in the binary channel.
#[derive(Default)]
struct BinaryQueue {
    frames: VecDeque<BinaryFrame>,
    gate: KeyframeGate,
    waker: Option<Waker>,
    closed: bool,
}

impl BinaryQueue {
    /// Queues `frame` unless the gate drops it, and returns the waker to wake.
    fn push(&mut self, frame: BinaryFrame) -> Option<Waker> {
        let room = self.frames.len() < BINARY_BUFFER;
        if self.gate.admit(&frame.header, room) {
            self.frames.push_back(frame);
            self.waker.take()
        } else {
            None
        }
    }

    /// Ends the stream once it has yielded what it holds.
    fn close(&mut self) -> Option<Waker> {
        self.closed = true;
        self.waker.take()
    }
}

/// The binary channel's stream.
struct BinaryChannel(Arc<Mutex<BinaryQueue>>);

impl Stream for BinaryChannel {
    type Item = BinaryFrame;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<BinaryFrame>> {
        let mut queue = lock(&self.0);
        if let Some(frame) = queue.frames.pop_front() {
            Poll::Ready(Some(frame))
        } else if queue.closed {
            Poll::Ready(None)
        } else {
            queue.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

/// Wakes `waker`, if there is one, with no lock held.
fn wake(waker: Option<Waker>) {
    if let Some(waker) = waker {
        waker.wake();
    }
}

/// One event subscription: the receiving end of the channel the session's thread feeds.
struct Subscription(mpsc::UnboundedReceiver<Event>);

impl Stream for Subscription {
    type Item = Event;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Event>> {
        self.0.poll_recv(cx)
    }
}

/// Builds the upgrade request: the URL (with the default API path), the bearer token and
/// the `Origin`.
fn upgrade_request(
    config: &WsConfig,
) -> Result<tungstenite::handshake::client::Request, ControlError> {
    let invalid = |why: &str| disconnected(format!("invalid engine URL `{}`: {why}", config.url));
    let uri: Uri = config.url.parse().map_err(|_| invalid("not a URL"))?;
    match uri.scheme_str() {
        Some("ws") => {}
        Some("wss") => return Err(invalid("TLS (`wss://`) is not supported yet; use `ws://`")),
        _ => return Err(invalid("the scheme must be `ws://`")),
    }
    let authority = uri
        .authority()
        .ok_or_else(|| invalid("it has no host"))?
        .as_str()
        .to_owned();
    let path = match uri.path_and_query().map(|p| p.as_str()) {
        None | Some("") | Some("/") => WS_PATH,
        Some(path) => path,
    };
    let url = format!("ws://{authority}{path}");
    let mut request = url
        .into_client_request()
        .map_err(|error| invalid(&error.to_string()))?;
    let bearer = HeaderValue::from_str(&format!("Bearer {}", config.token))
        .map_err(|_| disconnected("the control token contains characters a header can't carry"))?;
    let origin = config
        .origin
        .clone()
        .unwrap_or_else(|| format!("http://{authority}"));
    let origin = HeaderValue::from_str(&origin)
        .map_err(|_| disconnected(format!("invalid Origin `{origin}`")))?;
    let headers = request.headers_mut();
    headers.insert(header::AUTHORIZATION, bearer);
    headers.insert(header::ORIGIN, origin);
    Ok(request)
}

/// Explains a failed connection attempt.
fn connect_failure(error: tungstenite::Error) -> ControlError {
    match error {
        tungstenite::Error::Http(response) => match response.status() {
            StatusCode::UNAUTHORIZED => {
                disconnected("the engine refused the control token (HTTP 401)")
            }
            StatusCode::FORBIDDEN => {
                disconnected("the engine refused the connection's Origin or Host (HTTP 403)")
            }
            status => disconnected(format!("the engine refused the connection (HTTP {status})")),
        },
        other => disconnected(format!("could not connect to the engine: {other}")),
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The body of the connection thread: connects, reports the outcome on `ready`, then runs
/// the session until either side closes.
fn session_thread(
    request: tungstenite::handshake::client::Request,
    commands: mpsc::UnboundedReceiver<Command>,
    shared: Arc<Shared>,
    ready: oneshot::Sender<Result<(), ControlError>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            // Nobody is left to tell if the caller stopped waiting.
            let _ = ready.send(Err(disconnected(format!(
                "could not start the connection runtime: {error}"
            ))));
            return;
        }
    };
    runtime.block_on(async move {
        match tokio_tungstenite::connect_async(request).await {
            Ok((socket, _)) => {
                if ready.send(Ok(())).is_ok() {
                    run_session(socket, commands, &shared).await;
                }
            }
            Err(error) => {
                // As above: the caller may have stopped waiting.
                let _ = ready.send(Err(connect_failure(error)));
            }
        }
    });
}

/// The calls and handshakes waiting for an answer.
#[derive(Default)]
struct Pending {
    calls: HashMap<RequestId, oneshot::Sender<Result<Response, ControlError>>>,
    /// `welcome` carries no ID: handshakes are answered in order.
    hellos: VecDeque<oneshot::Sender<Result<Welcome, ControlError>>>,
}

/// Runs one connected session, then fails whatever is still waiting and ends the event
/// streams.
async fn run_session(
    socket: Socket,
    mut commands: mpsc::UnboundedReceiver<Command>,
    shared: &Shared,
) {
    let (mut sink, mut stream) = socket.split();
    let mut pending = Pending::default();
    let reason = loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    // The `WsControl` was dropped. Best effort: the peer may be gone.
                    let _ = sink.send(Message::Close(None)).await;
                    break "the session was closed".to_owned();
                };
                let message = match command {
                    Command::Hello(hello, tx) => {
                        pending.hellos.push_back(tx);
                        ClientMessage::Hello(hello)
                    }
                    Command::Call(request, tx) => {
                        pending.calls.insert(request.id.clone(), tx);
                        ClientMessage::Request(request)
                    }
                };
                let text = match serde_json::to_string(&message) {
                    Ok(text) => text,
                    Err(error) => break format!("could not encode a request: {error}"),
                };
                if let Err(error) = sink.send(Message::Text(text.into())).await {
                    break format!("could not send to the engine: {error}");
                }
            }
            message = stream.next() => match message {
                None => break "the engine closed the connection".to_owned(),
                Some(Err(error)) => break format!("the connection failed: {error}"),
                Some(Ok(Message::Text(text))) => {
                    if let Err(reason) = dispatch(&text, &mut pending, shared) {
                        break reason;
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    break match frame {
                        Some(frame) if !frame.reason.is_empty() => {
                            format!("the engine closed the connection: {}", frame.reason)
                        }
                        _ => "the engine closed the connection".to_owned(),
                    };
                }
                Some(Ok(Message::Binary(bytes))) => deliver(&bytes, shared),
                // Pings are answered by tungstenite.
                Some(Ok(_)) => {}
            },
        }
    };
    *lock(&shared.closed) = Some(reason.clone());
    // Ends every event stream once it has yielded what it buffered, and the binary one.
    lock(&shared.subscribers).take();
    let binary = lock(&shared.binary).take();
    if let Some(binary) = binary {
        let waker = lock(&binary).close();
        wake(waker);
    }
    commands.close();
    while let Ok(command) = commands.try_recv() {
        match command {
            Command::Hello(_, tx) => drop(tx.send(Err(disconnected(reason.clone())))),
            Command::Call(_, tx) => drop(tx.send(Err(disconnected(reason.clone())))),
        }
    }
    for (_, tx) in pending.calls.drain() {
        // A caller that stopped waiting needs no answer.
        let _ = tx.send(Err(disconnected(reason.clone())));
    }
    for tx in pending.hellos.drain(..) {
        let _ = tx.send(Err(disconnected(reason.clone())));
    }
}

/// Queues one binary frame for the binary channel, if it is open. A frame this version
/// can't read (a newer header version, kind or codec) is skipped, as the protocol asks.
fn deliver(bytes: &[u8], shared: &Shared) {
    let Ok(frame) = BinaryFrame::parse(bytes) else {
        return;
    };
    let binary = lock(&shared.binary).clone();
    if let Some(binary) = binary {
        let waker = lock(&binary).push(frame);
        wake(waker);
    }
}

/// Routes one message from the engine. An unreadable message ends the session (`Err`
/// with the reason).
fn dispatch(text: &str, pending: &mut Pending, shared: &Shared) -> Result<(), String> {
    let message: ServerMessage = serde_json::from_str(text)
        .map_err(|error| format!("the engine sent an unreadable message: {error}"))?;
    // A caller that stopped waiting needs no answer, so failed sends are ignored.
    match message {
        ServerMessage::Welcome(welcome) => {
            if let Some(tx) = pending.hellos.pop_front() {
                let _ = tx.send(Ok(welcome));
            }
        }
        ServerMessage::Response(response) => {
            if let Some(tx) = pending.calls.remove(&response.id) {
                let _ = tx.send(Ok(response));
            }
        }
        ServerMessage::Error(error) => match error.id {
            Some(id) => {
                if let Some(tx) = pending.calls.remove(&id) {
                    let _ = tx.send(Err(ControlError::Protocol(error.error)));
                }
            }
            // An error without an ID answers a handshake (or a message too broken to
            // read, which this client never sends).
            None => {
                if let Some(tx) = pending.hellos.pop_front() {
                    let _ = tx.send(Err(ControlError::Protocol(error.error)));
                }
            }
        },
        ServerMessage::Event(message) => {
            if let Some(subscribers) = lock(&shared.subscribers).as_mut() {
                subscribers.retain(|tx| tx.send(message.event.clone()).is_ok());
            }
        }
        // A message type from a newer server: ignored, as the protocol asks.
        _ => {}
    }
    Ok(())
}
