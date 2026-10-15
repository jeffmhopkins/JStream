//! The engine behind the server: one `Engine`, one `InProcessControl` session per
//! WebSocket connection and per MCP session, and the signals that stop it. On Windows
//! the engine is live: it runs the media pipeline on the OS backends and publishes
//! over RTMP(S).

use crate::args::Options;
use anyhow::Context;
use jstream_core::{AuditSink, Engine};
use jstream_server::{Connector, Server, ServerConfig, Token};
use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(any(windows, test))]
use {
    jstream_core::{LivePublish, OpenPublish, PublishError},
    jstream_platform::{Backends, Secret, StreamKeyStore},
    jstream_protocol::{OutputId, OutputKind, RtmpTarget},
    std::sync::{OnceLock, Weak},
};

/// How the publish opener connects to an RTMP(S) server URL with a stream key:
/// `connect_rtmp` in the daemon, a recording target in tests.
#[cfg(any(windows, test))]
pub type Connect =
    Arc<dyn Fn(&str, &Secret) -> Result<Box<dyn LivePublish>, PublishError> + Send + Sync>;

/// The engine the daemon serves: the collection in `--collection`'s file (else an empty
/// one that is not saved), the placeholder outputs, and `audit` recording every batch,
/// undo, redo and action.
///
/// On Windows it is live (see `live`, which is Windows and test only): the OS backends (WGC capture, WASAPI audio, Media
/// Foundation codecs and webcams, the QPC clock), stream keys in Credential Manager, and a publish
/// opener that connects RTMP(S) on each `output.start`. Elsewhere there are no
/// backends yet, so output actions only step the placeholder state machine.
///
/// # Errors
/// When the collection file can't be read, loaded or created, and on Windows, when a
/// backend can't be created (the QPC clock).
pub fn engine(options: &Options, audit: Arc<dyn AuditSink>) -> anyhow::Result<Arc<Engine>> {
    let mut engine = Engine::new().with_audit(audit);
    if let Some(path) = &options.collection {
        // The error names the file and what is wrong with it.
        engine = engine
            .with_collection_file(path)
            .context("could not open the collection")?;
    }
    // Declared outputs survive a load, so this order is only for reading.
    let engine = options
        .outputs
        .iter()
        .cloned()
        .fold(engine, Engine::with_output);
    #[cfg(windows)]
    let engine = live(
        engine,
        windows_backends()?,
        Arc::new(jstream_platform_windows::WindowsCredentials),
        rtmp(),
    );
    #[cfg(not(windows))]
    let engine = Arc::new(engine);
    Ok(engine)
}

/// The Windows backends. Creating them opens no device: each opens what it needs when
/// a pipeline starts. The virtual camera (`WindowsVcam`) checks on `output.start` that
/// this Windows has one and that it is registered (`--register-vcam`).
#[cfg(windows)]
fn windows_backends() -> anyhow::Result<Backends> {
    use jstream_platform_windows::{
        MfCodecs, MfVideoDevices, QpcClock, WasapiAudio, WgcCapture, WindowsGpu, WindowsVcam,
    };
    Ok(Backends {
        clock: Arc::new(QpcClock::new().context("could not open the QPC media clock")?),
        capture: Arc::new(WgcCapture::new()),
        video_devices: Arc::new(MfVideoDevices::new()),
        audio: Arc::new(WasapiAudio::new()),
        codecs: Arc::new(MfCodecs::new()),
        gpu: Arc::new(WindowsGpu::new()),
        vcam: Some(Box::new(WindowsVcam::new())),
    })
}

/// Connects with `connect_rtmp`, whose errors never carry the key (R-SEC-001).
#[cfg(windows)]
fn rtmp() -> Connect {
    Arc::new(|url, key| {
        let publish: Box<dyn LivePublish> =
            Box::new(jstream_core::connect_rtmp(url, key.expose())?);
        Ok(publish)
    })
}

/// Makes `engine` live: it runs on `backends`, keeps stream keys in `keys`, and opens
/// its publish target with [`opener`] on every `output.start`.
///
/// The engine stores the opener, so the opener must not hold the engine strongly: that
/// cycle would keep the engine, and its running pipeline, alive after the server and
/// every session let go of it. So the engine is returned in an `Arc` and the opener
/// gets only a `Weak` to it, set once the engine is built.
#[cfg(any(windows, test))]
pub fn live(
    engine: Engine,
    backends: Backends,
    keys: Arc<dyn StreamKeyStore>,
    connect: Connect,
) -> Arc<Engine> {
    let handle = Arc::new(OnceLock::new());
    let engine = Arc::new(
        engine
            .with_key_store(Arc::clone(&keys))
            .with_backends(backends)
            .with_publish_opener(opener(Arc::clone(&handle), keys, connect)),
    );
    // The cell is new, so this is its only value.
    let _ = handle.set(Arc::downgrade(&engine));
    engine
}

/// The publish opener: on each `output.start` it reads the output's server URL from
/// the collection (`Engine::output_kind`) and its stream key from `keys`, then calls
/// `connect`. A start fails with a reason that says what to fix when the output is not
/// in the collection (a `--output` placeholder has no target), has no stream key, or
/// the store can't be read. No reason carries the key (R-SEC-001).
#[cfg(any(windows, test))]
fn opener(
    engine: Arc<OnceLock<Weak<Engine>>>,
    keys: Arc<dyn StreamKeyStore>,
    connect: Connect,
) -> OpenPublish {
    Arc::new(move |output_id: &OutputId| {
        let kind = engine
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| PublishError::new("the engine is shutting down"))?
            .output_kind(output_id);
        let url = match kind {
            Some(OutputKind::Rtmp {
                target: RtmpTarget::Generic { url },
            }) => url,
            Some(_) => {
                return Err(PublishError::new(format!(
                    "output `{output_id}` is of a kind this daemon can't publish yet"
                )));
            }
            None => {
                return Err(PublishError::new(format!(
                    "output `{output_id}` has no target: create it with `output.create`"
                )));
            }
        };
        let key = keys
            .get(output_id.as_str())
            .map_err(|error| PublishError::new(format!("could not read the stream key: {error}")))?
            .ok_or_else(|| {
                PublishError::new(format!(
                    "output `{output_id}` has no stream key: set one with `output.set_key`"
                ))
            })?;
        connect(&url, &key)
    })
}

/// Binds the server on `127.0.0.1:<port>`, serving `engine` behind `token` and the
/// devices paired in `device_file` (ADR-0044; in memory without one), with MCP's streamable-HTTP transport on.
/// Each connection and each MCP session gets its own session (its own `client_id`); all
/// of them share the engine.
pub async fn bind(
    engine: Arc<Engine>,
    port: u16,
    token: Token,
    device_file: Option<PathBuf>,
) -> Result<Server, jstream_server::ServerError> {
    let connector: Connector = Arc::new(move || Box::new(engine.connect()));
    let mut config = ServerConfig::new(token);
    config.bind = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    config.mcp = true;
    config.device_file = device_file;
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
    use super::{Connect, bind, engine, live, opener};
    use crate::args::Options;
    use futures_util::StreamExt;
    use jstream_control::protocol::{Action, OutputKind, ProtocolError, RtmpTarget, SecretString};
    use jstream_control::protocol::{
        Batch, Call, CallResult, Edit, Event, Hello, OutputId, OutputState, PROTOCOL_VERSION,
        Query, QueryResult, Rev, SceneId,
    };
    use jstream_control::{Control, ControlError, WsConfig, WsControl};
    use jstream_core::{AuditMemory, Engine, LivePublish, RecordingPublish};
    use jstream_media::Timebase;
    use jstream_platform::fake::{FakeClock, MemoryKeyStore, backends};
    use jstream_platform::{Secret, StreamKeyStore};
    use jstream_server::Token;
    use std::sync::{Arc, Mutex, OnceLock};
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
        let server = bind(
            engine(&options, Arc::new(AuditMemory::new())).expect("an engine"),
            0,
            token,
            None,
        )
        .await
        .expect("binds");
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
            role: None,
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
        let server = bind(
            engine(&Options::default(), Arc::new(AuditMemory::new())).expect("an engine"),
            0,
            token.clone(),
            None,
        )
        .await
        .expect("binds");
        let port = server.local_addr().port();
        let error = bind(
            engine(&Options::default(), Arc::new(AuditMemory::new())).expect("an engine"),
            port,
            token,
            None,
        )
        .await
        .err()
        .expect("the port is taken");
        assert!(
            error.to_string().contains(&format!("127.0.0.1:{port}")),
            "{error}"
        );
    }

    const URL: &str = "rtmp://127.0.0.1:1935/live";
    const KEY: &str = "secret-stream-key-1234";

    /// The URL and key of each connect, in order.
    type Seen = Arc<Mutex<Vec<(String, String)>>>;

    /// A connect that records the URL and key it was given, and publishes into memory.
    fn recording() -> (Connect, Seen) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let connect: Connect = {
            let seen = Arc::clone(&seen);
            Arc::new(move |url, key: &Secret| {
                seen.lock()
                    .expect("seen")
                    .push((url.to_owned(), key.expose().to_owned()));
                let publish: Box<dyn LivePublish> = Box::new(RecordingPublish::new());
                Ok(publish)
            })
        };
        (connect, seen)
    }

    fn rtmp(url: &str) -> OutputKind {
        OutputKind::Rtmp {
            target: RtmpTarget::Generic { url: url.into() },
        }
    }

    /// A session on `engine`, past `hello`.
    async fn session(engine: &Engine) -> jstream_core::InProcessControl {
        let control = engine.connect();
        timely(control.hello(hello())).await.expect("welcome");
        control
    }

    async fn create(control: &jstream_core::InProcessControl, id: &str, url: &str) {
        timely(control.call(Call::Batch(Batch::single(Edit::OutputCreate {
            output_id: OutputId::new(id),
            kind: rtmp(url),
        }))))
        .await
        .expect("output.create");
    }

    fn start(id: &str) -> Call {
        Call::Action {
            action: Action::OutputStart {
                output_id: OutputId::new(id),
            },
        }
    }

    fn reason(error: &ControlError) -> String {
        match error.protocol() {
            Some(ProtocolError::ActionFailed { reason }) => reason.clone(),
            other => panic!("expected action_failed, got {other:?}"),
        }
    }

    /// The opener over `engine` alone, as `live` builds it.
    fn open_on(
        engine: &Arc<Engine>,
        keys: &MemoryKeyStore,
        connect: Connect,
    ) -> jstream_core::OpenPublish {
        let handle = Arc::new(OnceLock::new());
        let _ = handle.set(Arc::downgrade(engine));
        opener(handle, Arc::new(keys.clone()), connect)
    }

    /// The opener connects with the output's URL from the collection and its key from
    /// the store, and reads both again on every start.
    #[tokio::test]
    async fn the_opener_connects_with_the_collection_url_and_the_stored_key() {
        let engine = Arc::new(Engine::new());
        let control = session(&engine).await;
        create(&control, "out_yt", URL).await;
        let keys = MemoryKeyStore::new();
        keys.set("out_yt", &Secret::new(KEY)).expect("stored");
        let (connect, seen) = recording();
        let open = open_on(&engine, &keys, connect);
        assert!(open(&OutputId::new("out_yt")).is_ok());

        let other = "rtmps://a.rtmps.youtube.com:443/live2";
        timely(
            control.call(Call::Batch(Batch::single(Edit::OutputSetTarget {
                output_id: OutputId::new("out_yt"),
                target: RtmpTarget::Generic { url: other.into() },
            }))),
        )
        .await
        .expect("output.set_target");
        keys.set("out_yt", &Secret::new("another-key"))
            .expect("stored");
        assert!(open(&OutputId::new("out_yt")).is_ok());
        assert_eq!(
            *seen.lock().expect("seen"),
            [
                (URL.to_owned(), KEY.to_owned()),
                (other.to_owned(), "another-key".to_owned())
            ]
        );
    }

    /// Every way a start can't publish fails with a reason that says what to fix, and
    /// none of them connects or names the key (R-SEC-001).
    #[tokio::test]
    async fn the_opener_says_what_is_missing() {
        let engine = Arc::new(Engine::new().with_output(OutputId::new("placeholder")));
        let control = session(&engine).await;
        create(&control, "no_key", URL).await;
        create(&control, "out_yt", URL).await;
        let keys = MemoryKeyStore::new();
        keys.set("out_yt", &Secret::new(KEY)).expect("stored");
        let (connect, seen) = recording();
        let open = open_on(&engine, &keys, connect);
        let fails = |id: &str| match open(&OutputId::new(id)) {
            Ok(_) => panic!("`{id}` opened"),
            Err(error) => error.to_string(),
        };

        let placeholder = fails("placeholder");
        assert!(placeholder.contains("has no target"), "{placeholder}");
        assert!(fails("missing").contains("has no target"));
        let no_key = fails("no_key");
        assert!(no_key.contains("set one with `output.set_key`"), "{no_key}");
        keys.set_failing(true);
        let unreadable = fails("out_yt");
        assert!(
            unreadable.contains("could not read the stream key"),
            "{unreadable}"
        );
        assert!(!unreadable.contains(KEY), "{unreadable}");
        keys.set_failing(false);
        drop(control);
        drop(engine);
        let gone = fails("out_yt");
        assert!(gone.contains("shutting down"), "{gone}");
        assert!(seen.lock().expect("seen").is_empty(), "nothing connected");
    }

    /// The live engine runs `output.start` through the opener: a configured output with
    /// a key goes `active` on the fake backends, and one without a key fails with the
    /// opener's reason. Dropping the last handle ends the event stream, so the opener
    /// does not keep the engine alive.
    #[tokio::test]
    async fn the_live_engine_publishes_through_the_opener() {
        let clock = FakeClock::new(Timebase::MICROS);
        let keys = MemoryKeyStore::new();
        let (connect, seen) = recording();
        let engine = live(
            Engine::new(),
            backends(&clock).expect("fake backends"),
            Arc::new(keys.clone()),
            connect,
        );
        // Clones share the media host, so this sets the CPU compositor on `engine` too,
        // and the opener's weak handle stays the one `live` made.
        let _ = Engine::clone(&engine).with_cpu_compositor();
        let control = session(&engine).await;
        let mut events = control.events();
        create(&control, "out_yt", URL).await;

        let refused = timely(control.call(start("out_yt")))
            .await
            .expect_err("no key yet");
        assert!(reason(&refused).contains("has no stream key"), "{refused}");
        timely(control.call(Call::Action {
            action: Action::OutputSetKey {
                output_id: OutputId::new("out_yt"),
                key: SecretString::new(KEY),
            },
        }))
        .await
        .expect("output.set_key");
        timely(control.call(start("out_yt")))
            .await
            .expect("started");
        let status = timely(control.call(Call::Query {
            query: Query::OutputStatus {
                output_id: OutputId::new("out_yt"),
            },
        }))
        .await
        .expect("answered");
        match status.result {
            CallResult::Query {
                result: QueryResult::OutputStatus { state, .. },
            } => assert_eq!(state, OutputState::Active),
            other => panic!("expected output.status, got {other:?}"),
        }
        assert_eq!(
            *seen.lock().expect("seen"),
            [(URL.to_owned(), KEY.to_owned())]
        );
        timely(control.call(Call::Action {
            action: Action::OutputStop {
                output_id: OutputId::new("out_yt"),
            },
        }))
        .await
        .expect("stopped");

        drop(control);
        drop(engine);
        timely(async { while events.next().await.is_some() {} }).await;
    }

    /// `--collection` loads the file and saves every change to it, so a second engine on
    /// the same file starts where the first stopped; the audit sink sees the batch.
    #[tokio::test]
    async fn the_collection_file_persists_and_every_call_is_audited() {
        let dir =
            std::env::temp_dir().join(format!("jstream-daemon-collection-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let file = dir.join("collection.json");
        let options = Options {
            collection: Some(file.to_str().expect("a UTF-8 path").to_owned()),
            ..Options::default()
        };
        let audit = Arc::new(AuditMemory::new());
        let first = engine(&options, Arc::clone(&audit) as _).expect("an engine");
        assert!(file.exists(), "a missing file is created");
        let control = session(&first).await;
        timely(control.call(Call::Batch(Batch::single(Edit::SceneCreate {
            scene_id: SceneId::new("main"),
            name: "Main".to_owned(),
            role: None,
        }))))
        .await
        .expect("applied");
        let entries = audit.entries();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].call, "batch");
        assert_eq!(entries[0].edits, ["scene.create"]);
        assert_eq!(entries[0].rev_after, Rev(1));
        drop(control);
        drop(first);

        let second = engine(&options, Arc::new(AuditMemory::new())).expect("an engine");
        let control = session(&second).await;
        match timely(control.call(Call::Query {
            query: Query::SceneList,
        }))
        .await
        .expect("answered")
        .result
        {
            CallResult::Query {
                result: QueryResult::SceneList { rev, scenes },
            } => {
                assert_eq!(rev, Rev(1), "the rev is saved too");
                assert_eq!(scenes.len(), 1);
                assert_eq!(scenes[0].scene_id.as_str(), "main");
            }
            other => panic!("expected scene.list, got {other:?}"),
        }

        // A file that holds no collection is an error that names it, and is left as it is.
        std::fs::write(&file, "not json").expect("written");
        let Err(error) = engine(&options, Arc::new(AuditMemory::new())) else {
            panic!("an unloadable file was loaded");
        };
        let message = format!("{error:#}");
        assert!(
            message.starts_with("could not open the collection: cannot load the collection file"),
            "{message}"
        );
        assert_eq!(
            std::fs::read_to_string(&file).expect("still there"),
            "not json"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
