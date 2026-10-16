//! The engine behind the server: one `Engine`, one `InProcessControl` session per
//! WebSocket connection and per MCP session, and the signals that stop it. On Windows
//! the engine is live: it runs the media pipeline on the OS backends and publishes
//! over RTMP(S).

use crate::args::Options;
use anyhow::Context;
use jstream_core::{AuditSink, Engine};
#[cfg(windows)]
use jstream_daemon::live::{live, rtmp};
#[cfg(windows)]
use jstream_platform::Backends;
use jstream_server::{Connector, Server, ServerConfig, Token};
use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(any(all(windows, feature = "cef"), test))]
use {jstream_browser::BrowserBackend, jstream_platform::GpuContext};

/// The engine the daemon serves: the collection in `--collection`'s file (else an empty
/// one that is not saved), the placeholder outputs, and `audit` recording every batch,
/// undo, redo and action.
///
/// On Windows it is live (see `jstream_daemon::live`): the OS backends (WGC capture, WASAPI audio, Media
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
    let engine = {
        let backends = windows_backends()?;
        #[cfg(feature = "cef")]
        let (gpu, clock) = (Arc::clone(&backends.gpu), Arc::clone(&backends.clock));
        let engine = live(
            engine,
            backends,
            Arc::new(jstream_platform_windows::WindowsCredentials),
            rtmp(),
        );
        // After `with_backends`, so pages start on the media clock. Clones share the
        // engine's sources, so this gives the browser to `engine` too.
        #[cfg(feature = "cef")]
        let _ = cef_browser(Engine::clone(&engine), gpu.as_ref(), clock, options);
        engine
    };
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
        MfCodecs, MfMediaDecoders, MfVideoDevices, QpcClock, WasapiAudio, WgcCapture, WindowsGpu,
        WindowsVcam,
    };
    Ok(Backends {
        clock: Arc::new(QpcClock::new().context("could not open the QPC media clock")?),
        capture: Arc::new(WgcCapture::new()),
        video_devices: Arc::new(MfVideoDevices::new()),
        audio: Arc::new(WasapiAudio::new()),
        codecs: Arc::new(MfCodecs::new()),
        gpu: Arc::new(WindowsGpu::new()),
        media: Arc::new(MfMediaDecoders::new()),
        vcam: Some(Box::new(WindowsVcam::new())),
    })
}

/// The CEF browser backend for `browser` sources (feature `cef`, ADR-0046), on a
/// GPU device of the engine's media adapter, opened through the backends' `gpu`, with
/// pages' audio stamped on their `clock`.
///
/// CEF runs only in the sandboxed browser helper (ADR-0048), which starts when the first
/// page opens: the helper (`jstream-browser-helper.exe` and its DLL) and CEF's runtime
/// files go next to the daemon's exe, CEF's cache in the data directory's `cef` folder
/// and its log in the log directory's `cef.log`. Neither a
/// missing data directory nor a device that won't open stops the daemon: without a
/// device, pages still play their audio and only their frames are dropped, and when the
/// daemon's own path is unknown, browser sources show nothing (both are logged).
#[cfg(all(windows, feature = "cef"))]
fn cef_browser(
    engine: Engine,
    gpu: &dyn jstream_platform::GpuInterop,
    clock: Arc<dyn jstream_platform::MediaClock>,
    options: &Options,
) -> Engine {
    use jstream_browser::{CefBackend, CefConfig};
    let logs = options
        .log_dir
        .as_ref()
        .map(std::path::PathBuf::from)
        .or_else(crate::logs::default_dir);
    let Some(state) = jstream_platform::paths::data_dir().or_else(|| logs.clone()) else {
        tracing::warn!("no data directory for the browser's cache, so browser sources are off");
        return engine;
    };
    let mut config = match CefConfig::beside_exe(&state) {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!(%error, "browser sources are off");
            return engine;
        }
    };
    config.log_file = logs.map(|dir| dir.join("cef.log"));
    let gpu = gpu
        .open_device()
        .inspect_err(|error| {
            tracing::warn!(%error, "no GPU device for browser pages: their frames are dropped");
        })
        .ok();
    browse(
        engine,
        Arc::new(CefBackend::new(config, clock)),
        gpu.as_ref(),
    )
}

/// Gives `engine` `browser` for its `browser` sources, delivering pages' frames on `gpu`
/// (`BrowserBackend::use_device`): the device must come first, so the first page's first
/// paint already has it.
#[cfg(any(all(windows, feature = "cef"), test))]
pub fn browse(
    engine: Engine,
    browser: Arc<dyn BrowserBackend>,
    gpu: Option<&GpuContext>,
) -> Engine {
    browser.use_device(gpu);
    engine.with_browser(browser)
}

/// Binds the server on `127.0.0.1:<port>`, serving `engine` behind `token` and the
/// devices paired in `device_file` (ADR-0044; in memory without one), with MCP's
/// streamable-HTTP transport on, and the web UI's files from `ui_dir` when given. Each
/// connection and each MCP session gets its own session (its own `client_id`); all of
/// them share the engine.
pub async fn bind(
    engine: Arc<Engine>,
    port: u16,
    token: Token,
    device_file: Option<PathBuf>,
    ui_dir: Option<&Path>,
) -> Result<Server, jstream_server::ServerError> {
    let connector: Connector = Arc::new(move || Box::new(engine.connect()));
    let mut config = ServerConfig::new(token);
    config.bind = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    config.mcp = true;
    config.device_file = device_file;
    config.ui_dir = ui_dir.map(Path::to_path_buf);
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
    use super::{bind, engine};
    use crate::args::Options;
    use futures_util::StreamExt;
    use jstream_control::protocol::{
        Batch, Call, CallResult, Edit, Event, Hello, OutputId, OutputState, PROTOCOL_VERSION,
        Query, QueryResult, Rev, SceneId,
    };
    use jstream_control::{Control, ControlError, WsConfig, WsControl};
    use jstream_core::{AuditMemory, Engine};
    use jstream_server::Token;
    use std::sync::Arc;
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

    /// A session on `engine`, past `hello`.
    async fn session(engine: &Engine) -> jstream_core::InProcessControl {
        let control = engine.connect();
        timely(control.hello(hello())).await.expect("welcome");
        control
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

    /// `browse` hands the backend the device before the engine opens any page, and the
    /// engine opens a page for a `browser` source through it.
    #[tokio::test]
    async fn browser_sources_open_pages_through_the_backend() {
        use jstream_browser::fake::FakeBrowser;
        use jstream_browser::{BrowserBackend, BrowserError, BrowserPage, PageSpec};
        use jstream_control::protocol::{Settings, SourceId, SourceKind};
        use jstream_platform::GpuContext;
        use std::sync::Mutex;

        /// The fake, recording the order of `use_device` and `open` calls.
        struct Recording {
            inner: Arc<FakeBrowser>,
            calls: Mutex<Vec<&'static str>>,
        }
        impl BrowserBackend for Recording {
            fn open(&self, spec: &PageSpec) -> Result<Box<dyn BrowserPage>, BrowserError> {
                self.calls.lock().expect("calls").push("open");
                self.inner.open(spec)
            }
            fn use_device(&self, gpu: Option<&GpuContext>) {
                self.calls.lock().expect("calls").push("use_device");
                self.inner.use_device(gpu);
            }
        }

        let browser = FakeBrowser::new();
        let recording = Arc::new(Recording {
            inner: browser.clone(),
            calls: Mutex::new(Vec::new()),
        });
        let page = |id: &str| {
            Call::Batch(Batch::single(Edit::SourceCreate {
                source_id: SourceId::new(id),
                name: id.to_owned(),
                kind: SourceKind::Builtin {
                    kind: "browser".into(),
                },
                settings: Settings(
                    [("url".to_owned(), "https://example.com/".into())]
                        .into_iter()
                        .collect(),
                ),
                duplicated_from: None,
            }))
        };

        // A source that exists before the backend arrives (as from `--collection`)
        // opens when it does, on the device the backend was already given.
        let engine = Engine::new();
        let control = session(&engine).await;
        timely(control.call(page("before")))
            .await
            .expect("source.create");
        let engine = super::browse(engine, recording.clone(), None);
        /// Waits until `browser` has opened `count` pages.
        async fn pages(browser: &FakeBrowser, count: usize) {
            timely(async {
                while browser.pages().len() < count {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await;
        }
        pages(&browser, 1).await;
        assert_eq!(
            *recording.calls.lock().expect("calls"),
            ["use_device", "open"],
            "the device first, then the existing source's page"
        );
        assert!(!browser.has_device(), "no device was given");

        // A source added later opens too.
        timely(control.call(page("after")))
            .await
            .expect("source.create");
        pages(&browser, 2).await;
        assert!(browser.pages().iter().all(|page| !page.is_closed()));
        assert_eq!(
            *recording.calls.lock().expect("calls"),
            ["use_device", "open", "open"]
        );
        drop(engine);
    }
}
