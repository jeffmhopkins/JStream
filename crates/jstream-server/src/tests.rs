//! End-to-end tests: a [`MockEngine`] behind the server on an ephemeral `127.0.0.1` port,
//! and `jstream-control`'s WebSocket client (or a raw WebSocket) in front. Hermetic: only
//! loopback sockets (R-TEST-004).

use crate::{Connector, Server, ServerConfig, Token};
use futures_util::{SinkExt, StreamExt};
use jstream_control::protocol::{
    Action, Batch, Call, CallResult, ClientId, Edit, Event, Hello, OutputId, OutputState,
    PROTOCOL_VERSION, ProtocolError, ProtocolVersion, Query, QueryResult, RequestId, Rev, SceneId,
    ServerMessage,
};
use jstream_control::{Control, ControlError, EventStream, MockEngine, WsConfig, WsControl};
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
    stop: Option<oneshot::Sender<()>>,
}

fn start(engine: &MockEngine) -> Harness {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => panic!("runtime: {error}"),
    };
    let sessions = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&sessions);
    let engine = engine.clone();
    let connector: Connector = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        Box::new(engine.connect())
    });
    let config = match Token::new(TOKEN) {
        Ok(token) => ServerConfig::new(token),
        Err(error) => panic!("token: {error}"),
    };
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
        stop: Some(stop),
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
        None,
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
    let hello = r#"{"type":"hello","protocol":{"major":0,"minor":3},"client_name":"raw"}"#;
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
