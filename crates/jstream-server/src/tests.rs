//! End-to-end tests: a [`MockEngine`] behind the server on an ephemeral `127.0.0.1` port,
//! and `jstream-control`'s WebSocket client (or a raw WebSocket) in front. Hermetic: only
//! loopback sockets (R-TEST-004).

use crate::{Connector, Server, ServerConfig, Token};
use futures_util::{SinkExt, StreamExt};
use jstream_control::protocol::{
    Action, ActionResult, Batch, BinaryFrame, BinaryHeader, Call, CallResult, ClientId, DeviceId,
    DeviceInfo, Edit, Event, Hello, OutputId, OutputState, PROTOCOL_VERSION, PreviewCodec,
    ProtocolError, ProtocolVersion, Query, QueryResult, RequestId, Response, Rev, SceneId,
    ServerMessage, StreamKind, Welcome,
};
use jstream_control::{
    BinaryStream, BoxFuture, Control, ControlError, EventStream, MockEngine, WsConfig, WsControl,
};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::{Notify, Semaphore, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode, header};
use tokio_tungstenite::tungstenite::{self, Message};

const TOKEN: &str = "test-token-0123";

/// A server serving `engine`, and the runtime it runs on.
struct Harness {
    runtime: Runtime,
    url: String,
    /// `host:port` of the server.
    authority: String,
    /// How many sessions the connector has opened.
    sessions: Arc<AtomicUsize>,
    /// How many of those sessions the server still holds: a session is dropped when its
    /// connection task has ended.
    open: Arc<AtomicUsize>,
    /// How many events the server's sessions have taken from the engine, over all
    /// connections.
    taken: Arc<AtomicUsize>,
    /// While set, the engine never answers `hello`, as one that accepted the session and
    /// then hangs.
    silent: Arc<AtomicBool>,
    stop: Option<oneshot::Sender<()>>,
    /// Whether the server was configured as reached over TLS, so its own origin is
    /// `https://`.
    tls: bool,
    /// The serving task: done once `serve_with_shutdown` has returned.
    serving: Option<tokio::task::JoinHandle<Result<(), crate::ServerError>>>,
}

fn start(engine: &MockEngine) -> Harness {
    start_with(engine, |_| {})
}

/// A server serving `engine`, with its configuration changed by `configure`.
fn start_with(engine: &MockEngine, configure: impl FnOnce(&mut ServerConfig)) -> Harness {
    start_on(multi_thread(), engine, configure)
}

/// [`start_with`] on a runtime that runs every task on the thread that calls
/// [`Harness::run`], so a subscriber set there sees every log line.
fn start_on_this_thread(engine: &MockEngine, configure: impl FnOnce(&mut ServerConfig)) -> Harness {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    start_on(runtime, engine, configure)
}

/// The runtime the tests' servers run on, unless they ask for another.
fn multi_thread() -> std::io::Result<Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
}

fn start_on(
    runtime: std::io::Result<Runtime>,
    engine: &MockEngine,
    configure: impl FnOnce(&mut ServerConfig),
) -> Harness {
    let sessions = Arc::new(AtomicUsize::new(0));
    let open = Arc::new(AtomicUsize::new(0));
    let taken = Arc::new(AtomicUsize::new(0));
    let silent = Arc::new(AtomicBool::new(false));
    let counters = (
        Arc::clone(&sessions),
        Arc::clone(&open),
        Arc::clone(&taken),
        Arc::clone(&silent),
    );
    let engine = engine.clone();
    let connector: Connector = Arc::new(move || {
        let (sessions, open, taken, silent) = &counters;
        sessions.fetch_add(1, Ordering::SeqCst);
        open.fetch_add(1, Ordering::SeqCst);
        Box::new(Tracked {
            inner: engine.connect(),
            open: Arc::clone(open),
            taken: Arc::clone(taken),
            silent: Arc::clone(silent),
        })
    });
    serve_on(runtime, connector, configure, sessions, open, taken, silent)
}

/// A server opening its sessions with `connector`, counting them in `sessions`,
/// `open` and `taken`, with `silent` as its [`Harness`]'s flag (see [`Harness`]). The
/// connector's sessions honour `silent` only if they are [`Tracked`].
fn serve(
    connector: Connector,
    configure: impl FnOnce(&mut ServerConfig),
    sessions: Arc<AtomicUsize>,
    open: Arc<AtomicUsize>,
    taken: Arc<AtomicUsize>,
    silent: Arc<AtomicBool>,
) -> Harness {
    serve_on(
        multi_thread(),
        connector,
        configure,
        sessions,
        open,
        taken,
        silent,
    )
}

/// [`serve`] on `runtime`.
fn serve_on(
    runtime: std::io::Result<Runtime>,
    connector: Connector,
    configure: impl FnOnce(&mut ServerConfig),
    sessions: Arc<AtomicUsize>,
    open: Arc<AtomicUsize>,
    taken: Arc<AtomicUsize>,
    silent: Arc<AtomicBool>,
) -> Harness {
    let runtime = match runtime {
        Ok(runtime) => runtime,
        Err(error) => panic!("runtime: {error}"),
    };
    let mut config = match Token::new(TOKEN) {
        Ok(token) => ServerConfig::new(token),
        Err(error) => panic!("token: {error}"),
    };
    configure(&mut config);
    let tls = config.tls;
    let server = match runtime.block_on(Server::bind(config, connector)) {
        Ok(server) => server,
        Err(error) => panic!("bind: {error}"),
    };
    assert!(
        server.local_addr().ip().is_loopback(),
        "binds to localhost by default"
    );
    // The test server speaks plain HTTP even with `tls` on, whose `wss://` it names.
    let url = server.ws_url().replacen("wss://", "ws://", 1);
    let authority = server.local_addr().to_string();
    let (stop, stopped) = oneshot::channel::<()>();
    let serving = runtime.spawn(server.serve_with_shutdown(async move {
        // A dropped sender also stops the server.
        let _ = stopped.await;
    }));
    Harness {
        runtime,
        url,
        authority,
        sessions,
        open,
        taken,
        silent,
        stop: Some(stop),
        tls,
        serving: Some(serving),
    }
}

/// A mock session that counts, for the [`Harness`], the events the server takes from it
/// and whether the server still holds it. These are what the shutdown tests wait on,
/// rather than on wall-clock time, which a loaded machine stretches (#137). While
/// `silent` is set, `hello` is never answered.
struct Tracked {
    inner: jstream_control::MockControl,
    open: Arc<AtomicUsize>,
    taken: Arc<AtomicUsize>,
    silent: Arc<AtomicBool>,
}

impl Control for Tracked {
    fn hello(&self, hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
        if self.silent.load(Ordering::SeqCst) {
            return Box::pin(std::future::pending());
        }
        self.inner.hello(hello)
    }

    fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
        self.inner.call(call)
    }

    fn events(&self) -> EventStream {
        let taken = Arc::clone(&self.taken);
        Box::pin(self.inner.events().inspect(move |_| {
            taken.fetch_add(1, Ordering::SeqCst);
        }))
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        self.open.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Harness {
    /// Runs `future` on the server's runtime, failing the test after 10 s.
    fn run<T>(&self, future: impl Future<Output = T>) -> T {
        self.runtime.block_on(async {
            match tokio::time::timeout(Duration::from_secs(10), future).await {
                Ok(value) => value,
                Err(_) => panic!("timed out"),
            }
        })
    }

    /// Waits until `condition` holds, checking every 10 ms (and failing the test after
    /// 10 s, like [`Self::run`]).
    fn wait_until(&self, mut condition: impl FnMut() -> bool) {
        self.run(async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
    }

    fn connect(&self) -> Result<WsControl, ControlError> {
        let mut config = WsConfig::new(self.url.clone(), TOKEN);
        if self.tls {
            config.origin = Some(format!("https://{}", self.authority));
        }
        self.run(WsControl::connect(config))
    }

    /// A connected client that has completed the handshake.
    fn session(&self) -> (WsControl, ClientId) {
        let control = match self.connect() {
            Ok(control) => control,
            Err(error) => panic!("connect: {error}"),
        };
        let welcome = match self.run(control.hello(hello())) {
            Ok(welcome) => welcome,
            Err(error) => panic!("hello: {error}"),
        };
        (control, welcome.client_id)
    }

    /// Stops the server: open connections are closed.
    fn shut_down(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }

    /// Opens a raw WebSocket with these headers (`None` leaves one out), returning the
    /// HTTP status of a refused upgrade.
    fn raw(
        &self,
        origin: Option<&str>,
        host: Option<&str>,
        authorization: Option<&str>,
    ) -> Result<RawSocket, StatusCode> {
        let mut request = match self.url.as_str().into_client_request() {
            Ok(request) => request,
            Err(error) => panic!("request: {error}"),
        };
        let headers = request.headers_mut();
        for (name, value) in [
            (header::ORIGIN, origin),
            (header::HOST, host),
            (header::AUTHORIZATION, authorization),
        ] {
            headers.remove(&name);
            if let Some(value) = value {
                match HeaderValue::from_str(value) {
                    Ok(value) => headers.insert(name, value),
                    Err(error) => panic!("header: {error}"),
                };
            }
        }
        match self.run(tokio_tungstenite::connect_async(request)) {
            Ok((socket, _)) => Ok(socket),
            Err(tungstenite::Error::Http(response)) => Err(response.status()),
            Err(error) => panic!("unexpected connection error: {error}"),
        }
    }

    fn own_origin(&self) -> String {
        format!("http://{}", self.authority)
    }
}

type RawSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn hello() -> Hello {
    Hello {
        protocol: PROTOCOL_VERSION,
        client_name: "e2e".into(),
    }
}

fn ok<T>(result: Result<T, ControlError>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("unexpected error: {error}"),
    }
}

fn create_scene(id: &str) -> Call {
    Call::Batch(Batch::single(Edit::SceneCreate {
        scene_id: SceneId::new(id),
        name: id.to_uppercase(),
        role: None,
    }))
}

fn protocol_code(result: Result<impl std::fmt::Debug, ControlError>) -> &'static str {
    match result {
        Err(error) => match error.protocol() {
            Some(error) => error.code(),
            None => panic!("expected a protocol error, got {error}"),
        },
        Ok(value) => panic!("expected an error, got {value:?}"),
    }
}

fn expect_disconnected<T: std::fmt::Debug>(result: Result<T, ControlError>) -> String {
    match result {
        Err(ControlError::Disconnected { reason }) => reason,
        other => panic!("expected Disconnected, got {other:?}"),
    }
}

fn next_event(harness: &Harness, events: &mut EventStream) -> Option<Event> {
    harness.run(events.next())
}

/// Sends a raw text frame and reads the next server message.
fn exchange(harness: &Harness, socket: &mut RawSocket, text: &str) -> ServerMessage {
    harness.run(async {
        if let Err(error) = socket.send(Message::Text(text.into())).await {
            panic!("send: {error}");
        }
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(reply))) => match serde_json::from_str(reply.as_str()) {
                    Ok(message) => return message,
                    Err(error) => panic!("unreadable reply {reply}: {error}"),
                },
                Some(Ok(_)) => continue,
                other => panic!("no reply: {other:?}"),
            }
        }
    })
}

pub(crate) fn check_rejects_missing_token() {
    let harness = start(&MockEngine::new());
    let origin = harness.own_origin();
    let host = harness.authority.clone();
    for authorization in [None, Some("Bearer wrong-token"), Some("Basic dGVzdA==")] {
        let refused = harness.raw(Some(&origin), Some(&host), authorization).err();
        assert_eq!(refused, Some(StatusCode::UNAUTHORIZED), "{authorization:?}");
    }
    // Through the client: the reason names the refusal.
    let wrong = WsConfig::new(harness.url.clone(), "wrong-token");
    let reason = expect_disconnected(harness.run(WsControl::connect(wrong)));
    assert!(reason.contains("401"), "{reason}");
    assert_eq!(
        harness.sessions.load(Ordering::SeqCst),
        0,
        "no session opened"
    );
    // The right token gets in.
    let bearer = format!("Bearer {TOKEN}");
    assert!(
        harness
            .raw(Some(&origin), Some(&host), Some(&bearer))
            .is_ok()
    );
}

pub(crate) fn check_rejects_foreign_origin() {
    let harness = start(&MockEngine::new());
    let bearer = format!("Bearer {TOKEN}");
    let host = harness.authority.clone();
    for origin in [
        Some("http://evil.example"),
        Some("null"),
        Some("https://127.0.0.1"),
    ] {
        let refused = harness.raw(origin, Some(&host), Some(&bearer)).err();
        assert_eq!(refused, Some(StatusCode::FORBIDDEN), "{origin:?}");
    }
    // DNS rebinding: a foreign name that resolves to 127.0.0.1, with a matching Origin.
    let port = host.rsplit(':').next().unwrap_or_default();
    let rebound = format!("evil.example:{port}");
    let refused = harness
        .raw(
            Some(&format!("http://{rebound}")),
            Some(&rebound),
            Some(&bearer),
        )
        .err();
    assert_eq!(refused, Some(StatusCode::FORBIDDEN));
    // Through the client.
    let mut foreign = WsConfig::new(harness.url.clone(), TOKEN);
    foreign.origin = Some("http://evil.example".into());
    let reason = expect_disconnected(harness.run(WsControl::connect(foreign)));
    assert!(reason.contains("403"), "{reason}");
    assert_eq!(
        harness.sessions.load(Ordering::SeqCst),
        0,
        "no session opened"
    );
    // No `Origin` at all is a native client (#44): the token decides.
    assert_eq!(
        harness.raw(None, Some(&host), None).err(),
        Some(StatusCode::UNAUTHORIZED)
    );
    assert!(harness.raw(None, Some(&host), Some(&bearer)).is_ok());
}

#[test]
fn a_batch_a_query_and_events_round_trip() {
    let engine = MockEngine::new().with_output(OutputId::new("out_1"));
    let harness = start(&engine);
    let (control, client_id) = harness.session();
    let mut events = control.events();

    let response = ok(harness.run(control.call(create_scene("main"))));
    assert_eq!(response.id, RequestId::new("req_1"));
    assert_eq!(response.result, CallResult::Batch { rev: Rev(1) });
    assert_eq!(engine.rev(), Rev(1));
    // The event carries this client's request ID, as the client chose it.
    assert_eq!(
        next_event(&harness, &mut events),
        Some(Event::StateChanged {
            rev: Rev(1),
            client_id: client_id.clone(),
            request_id: Some(RequestId::new("req_1")),
            cause: None,
        })
    );
    // The first scene went on program, by the same request.
    assert_eq!(
        next_event(&harness, &mut events),
        Some(Event::StudioChanged {
            program_scene: Some(SceneId::new("main")),
            client_id: client_id.clone(),
            request_id: Some(RequestId::new("req_1")),
        })
    );

    let query = Call::Query {
        query: Query::SceneList,
    };
    let response = ok(harness.run(control.call(query)));
    match response.result {
        CallResult::Query {
            result: QueryResult::SceneList { rev, scenes },
        } => {
            assert_eq!(rev, Rev(1));
            let ids: Vec<_> = scenes.iter().map(|s| s.scene_id.as_str()).collect();
            assert_eq!(ids, ["main"]);
        }
        other => panic!("unexpected result {other:?}"),
    }

    let start = Call::Action {
        action: Action::OutputStart {
            output_id: OutputId::new("out_1"),
        },
    };
    let response = ok(harness.run(control.call(start)));
    assert_eq!(response.result, CallResult::action());
    for state in [OutputState::Starting, OutputState::Active] {
        assert_eq!(
            next_event(&harness, &mut events),
            Some(Event::OutputStateChanged {
                output_id: OutputId::new("out_1"),
                state,
            })
        );
    }

    // Events the engine emits on its own reach the client too.
    let injected = Event::OutputStateChanged {
        output_id: OutputId::new("out_1"),
        state: OutputState::Stopped,
    };
    engine.emit(injected.clone());
    assert_eq!(next_event(&harness, &mut events), Some(injected));
}

#[test]
fn concurrent_calls_each_get_their_own_answer_and_event() {
    let harness = start(&MockEngine::new());
    let (control, client_id) = harness.session();
    let mut events = control.events();
    let names = ["a", "b", "c", "d", "e"];
    let calls = names.map(|name| control.call(create_scene(name)));
    let answers = harness.run(futures_util::future::join_all(calls));
    let mut by_request = Vec::new();
    for answer in answers {
        let response = ok(answer);
        let CallResult::Batch { rev } = response.result else {
            panic!("not a batch result: {:?}", response.result);
        };
        by_request.push((response.id, rev));
    }
    by_request.sort();
    let mut seen = Vec::new();
    // One `state.changed` per batch, and one `studio.changed` for the first scene.
    let mut program_changes = 0;
    for _ in 0..=names.len() {
        match next_event(&harness, &mut events) {
            Some(Event::StateChanged {
                rev,
                client_id: from,
                request_id: Some(id),
                cause: None,
            }) => {
                assert_eq!(from, client_id);
                seen.push((id, rev));
            }
            Some(Event::StudioChanged { .. }) => program_changes += 1,
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert_eq!(program_changes, 1);
    seen.sort();
    assert_eq!(seen, by_request);
}

#[test]
fn protocol_errors_keep_their_code_and_request() {
    let harness = start(&MockEngine::new());
    let control = match harness.connect() {
        Ok(control) => control,
        Err(error) => panic!("connect: {error}"),
    };
    // Before the handshake.
    let early = harness.run(control.call(create_scene("main")));
    assert_eq!(protocol_code(early), "handshake_required");
    ok(harness.run(control.hello(hello())));
    ok(harness.run(control.call(create_scene("main"))));
    let again = harness.run(control.call(create_scene("main")));
    assert_eq!(protocol_code(again), "already_exists");
    let stale = Call::Batch(
        Batch::single(Edit::SceneDelete {
            scene_id: SceneId::new("main"),
        })
        .expecting(Rev(0)),
    );
    match harness.run(control.call(stale)) {
        Err(error) => assert_eq!(
            error.protocol(),
            Some(&ProtocolError::RevConflict {
                expected: Rev(0),
                current: Rev(1),
            })
        ),
        Ok(response) => panic!("expected rev_conflict, got {response:?}"),
    }
}

#[test]
fn an_unsupported_version_is_refused_and_closes_the_connection() {
    let harness = start(&MockEngine::new());
    let control = match harness.connect() {
        Ok(control) => control,
        Err(error) => panic!("connect: {error}"),
    };
    let future = Hello {
        protocol: ProtocolVersion {
            major: PROTOCOL_VERSION.major + 1,
            minor: 0,
        },
        client_name: "e2e".into(),
    };
    assert_eq!(
        protocol_code(harness.run(control.hello(future))),
        "unsupported_version"
    );
    let reason = expect_disconnected(harness.run(control.call(create_scene("main"))));
    assert!(reason.contains("unsupported protocol version"), "{reason}");
}

#[test]
fn malformed_messages_get_malformed_errors() {
    let harness = start(&MockEngine::new());
    let bearer = format!("Bearer {TOKEN}");
    let mut socket = match harness.raw(
        Some(&harness.own_origin()),
        Some(&harness.authority.clone()),
        Some(&bearer),
    ) {
        Ok(socket) => socket,
        Err(status) => panic!("refused: {status}"),
    };
    match exchange(&harness, &mut socket, "not json") {
        ServerMessage::Error(error) => {
            assert_eq!(error.id, None);
            assert_eq!(error.error.error.code(), "malformed");
        }
        other => panic!("unexpected reply {other:?}"),
    }
    let bad_call = r#"{"type":"request","id":"c9","call":{"kind":"teleport"}}"#;
    match exchange(&harness, &mut socket, bad_call) {
        ServerMessage::Error(error) => {
            assert_eq!(error.id, Some(RequestId::new("c9")));
            assert_eq!(error.error.error.code(), "malformed");
        }
        other => panic!("unexpected reply {other:?}"),
    }
    // The wire form from the protocol README: the response and the events carry the
    // client's own request ID, `studio.changed` (the first scene went on program) too.
    let hello = r#"{"type":"hello","protocol":{"major":0,"minor":5},"client_name":"raw"}"#;
    assert!(matches!(
        exchange(&harness, &mut socket, hello),
        ServerMessage::Welcome(_)
    ));
    let request = r#"{"type":"request","id":"r1","call":{"kind":"batch","edits":[{"type":"scene.create","scene_id":"scn_1","name":"Main"}],"expected_rev":0}}"#;
    let first = exchange(&harness, &mut socket, request);
    let mut messages = vec![first];
    for _ in 0..2 {
        messages.push(harness.run(async {
            loop {
                match socket.next().await {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ServerMessage>(text.as_str()) {
                            Ok(message) => return message,
                            Err(error) => panic!("unreadable: {error}"),
                        }
                    }
                    Some(Ok(_)) => continue,
                    other => panic!("no further message: {other:?}"),
                }
            }
        }));
    }
    let mut ids = Vec::new();
    for message in messages {
        match message {
            ServerMessage::Response(response) => ids.push(("response", response.id)),
            ServerMessage::Event(event) => match event.event {
                Event::StateChanged {
                    request_id: Some(id),
                    ..
                } => ids.push(("state.changed", id)),
                Event::StudioChanged {
                    request_id: Some(id),
                    ..
                } => ids.push(("studio.changed", id)),
                other => panic!("unexpected event {other:?}"),
            },
            other => panic!("unexpected message {other:?}"),
        }
    }
    ids.sort();
    assert_eq!(
        ids,
        [
            ("response", RequestId::new("r1")),
            ("state.changed", RequestId::new("r1")),
            ("studio.changed", RequestId::new("r1"))
        ]
    );
}

#[test]
fn a_server_shutdown_disconnects_calls_and_ends_events() {
    let mut harness = start(&MockEngine::new());
    let (control, _) = harness.session();
    let mut events = control.events();
    ok(harness.run(control.call(create_scene("main"))));
    // `state.changed`, then `studio.changed` for the first scene.
    assert!(next_event(&harness, &mut events).is_some());
    assert!(next_event(&harness, &mut events).is_some());
    harness.shut_down();
    assert_eq!(next_event(&harness, &mut events), None, "the stream ends");
    let reason = expect_disconnected(harness.run(control.call(create_scene("other"))));
    assert!(reason.contains("shutting down"), "{reason}");
    // The session never reopens: a second `hello` fails too, rather than getting a new
    // `client_id` (jstream-mcp relies on this).
    let reason = expect_disconnected(harness.run(control.hello(hello())));
    assert!(reason.contains("shutting down"), "{reason}");
    // A subscription opened after the close ends at once.
    let mut late = control.events();
    assert_eq!(next_event(&harness, &mut late), None);
}

/// A session whose `hello` signals `entered`, then waits for a permit from `gate`, so a
/// test can hold a connection's task inside the handshake while the client queues more
/// messages behind it.
struct Gated {
    inner: jstream_control::MockControl,
    entered: Arc<Notify>,
    gate: Arc<Semaphore>,
}

impl Control for Gated {
    fn hello(&self, hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
        Box::pin(async move {
            self.entered.notify_one();
            match self.gate.acquire().await {
                Ok(permit) => permit.forget(),
                Err(error) => panic!("gate: {error}"),
            }
            self.inner.hello(hello).await
        })
    }

    fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
        self.inner.call(call)
    }

    fn events(&self) -> EventStream {
        self.inner.events()
    }
}

/// #294: a request the server reads once it is shutting down is never answered. The
/// connection closes with 1001 instead, so the client's call fails with `Disconnected`
/// rather than succeeding after `serve_with_shutdown` has returned. The close frame
/// arrives, and the connection then ends cleanly: the server reads the client's close
/// before closing the socket, rather than resetting it with the request unread.
///
/// Each round holds the connection's task inside `hello` while the client queues a
/// request and the server begins shutting down, so when the task goes on, the shutdown
/// and the request are ready together. `tokio::select!` picks among ready branches at random,
/// and without the check the task took and answered the request in about one round in
/// twenty; 200 rounds (well under a second) miss that with a probability near 1e-4.
#[test]
fn a_request_read_after_the_shutdown_is_not_answered() {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => panic!("runtime: {error}"),
    };
    let query =
        r#"{"type":"request","id":"q1","call":{"kind":"query","query":{"type":"scene.list"}}}"#;
    let hello = r#"{"type":"hello","protocol":{"major":0,"minor":5},"client_name":"raw"}"#;
    for round in 0..200 {
        let entered = Arc::new(Notify::new());
        let gate = Arc::new(Semaphore::new(0));
        let engine = MockEngine::new();
        let connector: Connector = {
            let (entered, gate) = (Arc::clone(&entered), Arc::clone(&gate));
            Arc::new(move || {
                Box::new(Gated {
                    inner: engine.connect(),
                    entered: Arc::clone(&entered),
                    gate: Arc::clone(&gate),
                })
            })
        };
        let config = match Token::new(TOKEN) {
            Ok(token) => ServerConfig::new(token),
            Err(error) => panic!("token: {error}"),
        };
        let closed = runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(10), async {
                let server = match Server::bind(config, connector).await {
                    Ok(server) => server,
                    Err(error) => panic!("bind: {error}"),
                };
                let mut request = match server.ws_url().into_client_request() {
                    Ok(request) => request,
                    Err(error) => panic!("request: {error}"),
                };
                match HeaderValue::from_str(&format!("Bearer {TOKEN}")) {
                    Ok(value) => request.headers_mut().insert(header::AUTHORIZATION, value),
                    Err(error) => panic!("header: {error}"),
                };
                let addr = server.local_addr();
                let (stop, stopped) = oneshot::channel::<()>();
                let serving = tokio::spawn(server.serve_with_shutdown(async move {
                    let _ = stopped.await;
                }));
                let mut socket = match tokio_tungstenite::connect_async(request).await {
                    Ok((socket, _)) => socket,
                    Err(error) => panic!("connect: {error}"),
                };
                for text in [hello, query] {
                    if let Err(error) = socket.send(Message::Text(text.into())).await {
                        panic!("send: {error}");
                    }
                    if text == hello {
                        // The connection's task is now held inside `hello`.
                        entered.notified().await;
                    }
                }
                let _ = stop.send(());
                // The server stops listening only after it has told its connections it is
                // shutting down. (Awaiting `serving` here would wait for this connection,
                // which `hello` holds.)
                while tokio::net::TcpStream::connect(addr).await.is_ok() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                gate.add_permits(1);
                let code = loop {
                    match socket.next().await {
                        Some(Ok(Message::Text(text))) => {
                            match serde_json::from_str::<ServerMessage>(text.as_str()) {
                                // The `hello` was read before the shutdown.
                                Ok(ServerMessage::Welcome(_)) => {}
                                other => panic!("round {round}: answered {other:?}"),
                            }
                        }
                        Some(Ok(Message::Close(frame))) => {
                            // The server finishes the closing handshake before it closes
                            // the socket, so the stream then ends without a reset. Linux
                            // keeps a frame that arrived before a reset readable, but
                            // macOS and Windows discard it, so this checks the reset
                            // itself.
                            match socket.next().await {
                                None => {}
                                other => panic!("round {round}: after the close: {other:?}"),
                            }
                            break frame.map(|frame| u16::from(frame.code));
                        }
                        Some(Ok(_)) => {}
                        None | Some(Err(_)) => break None,
                    }
                };
                match serving.await {
                    Ok(Ok(())) => {}
                    other => panic!("the server did not stop cleanly: {other:?}"),
                }
                code
            })
            .await
        });
        match closed {
            Ok(code) => assert_eq!(code, Some(1001), "round {round}"),
            Err(_) => panic!("round {round} timed out"),
        }
    }
}

/// #294: `serve_with_shutdown` returns only once every connection has sent its close
/// and finished the closing handshake. Returning sooner let a host that exits right
/// after it (the daemon drops its runtime) cut those closes short, so clients lost their
/// close frame and last replies.
#[test]
fn serve_with_shutdown_returns_once_its_connections_have_closed() {
    let mut harness = start(&MockEngine::new());
    let (first, _) = harness.session();
    let (second, _) = harness.session();
    let mut events = second.events();
    harness.shut_down();
    let Some(serving) = harness.serving.take() else {
        panic!("the harness has no serving task");
    };
    match harness.run(serving) {
        Ok(Ok(())) => {}
        other => panic!("the server did not stop cleanly: {other:?}"),
    }
    assert_eq!(
        harness.open.load(Ordering::SeqCst),
        0,
        "a connection was still open when the server returned"
    );
    // Both clients got the close.
    assert_eq!(next_event(&harness, &mut events), None);
    let reason = expect_disconnected(harness.run(first.call(create_scene("main"))));
    assert!(reason.contains("shutting down"), "{reason}");
}

#[test]
fn a_client_without_a_server_is_disconnected() {
    let harness = start(&MockEngine::new());
    // Bind and release a port, so nothing listens on it.
    let free = harness.run(async {
        match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener.local_addr(),
            Err(error) => panic!("bind: {error}"),
        }
    });
    let addr = match free {
        Ok(addr) => addr,
        Err(error) => panic!("addr: {error}"),
    };
    let config = WsConfig::new(format!("ws://{addr}"), TOKEN);
    let reason = expect_disconnected(harness.run(WsControl::connect(config)));
    assert!(reason.contains("could not connect"), "{reason}");
    for url in ["wss://127.0.0.1:1", "http://127.0.0.1:1", "not a url"] {
        let reason =
            expect_disconnected(harness.run(WsControl::connect(WsConfig::new(url, TOKEN))));
        assert!(reason.contains("invalid engine URL"), "{url}: {reason}");
    }
}

#[test]
fn dropping_the_client_closes_its_session() {
    let engine = MockEngine::new();
    let harness = start(&engine);
    let (watcher, _) = harness.session();
    let mut events = watcher.events();
    let (control, _) = harness.session();
    ok(harness.run(control.call(create_scene("main"))));
    drop(control);
    // The other session is unaffected and saw the change.
    assert!(matches!(
        next_event(&harness, &mut events),
        Some(Event::StateChanged { rev: Rev(1), .. })
    ));
    ok(harness.run(watcher.call(create_scene("other"))));
    assert_eq!(engine.rev(), Rev(2));
    assert_eq!(harness.sessions.load(Ordering::SeqCst), 2);
}

/// #44: an extra host without a port could never match a request, so binding refuses it
/// instead of starting a server that silently ignores it.
#[test]
fn binding_refuses_an_extra_host_without_a_port() {
    let harness = start(&MockEngine::new());
    let engine = MockEngine::new();
    let connector: Connector = Arc::new(move || Box::new(engine.connect()));
    let mut config = match Token::new(TOKEN) {
        Ok(token) => ServerConfig::new(token),
        Err(error) => panic!("token: {error}"),
    };
    config.extra_hosts = vec!["studio.lan".to_owned()];
    match harness.run(Server::bind(config, connector)) {
        Err(crate::ServerError::InvalidHost { host, .. }) => assert_eq!(host, "studio.lan"),
        Err(other) => panic!("expected InvalidHost, got {other}"),
        Ok(server) => panic!(
            "bound on {} with a portless extra host",
            server.local_addr()
        ),
    }
}

/// An event numbered `n`, so a test can tell which ones arrived, about 1 KB long so a
/// few thousand fill a socket's buffers.
fn numbered(n: usize) -> Event {
    Event::OutputStateChanged {
        output_id: OutputId::new(format!("{n}-{}", "x".repeat(1000))),
        state: OutputState::Active,
    }
}

/// A raw client with a 4 KB receive buffer, past its handshake, so the server's sends to
/// it block as soon as it stops reading.
fn slow_client(harness: &Harness) -> tokio_tungstenite::WebSocketStream<tokio::net::TcpStream> {
    let mut slow = harness.run(async {
        let socket = match tokio::net::TcpSocket::new_v4() {
            Ok(socket) => socket,
            Err(error) => panic!("socket: {error}"),
        };
        if let Err(error) = socket.set_recv_buffer_size(4096) {
            panic!("receive buffer: {error}");
        }
        let addr = match harness.authority.parse() {
            Ok(addr) => addr,
            Err(error) => panic!("address: {error}"),
        };
        let stream = match socket.connect(addr).await {
            Ok(stream) => stream,
            Err(error) => panic!("connect: {error}"),
        };
        let mut request = match harness.url.as_str().into_client_request() {
            Ok(request) => request,
            Err(error) => panic!("request: {error}"),
        };
        for (name, value) in [
            (header::ORIGIN, harness.own_origin()),
            (header::AUTHORIZATION, format!("Bearer {TOKEN}")),
        ] {
            match HeaderValue::from_str(&value) {
                Ok(value) => request.headers_mut().insert(name, value),
                Err(error) => panic!("header: {error}"),
            };
        }
        match tokio_tungstenite::client_async(request, stream).await {
            Ok((socket, _)) => socket,
            Err(error) => panic!("upgrade: {error}"),
        }
    });
    let hello =
        match serde_json::to_string(&jstream_control::protocol::ClientMessage::Hello(hello())) {
            Ok(text) => text,
            Err(error) => panic!("hello: {error}"),
        };
    assert!(matches!(
        harness.run(exchange_on(&mut slow, &hello)),
        ServerMessage::Welcome(_)
    ));
    slow
}

/// #44 "Slow clients": a client that stops reading gets its queued events dropped and
/// one `events.dropped` notice counting them, while the engine and every other client
/// carry on.
#[test]
fn a_client_that_stops_reading_gets_events_dropped_and_slows_no_one() {
    // About 10 MB: far more than the slow socket's kernel buffers hold, so its outbox
    // must overflow.
    const SENT: usize = 10_000;
    let engine = MockEngine::new();
    let harness = start(&engine);

    // A raw client with a small receive buffer, so the server's sends block early.
    let mut slow = slow_client(&harness);

    // The slow client now reads nothing while the engine emits.
    let (control, _) = harness.session();
    let mut events = control.events();
    // The other client reads as they come and gets every event, in order. (It reads
    // between rounds smaller than the buffer: a burst of the whole lot at once would
    // overflow a client that reads, too, since the server queues faster than it reads.)
    harness.run(async {
        for round in 0..SENT / 500 {
            for n in round * 500..(round + 1) * 500 {
                engine.emit(numbered(n));
            }
            for n in round * 500..(round + 1) * 500 {
                assert_eq!(events.next().await, Some(numbered(n)));
            }
        }
    });
    // And its calls are answered.
    let response = ok(harness.run(control.call(create_scene("main"))));
    assert_eq!(response.result, CallResult::Batch { rev: Rev(1) });
    assert!(matches!(
        next_event(&harness, &mut events),
        Some(Event::StateChanged { rev: Rev(1), .. })
    ));
    assert!(matches!(
        next_event(&harness, &mut events),
        Some(Event::StudioChanged { .. })
    ));
    // Time for the slow connection to drain the engine's queue into its outbox, which is
    // what overflows; reading too early would only make the drop smaller, never hide it
    // from the count below.
    harness.run(async { tokio::time::sleep(Duration::from_millis(500)).await });

    // Every event is either delivered, in order, or counted by a notice.
    let (delivered, dropped, notices) = harness.run(async {
        let (mut delivered, mut dropped, mut notices) = (0u64, 0u64, 0u64);
        let mut last: Option<usize> = None;
        // The numbered events, then the batch's `state.changed` and `studio.changed`.
        while delivered + dropped < SENT as u64 + 2 {
            let text = match slow.next().await {
                Some(Ok(Message::Text(text))) => text,
                Some(Ok(_)) => continue,
                other => panic!("connection ended: {other:?}"),
            };
            match serde_json::from_str::<ServerMessage>(text.as_str()) {
                Ok(ServerMessage::Event(message)) => match message.event {
                    Event::EventsDropped { count } => {
                        assert!(count > 0);
                        dropped += count;
                        notices += 1;
                    }
                    Event::OutputStateChanged { output_id, .. } => {
                        let digits = output_id.as_str().split('-').next().unwrap_or_default();
                        let n = match digits.parse::<usize>() {
                            Ok(n) => n,
                            Err(error) => panic!("event {output_id:?}: {error}"),
                        };
                        assert!(last.is_none_or(|last| n > last), "out of order");
                        last = Some(n);
                        delivered += 1;
                    }
                    Event::StateChanged { rev, .. } => {
                        assert_eq!(rev, Rev(1));
                        delivered += 1;
                    }
                    Event::StudioChanged { .. } => delivered += 1,
                    other => panic!("unexpected event {other:?}"),
                },
                other => panic!("unexpected message {other:?}"),
            }
        }
        (delivered, dropped, notices)
    });
    assert!(dropped > 0, "the slow client's outbox never overflowed");
    assert!(notices >= 1);
    assert_eq!(delivered + dropped, SENT as u64 + 2);

    // Once it reads again, the slow client is served like any other.
    let query =
        r#"{"type":"request","id":"q1","call":{"kind":"query","query":{"type":"scene.list"}}}"#;
    match harness.run(exchange_on(&mut slow, query)) {
        ServerMessage::Response(response) => assert_eq!(response.id, RequestId::new("q1")),
        other => panic!("unexpected reply {other:?}"),
    }
}

/// Sends a text frame and reads the next server message, on any WebSocket client.
async fn exchange_on<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    text: &str,
) -> ServerMessage
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if let Err(error) = socket.send(Message::Text(text.into())).await {
        panic!("send: {error}");
    }
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(reply))) => match serde_json::from_str(reply.as_str()) {
                Ok(message) => return message,
                Err(error) => panic!("unreadable reply {reply}: {error}"),
            },
            Some(Ok(_)) => continue,
            other => panic!("no reply: {other:?}"),
        }
    }
}

/// #108 review N1: a connection whose client has stopped reading is dropped
/// `CLOSE_TIMEOUT` after the server shuts down, instead of waiting for it for good. The
/// client gets what its kernel buffers already held, then the connection ends without a
/// close frame (with no timeout, reading would deliver the rest and the close frame).
#[test]
fn shutdown_drops_a_client_that_stopped_reading() {
    let engine = MockEngine::new();
    let mut harness = start(&engine);
    let mut slow = slow_client(&harness);
    // About 10 MB, as above: far more than the kernel buffers hold, so the connection's
    // sends block and its outbox stays full.
    for n in 0..10_000 {
        engine.emit(numbered(n));
    }
    // Once the connection has taken every event, it has filled the socket and queued
    // the rest. Before that, a shutdown would find little queued and could finish its
    // close in time, which is what made this test fail under CPU load (#137).
    harness.wait_until(|| harness.taken.load(Ordering::SeqCst) >= 10_000);
    harness.shut_down();
    // The server gives up on the connection once the close timeout has passed.
    harness.wait_until(|| harness.open.load(Ordering::SeqCst) == 0);
    let closed = harness.run(async {
        loop {
            match slow.next().await {
                Some(Ok(Message::Close(_))) => break true,
                Some(Ok(_)) => continue,
                None | Some(Err(_)) => break false,
            }
        }
    });
    assert!(
        !closed,
        "the server waited for the client and sent its close frame"
    );
}

/// #110 review N1: dropping the connection's event stream, which happens before the
/// close timeout, stops the engine queueing for that connection. A client that has
/// stopped reading is still held open here, so the count is not falling only because
/// the connection task has exited.
#[test]
fn closing_a_connection_drops_its_event_subscription() {
    let engine = MockEngine::new();
    let mut harness = start(&engine);
    // A second connection that reads, so shutdown has more than the stuck client to close.
    let _steady = harness.session();
    let mut slow = slow_client(&harness);
    harness.wait_until(|| engine.subscriber_count() == 2);
    for n in 0..10_000 {
        engine.emit(numbered(n));
    }
    // Both connections have taken every event: the stuck one's writer is blocked (see
    // the test above).
    harness.wait_until(|| harness.taken.load(Ordering::SeqCst) >= 20_000);
    assert_eq!(
        engine.subscriber_count(),
        2,
        "both connections are still subscribed while the server is up"
    );
    let stopped = std::time::Instant::now();
    harness.shut_down();
    // Without reading: reading would unblock the writer and let the close finish.
    harness.wait_until(|| engine.subscriber_count() == 0);
    // The stuck connection is held for the close timeout after the shutdown, so a count
    // that fell sooner fell because the subscription was dropped, not because the
    // connection task ended.
    assert!(
        stopped.elapsed() < crate::session::CLOSE_TIMEOUT,
        "the closing connection's subscription lasted until the close timeout"
    );
    harness.wait_until(|| harness.open.load(Ordering::SeqCst) == 0);
    let closed = harness.run(async {
        loop {
            match slow.next().await {
                Some(Ok(Message::Close(_))) => break true,
                Some(Ok(_)) => continue,
                None | Some(Err(_)) => break false,
            }
        }
    });
    assert!(
        !closed,
        "the server waited for the client and sent its close frame"
    );
}

/// Reads until the server ends the connection: the close frame's code, or `None` if it
/// ended without one.
fn closed_by_server(harness: &Harness, socket: &mut RawSocket) -> Option<u16> {
    harness.run(async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Close(frame))) => break frame.map(|frame| frame.code.into()),
                Some(Ok(_)) => continue,
                None | Some(Err(_)) => break None,
            }
        }
    })
}

/// A connection that completes the WebSocket upgrade but never sends `hello` is closed
/// (close code 1008) once the handshake timeout has passed, while one that completed its
/// handshake stays open past it.
#[test]
fn a_client_that_never_says_hello_is_closed() {
    let harness = start_with(&MockEngine::new(), |config| {
        config.handshake_timeout = Duration::from_millis(300);
    });
    let (control, _) = harness.session();
    let bearer = format!("Bearer {TOKEN}");
    let mut silent = match harness.raw(None, Some(&harness.authority), Some(&bearer)) {
        Ok(socket) => socket,
        Err(status) => panic!("upgrade refused: {status}"),
    };
    assert_eq!(closed_by_server(&harness, &mut silent), Some(1008));
    // The client that said hello in time is still served after the deadline.
    harness.run(async { tokio::time::sleep(Duration::from_millis(400)).await });
    let response = ok(harness.run(control.call(create_scene("main"))));
    assert_eq!(response.result, CallResult::Batch { rev: Rev(1) });
}

/// A client that says `hello` in time to an engine that never answers it is still closed
/// (close code 1008) at the handshake deadline, and its session is dropped, rather than
/// holding its connection slot until the engine answers (#273).
#[test]
fn a_hello_the_engine_never_answers_is_closed_at_the_deadline() {
    let harness = start_with(&MockEngine::new(), |config| {
        config.handshake_timeout = Duration::from_millis(300);
    });
    harness.silent.store(true, Ordering::SeqCst);
    let bearer = format!("Bearer {TOKEN}");
    let mut socket = match harness.raw(None, Some(&harness.authority), Some(&bearer)) {
        Ok(socket) => socket,
        Err(status) => panic!("upgrade refused: {status}"),
    };
    let hello =
        match serde_json::to_string(&jstream_control::protocol::ClientMessage::Hello(hello())) {
            Ok(text) => text,
            Err(error) => panic!("hello: {error}"),
        };
    harness.run(async {
        if let Err(error) = socket.send(Message::Text(hello.into())).await {
            panic!("send: {error}");
        }
    });
    assert_eq!(closed_by_server(&harness, &mut socket), Some(1008));
    harness.wait_until(|| harness.open.load(Ordering::SeqCst) == 0);
}

/// At most `max_connections` (32 by default) WebSocket connections are open at once: one
/// more is refused at the upgrade with HTTP 503 and no session is opened, and a slot
/// freed by a closed connection can be taken again.
#[test]
fn connections_over_the_cap_are_refused() {
    let harness = start_with(&MockEngine::new(), |config| {
        // Long enough that no connection here is closed for its silence.
        config.handshake_timeout = Duration::from_secs(60);
    });
    let bearer = format!("Bearer {TOKEN}");
    let host = harness.authority.clone();
    let mut open: Vec<RawSocket> = (0..32)
        .map(|n| match harness.raw(None, Some(&host), Some(&bearer)) {
            Ok(socket) => socket,
            Err(status) => panic!("connection {n} refused: {status}"),
        })
        .collect();
    assert_eq!(
        harness.raw(None, Some(&host), Some(&bearer)).err(),
        Some(StatusCode::SERVICE_UNAVAILABLE)
    );
    assert_eq!(
        harness.sessions.load(Ordering::SeqCst),
        32,
        "no 33rd session"
    );
    // Through the client, too.
    let reason = expect_disconnected(harness.connect());
    assert!(reason.contains("503"), "{reason}");
    // Closing one frees its slot, once the server has seen it go.
    if let Some(mut first) = open.pop() {
        harness.run(async {
            let _ = first.close(None).await;
            while first.next().await.is_some() {}
        });
    }
    harness.run(async {
        loop {
            let mut request = match harness.url.as_str().into_client_request() {
                Ok(request) => request,
                Err(error) => panic!("request: {error}"),
            };
            match HeaderValue::from_str(&bearer) {
                Ok(value) => request.headers_mut().insert(header::AUTHORIZATION, value),
                Err(error) => panic!("header: {error}"),
            };
            match tokio_tungstenite::connect_async(request).await {
                Ok((socket, _)) => break open.push(socket),
                Err(tungstenite::Error::Http(response))
                    if response.status() == StatusCode::SERVICE_UNAVAILABLE =>
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("unexpected connection error: {error}"),
            }
        }
    });
    assert_eq!(harness.sessions.load(Ordering::SeqCst), 33);
}

/// A connection that sends its upgrade request's headers too slowly is dropped once the
/// header-read timeout has passed, and no session is opened; a connection whose request
/// was accepted is not dropped by it.
#[test]
fn a_client_that_sends_headers_too_slowly_is_dropped() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let harness = start_with(&MockEngine::new(), |config| {
        config.header_read_timeout = Duration::from_millis(300);
    });
    let (control, _) = harness.session();
    let addr = harness.authority.clone();
    let read = harness.run(async {
        let mut stream = match tokio::net::TcpStream::connect(&addr).await {
            Ok(stream) => stream,
            Err(error) => panic!("connect: {error}"),
        };
        // Part of a request, then nothing more.
        let partial = format!("GET {} HTTP/1.1\r\nHost: {addr}\r\n", crate::WS_PATH);
        if let Err(error) = stream.write_all(partial.as_bytes()).await {
            panic!("write: {error}");
        }
        let mut buf = [0u8; 256];
        stream.read(&mut buf).await
    });
    // Dropped: the server closed (or reset) the connection without answering.
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    assert_eq!(
        harness.sessions.load(Ordering::SeqCst),
        1,
        "no session opened"
    );
    // The client whose request was accepted is still served after the deadline.
    harness.run(async { tokio::time::sleep(Duration::from_millis(400)).await });
    let response = ok(harness.run(control.call(create_scene("main"))));
    assert_eq!(response.result, CallResult::Batch { rev: Rev(1) });
}

/// #354: at most `max_tcp_connections` TCP connections are open at once, whatever they
/// carry: ones that never send a request count, and so do upgraded WebSocket
/// connections, which the header-read deadline no longer bounds. One accepted beyond it
/// is closed at once, unanswered; once one closes, a new connection is served again.
#[test]
fn tcp_connections_over_the_cap_are_dropped() {
    use tokio::io::AsyncReadExt;
    let harness = start_with(&MockEngine::new(), |config| {
        config.max_tcp_connections = 4;
        // Long enough that only the cap can drop a connection here.
        config.header_read_timeout = Duration::from_secs(60);
        config.handshake_timeout = Duration::from_secs(60);
    });
    // Two authenticated WebSocket sessions and two connections that send nothing.
    let sessions = [harness.session(), harness.session()];
    let addr = harness.authority.clone();
    let mut idle: Vec<tokio::net::TcpStream> = harness.run(async {
        let mut idle = Vec::new();
        for _ in 0..2 {
            match tokio::net::TcpStream::connect(&addr).await {
                Ok(stream) => idle.push(stream),
                Err(error) => panic!("connect: {error}"),
            }
        }
        idle
    });
    // A fifth is closed by the server at once (without the cap it would wait for the
    // 60 s header-read deadline).
    let read = harness.run(async {
        let mut stream = match tokio::net::TcpStream::connect(&addr).await {
            Ok(stream) => stream,
            Err(error) => panic!("connect: {error}"),
        };
        let mut buf = [0u8; 64];
        tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await
    });
    assert!(
        matches!(read, Ok(Ok(0) | Err(_))),
        "the fifth connection was not dropped: {read:?}"
    );
    // Through the client too: it gets no session.
    expect_disconnected(harness.connect());
    assert_eq!(
        harness.sessions.load(Ordering::SeqCst),
        2,
        "no third session"
    );
    // The sessions under the cap are still served.
    for (control, _) in &sessions {
        ok(harness.run(control.call(Call::Query {
            query: Query::SceneList,
        })));
    }
    // Closing an idle connection frees its place, once the server has seen it go.
    drop(idle.pop());
    harness.run(async {
        loop {
            match WsControl::connect(WsConfig::new(harness.url.clone(), TOKEN)).await {
                Ok(control) => break drop(control),
                Err(ControlError::Disconnected { .. }) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("unexpected connection error: {error}"),
            }
        }
    });
    assert_eq!(harness.sessions.load(Ordering::SeqCst), 3);
}

/// A session whose calls count themselves in `entered`, then each wait for a permit
/// from `gate`, so a test can hold calls in flight.
struct Stalled {
    inner: jstream_control::MockControl,
    entered: Arc<AtomicUsize>,
    gate: Arc<Semaphore>,
}

impl Control for Stalled {
    fn hello(&self, hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
        self.inner.hello(hello)
    }

    fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
        Box::pin(async move {
            self.entered.fetch_add(1, Ordering::SeqCst);
            match self.gate.acquire().await {
                Ok(permit) => permit.forget(),
                Err(error) => panic!("gate: {error}"),
            }
            self.inner.call(call).await
        })
    }

    fn events(&self) -> EventStream {
        self.inner.events()
    }
}

/// #354: a connection has at most `MAX_IN_FLIGHT` calls in flight. A client that sends
/// more without waiting for answers is not read until one completes, so the session
/// never sees more than that at once; every request is still answered once the engine
/// catches up.
#[test]
fn calls_in_flight_per_connection_are_capped() {
    use crate::session::MAX_IN_FLIGHT;
    let entered = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Semaphore::new(0));
    let engine = MockEngine::new();
    let connector: Connector = {
        let (entered, gate) = (Arc::clone(&entered), Arc::clone(&gate));
        Arc::new(move || {
            Box::new(Stalled {
                inner: engine.connect(),
                entered: Arc::clone(&entered),
                gate: Arc::clone(&gate),
            })
        })
    };
    let harness = serve(
        connector,
        |_| {},
        Arc::default(),
        Arc::default(),
        Arc::default(),
        Arc::default(),
    );
    let bearer = format!("Bearer {TOKEN}");
    let host = harness.authority.clone();
    let mut socket = match harness.raw(None, Some(&host), Some(&bearer)) {
        Ok(socket) => socket,
        Err(status) => panic!("refused: {status}"),
    };
    let hello = format!(
        r#"{{"type":"hello","protocol":{{"major":{},"minor":{}}},"client_name":"raw"}}"#,
        PROTOCOL_VERSION.major, PROTOCOL_VERSION.minor
    );
    match exchange(&harness, &mut socket, &hello) {
        ServerMessage::Welcome(_) => {}
        other => panic!("unexpected reply {other:?}"),
    }
    let sent = MAX_IN_FLIGHT * 2;
    harness.run(async {
        for n in 0..sent {
            let request = format!(
                r#"{{"type":"request","id":"q{n}","call":{{"kind":"query","query":{{"type":"scene.list"}}}}}}"#
            );
            if let Err(error) = socket.send(Message::Text(request.into())).await {
                panic!("send: {error}");
            }
        }
    });
    harness.wait_until(|| entered.load(Ordering::SeqCst) >= MAX_IN_FLIGHT);
    // Give the connection time to read more, which it must not.
    harness.run(async { tokio::time::sleep(Duration::from_millis(300)).await });
    assert_eq!(entered.load(Ordering::SeqCst), MAX_IN_FLIGHT);
    // Once the engine answers, the rest are read, and everything is answered.
    gate.add_permits(sent);
    let answered = harness.run(async {
        let mut answered = 0;
        while answered < sent {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<ServerMessage>(text.as_str()) {
                        Ok(ServerMessage::Response(_)) => answered += 1,
                        Ok(ServerMessage::Event(_)) => {}
                        other => panic!("unexpected reply {other:?}"),
                    }
                }
                Some(Ok(_)) => {}
                other => panic!("connection ended: {other:?}"),
            }
        }
        answered
    });
    assert_eq!(answered, sent);
    assert_eq!(entered.load(Ordering::SeqCst), sent);
}

// MCP's streamable-HTTP transport (`mcp.rs`), driven with raw HTTP/1.1 requests.

/// An HTTP answer: its status, headers (names lowercase) and body.
struct HttpReply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpReply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> serde_json::Value {
        match serde_json::from_str(&self.body) {
            Ok(value) => value,
            Err(error) => panic!("not JSON ({error}): {}", self.body),
        }
    }
}

/// Writes one HTTP/1.1 request on `stream` and reads its answer (a `Content-Length`
/// body, or none).
async fn http_on(
    stream: &mut tokio::net::TcpStream,
    method: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> HttpReply {
    http_to(stream, method, crate::MCP_PATH, headers, body).await
}

/// [`http_on`] for any path. A `HEAD` answer's `Content-Length` announces no body.
async fn http_to(
    stream: &mut tokio::net::TcpStream,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> HttpReply {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut request = format!("{method} {path} HTTP/1.1\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    if let Err(error) = stream.write_all(request.as_bytes()).await {
        panic!("write: {error}");
    }
    let mut buf = Vec::new();
    let (head_len, length) = loop {
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk).await {
            Ok(0) => panic!(
                "closed before the answer: {}",
                String::from_utf8_lossy(&buf)
            ),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(error) => panic!("read: {error}"),
        }
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .filter(|_| method != "HEAD")
                .unwrap_or(0);
            break (end + 4, length);
        }
    };
    while buf.len() < head_len + length {
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk).await {
            Ok(0) => panic!("closed mid-body"),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(error) => panic!("read: {error}"),
        }
    }
    let head = String::from_utf8_lossy(&buf[..head_len - 4]).into_owned();
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let body = String::from_utf8_lossy(&buf[head_len..head_len + length]).into_owned();
    HttpReply {
        status,
        headers,
        body,
    }
}

impl Harness {
    /// Sends one request to the MCP endpoint on a new connection, with `headers` (`Host`
    /// is added unless given).
    fn http(&self, method: &str, headers: &[(&str, &str)], body: &str) -> HttpReply {
        let addr = self.authority.clone();
        let mut all: Vec<(&str, &str)> = Vec::new();
        if !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("host")) {
            all.push(("Host", &addr));
        }
        all.extend_from_slice(headers);
        self.run(async {
            let mut stream = match tokio::net::TcpStream::connect(&addr).await {
                Ok(stream) => stream,
                Err(error) => panic!("connect: {error}"),
            };
            http_on(&mut stream, method, &all, body).await
        })
    }

    /// POSTs one JSON-RPC message with the token, and the session ID if given.
    fn mcp(&self, session: Option<&str>, message: &serde_json::Value) -> HttpReply {
        let bearer = format!("Bearer {TOKEN}");
        let mut headers = vec![
            ("Authorization", bearer.as_str()),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
        ];
        if let Some(id) = session {
            headers.push(("Mcp-Session-Id", id));
        }
        self.http("POST", &headers, &message.to_string())
    }

    /// A handshake-based MCP session: `initialize` and `notifications/initialized`.
    fn mcp_session(&self, client: &str) -> String {
        let reply = self.mcp(None, &mcp_initialize(client));
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(reply.header("content-type"), Some("application/json"));
        assert_eq!(reply.json()["result"]["protocolVersion"], "2025-11-25");
        let Some(id) = reply.header("mcp-session-id").map(str::to_owned) else {
            panic!("no Mcp-Session-Id");
        };
        let note = serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let reply = self.mcp(Some(&id), &note);
        assert_eq!((reply.status, reply.body.as_str()), (202, ""));
        id
    }
}

fn mcp_on() -> impl FnOnce(&mut ServerConfig) {
    |config| config.mcp = true
}

fn mcp_initialize(client: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                   "clientInfo": {"name": client, "version": "1"}}
    })
}

fn mcp_create_scene(id: &str, meta: Option<serde_json::Value>) -> serde_json::Value {
    let mut params = serde_json::json!({
        "name": "scene",
        "arguments": {"op": {"type": "scene.create", "scene_id": id, "name": id}}
    });
    if let Some(meta) = meta {
        params["_meta"] = meta;
    }
    serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": params})
}

fn modern_meta() -> serde_json::Value {
    serde_json::json!({
        "io.modelcontextprotocol/protocolVersion": jstream_mcp::MODERN_VERSION,
        "io.modelcontextprotocol/clientInfo": {"name": "stateless-agent", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

pub(crate) fn check_mcp_rejects_missing_token() {
    let harness = start_with(&MockEngine::new(), mcp_on());
    let body = mcp_initialize("agent").to_string();
    for authorization in [None, Some("Bearer wrong-token"), Some("Basic dGVzdA==")] {
        let mut headers = vec![("Content-Type", "application/json")];
        if let Some(value) = authorization {
            headers.push(("Authorization", value));
        }
        let reply = harness.http("POST", &headers, &body);
        assert_eq!(reply.status, 401, "{authorization:?}");
        assert_eq!(reply.header("www-authenticate"), Some("Bearer"));
    }
    assert_eq!(
        harness.sessions.load(Ordering::SeqCst),
        0,
        "no session opened"
    );
}

pub(crate) fn check_mcp_rejects_foreign_origin() {
    let harness = start_with(&MockEngine::new(), mcp_on());
    let bearer = format!("Bearer {TOKEN}");
    let body = mcp_initialize("agent").to_string();
    let port = harness
        .authority
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .to_owned();
    let rebound = format!("evil.example:{port}");
    let rebound_origin = format!("http://{rebound}");
    for (origin, host) in [
        (Some("http://evil.example"), None),
        (Some("null"), None),
        (Some(rebound_origin.as_str()), Some(rebound.as_str())),
        (None, Some(rebound.as_str())),
    ] {
        let mut headers = vec![
            ("Authorization", bearer.as_str()),
            ("Content-Type", "application/json"),
        ];
        headers.extend(origin.map(|o| ("Origin", o)));
        headers.extend(host.map(|h| ("Host", h)));
        let reply = harness.http("POST", &headers, &body);
        assert_eq!(reply.status, 403, "{origin:?} {host:?}");
    }
    assert_eq!(
        harness.sessions.load(Ordering::SeqCst),
        0,
        "no session opened"
    );
    // The server's own origin, and no `Origin` at all (a native client), get in.
    let own = harness.own_origin();
    let reply = harness.http(
        "POST",
        &[
            ("Authorization", &bearer),
            ("Content-Type", "application/json"),
            ("Origin", &own),
        ],
        &body,
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    let reply = harness.http(
        "POST",
        &[
            ("Authorization", &bearer),
            ("Content-Type", "application/json"),
        ],
        &body,
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
}

/// The MCP endpoint is off unless the host turns it on.
#[test]
fn mcp_is_off_by_default() {
    let harness = start(&MockEngine::new());
    let reply = harness.mcp(None, &mcp_initialize("agent"));
    assert_eq!(reply.status, 404);
    assert_eq!(harness.sessions.load(Ordering::SeqCst), 0);
}

/// Each `initialize` opens its own session, with its own engine client; requests carrying
/// its ID act as that client, and the engine sees the edits.
#[test]
fn mcp_sessions_are_separate_clients() {
    let engine = MockEngine::new();
    let harness = start_with(&engine, mcp_on());
    let first = harness.mcp_session("agent-a");
    let second = harness.mcp_session("agent-b");
    assert_ne!(first, second);
    assert_eq!(first.len(), 32, "{first}");
    assert_eq!(
        harness.sessions.load(Ordering::SeqCst),
        2,
        "one per initialize"
    );

    let list = serde_json::json!({"jsonrpc": "2.0", "id": 5, "method": "tools/list"});
    let reply = harness.mcp(Some(&first), &list);
    assert_eq!(reply.status, 200);
    assert!(
        reply.json()["result"]["tools"]
            .as_array()
            .is_some_and(|t| !t.is_empty())
    );

    let reply = harness.mcp(Some(&first), &mcp_create_scene("main", None));
    assert_eq!(reply.status, 200, "{}", reply.body);
    let result = &reply.json()["result"];
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["structuredContent"]["result"]["rev"], 1, "{result}");
    // The second session sees the shared engine's state.
    let reply = harness.mcp(Some(&second), &mcp_create_scene("main", None));
    let result = &reply.json()["result"];
    assert_eq!(result["isError"], true, "the scene exists: {result}");

    // A WebSocket client sees the MCP client's edit.
    let (control, _) = harness.session();
    let response = ok(harness.run(control.call(Call::Query {
        query: Query::SceneList,
    })));
    assert!(format!("{response:?}").contains("main"), "{response:?}");
}

/// Requests without `Mcp-Session-Id` (MCP 2026-07-28) share one stateless server.
#[test]
fn mcp_stateless_requests_share_one_server() {
    let harness = start_with(&MockEngine::new(), mcp_on());
    let discover = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "server/discover",
        "params": {"_meta": modern_meta()}
    });
    let reply = harness.mcp(None, &discover);
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.header("mcp-session-id"), None);
    assert!(reply.json()["result"]["supportedVersions"].is_array());
    for scene in ["one", "two"] {
        let reply = harness.mcp(None, &mcp_create_scene(scene, Some(modern_meta())));
        assert_eq!(reply.json()["result"]["isError"], false, "{}", reply.body);
    }
    assert_eq!(
        harness.sessions.load(Ordering::SeqCst),
        1,
        "one shared session"
    );
    // A handshake-era request without a session isn't served statelessly.
    let reply = harness.mcp(None, &mcp_create_scene("three", None));
    assert_eq!(reply.json()["error"]["code"], -32602, "{}", reply.body);
}

/// `DELETE` ends a session, an unknown ID is 404, `GET` is 405, and a failed
/// `initialize` keeps no session.
#[test]
fn mcp_sessions_end_and_unknown_ones_are_not_found() {
    let harness = start_with(&MockEngine::new(), mcp_on());
    let bearer = format!("Bearer {TOKEN}");
    let id = harness.mcp_session("agent");
    let reply = harness.http(
        "DELETE",
        &[("Authorization", &bearer), ("Mcp-Session-Id", &id)],
        "",
    );
    assert_eq!(reply.status, 204);
    let reply = harness.mcp(Some(&id), &mcp_create_scene("main", None));
    assert_eq!(reply.status, 404, "{}", reply.body);
    let reply = harness.http(
        "DELETE",
        &[("Authorization", &bearer), ("Mcp-Session-Id", &id)],
        "",
    );
    assert_eq!(reply.status, 404);
    let reply = harness.http("DELETE", &[("Authorization", &bearer)], "");
    assert_eq!(reply.status, 400);
    let reply = harness.http("GET", &[("Authorization", &bearer)], "");
    assert_eq!(reply.status, 405);
    assert_eq!(reply.header("allow"), Some("POST, DELETE"));

    // An `initialize` on an open session replaces it.
    let old = harness.mcp_session("agent");
    let reply = harness.mcp(Some(&old), &mcp_initialize("agent"));
    assert_eq!(reply.status, 200, "{}", reply.body);
    let new = reply.header("mcp-session-id").map(str::to_owned);
    assert!(new.as_ref().is_some_and(|new| *new != old), "{new:?}");
    let reply = harness.mcp(Some(&old), &mcp_create_scene("main", None));
    assert_eq!(reply.status, 404, "the old session ended: {}", reply.body);

    let bad = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
    let reply = harness.mcp(None, &bad);
    assert_eq!(reply.json()["error"]["code"], -32602, "{}", reply.body);
    assert_eq!(reply.header("mcp-session-id"), None);
}

/// Malformed requests: a body that isn't declared JSON, an unsupported
/// `MCP-Protocol-Version`, a body over 4 MiB, and a body that doesn't parse.
#[test]
fn mcp_refuses_malformed_requests() {
    let harness = start_with(&MockEngine::new(), mcp_on());
    let bearer = format!("Bearer {TOKEN}");
    let body = mcp_initialize("agent").to_string();
    let reply = harness.http(
        "POST",
        &[("Authorization", &bearer), ("Content-Type", "text/plain")],
        &body,
    );
    assert_eq!(reply.status, 415);
    let reply = harness.http(
        "POST",
        &[
            ("Authorization", &bearer),
            ("Content-Type", "application/json; charset=utf-8"),
            ("MCP-Protocol-Version", "1900-01-01"),
        ],
        &body,
    );
    assert_eq!(reply.status, 400);
    // A body declared over 4 MiB is refused from its `Content-Length`, before it is
    // read. Only the headers are sent: the server may close the connection as soon as it
    // has answered, so writing the whole body could fail on the client's side.
    let addr = harness.authority.clone();
    let reply = harness.run(async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = match tokio::net::TcpStream::connect(&addr).await {
            Ok(stream) => stream,
            Err(error) => panic!("connect: {error}"),
        };
        let head = format!(
            "POST {} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: {bearer}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            crate::MCP_PATH,
            5 << 20
        );
        if let Err(error) = stream.write_all(head.as_bytes()).await {
            panic!("write: {error}");
        }
        // The status line is enough; the server may keep the connection open.
        let mut answer = Vec::new();
        let mut chunk = [0u8; 1024];
        while !answer.windows(2).any(|w| w == b"\r\n") {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => answer.extend_from_slice(&chunk[..n]),
            }
        }
        String::from_utf8_lossy(&answer).into_owned()
    });
    assert!(reply.starts_with("HTTP/1.1 413"), "{reply}");
    assert_eq!(
        harness.sessions.load(Ordering::SeqCst),
        0,
        "no session opened"
    );
    // A body that doesn't parse goes to the stateless server, which answers it.
    let reply = harness.http(
        "POST",
        &[
            ("Authorization", &bearer),
            ("Content-Type", "application/json"),
        ],
        "{not json",
    );
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["error"]["code"], -32700, "{}", reply.body);
}

/// `initialize` beyond `max_mcp_sessions` is 503; an idle session expires, freeing its
/// place.
#[test]
fn mcp_sessions_are_capped_and_expire() {
    let harness = start_with(&MockEngine::new(), |config| {
        config.mcp = true;
        config.max_mcp_sessions = 1;
        config.mcp_idle_timeout = Duration::from_millis(300);
    });
    let id = harness.mcp_session("agent");
    let reply = harness.mcp(None, &mcp_initialize("other"));
    assert_eq!(reply.status, 503, "{}", reply.body);
    harness.run(async { tokio::time::sleep(Duration::from_millis(400)).await });
    let reply = harness.mcp(Some(&id), &mcp_create_scene("main", None));
    assert_eq!(reply.status, 404, "expired: {}", reply.body);
    harness.mcp_session("other");
}

/// An authenticated MCP connection may carry more requests after the header-read
/// timeout has passed (HTTP keep-alive).
#[test]
fn mcp_keep_alive_outlasts_the_header_read_timeout() {
    let harness = start_with(&MockEngine::new(), |config| {
        config.mcp = true;
        config.header_read_timeout = Duration::from_millis(300);
    });
    let addr = harness.authority.clone();
    let bearer = format!("Bearer {TOKEN}");
    let headers = [
        ("Host", addr.as_str()),
        ("Authorization", bearer.as_str()),
        ("Content-Type", "application/json"),
    ];
    let body = mcp_initialize("agent").to_string();
    let (first, second) = harness.run(async {
        let mut stream = match tokio::net::TcpStream::connect(&addr).await {
            Ok(stream) => stream,
            Err(error) => panic!("connect: {error}"),
        };
        let first = http_on(&mut stream, "POST", &headers, &body).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        let second = http_on(&mut stream, "POST", &headers, &body).await;
        (first, second)
    });
    assert_eq!((first.status, second.status), (200, 200));
}

/// A mock session with a binary channel the test feeds: [`Control::binary`] hands the
/// sending end of a fresh channel to `feed`.
struct Framed {
    inner: jstream_control::MockControl,
    feed: Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<BinaryFrame>>>>,
}

impl Control for Framed {
    fn hello(&self, hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
        self.inner.hello(hello)
    }

    fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
        self.inner.call(call)
    }

    fn events(&self) -> EventStream {
        self.inner.events()
    }

    fn binary(&self) -> BinaryStream {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        *self
            .feed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tx);
        Box::pin(futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx)))
    }
}

fn preview_frame(channel: u32, keyframe: bool, n: u8) -> BinaryFrame {
    BinaryFrame {
        header: BinaryHeader {
            kind: StreamKind::Preview,
            channel,
            codec: Some(PreviewCodec::H264),
            keyframe,
            end: false,
            width: 640,
            height: 360,
            pts_us: i64::from(n) * 66_666,
            rev: Rev(u64::from(n)),
        },
        payload: Arc::from(&[0, 0, 0, 1, n][..]),
    }
}

/// The session's binary frames reach the client as binary messages, in order and
/// byte for byte, once the handshake has opened the channel; the server reads none
/// from the client.
#[test]
fn preview_frames_go_out_as_binary_messages() {
    let engine = MockEngine::new();
    let feed = Arc::new(std::sync::Mutex::new(None));
    let slot = Arc::clone(&feed);
    let connector: Connector = Arc::new(move || {
        Box::new(Framed {
            inner: engine.connect(),
            feed: Arc::clone(&slot),
        })
    });
    let counter = || Arc::new(AtomicUsize::new(0));
    let silent = Arc::new(AtomicBool::new(false));
    let harness = serve(connector, |_| {}, counter(), counter(), counter(), silent);
    let (control, _) = harness.session();
    let mut frames = control.binary();
    // The server opened the session's channel when the handshake succeeded.
    let sender = feed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .expect("the server opened the binary channel");
    let sent: Vec<_> = (0..3).map(|n| preview_frame(5, n == 0, n)).collect();
    for frame in &sent {
        assert!(sender.send(frame.clone()).is_ok());
    }
    let end = BinaryFrame::end(StreamKind::Preview, 5, Rev(3), "the encoder stopped");
    assert!(sender.send(end.clone()).is_ok());
    let received: Vec<_> = harness.run(async {
        let mut received = Vec::new();
        while received.len() < 4 {
            match frames.next().await {
                Some(frame) => received.push(frame),
                None => panic!("the binary channel ended"),
            }
        }
        received
    });
    assert_eq!(received[..3], sent[..]);
    assert_eq!(received[3], end);

    // A client's binary message is not part of the protocol.
    let bearer = format!("Bearer {TOKEN}");
    let mut socket = match harness.raw(
        Some(&harness.own_origin()),
        Some(&harness.authority.clone()),
        Some(&bearer),
    ) {
        Ok(socket) => socket,
        Err(status) => panic!("refused: {status}"),
    };
    let reply = harness.run(async {
        if let Err(error) = socket.send(Message::Binary(vec![1, 2, 3].into())).await {
            panic!("send: {error}");
        }
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(reply))) => {
                    match serde_json::from_str::<ServerMessage>(reply.as_str()) {
                        Ok(message) => return message,
                        Err(error) => panic!("unreadable reply {reply}: {error}"),
                    }
                }
                Some(Ok(_)) => continue,
                other => panic!("no reply: {other:?}"),
            }
        }
    });
    match reply {
        ServerMessage::Error(error) => assert_eq!(error.error.error.code(), "malformed"),
        other => panic!("unexpected reply {other:?}"),
    }
}

/// A UI bundle in a fresh temporary directory, removed on drop, beside a `secret` file
/// outside it that no request may reach.
struct Bundle {
    base: std::path::PathBuf,
}

impl Bundle {
    fn new(name: &str) -> Bundle {
        let base =
            std::env::temp_dir().join(format!("jstream-server-ui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let files: [(&str, &[u8]); 4] = [
            (
                "dist/index.html",
                b"<script nonce=\"{{__TRUNK NONCE__}}\" type=\"module\">init()</script>",
            ),
            ("dist/app_bg.wasm", b"\0asm\x01\0\0\0"),
            ("dist/assets/style.css", b"body {}"),
            ("secret", b"not for the browser"),
        ];
        for (path, contents) in files {
            let path = base.join(path);
            if let Some(parent) = path.parent()
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                panic!("create {}: {error}", parent.display());
            }
            if let Err(error) = std::fs::write(&path, contents) {
                panic!("write {}: {error}", path.display());
            }
        }
        #[cfg(unix)]
        if let Err(error) =
            std::os::unix::fs::symlink(base.join("secret"), base.join("dist/link.txt"))
        {
            panic!("symlink: {error}");
        }
        Bundle { base }
    }

    fn dist(&self) -> std::path::PathBuf {
        self.base.join("dist")
    }
}

impl Drop for Bundle {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Harness {
    /// Sends `method path` on a new connection, with `headers` (`Host` is added unless
    /// given).
    fn request(&self, method: &str, path: &str, headers: &[(&str, &str)]) -> HttpReply {
        let addr = self.authority.clone();
        let mut all: Vec<(&str, &str)> = Vec::new();
        if !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("host")) {
            all.push(("Host", &addr));
        }
        all.extend_from_slice(headers);
        self.run(async {
            let mut stream = match tokio::net::TcpStream::connect(&addr).await {
                Ok(stream) => stream,
                Err(error) => panic!("connect: {error}"),
            };
            http_to(&mut stream, method, path, &all, "").await
        })
    }
}

/// `Basic` credentials whose password is the test token, as a browser sends them.
fn basic() -> String {
    // base64("jstream:test-token-0123")
    "Basic anN0cmVhbTp0ZXN0LXRva2VuLTAxMjM=".to_owned()
}

#[test]
fn the_ui_is_off_by_default() {
    let harness = start(&MockEngine::new());
    let bearer = format!("Bearer {TOKEN}");
    let reply = harness.request("GET", "/", &[("Authorization", &bearer)]);
    assert_eq!(reply.status, 404);
}

#[test]
fn the_ui_needs_the_token_and_challenges_a_browser() {
    let bundle = Bundle::new("token");
    let dist = bundle.dist();
    let harness = start_with(&MockEngine::new(), |config| config.ui_dir = Some(dist));
    for authorization in [
        None,
        Some("Bearer wrong"),
        Some("Basic anN0cmVhbTp3cm9uZw=="),
    ] {
        let mut headers = Vec::new();
        if let Some(value) = authorization {
            headers.push(("Authorization", value));
        }
        let reply = harness.request("GET", "/", &headers);
        assert_eq!(reply.status, 401, "{authorization:?}");
        assert_eq!(
            reply.header("www-authenticate"),
            Some("Basic realm=\"JStream\", charset=\"UTF-8\"")
        );
        assert!(!reply.body.contains("init()"), "{}", reply.body);
    }
    // A foreign Origin or Host is refused even with the token.
    let basic = basic();
    let foreign = harness.request(
        "GET",
        "/",
        &[("Authorization", &basic), ("Origin", "http://evil.example")],
    );
    assert_eq!(foreign.status, 403);
    let rebound = harness.request(
        "GET",
        "/",
        &[("Authorization", &basic), ("Host", "evil.example:80")],
    );
    assert_eq!(rebound.status, 403);
    assert_eq!(harness.sessions.load(Ordering::SeqCst), 0);
}

#[test]
fn the_ui_serves_its_files_with_types_a_nonce_and_a_policy() {
    let bundle = Bundle::new("files");
    let dist = bundle.dist();
    let harness = start_with(&MockEngine::new(), |config| config.ui_dir = Some(dist));
    let basic = basic();
    let auth = [("Authorization", basic.as_str())];

    let index = harness.request("GET", "/", &auth);
    assert_eq!(index.status, 200, "{}", index.body);
    assert_eq!(
        index.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(index.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(index.header("cache-control"), Some("no-cache"));
    let Some(policy) = index.header("content-security-policy") else {
        panic!("no CSP");
    };
    let Some(nonce) = policy
        .split("'nonce-")
        .nth(1)
        .and_then(|rest| rest.split('\'').next())
    else {
        panic!("no nonce in {policy}");
    };
    assert_eq!(nonce.len(), 32, "{nonce}");
    assert_eq!(
        index.body,
        format!("<script nonce=\"{nonce}\" type=\"module\">init()</script>")
    );
    assert!(
        policy.contains(&format!("connect-src 'self' ws://{};", harness.authority)),
        "{policy}"
    );
    // A fresh nonce on every response.
    let again = harness.request("GET", "/index.html", &auth);
    assert_eq!(again.status, 200);
    assert!(!again.body.contains(nonce), "{}", again.body);

    let wasm = harness.request("GET", "/app_bg.wasm", &auth);
    assert_eq!(wasm.status, 200);
    assert_eq!(wasm.header("content-type"), Some("application/wasm"));
    assert_eq!(wasm.body.as_bytes(), b"\0asm\x01\0\0\0");
    assert!(
        wasm.header("content-security-policy")
            .is_some_and(|p| !p.contains("nonce")),
        "no nonce outside HTML"
    );

    let css = harness.request("GET", "/assets/style.css", &auth);
    assert_eq!(
        (css.status, css.header("content-type")),
        (200, Some("text/css; charset=utf-8"))
    );
    let head = harness.request("HEAD", "/assets/style.css", &auth);
    assert_eq!((head.status, head.body.as_str()), (200, ""));

    // The UI's own routes serve the index; a missing file is 404.
    let route = harness.request("GET", "/scenes/main", &auth);
    assert_eq!(route.status, 200);
    assert!(route.body.contains("init()"));
    assert_eq!(harness.request("GET", "/missing.js", &auth).status, 404);
    // The API's paths are never the UI's: `/mcp` with MCP off, or anything under `/api/`.
    for path in ["/mcp", "/api/v2/ws", "/api"] {
        assert_eq!(harness.request("GET", path, &auth).status, 404, "{path}");
    }
    // Only GET and HEAD.
    let post = harness.request("POST", "/", &auth);
    assert_eq!(
        (post.status, post.header("allow")),
        (405, Some("GET, HEAD"))
    );
    // The bearer token works too, for native clients.
    let bearer = format!("Bearer {TOKEN}");
    assert_eq!(
        harness
            .request("GET", "/", &[("Authorization", &bearer)])
            .status,
        200
    );
}

#[test]
fn the_ui_never_serves_a_file_outside_its_directory() {
    let bundle = Bundle::new("escape");
    let dist = bundle.dist();
    let harness = start_with(&MockEngine::new(), |config| config.ui_dir = Some(dist));
    let basic = basic();
    let auth = [("Authorization", basic.as_str())];
    for path in [
        "/../secret",
        "/%2e%2e/secret",
        "/assets/..%2f..%2fsecret",
        "/..%5csecret",
        "//secret",
        "/link.txt",
    ] {
        let reply = harness.request("GET", path, &auth);
        assert!(
            !reply.body.contains("not for the browser"),
            "{path}: {}",
            reply.body
        );
        assert_ne!(reply.status, 200, "{path}");
    }
}

#[test]
fn binding_refuses_a_ui_dir_that_is_not_a_directory() {
    let harness = start(&MockEngine::new());
    let engine = MockEngine::new();
    let connector: Connector = Arc::new(move || Box::new(engine.connect()));
    let mut config = match Token::new(TOKEN) {
        Ok(token) => ServerConfig::new(token),
        Err(error) => panic!("token: {error}"),
    };
    config.ui_dir = Some(std::env::temp_dir().join("jstream-server-ui-no-such-dir"));
    match harness.run(Server::bind(config, connector)) {
        Err(crate::ServerError::UiDir { .. }) => {}
        Err(other) => panic!("expected UiDir, got {other}"),
        Ok(server) => panic!("bound on {} without a UI dir", server.local_addr()),
    }
}

/// The browser's cached `Basic` credentials ride on the UI's WebSocket upgrade too, so
/// the page never handles the token.
#[test]
fn a_websocket_upgrade_accepts_basic_credentials() {
    let harness = start(&MockEngine::new());
    let origin = harness.own_origin();
    let host = harness.authority.clone();
    assert!(
        harness
            .raw(Some(&origin), Some(&host), Some(&basic()))
            .is_ok()
    );
}

/// `jstream-control`'s browser client offers `jstream` and `jstream.token.<token>`
/// without `Authorization`: the server admits it and selects `jstream`, never echoing
/// the token's subprotocol.
#[test]
fn a_websocket_upgrade_accepts_the_token_as_a_subprotocol() {
    let harness = start(&MockEngine::new());
    let origin = harness.own_origin();
    let offer = |token: &str| {
        let mut request = match harness.url.as_str().into_client_request() {
            Ok(request) => request,
            Err(error) => panic!("request: {error}"),
        };
        let headers = request.headers_mut();
        let protocols = format!("jstream, jstream.token.{token}");
        for (name, value) in [
            (header::ORIGIN, origin.as_str()),
            (header::SEC_WEBSOCKET_PROTOCOL, protocols.as_str()),
        ] {
            match HeaderValue::from_str(value) {
                Ok(value) => headers.insert(name, value),
                Err(error) => panic!("header: {error}"),
            };
        }
        harness.run(tokio_tungstenite::connect_async(request))
    };
    match offer(TOKEN) {
        Ok((_, response)) => assert_eq!(
            response
                .headers()
                .get(header::SEC_WEBSOCKET_PROTOCOL)
                .and_then(|v| v.to_str().ok()),
            Some("jstream")
        ),
        Err(error) => panic!("refused: {error}"),
    }
    match offer("wrong-token") {
        Err(tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED)
        }
        other => panic!("expected 401, got {:?}", other.map(|(_, r)| r.status())),
    }
}

// Pairing (ADR-0044).

impl Harness {
    /// POSTs `body` as JSON to the pairing path on a new connection, with `headers`
    /// (`Host` is added unless given).
    fn post_pair(&self, headers: &[(&str, &str)], body: &str) -> HttpReply {
        let addr = self.authority.clone();
        let mut all: Vec<(&str, &str)> = vec![("Content-Type", "application/json")];
        if !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("host")) {
            all.push(("Host", &addr));
        }
        all.extend_from_slice(headers);
        self.run(async {
            let mut stream = match tokio::net::TcpStream::connect(&addr).await {
                Ok(stream) => stream,
                Err(error) => panic!("connect: {error}"),
            };
            http_to(&mut stream, "POST", crate::PAIR_PATH, &all, body).await
        })
    }

    /// Starts pairing over `control` and returns the code.
    fn start_pairing(&self, control: &WsControl) -> String {
        let response = ok(self.run(control.call(Call::Action {
            action: Action::PairingStart,
        })));
        match response.result {
            CallResult::Action {
                result:
                    Some(ActionResult::PairingStart {
                        code,
                        expires_in_secs,
                    }),
            } => {
                assert_eq!(expires_in_secs, 120);
                code.expose().to_owned()
            }
            other => panic!("expected a pairing code, got {other:?}"),
        }
    }

    /// Pairs a device named `name` with `code`, returning its ID and token.
    fn pair(&self, code: &str, name: &str) -> (String, String) {
        let body = serde_json::json!({"code": code, "name": name}).to_string();
        let origin = self.own_origin();
        let reply = self.post_pair(&[("Origin", &origin)], &body);
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(reply.header("cache-control"), Some("no-store"));
        let answer = reply.json();
        match (answer["device_id"].as_str(), answer["token"].as_str()) {
            (Some(id), Some(token)) => (id.to_owned(), token.to_owned()),
            _ => panic!("no device in {answer}"),
        }
    }

    fn device_list(&self, control: &WsControl) -> Vec<DeviceInfo> {
        let call = Call::Query {
            query: Query::DeviceList,
        };
        match ok(self.run(control.call(call))).result {
            CallResult::Query {
                result: QueryResult::DeviceList { devices },
            } => devices,
            other => panic!("expected device.list, got {other:?}"),
        }
    }
}

/// A code that differs from `code` in its last symbol.
fn wrong_code(code: &str) -> String {
    if code.ends_with('0') {
        format!("{}1", &code[..8])
    } else {
        format!("{}0", &code[..8])
    }
}

/// ADR-0044 end to end: an authenticated client starts pairing, a device trades the code
/// for its own token, connects with it as itself, and a revoke closes it and locks it out.
#[test]
fn a_paired_device_connects_with_its_own_token_until_revoked() {
    let harness = start(&MockEngine::new());
    let (host, _) = harness.session();
    // No code open: the one unauthenticated request is refused like a missing token.
    let body = r#"{"code":"0000-0000","name":"Tablet"}"#;
    assert_eq!(harness.post_pair(&[], body).status, 401);

    let code = harness.start_pairing(&host);
    // A foreign page can't pair, even with the code.
    let foreign = serde_json::json!({"code": code, "name": "Evil"}).to_string();
    let reply = harness.post_pair(&[("Origin", "http://evil.example")], &foreign);
    assert_eq!(reply.status, 403);
    // A bad name uses up no attempt; then the code pairs, once.
    let unnamed = serde_json::json!({"code": code, "name": " "}).to_string();
    assert_eq!(harness.post_pair(&[], &unnamed).status, 400);
    let (device_id, token) = harness.pair(&code, "Tablet");
    assert_eq!(token.len(), 64);
    let again = serde_json::json!({"code": code, "name": "Again"}).to_string();
    assert_eq!(harness.post_pair(&[], &again).status, 401);

    // The device token gets in, and the device acts like any client.
    let device = ok(harness.run(WsControl::connect(WsConfig::new(
        harness.url.clone(),
        token.clone(),
    ))));
    ok(harness.run(device.hello(hello())));
    ok(harness.run(device.call(create_scene("scn_tablet"))));
    let devices = harness.device_list(&device);
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].device_id.as_str(), device_id);
    assert_eq!(devices[0].name, "Tablet");
    let origin = harness.own_origin();
    let bearer = format!("Bearer {token}");
    assert!(
        harness
            .raw(Some(&origin), Some(&harness.authority), Some(&bearer))
            .is_ok()
    );

    // Revoking it closes its connection and locks its token out.
    let mut events = device.events();
    let revoke = Call::Action {
        action: Action::DeviceRevoke {
            device_id: DeviceId::new(device_id.as_str()),
        },
    };
    ok(harness.run(host.call(revoke.clone())));
    while next_event(&harness, &mut events).is_some() {}
    let refused = harness.run(WsControl::connect(WsConfig::new(
        harness.url.clone(),
        token,
    )));
    assert!(expect_disconnected(refused).contains("401"));
    assert!(harness.device_list(&host).is_empty());
    assert_eq!(protocol_code(harness.run(host.call(revoke))), "not_found");
}

/// The server answers the pairing calls itself, after the handshake like any call, and
/// the newest code replaces the one open.
#[test]
fn pairing_needs_the_handshake_and_the_newest_code_wins() {
    let harness = start(&MockEngine::new());
    let control = ok(harness.connect());
    let early = harness.run(control.call(Call::Action {
        action: Action::PairingStart,
    }));
    assert_eq!(protocol_code(early), "handshake_required");
    ok(harness.run(control.hello(hello())));
    let code = harness.start_pairing(&control);
    let newer = harness.start_pairing(&control);
    if newer != code {
        let stale = serde_json::json!({"code": code, "name": "Old"}).to_string();
        assert_eq!(harness.post_pair(&[], &stale).status, 403);
    }
    harness.pair(&newer, "Kitchen tablet");
}

/// Five wrong codes void the open one, so a guesser gets five tries per code.
#[test]
fn five_wrong_codes_void_the_open_one() {
    let harness = start(&MockEngine::new());
    let (control, _) = harness.session();
    let code = harness.start_pairing(&control);
    let guess = serde_json::json!({"code": wrong_code(&code), "name": "Guess"}).to_string();
    for _ in 0..5 {
        assert_eq!(harness.post_pair(&[], &guess).status, 403);
    }
    let right = serde_json::json!({"code": code, "name": "Late"}).to_string();
    assert_eq!(harness.post_pair(&[], &right).status, 401);
}

/// MCP agents reach the pairing calls as the `pairing` and `device` tools, and a device
/// token opens MCP sessions of its own, which the control token can't use.
#[test]
fn mcp_agents_can_pair_and_list_devices() {
    let harness = start_with(&MockEngine::new(), mcp_on());
    let session = harness.mcp_session("agent");
    let call = |tool: &str, op: serde_json::Value| {
        serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                           "params": {"name": tool, "arguments": {"op": op}}})
    };
    let reply = harness.mcp(
        Some(&session),
        &call("pairing", serde_json::json!({"type": "pairing.start"})),
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    let answer = reply.json();
    let Some(code) = answer
        .pointer("/result/structuredContent/result/result/code")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
    else {
        panic!("no code in {answer}");
    };
    let (device_id, token) = harness.pair(&code, "Agent box");
    let reply = harness.mcp(
        Some(&session),
        &call("device", serde_json::json!({"type": "device.list"})),
    );
    let listed = reply.json();
    assert_eq!(
        listed.pointer("/result/structuredContent/result/result/devices/0/device_id"),
        Some(&serde_json::Value::from(device_id.as_str())),
        "{listed}"
    );
    assert!(
        !reply.body.contains(&token),
        "device tokens are never reported"
    );
    // The device's own session; the control token can't use it.
    let bearer = format!("Bearer {token}");
    let headers = [
        ("Authorization", bearer.as_str()),
        ("Content-Type", "application/json"),
        ("Accept", "application/json, text/event-stream"),
    ];
    let reply = harness.http("POST", &headers, &mcp_initialize("device").to_string());
    assert_eq!(reply.status, 200, "{}", reply.body);
    let Some(device_session) = reply.header("mcp-session-id").map(str::to_owned) else {
        panic!("no session");
    };
    let note = serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(harness.mcp(Some(&device_session), &note).status, 404);
}

/// R-SEC-001: neither a pairing code nor a device token appears in what the server
/// logs, from starting pairing, through wrong and right attempts and the device's
/// connection, to its revocation.
#[test]
fn pairing_codes_and_device_tokens_are_never_logged() {
    let capture = crate::gate::tests::Capture::default();
    let mut secrets = Vec::new();
    tracing::subscriber::with_default(capture.clone(), || {
        let harness = start_on_this_thread(&MockEngine::new(), |_| {});
        let (host, _) = harness.session();
        let first = harness.start_pairing(&host);
        let wrong = wrong_code(&first);
        let guess = serde_json::json!({"code": wrong, "name": "Guess"}).to_string();
        for _ in 0..5 {
            assert_eq!(harness.post_pair(&[], &guess).status, 403);
        }
        let code = harness.start_pairing(&host);
        let (device_id, token) = harness.pair(&code, "Tablet");
        let device = ok(harness.run(WsControl::connect(WsConfig::new(
            harness.url.clone(),
            token.clone(),
        ))));
        ok(harness.run(device.hello(hello())));
        let revoke = Call::Action {
            action: Action::DeviceRevoke {
                device_id: DeviceId::new(device_id.as_str()),
            },
        };
        ok(harness.run(host.call(revoke)));
        let refused = harness.run(WsControl::connect(WsConfig::new(
            harness.url.clone(),
            token.clone(),
        )));
        assert!(refused.is_err());
        secrets.extend([first, wrong, code, token]);
    });
    let logged = capture.text();
    for expected in [
        "pairing started",
        "pairing code voided after 5 wrong attempts",
        "wrong pairing code",
        "device paired",
        "Tablet",
        "device revoked",
    ] {
        assert!(logged.contains(expected), "{expected} not logged: {logged}");
    }
    for secret in &secrets {
        let bare = secret.replace('-', "");
        assert!(
            !logged.contains(secret.as_str()) && !logged.contains(&bare),
            "{secret} logged: {logged}"
        );
    }
}

/// Sends a raw text frame and returns every text frame the server sends until the
/// answer (a response or an error), the answer last, as the server wrote them.
fn exchange_texts(harness: &Harness, socket: &mut RawSocket, text: &str) -> Vec<String> {
    harness.run(async {
        if let Err(error) = socket.send(Message::Text(text.into())).await {
            panic!("send: {error}");
        }
        let mut texts = Vec::new();
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(reply))) => {
                    let answered = matches!(
                        serde_json::from_str(reply.as_str()),
                        Ok(ServerMessage::Response(_) | ServerMessage::Error(_))
                    );
                    texts.push(reply.as_str().to_owned());
                    if answered {
                        return texts;
                    }
                }
                Some(Ok(_)) => continue,
                other => panic!("no answer to {text}: {other:?}"),
            }
        }
    })
}

/// R-SEC-001 over the server's WebSocket layer and its MCP endpoint (#353): a stream
/// key sent with `output.set_key` reaches the engine and nothing the server sends back
/// or logs. That covers the answer, the errors for a key of the wrong shape, for a key
/// that isn't a string and for a frame that isn't JSON, `output.status`, the events, and
/// MCP tool results, valid and refused.
#[test]
fn stream_keys_are_never_echoed_or_logged() {
    const SECRET: &str = "live_SERVER-REDACT-ME-41c9";
    let capture = crate::gate::tests::Capture::default();
    let engine = MockEngine::new();
    let mut seen = Vec::new();
    tracing::subscriber::with_default(capture.clone(), || {
        let harness = start_on_this_thread(&engine, mcp_on());
        let bearer = format!("Bearer {TOKEN}");
        let mut socket = match harness.raw(
            Some(&harness.own_origin()),
            Some(&harness.authority.clone()),
            Some(&bearer),
        ) {
            Ok(socket) => socket,
            Err(status) => panic!("refused: {status}"),
        };
        let hello = r#"{"type":"hello","protocol":{"major":0,"minor":7},"client_name":"raw"}"#;
        assert!(matches!(
            exchange(&harness, &mut socket, hello),
            ServerMessage::Welcome(_)
        ));
        let create = r#"{"type":"request","id":"c1","call":{"kind":"batch","edits":[{"type":"output.create","output_id":"out_yt","kind":{"type":"rtmp","target":{"type":"generic","url":"rtmps://a.rtmps.youtube.com:443/live2"}}}]}}"#;
        seen.extend(exchange_texts(&harness, &mut socket, create));
        let set_key = |id: &str, key: &str| {
            format!(
                r#"{{"type":"request","id":"{id}","call":{{"kind":"action","action":{{"type":"output.set_key","output_id":"out_yt","key":{key}}}}}}}"#
            )
        };
        let answers = [
            (set_key("k1", &format!("\"{SECRET}\"")), None),
            (
                set_key("k2", &format!("\"{SECRET} with spaces\"")),
                Some("invalid_argument"),
            ),
            (set_key("k3", &format!("[\"{SECRET}\"]")), Some("malformed")),
            (
                format!(r#"{{"type":"request","id":"k4","call":{{"kind":"action","action":{{"type":"output.set_key","key":"{SECRET}""#),
                Some("malformed"),
            ),
            (
                r#"{"type":"request","id":"k5","call":{"kind":"query","query":{"type":"output.status","output_id":"out_yt"}}}"#.to_owned(),
                None,
            ),
        ];
        for (frame, error) in answers {
            let texts = exchange_texts(&harness, &mut socket, &frame);
            let answer = texts.last().map(String::as_str).unwrap_or_default();
            match (serde_json::from_str::<ServerMessage>(answer), error) {
                (Ok(ServerMessage::Response(_)), None) => {}
                (Ok(ServerMessage::Error(got)), Some(code)) => {
                    assert_eq!(got.error.error.code(), code, "{answer}");
                }
                other => panic!("unexpected answer to {frame}: {other:?}"),
            }
            seen.extend(texts);
        }
        assert!(
            seen.iter().any(|text| text.contains("\"key_set\":true")),
            "output.status reports the key: {seen:?}"
        );

        let session = harness.mcp_session("agent");
        let mcp_set_key = |key: &str| {
            serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
                "name": "output",
                "arguments": {"op": {"type": "output.set_key", "output_id": "out_yt", "key": key}}
            }})
        };
        let reply = harness.mcp(Some(&session), &mcp_set_key(SECRET));
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(reply.json()["result"]["isError"], false, "{}", reply.body);
        seen.push(reply.body);
        let reply = harness.mcp(Some(&session), &mcp_set_key(&format!("{SECRET}\n")));
        assert_eq!(reply.json()["result"]["isError"], true, "{}", reply.body);
        seen.push(reply.body);
        let events = serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "events", "arguments": {}}});
        let reply = harness.mcp(Some(&session), &events);
        assert_eq!(reply.status, 200, "{}", reply.body);
        seen.push(reply.body);
    });
    assert_eq!(
        engine
            .stream_key(&OutputId::new("out_yt"))
            .as_ref()
            .map(jstream_control::protocol::SecretString::expose),
        Some(SECRET),
        "the key reached the engine"
    );
    let logged = capture.text();
    assert!(
        logged.contains("output.set_key") || logged.contains("tools/call"),
        "the server logged the calls: {logged}"
    );
    seen.push(logged);
    for text in &seen {
        assert!(!text.contains("REDACT-ME"), "the key leaked: {text}");
    }
}

// The browser pairing page and the device cookie (ADR-0047).

/// A server with the UI and `ServerConfig::tls` on, as if it were reached over TLS. The
/// test still speaks plain HTTP to it.
fn tls_ui(dist: std::path::PathBuf) -> impl FnOnce(&mut ServerConfig) {
    |config| {
        config.ui_dir = Some(dist);
        config.tls = true;
    }
}

impl Harness {
    /// The server's own origin on a TLS listener.
    fn tls_origin(&self) -> String {
        format!("https://{}", self.authority)
    }

    /// Opens a raw WebSocket as a browser holding `cookie` would: its own `https`
    /// `Origin`, the `Cookie` line, `Sec-Fetch-Site` when given, and no `Authorization`.
    fn cookie_upgrade(&self, cookie: &str, site: Option<&str>) -> Result<RawSocket, StatusCode> {
        let mut request = match self.url.as_str().into_client_request() {
            Ok(request) => request,
            Err(error) => panic!("request: {error}"),
        };
        let origin = self.tls_origin();
        let headers = request.headers_mut();
        for (name, value) in [
            ("origin", Some(origin.as_str())),
            ("cookie", Some(cookie)),
            ("sec-fetch-site", site),
        ] {
            if let Some(value) = value {
                match HeaderValue::from_str(value) {
                    Ok(value) => headers.insert(name, value),
                    Err(error) => panic!("header: {error}"),
                };
            }
        }
        match self.run(tokio_tungstenite::connect_async(request)) {
            Ok((socket, _)) => Ok(socket),
            Err(tungstenite::Error::Http(response)) => Err(response.status()),
            Err(error) => panic!("unexpected connection error: {error}"),
        }
    }
}

/// ADR-0047: without TLS there is no pairing page, the UI's `401` links to nothing, and
/// a browser exchange is refused without using up an attempt.
#[test]
fn without_tls_there_is_no_pairing_page_or_cookie() {
    let bundle = Bundle::new("no-tls");
    let dist = bundle.dist();
    let harness = start_with(&MockEngine::new(), |config| config.ui_dir = Some(dist));
    let bearer = format!("Bearer {TOKEN}");
    for headers in [vec![], vec![("Authorization", bearer.as_str())]] {
        let page = harness.request("GET", crate::PAIR_PAGE_PATH, &headers);
        assert_eq!(page.status, 404, "{headers:?}");
        assert!(!page.body.contains("<form"), "{}", page.body);
    }
    let refused = harness.request("GET", "/", &[]);
    assert_eq!(refused.status, 401);
    assert!(!refused.body.contains("/pair"), "{}", refused.body);

    let (host, _) = harness.session();
    let code = harness.start_pairing(&host);
    let browser = serde_json::json!({"code": code, "name": "Browser", "browser": true});
    let reply = harness.post_pair(&[], &browser.to_string());
    assert_eq!(reply.status, 400, "{}", reply.body);
    assert_eq!(reply.header("set-cookie"), None);
    let not_bool = serde_json::json!({"code": code, "name": "Browser", "browser": "yes"});
    assert_eq!(harness.post_pair(&[], &not_bool.to_string()).status, 400);
    // The code is still open: a native exchange (with `browser: false`) pairs.
    let native = serde_json::json!({"code": code, "name": "Native", "browser": false});
    let reply = harness.post_pair(&[], &native.to_string());
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(reply.json()["token"].as_str().is_some());
    assert_eq!(reply.header("set-cookie"), None);
}

/// ADR-0047: on a TLS listener `GET /pair` is a fixed page served without a token,
/// after the `Host` and `Origin` checks, under the UI's nonce policy; the UI's `401`
/// links to it, and the UI's fallback never serves the path.
#[test]
fn on_tls_the_pairing_page_is_served_without_a_token() {
    let bundle = Bundle::new("tls-page");
    let harness = start_with(&MockEngine::new(), tls_ui(bundle.dist()));
    let page = harness.request("GET", crate::PAIR_PAGE_PATH, &[]);
    assert_eq!(page.status, 200, "{}", page.body);
    assert_eq!(
        page.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(page.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(page.header("referrer-policy"), Some("no-referrer"));
    assert_eq!(page.header("set-cookie"), None);
    let Some(policy) = page.header("content-security-policy") else {
        panic!("no CSP");
    };
    let Some(nonce) = policy
        .split("'nonce-")
        .nth(1)
        .and_then(|rest| rest.split('\'').next())
    else {
        panic!("no nonce in {policy}");
    };
    assert_eq!(nonce.len(), 32);
    assert_eq!(
        page.body
            .matches(&format!("<script nonce=\"{nonce}\">"))
            .count(),
        1,
        "{}",
        page.body
    );
    assert!(!page.body.contains("{{__TRUNK NONCE__}}"));
    assert!(
        policy.contains(&format!("connect-src 'self' wss://{};", harness.authority)),
        "{policy}"
    );
    assert!(!page.body.contains("init()"), "not the UI bundle");
    let head = harness.request("HEAD", crate::PAIR_PAGE_PATH, &[]);
    assert_eq!(head.status, 200);
    let post = harness.request("POST", crate::PAIR_PAGE_PATH, &[]);
    assert_eq!(
        (post.status, post.header("allow")),
        (405, Some("GET, HEAD"))
    );

    // Still behind the `Host` and `Origin` checks.
    let foreign = harness.request(
        "GET",
        crate::PAIR_PAGE_PATH,
        &[("Origin", "https://evil.example")],
    );
    assert_eq!(foreign.status, 403);
    let plain_origin = format!("http://{}", harness.authority);
    let plain = harness.request("GET", crate::PAIR_PAGE_PATH, &[("Origin", &plain_origin)]);
    assert_eq!(plain.status, 403, "a TLS listener's own origin is https");
    let rebound = harness.request(
        "GET",
        crate::PAIR_PAGE_PATH,
        &[("Host", "evil.example:443")],
    );
    assert_eq!(rebound.status, 403);

    // A UI path without a credential: the `Basic` challenge and a link to the page.
    let refused = harness.request("GET", "/", &[]);
    assert_eq!(refused.status, 401);
    assert_eq!(
        refused.header("www-authenticate"),
        Some("Basic realm=\"JStream\", charset=\"UTF-8\"")
    );
    assert!(
        refused.body.contains("<a href=\"/pair\">"),
        "{}",
        refused.body
    );
    assert_eq!(
        refused.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    // Paths below `/pair` are reserved too: the UI never serves its index for them.
    let basic = basic();
    let below = harness.request("GET", "/pair/x", &[("Authorization", &basic)]);
    assert_eq!(below.status, 404, "{}", below.body);
    let ui = harness.request("GET", "/scenes", &[("Authorization", &basic)]);
    assert_eq!(ui.status, 200, "the fallback still serves the UI's routes");
    assert_eq!(harness.sessions.load(Ordering::SeqCst), 0);
}

/// The value of the `__Host-jstream_device` cookie a `Set-Cookie` line sets, after
/// checking it has every attribute ADR-0047 names.
fn set_device_cookie(set_cookie: Option<&str>) -> String {
    let Some(line) = set_cookie else {
        panic!("no Set-Cookie");
    };
    let mut parts = line.split("; ");
    let Some(token) = parts
        .next()
        .and_then(|pair| pair.strip_prefix("__Host-jstream_device="))
    else {
        panic!("not the device cookie: {line}");
    };
    let attributes: Vec<&str> = parts.collect();
    assert_eq!(
        attributes,
        [
            "Secure",
            "HttpOnly",
            "SameSite=Strict",
            "Path=/",
            "Max-Age=2592000"
        ],
        "{line}"
    );
    token.to_owned()
}

/// ADR-0047 end to end: a browser exchange answers the device ID only and sets the
/// token as the cookie; the cookie then admits the browser to the UI and its WebSocket,
/// as that device, only on same-origin or typed requests, only once per request, never
/// for the control token, and not after a revoke. Nothing logs the cookie.
#[test]
fn a_browser_pairs_into_a_cookie_that_admits_it_until_revoked() {
    let bundle = Bundle::new("tls-cookie");
    let dist = bundle.dist();
    let capture = crate::gate::tests::Capture::default();
    let mut token = String::new();
    tracing::subscriber::with_default(capture.clone(), || {
        let harness = start_on_this_thread(&MockEngine::new(), tls_ui(dist));
        let (host, _) = harness.session();
        let code = harness.start_pairing(&host);
        let body = serde_json::json!({"code": code, "name": "Browser", "browser": true});
        let origin = harness.tls_origin();
        let reply = harness.post_pair(&[("Origin", &origin)], &body.to_string());
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(reply.header("cache-control"), Some("no-store"));
        token = set_device_cookie(reply.header("set-cookie"));
        assert_eq!(token.len(), 64);
        let answer = reply.json();
        let Some(device_id) = answer["device_id"].as_str().map(str::to_owned) else {
            panic!("no device ID in {answer}");
        };
        assert_eq!(answer.as_object().map(|o| o.len()), Some(1), "{answer}");
        assert!(
            !reply.body.contains(&token),
            "the page never sees the token"
        );

        let cookie = format!("__Host-jstream_device={token}");
        let get = |headers: &[(&str, &str)]| harness.request("GET", "/", headers).status;
        assert_eq!(get(&[("Cookie", &cookie), ("Sec-Fetch-Site", "none")]), 200);
        assert_eq!(
            get(&[
                ("Cookie", &cookie),
                ("Sec-Fetch-Site", "same-origin"),
                ("Origin", &origin)
            ]),
            200
        );
        // A page on another port or a sibling subdomain (same-site), another site, or a
        // browser that sends no `Sec-Fetch-Site` falls back to the challenge.
        for site in [Some("same-site"), Some("cross-site"), None] {
            let mut headers = vec![("Cookie", cookie.as_str())];
            if let Some(site) = site {
                headers.push(("Sec-Fetch-Site", site));
            }
            assert_eq!(get(&headers), 401, "{site:?}");
        }
        // Twice, on one line or two.
        let twice = format!("{cookie}; {cookie}");
        assert_eq!(get(&[("Cookie", &twice), ("Sec-Fetch-Site", "none")]), 401);
        assert_eq!(
            get(&[
                ("Cookie", &cookie),
                ("Cookie", &cookie),
                ("Sec-Fetch-Site", "none")
            ]),
            401
        );
        // The control token is never a cookie.
        let control = format!("__Host-jstream_device={TOKEN}");
        assert_eq!(
            get(&[("Cookie", &control), ("Sec-Fetch-Site", "none")]),
            401
        );

        // The UI's WebSocket, as the device.
        assert!(harness.cookie_upgrade(&cookie, Some("same-origin")).is_ok());
        assert_eq!(
            harness.cookie_upgrade(&cookie, Some("same-site")).err(),
            Some(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(
            harness.cookie_upgrade(&control, Some("same-origin")).err(),
            Some(StatusCode::UNAUTHORIZED)
        );
        let devices = harness.device_list(&host);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].device_id.as_str(), device_id);

        // Revoked, the cookie is worthless.
        ok(harness.run(host.call(Call::Action {
            action: Action::DeviceRevoke {
                device_id: DeviceId::new(device_id.as_str()),
            },
        })));
        assert_eq!(get(&[("Cookie", &cookie), ("Sec-Fetch-Site", "none")]), 401);
        assert_eq!(
            harness.cookie_upgrade(&cookie, Some("same-origin")).err(),
            Some(StatusCode::UNAUTHORIZED)
        );
    });
    let logged = capture.text();
    assert!(logged.contains("device paired"), "{logged}");
    assert!(
        logged.contains("device cookie without a same-origin Sec-Fetch-Site")
            && logged.contains("device cookie repeated"),
        "{logged}"
    );
    assert!(!token.is_empty() && !logged.contains(&token), "{logged}");
    assert!(!logged.contains(TOKEN), "{logged}");
}
