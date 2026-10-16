//! Headless end to end on Linux (and every other OS): the daemon's engine on the
//! platform fakes, served by `jstream-server` with MCP on, as `serve.rs` wires it, and
//! driven the way A4's smoke script (`scripts/smoke.ps1`) drives the live daemon. MCP
//! over streamable HTTP builds a scene with a display source and creates two outputs,
//! an RTMP stream to a local sink and a recording; the WebSocket client sets the stream
//! key and starts the stream, MCP starts the recording, the WebSocket client watches
//! `stats.tick` and the state events, and each interface stops the output it started.
//! The test then checks what the sink received, the recorded MP4, and the audit log.
//!
//! The built binary has no backends off Windows, so this runs the same pieces in
//! process instead (`tests/daemon.rs` covers the binary itself). The fake clock is
//! pumped ahead of real time and the compositor runs on the CPU, so no device, GPU or
//! network is needed (R-TEST-004): the sink is a minimal RTMP server on loopback.

use futures_util::StreamExt;
use jstream_control::protocol::{
    Action, Call, CallResult, Event, Hello, OutputId, OutputKind, OutputState, PROTOCOL_VERSION,
    Query, QueryResult, RtmpTarget, SecretString,
};
use jstream_control::{Control, EventStream, WsConfig, WsControl};
use jstream_core::{AuditFile, Engine, LivePublish, OpenPublish, PublishError, connect_rtmp};
use jstream_media::{ChannelLayout, FrameSize, Timebase};
use jstream_platform::fake::{
    FakeAudioIo, FakeCapture, FakeClock, FakeCodecs, FakeGpuInterop, FakeMediaDecoders,
    FakeVideoDevices, MemoryKeyStore,
};
use jstream_platform::{Backends, StreamKeyStore};
use jstream_server::{Connector, Server, ServerConfig, Token};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const TOKEN: &str = "daemon-e2e-token-0123456789";
/// The sink's stream key: a placeholder, not a secret, like the smoke script's.
const SINK_KEY: &str = "e2e-sink-key";
/// The RTMP output and the recording output.
const LIVE: &str = "live";
const REC: &str = "rec";

/// A fresh, empty directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("e2e-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scratch directory is created");
    dir
}

// ---------------------------------------------------------------------------------------
// The engine, as the daemon builds a live one, on the fakes

/// The fake devices, and the pump that moves them: a microsecond clock, a 64x36 display
/// and a stereo 48 kHz microphone.
struct Fakes {
    clock: FakeClock,
    audio: FakeAudioIo,
}

impl Fakes {
    fn new() -> Self {
        let clock = FakeClock::new(Timebase::MICROS);
        let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
        Self { clock, audio }
    }

    fn backends(&self) -> Backends {
        let size = FrameSize::new(64, 36).expect("size");
        Backends {
            clock: Arc::new(self.clock.clone()),
            video_devices: Arc::new(FakeVideoDevices::new(self.clock.clone())),
            capture: Arc::new(FakeCapture::new(self.clock.clone(), size)),
            audio: Arc::new(self.audio.clone()),
            codecs: Arc::new(FakeCodecs),
            gpu: Arc::new(FakeGpuInterop),
            vcam: None,
            media: Arc::new(FakeMediaDecoders::new()),
        }
    }

    /// Moves the media clock on by 50 ms every 10 ms, feeding the microphone as it goes,
    /// until the returned flag is set. Five times real time, so the recording spans a
    /// few keyframes in a few seconds.
    fn pump(&self) -> (Arc<AtomicBool>, thread::JoinHandle<()>) {
        let stop = Arc::new(AtomicBool::new(false));
        let (clock, audio) = (self.clock.clone(), self.audio.clone());
        let flag = Arc::clone(&stop);
        let pump = thread::spawn(move || {
            while !flag.load(Ordering::Acquire) {
                audio.deliver(2400, 0.2);
                clock.advance(50_000);
                thread::sleep(Duration::from_millis(10));
            }
        });
        (stop, pump)
    }
}

/// The engine the daemon serves on Windows (`serve::live`), on `backends`: stream keys
/// in `keys`, and on each `output.start` an opener that reads the output's URL from the
/// collection and its key from the store and connects with `connect_rtmp`. The opener
/// holds the engine weakly, as the daemon's does. The render thread composites on the
/// CPU, so no adapter is needed.
fn live_engine(backends: Backends, keys: Arc<dyn StreamKeyStore>, audit: AuditFile) -> Arc<Engine> {
    let handle: Arc<OnceLock<Weak<Engine>>> = Arc::new(OnceLock::new());
    let open: OpenPublish = {
        let handle = Arc::clone(&handle);
        let keys = Arc::clone(&keys);
        Arc::new(move |output_id: &OutputId| {
            let engine = handle
                .get()
                .and_then(Weak::upgrade)
                .ok_or_else(|| PublishError::new("the engine is shutting down"))?;
            let Some(OutputKind::Rtmp {
                target: RtmpTarget::Generic { url },
            }) = engine.output_kind(output_id)
            else {
                return Err(PublishError::new("not a generic RTMP output"));
            };
            let key = keys
                .get(output_id.as_str())
                .map_err(|error| PublishError::new(error.to_string()))?
                .ok_or_else(|| PublishError::new("no stream key"))?;
            let publish: Box<dyn LivePublish> = Box::new(connect_rtmp(&url, key.expose())?);
            Ok(publish)
        })
    };
    let engine = Arc::new(
        Engine::new()
            .with_audit(Arc::new(audit))
            .with_key_store(keys)
            .with_backends(backends)
            .with_publish_opener(open)
            .with_cpu_compositor(),
    );
    let _ = handle.set(Arc::downgrade(&engine));
    engine
}

/// Binds the server on a free loopback port with MCP on, as `serve::bind` does.
async fn bind(engine: Arc<Engine>) -> Server {
    let connector: Connector = Arc::new(move || Box::new(engine.connect()));
    let mut config = ServerConfig::new(Token::new(TOKEN).expect("a valid token"));
    config.bind = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    config.mcp = true;
    Server::bind(config, connector).await.expect("binds")
}

// ---------------------------------------------------------------------------------------
// MCP over streamable HTTP, raw, as an agent sends it

/// One MCP session on the daemon's `/mcp`.
struct Mcp {
    authority: String,
    session: Option<String>,
    next_id: u64,
}

impl Mcp {
    /// Opens a session with `initialize`.
    fn initialize(authority: &str) -> Self {
        let mut mcp = Self {
            authority: authority.to_owned(),
            session: None,
            next_id: 0,
        };
        let body = mcp.request(
            "initialize",
            r#"{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"daemon-e2e","version":"1"}}"#,
        );
        assert!(
            body.contains("\"protocolVersion\":\"2025-11-25\""),
            "{body}"
        );
        assert!(mcp.session.is_some(), "no Mcp-Session-Id: {body}");
        mcp
    }

    /// Calls `tool` with `arguments` and returns the answer's body, which must not be a
    /// tool error.
    fn call(&mut self, tool: &str, arguments: &str) -> String {
        let params = format!(r#"{{"name":"{tool}","arguments":{arguments}}}"#);
        let body = self.request("tools/call", &params);
        assert!(body.contains("\"isError\":false"), "{tool}: {body}");
        body
    }

    /// POSTs one JSON-RPC request and returns the body of a 200 answer. The I/O blocks,
    /// so it runs in `block_in_place`: the server under test shares this runtime, and
    /// its tasks move off this worker meanwhile.
    fn request(&mut self, method: &str, params: &str) -> String {
        tokio::task::block_in_place(|| self.request_blocking(method, params))
    }

    fn request_blocking(&mut self, method: &str, params: &str) -> String {
        self.next_id += 1;
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":{},"method":"{method}","params":{params}}}"#,
            self.next_id
        );
        let mut stream = TcpStream::connect(&self.authority).expect("connects");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("a read timeout");
        let session = self
            .session
            .as_ref()
            .map(|id| format!("Mcp-Session-Id: {id}\r\n"))
            .unwrap_or_default();
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {TOKEN}\r\n\
             Content-Type: application/json\r\n{session}Connection: close\r\n\
             Content-Length: {}\r\n\r\n{body}",
            self.authority,
            body.len()
        );
        stream.write_all(request.as_bytes()).expect("sends");
        let mut answer = String::new();
        stream
            .read_to_string(&mut answer)
            .expect("reads the answer");
        let (head, body) = answer.split_once("\r\n\r\n").unwrap_or((&answer, ""));
        assert!(
            head.starts_with("HTTP/1.1 200"),
            "{method}: {head}\n\n{body}"
        );
        if let Some(id) = head.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("mcp-session-id")
                .then(|| value.trim().to_owned())
        }) {
            self.session = Some(id);
        }
        body.to_owned()
    }
}

// ---------------------------------------------------------------------------------------
// The local RTMP sink

/// What the sink received from its one publisher.
#[derive(Debug, Default)]
struct Received {
    /// The `app` the client connected to.
    app: Option<String>,
    /// The stream name the client published as: its stream key.
    published: Option<String>,
    /// Audio (8), video (9) and data (18) messages, by count.
    audio: usize,
    video: usize,
    data: usize,
    /// The first video message's first byte: frame type and codec ID.
    first_video: Option<u8>,
}

/// An AMF0 value, as far as the sink needs to read one.
#[derive(Debug)]
enum Amf {
    Number(f64),
    String(String),
    Object(Vec<(String, Amf)>),
    Other,
}

/// Reads a sequence of AMF0 values; stops at the first type it doesn't know.
fn amf_values(mut bytes: &[u8]) -> Vec<Amf> {
    fn string(bytes: &mut &[u8]) -> Option<String> {
        let len = usize::from(u16::from_be_bytes([*bytes.first()?, *bytes.get(1)?]));
        let text = String::from_utf8_lossy(bytes.get(2..2 + len)?).into_owned();
        *bytes = &bytes[2 + len..];
        Some(text)
    }
    fn value(bytes: &mut &[u8]) -> Option<Amf> {
        let (&marker, rest) = bytes.split_first()?;
        *bytes = rest;
        Some(match marker {
            0x00 => {
                let number = f64::from_be_bytes(bytes.get(..8)?.try_into().ok()?);
                *bytes = &bytes[8..];
                Amf::Number(number)
            }
            0x01 => {
                *bytes = bytes.get(1..)?;
                Amf::Other
            }
            0x02 => Amf::String(string(bytes)?),
            0x03 | 0x08 => {
                if marker == 0x08 {
                    *bytes = bytes.get(4..)?;
                }
                let mut fields = Vec::new();
                loop {
                    if bytes.starts_with(&[0, 0, 9]) {
                        *bytes = &bytes[3..];
                        break Amf::Object(fields);
                    }
                    let key = string(bytes)?;
                    fields.push((key, value(bytes)?));
                }
            }
            0x05 | 0x06 => Amf::Other,
            _ => return None,
        })
    }
    let mut values = Vec::new();
    while let Some(next) = value(&mut bytes) {
        values.push(next);
    }
    values
}

fn amf_string(out: &mut Vec<u8>, text: &str) {
    let len = u16::try_from(text.len()).expect("short");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(text.as_bytes());
}

/// An AMF0 command reply: `name`, transaction `txn`, then a null command object (or
/// the connect properties) and a status object with `code`, or a number.
fn reply(name: &str, txn: f64, info: Option<&str>, number: Option<f64>) -> Vec<u8> {
    let mut body = vec![0x02];
    amf_string(&mut body, name);
    body.push(0x00);
    body.extend_from_slice(&txn.to_be_bytes());
    body.push(0x05);
    if let Some(code) = info {
        body.push(0x03);
        for (key, text) in [("level", "status"), ("code", code), ("description", "e2e")] {
            amf_string(&mut body, key);
            body.push(0x02);
            amf_string(&mut body, text);
        }
        body.extend_from_slice(&[0, 0, 9]);
    }
    if let Some(number) = number {
        body.push(0x00);
        body.extend_from_slice(&number.to_be_bytes());
    }
    body
}

/// Writes `body` as one AMF0 command message on chunk stream 3, message stream
/// `stream`, at the default 128-byte chunk size.
fn send_command(io: &mut TcpStream, stream: u32, body: &[u8]) {
    let len = u32::try_from(body.len()).expect("small");
    let mut out = vec![0x03, 0, 0, 0];
    out.extend_from_slice(&len.to_be_bytes()[1..]);
    out.push(20);
    out.extend_from_slice(&stream.to_le_bytes());
    for (i, chunk) in body.chunks(128).enumerate() {
        if i > 0 {
            out.push(0xC3);
        }
        out.extend_from_slice(chunk);
    }
    io.write_all(&out).expect("a command reply");
}

/// Reads `n` bytes, or `None` at the end of the stream.
fn read_n(io: &mut impl Read, n: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0; n];
    io.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn u24(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]])
}

/// One chunk stream's last header, and the message it is assembling.
#[derive(Default, Clone)]
struct ChunkStream {
    len: usize,
    kind: u8,
    extended: bool,
    partial: Vec<u8>,
}

/// A minimal RTMP server on a loopback port: it answers the handshake, `connect`,
/// `createStream` and `publish`, then reads the publisher's chunk stream until it hangs
/// up, and returns what it received. The thread is bounded by a 30 s read timeout.
fn rtmp_sink() -> (u16, mpsc::Receiver<Received>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("binds the sink");
    let port = listener.local_addr().expect("an address").port();
    let (done, received) = mpsc::channel();
    thread::spawn(move || {
        let Ok((mut io, _)) = listener.accept() else {
            return;
        };
        let _ = io.set_read_timeout(Some(Duration::from_secs(30)));
        let _ = done.send(serve_publisher(&mut io));
    });
    (port, received)
}

fn serve_publisher(io: &mut TcpStream) -> Received {
    let mut got = Received::default();
    let Some(c0c1) = read_n(io, 1 + 1536) else {
        return got;
    };
    let mut s0s1s2 = vec![3];
    s0s1s2.extend((0..1536).map(|i| (i % 251) as u8));
    s0s1s2.extend_from_slice(&c0c1[1..]);
    io.write_all(&s0s1s2).expect("S0S1S2");
    if read_n(io, 1536).is_none() {
        return got;
    }
    let mut chunk_size = 128;
    let mut streams: std::collections::HashMap<u32, ChunkStream> = Default::default();
    loop {
        let Some(basic) = read_n(io, 1) else {
            return got;
        };
        let fmt = basic[0] >> 6;
        let csid = match basic[0] & 0x3F {
            0 => 64 + u32::from(read_n(io, 1).map_or(0, |b| b[0])),
            1 => {
                let b = read_n(io, 2).unwrap_or_default();
                64 + u32::from(b.first().copied().unwrap_or(0))
                    + 256 * u32::from(b.get(1).copied().unwrap_or(0))
            }
            id => u32::from(id),
        };
        let stream = streams.entry(csid).or_default();
        let header_len = [11, 7, 3, 0][usize::from(fmt)];
        let Some(header) = read_n(io, header_len) else {
            return got;
        };
        if fmt <= 2 {
            stream.extended = u24(&header[0..3]) == 0xFF_FFFF;
        }
        if fmt <= 1 {
            stream.len = u24(&header[3..6]) as usize;
            stream.kind = header[6];
        }
        if stream.extended && read_n(io, 4).is_none() {
            return got;
        }
        let want = chunk_size.min(stream.len - stream.partial.len());
        let Some(payload) = read_n(io, want) else {
            return got;
        };
        stream.partial.extend_from_slice(&payload);
        if stream.partial.len() < stream.len {
            continue;
        }
        let message = std::mem::take(&mut stream.partial);
        match stream.kind {
            1 => {
                chunk_size = (u32::from_be_bytes(message[..4].try_into().expect("4 bytes"))
                    & 0x7FFF_FFFF) as usize;
            }
            8 => got.audio += 1,
            9 => {
                got.video += 1;
                got.first_video = got.first_video.or(message.first().copied());
            }
            18 => got.data += 1,
            20 => {
                let values = amf_values(&message);
                let txn = match values.get(1) {
                    Some(Amf::Number(txn)) => *txn,
                    _ => 0.0,
                };
                match values.first() {
                    Some(Amf::String(name)) if name == "connect" => {
                        if let Some(Amf::Object(fields)) = values.get(2) {
                            got.app = fields.iter().find_map(|(key, value)| match value {
                                Amf::String(app) if key == "app" => Some(app.clone()),
                                _ => None,
                            });
                        }
                        let body =
                            reply("_result", txn, Some("NetConnection.Connect.Success"), None);
                        send_command(io, 0, &body);
                    }
                    Some(Amf::String(name)) if name == "createStream" => {
                        send_command(io, 0, &reply("_result", txn, None, Some(1.0)));
                    }
                    Some(Amf::String(name)) if name == "publish" => {
                        if let Some(Amf::String(key)) = values.get(3) {
                            got.published = Some(key.clone());
                        }
                        let body = reply("onStatus", 0.0, Some("NetStream.Publish.Start"), None);
                        send_command(io, 1, &body);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------
// The run

/// Fails the test after 20 s instead of hanging.
async fn timely<T>(future: impl Future<Output = T>) -> T {
    match tokio::time::timeout(Duration::from_secs(20), future).await {
        Ok(value) => value,
        Err(_) => panic!("timed out"),
    }
}

/// What the event stream showed: each output's state changes, and the `stats.tick`s.
#[derive(Default)]
struct Seen {
    live: Vec<OutputState>,
    rec: Vec<OutputState>,
    ticks: Vec<u64>,
}

impl Seen {
    /// Reads events until `done` holds, recording the ones the test checks.
    async fn until(&mut self, events: &mut EventStream, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done(self) {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = match tokio::time::timeout(left, events.next()).await {
                Ok(Some(event)) => event,
                Ok(None) => panic!("the event stream ended"),
                Err(_) => panic!(
                    "timed out: live {:?}, rec {:?}, {} ticks",
                    self.live,
                    self.rec,
                    self.ticks.len()
                ),
            };
            match event {
                Event::OutputStateChanged { output_id, state } if output_id.as_str() == LIVE => {
                    self.live.push(state);
                }
                Event::OutputStateChanged { output_id, state } if output_id.as_str() == REC => {
                    self.rec.push(state);
                }
                Event::StatsTick { seq, .. } => self.ticks.push(seq),
                _ => {}
            }
        }
    }
}

fn action(action: Action) -> Call {
    Call::Action { action }
}

fn start(id: &str) -> Call {
    action(Action::OutputStart {
        output_id: OutputId::new(id),
    })
}

fn stop(id: &str) -> Call {
    action(Action::OutputStop {
        output_id: OutputId::new(id),
    })
}

/// The top-level MP4 boxes of `bytes`, by type.
fn boxes(bytes: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    let mut at = 0;
    while at + 8 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[at..at + 4].try_into().expect("4 bytes")) as usize;
        found.push(String::from_utf8_lossy(&bytes[at + 4..at + 8]).into_owned());
        if size < 8 {
            break;
        }
        at += size;
    }
    found
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streams_and_records_on_the_fakes_driven_over_websocket_and_mcp() {
    let dir = scratch("stream-and-record");
    let recording = dir.join("show.mp4");
    let fakes = Fakes::new();
    let keys = Arc::new(MemoryKeyStore::new());
    let audit = AuditFile::open(&dir).expect("the audit log opens");
    let engine = live_engine(fakes.backends(), Arc::clone(&keys) as _, audit);
    let server = bind(Arc::clone(&engine)).await;
    let ws_url = server.ws_url();
    let authority = server.local_addr().to_string();
    let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(server.serve_with_shutdown(async {
        let _ = stopped.await;
    }));
    let (sink_port, sink) = rtmp_sink();

    // The WebSocket client, subscribed before anything starts.
    let control = timely(WsControl::connect(WsConfig::new(ws_url, TOKEN)))
        .await
        .expect("connects with the token");
    timely(control.hello(Hello {
        protocol: PROTOCOL_VERSION,
        client_name: "daemon-e2e".into(),
    }))
    .await
    .expect("welcome");
    let mut events = control.events();

    // MCP builds the show: the display's size as the canvas (the outputs encode at the
    // canvas), a scene with a display source, then the two outputs.
    let mut mcp = Mcp::initialize(&authority);
    mcp.call(
        "batch",
        r#"{"edits":[
            {"type":"canvas.set_size","width":64,"height":36},
            {"type":"scene.create","scene_id":"main","name":"Main"},
            {"type":"source.create","source_id":"screen","name":"Screen","kind":{"type":"builtin","kind":"display"}},
            {"type":"item.add","scene_id":"main","item_id":"screen_1","source_id":"screen"}
        ]}"#,
    );
    let sink_url = format!("rtmp://127.0.0.1:{sink_port}/live");
    mcp.call(
        "output",
        &format!(
            r#"{{"op":{{"type":"output.create","output_id":"{LIVE}","kind":{{"type":"rtmp","target":{{"type":"generic","url":"{sink_url}"}}}}}}}}"#
        ),
    );
    let path = recording
        .to_str()
        .expect("a UTF-8 path")
        .replace('\\', "\\\\");
    mcp.call(
        "output",
        &format!(
            r#"{{"op":{{"type":"output.create","output_id":"{REC}","kind":{{"type":"record","path":"{path}","container":"mp4"}}}}}}"#
        ),
    );
    let listed = mcp.call("item", r#"{"op":{"type":"item.list","scene_id":"main"}}"#);
    assert!(listed.contains("screen_1"), "{listed}");

    // The stream key goes over the WebSocket, never through the agent; status says
    // only that one is set.
    timely(control.call(action(Action::OutputSetKey {
        output_id: OutputId::new(LIVE),
        key: SecretString::new(SINK_KEY),
    })))
    .await
    .expect("output.set_key");
    let status = mcp.call(
        "output",
        &format!(r#"{{"op":{{"type":"output.status","output_id":"{LIVE}"}}}}"#),
    );
    assert!(status.contains("\"key_set\":true"), "{status}");
    assert!(!status.contains(SINK_KEY), "the key came back: {status}");

    // Go live over the WebSocket, then start the recording over MCP, with the fake
    // devices running.
    let (pumping, pump) = fakes.pump();
    let mut seen = Seen::default();
    timely(control.call(start(LIVE)))
        .await
        .expect("output.start");
    seen.until(&mut events, |seen| {
        seen.live.last() == Some(&OutputState::Active)
    })
    .await;
    mcp.call(
        "output",
        &format!(r#"{{"op":{{"type":"output.start","output_id":"{REC}"}}}}"#),
    );
    seen.until(&mut events, |seen| {
        seen.rec.last() == Some(&OutputState::Active)
    })
    .await;
    assert!(
        dir.join("show.mp4.part").exists(),
        "recording to the .part name"
    );

    // Stay live until `stats.tick` has reported a few times and media has reached
    // both outputs: enough for the recording to span several keyframes.
    let flowing = |id: &str| {
        engine
            .output_stats_of(&OutputId::new(id))
            .is_some_and(|stats| {
                stats.video_subscriber.delivered >= 90 && stats.audio_subscriber.delivered >= 20
            })
    };
    seen.until(&mut events, |seen| {
        seen.ticks.len() >= 3 && flowing(LIVE) && flowing(REC)
    })
    .await;
    assert!(
        seen.ticks.windows(2).all(|pair| pair[1] > pair[0]),
        "stats.tick numbers go up: {:?}",
        seen.ticks
    );
    match timely(control.call(Call::Query {
        query: Query::OutputStatus {
            output_id: OutputId::new(REC),
        },
    }))
    .await
    .expect("output.status")
    .result
    {
        CallResult::Query {
            result: QueryResult::OutputStatus { state, .. },
        } => assert_eq!(state, OutputState::Active),
        other => panic!("expected output.status, got {other:?}"),
    }

    // Stop both, one over each interface.
    mcp.call(
        "output",
        &format!(r#"{{"op":{{"type":"output.stop","output_id":"{REC}"}}}}"#),
    );
    timely(control.call(stop(LIVE))).await.expect("output.stop");
    seen.until(&mut events, |seen| {
        seen.live.last() == Some(&OutputState::Stopped)
            && seen.rec.last() == Some(&OutputState::Stopped)
    })
    .await;
    pumping.store(true, Ordering::Release);
    pump.join().expect("the pump");
    let steps = [
        OutputState::Starting,
        OutputState::Active,
        OutputState::Stopping,
        OutputState::Stopped,
    ];
    assert_eq!(seen.live, steps, "the stream never failed");
    assert_eq!(seen.rec, steps, "the recording never failed");

    // The sink got the stream: published with the key, video and audio.
    let received = sink
        .recv_timeout(Duration::from_secs(30))
        .expect("the publisher hung up");
    assert_eq!(received.app.as_deref(), Some("live"), "{received:?}");
    assert_eq!(
        received.published.as_deref(),
        Some(SINK_KEY),
        "{received:?}"
    );
    assert!(received.video >= 30 && received.audio >= 10, "{received:?}");
    assert_eq!(
        received.first_video.map(|byte| byte >> 4),
        Some(1),
        "the stream opens on a keyframe: {received:?}"
    );

    // The recording was renamed into place: `ftyp`, `moov`, then whole fragments.
    assert!(!dir.join("show.mp4.part").exists(), "the .part was renamed");
    let bytes = std::fs::read(&recording).expect("the finished recording");
    let found = boxes(&bytes);
    assert_eq!(found.first().map(String::as_str), Some("ftyp"), "{found:?}");
    assert_eq!(found.get(1).map(String::as_str), Some("moov"), "{found:?}");
    let fragments = found.iter().filter(|name| *name == "moof").count();
    assert!(fragments >= 2, "several keyframes: {found:?}");
    assert_eq!(
        found.iter().filter(|name| *name == "mdat").count(),
        fragments,
        "{found:?}"
    );

    // A shutdown ends the event stream; the audit log has both clients' calls and no
    // key.
    let _ = shutdown.send(());
    timely(serving)
        .await
        .expect("the server task")
        .expect("served until shut down");
    assert_eq!(timely(events.next()).await, None, "the event stream ends");
    drop(engine);
    let audit = std::fs::read_to_string(dir.join("audit.jsonl")).expect("the audit log");
    assert!(
        audit.contains(
            "\"edits\":[\"canvas.set_size\",\"scene.create\",\"source.create\",\"item.add\"]"
        ),
        "{audit}"
    );
    assert!(audit.contains("output.start"), "{audit}");
    assert!(audit.contains("\"client_name\":\"daemon-e2e\""), "{audit}");
    assert!(!audit.contains(SINK_KEY), "the key was audited: {audit}");
    let _ = std::fs::remove_dir_all(&dir);
}
