//! End-to-end tests: a [`MockEngine`] behind the server on an ephemeral `127.0.0.1` port,
//! and `jstream-control`'s WebSocket client (or a raw WebSocket) in front. Hermetic: only
//! loopback sockets (R-TEST-004).

use crate::{Connector, Server, ServerConfig, Token};
use futures_util::{SinkExt, StreamExt};
use jstream_control::protocol::{
    Action, Batch, Call, CallResult, ClientId, Edit, Event, Hello, OutputId, OutputState,
    PROTOCOL_VERSION, ProtocolError, ProtocolVersion, Query, QueryResult, RequestId, Response, Rev,
    SceneId, ServerMessage, Welcome,
};
use jstream_control::{
    BoxFuture, Control, ControlError, EventStream, MockEngine, WsConfig, WsControl,
};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::oneshot;
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
    stop: Option<oneshot::Sender<()>>,
}

fn start(engine: &MockEngine) -> Harness {
    start_with(engine, |_| {})
}

/// A server serving `engine`, with its configuration changed by `configure`.
fn start_with(engine: &MockEngine, configure: impl FnOnce(&mut ServerConfig)) -> Harness {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => panic!("runtime: {error}"),
    };
    let sessions = Arc::new(AtomicUsize::new(0));
    let open = Arc::new(AtomicUsize::new(0));
    let taken = Arc::new(AtomicUsize::new(0));
    let counters = (Arc::clone(&sessions), Arc::clone(&open), Arc::clone(&taken));
    let engine = engine.clone();
    let connector: Connector = Arc::new(move || {
        let (sessions, open, taken) = &counters;
        sessions.fetch_add(1, Ordering::SeqCst);
        open.fetch_add(1, Ordering::SeqCst);
        Box::new(Tracked {
            inner: engine.connect(),
            open: Arc::clone(open),
            taken: Arc::clone(taken),
        })
    });
    let mut config = match Token::new(TOKEN) {
        Ok(token) => ServerConfig::new(token),
        Err(error) => panic!("token: {error}"),
    };
    configure(&mut config);
    let server = match runtime.block_on(Server::bind(config, connector)) {
        Ok(server) => server,
        Err(error) => panic!("bind: {error}"),
    };
    assert!(
        server.local_addr().ip().is_loopback(),
        "binds to localhost by default"
    );
    let url = server.ws_url();
    let authority = server.local_addr().to_string();
    let (stop, stopped) = oneshot::channel::<()>();
    runtime.spawn(server.serve_with_shutdown(async move {
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
        stop: Some(stop),
    }
}

/// A mock session that counts, for the [`Harness`], the events the server takes from it
/// and whether the server still holds it. These are what the shutdown tests wait on,
/// rather than on wall-clock time, which a loaded machine stretches (#137).
struct Tracked {
    inner: jstream_control::MockControl,
    open: Arc<AtomicUsize>,
    taken: Arc<AtomicUsize>,
}

impl Control for Tracked {
    fn hello(&self, hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
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
        self.run(WsControl::connect(WsConfig::new(self.url.clone(), TOKEN)))
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
    assert_eq!(response.result, CallResult::Action {});
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
    for _ in names {
        match next_event(&harness, &mut events) {
            Some(Event::StateChanged {
                rev,
                client_id: from,
                request_id: Some(id),
            }) => {
                assert_eq!(from, client_id);
                seen.push((id, rev));
            }
            other => panic!("unexpected event {other:?}"),
        }
    }
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
    // The wire form from the protocol README: the response and the event carry the
    // client's own request ID.
    let hello = r#"{"type":"hello","protocol":{"major":0,"minor":5},"client_name":"raw"}"#;
    assert!(matches!(
        exchange(&harness, &mut socket, hello),
        ServerMessage::Welcome(_)
    ));
    let request = r#"{"type":"request","id":"r1","call":{"kind":"batch","edits":[{"type":"scene.create","scene_id":"scn_1","name":"Main"}],"expected_rev":0}}"#;
    let first = exchange(&harness, &mut socket, request);
    let second = harness.run(async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<ServerMessage>(text.as_str()) {
                        Ok(message) => return message,
                        Err(error) => panic!("unreadable: {error}"),
                    }
                }
                Some(Ok(_)) => continue,
                other => panic!("no second message: {other:?}"),
            }
        }
    });
    let mut ids = Vec::new();
    for message in [first, second] {
        match message {
            ServerMessage::Response(response) => ids.push(("response", response.id)),
            ServerMessage::Event(event) => match event.event {
                Event::StateChanged {
                    request_id: Some(id),
                    ..
                } => ids.push(("event", id)),
                other => panic!("unexpected event {other:?}"),
            },
            other => panic!("unexpected message {other:?}"),
        }
    }
    ids.sort();
    assert_eq!(
        ids,
        [
            ("event", RequestId::new("r1")),
            ("response", RequestId::new("r1"))
        ]
    );
}

#[test]
fn a_server_shutdown_disconnects_calls_and_ends_events() {
    let mut harness = start(&MockEngine::new());
    let (control, _) = harness.session();
    let mut events = control.events();
    ok(harness.run(control.call(create_scene("main"))));
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
    // Time for the slow connection to drain the engine's queue into its outbox, which is
    // what overflows; reading too early would only make the drop smaller, never hide it
    // from the count below.
    harness.run(async { tokio::time::sleep(Duration::from_millis(500)).await });

    // Every event is either delivered, in order, or counted by a notice.
    let (delivered, dropped, notices) = harness.run(async {
        let (mut delivered, mut dropped, mut notices) = (0u64, 0u64, 0u64);
        let mut last: Option<usize> = None;
        while delivered + dropped < SENT as u64 + 1 {
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
                    other => panic!("unexpected event {other:?}"),
                },
                other => panic!("unexpected message {other:?}"),
            }
        }
        (delivered, dropped, notices)
    });
    assert!(dropped > 0, "the slow client's outbox never overflowed");
    assert!(notices >= 1);
    assert_eq!(delivered + dropped, SENT as u64 + 1);

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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut request = format!("{method} {} HTTP/1.1\r\n", crate::MCP_PATH);
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
