//! The engine in the app's own process, served to the webview over the same
//! authenticated localhost WebSocket API a remote client uses (ARCH › Hosts), so the UI
//! has one code path.
//!
//! The server listens on `127.0.0.1` on a port the OS picks, behind a control token made
//! fresh at each start and kept only in memory, and accepts the webview's origin besides
//! its own. MCP is off: nothing outside the app knows the token.
//!
//! Docs: apps/jstream-desktop/README.md#design

use anyhow::Context;
use jstream_core::{AuditSink, Engine};
use jstream_crash::Secrets;
use jstream_server::{Connector, Server, ServerConfig, Token};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
#[cfg(windows)]
use {
    jstream_core::{LivePublish, OpenPublish, PublishError},
    jstream_crash::Redacting,
    jstream_platform::{Backends, Secret, StreamKeyStore},
    jstream_protocol::{OutputId, OutputKind, RtmpTarget},
    std::sync::{OnceLock, Weak},
};

/// How long [`Host::stop`] waits for the server to close its connections.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// The running engine and its server.
pub struct Host {
    runtime: Runtime,
    stop: Option<oneshot::Sender<()>>,
    serving: Option<JoinHandle<Result<(), jstream_server::ServerError>>>,
    ws_url: String,
    token: String,
}

impl Host {
    /// Serves `engine` on `127.0.0.1` with a new token, accepting pages from `origin`.
    ///
    /// # Errors
    /// When the runtime can't start, the OS has no randomness for the token, or the
    /// server can't bind.
    pub fn start(engine: Arc<Engine>, origin: &str, secrets: &Secrets) -> anyhow::Result<Host> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("jstream-host")
            .build()
            .context("could not start the async runtime")?;
        let token = new_token()?;
        secrets.register(&token);
        let mut config = ServerConfig::new(Token::new(token.clone())?);
        config.extra_origins = vec![origin.to_owned()];
        // The engine serves only this webview, so it pairs no device (ADR-0047).
        config.pairing = false;
        let connector: Connector = Arc::new(move || Box::new(engine.connect()));
        let server: Server = runtime
            .block_on(Server::bind(config, connector))
            .context("could not start the engine's server")?;
        let ws_url = server.ws_url();
        let (stop, stopped) = oneshot::channel::<()>();
        let serving = runtime.spawn(server.serve_with_shutdown(async move {
            let _ = stopped.await;
        }));
        tracing::info!(url = %ws_url, "engine serving the webview");
        Ok(Host {
            runtime,
            stop: Some(stop),
            serving: Some(serving),
            ws_url,
            token,
        })
    }

    /// The WebSocket URL the UI connects to: `ws://127.0.0.1:<port>/api/v1/ws`.
    pub fn ws_url(&self) -> &str {
        &self.ws_url
    }

    /// The WebSocket origin, `ws://127.0.0.1:<port>`, for the UI's `connect-src`.
    pub fn ws_origin(&self) -> String {
        let rest = self.ws_url.strip_prefix("ws://").unwrap_or(&self.ws_url);
        let authority = rest.split('/').next().unwrap_or_default();
        format!("ws://{authority}")
    }

    /// The control token. Handed only to the webview; never logged or stored.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Stops the server, closing every connection, and waits for it (at most
    /// 5 s). The engine stops when its last session goes.
    pub fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(serving) = self.serving.take() {
            match self
                .runtime
                .block_on(async { tokio::time::timeout(STOP_TIMEOUT, serving).await })
            {
                Ok(Ok(Ok(()))) => tracing::info!("engine server stopped"),
                Ok(Ok(Err(error))) => tracing::warn!(%error, "engine server failed"),
                Ok(Err(error)) => tracing::warn!(%error, "engine server task failed"),
                Err(_) => tracing::warn!("engine server did not stop in time"),
            }
        }
        self.runtime.shutdown_timeout(STOP_TIMEOUT);
    }
}

/// A new control token: 32 random bytes from the OS, as lowercase hex, which is valid
/// both in an `Authorization` header and as a WebSocket subprotocol name.
fn new_token() -> anyhow::Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow::anyhow!("the OS has no randomness for the token: {error}"))?;
    Ok(bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

/// The engine the app runs: the collection in `collection` (created empty the first
/// time), saved on every change, with `audit` recording every batch and action.
///
/// On Windows it is live, as the daemon's is: the OS backends, stream keys in
/// Credential Manager, and RTMP(S) publishing on `output.start`, plus the virtual
/// camera (`WindowsVcam`) unless `vcam` is false (`--no-vcam`). With the `cef` feature
/// it also opens browser sources' pages, keeping CEF's cache and log under `data`
/// (`browser::backend`). Elsewhere there are no backends yet. Either way it starts the
/// engine's stats heartbeat, and on Windows each `stats.tick` carries the process's
/// CPU time.
///
/// # Errors
/// When the collection file can't be read, loaded or created, and on Windows when a
/// backend can't be created (the QPC clock).
pub fn engine(
    collection: &Path,
    data: &Path,
    audit: Arc<dyn AuditSink>,
    vcam: bool,
    secrets: &Secrets,
) -> anyhow::Result<Arc<Engine>> {
    let engine = Engine::new()
        .with_audit(audit)
        .with_collection_file(collection)
        .context("could not open the collection")?;
    #[cfg(windows)]
    let engine = {
        let backends = windows_backends(vcam)?;
        #[cfg(feature = "cef")]
        let browser = crate::browser::backend(data, Arc::clone(&backends.clock));
        #[cfg(not(feature = "cef"))]
        let browser = {
            let _ = data;
            None
        };
        live(engine, backends, browser, secrets)
    };
    #[cfg(not(windows))]
    let engine = {
        let _ = (data, vcam, secrets);
        Arc::new(engine)
    };
    // `stats.tick`'s load: the process's CPU time, and a tick a second while idle.
    #[cfg(windows)]
    engine.set_process_cpu(Arc::new(jstream_platform_windows::WindowsProcessCpu));
    engine.start_stats_heartbeat();
    Ok(engine)
}

/// What the app tells the UI about its virtual camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VcamStatus {
    /// The administrators-only copy is registered and has the content of the DLL
    /// beside the exe: a `virtual_cam` output can start.
    Registered,
    /// Not registered, or the registration couldn't be read: the optional
    /// `jstream-daemon --register-vcam` registers it.
    NotRegistered,
    /// Registered, but not for a current copy of the DLL beside the exe: an update
    /// changed it, or an older JStream registered its own folder (ADR-0066).
    /// Registering again is optional.
    Outdated,
    /// Turned off with `--no-vcam`: the engine has no camera.
    Off,
    /// No virtual camera on this platform.
    Unavailable,
}

impl VcamStatus {
    /// The value the bootstrap script hands the UI (`jstream-ui`'s `Vcam::from_host`).
    pub fn as_str(self) -> &'static str {
        match self {
            VcamStatus::Registered => "registered",
            VcamStatus::NotRegistered => "not_registered",
            VcamStatus::Outdated => "outdated",
            VcamStatus::Off => "off",
            VcamStatus::Unavailable => "unavailable",
        }
    }
}

/// The virtual camera's status: off when `enabled` is false, else on Windows whether
/// the registered copy has the content of the DLL beside the exe (`exe_dir`), read
/// with no elevation. It only reports: it never registers or prompts (ADR-0066).
pub fn vcam_status(exe_dir: Option<&Path>, enabled: bool) -> VcamStatus {
    if !enabled {
        return VcamStatus::Off;
    }
    #[cfg(windows)]
    {
        use jstream_platform_windows::{VCAM_DLL, VcamRegistration, vcam_registration};
        let Some(dll) = exe_dir.map(|dir| dir.join(VCAM_DLL)) else {
            return VcamStatus::NotRegistered;
        };
        match vcam_registration(&dll) {
            Ok(VcamRegistration::Current { .. }) => VcamStatus::Registered,
            Ok(VcamRegistration::Missing) => VcamStatus::NotRegistered,
            Ok(VcamRegistration::Outdated { registered, why }) => {
                tracing::warn!(
                    registered = %registered.display(),
                    ?why,
                    "the virtual camera's registration is not a current copy of this \
                     app's DLL; registering again (`jstream-daemon --register-vcam`) is \
                     optional"
                );
                VcamStatus::Outdated
            }
            Err(error) => {
                tracing::warn!(%error, "could not read the virtual camera's registration");
                VcamStatus::NotRegistered
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = exe_dir;
        VcamStatus::Unavailable
    }
}

/// The Windows backends, as the daemon creates them, plus the virtual camera when
/// `vcam` is true. Creating them opens no device: the camera checks its registration
/// and opens only on `output.start`.
#[cfg(windows)]
fn windows_backends(vcam: bool) -> anyhow::Result<Backends> {
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
        vcam: vcam.then(|| Box::new(WindowsVcam::new()) as Box<dyn jstream_platform::VcamSink>),
        media: Arc::new(MfMediaDecoders::new()),
    })
}

/// Makes `engine` live on `backends`, with keys in Credential Manager (behind
/// [`Redacting`], registering each with `secrets`), a publish
/// opener that connects RTMP(S) and, when there is one, `browser` for browser sources.
/// The opener holds the engine weakly, so the engine still drops when the server lets
/// go of it. The same wiring as the daemon's `live::live`
/// (apps/jstream-daemon/src/live.rs), whose tests cover the opener.
#[cfg(windows)]
fn live(
    engine: Engine,
    backends: Backends,
    browser: Option<Arc<dyn jstream_sources::BrowserBackend>>,
    secrets: &Secrets,
) -> Arc<Engine> {
    // Behind `Redacting`, so each key is registered with the crash reports' redaction
    // as it is stored or read (#353).
    let keys: Arc<dyn StreamKeyStore> = Arc::new(Redacting::new(
        secrets.clone(),
        Arc::new(jstream_platform_windows::WindowsCredentials),
    ));
    let handle = Arc::new(OnceLock::new());
    let engine = engine
        .with_key_store(Arc::clone(&keys))
        .with_backends(backends)
        .with_publish_opener(opener(Arc::clone(&handle), keys));
    let engine = Arc::new(match browser {
        Some(browser) => engine.with_browser(browser),
        None => engine,
    });
    // The cell is new, so this is its only value.
    let _ = handle.set(Arc::downgrade(&engine));
    engine
}

/// On each `output.start`: the output's server URL from the collection, its key from
/// `keys`, then `connect_rtmp`. No reason carries the key (R-SEC-001).
#[cfg(windows)]
fn opener(engine: Arc<OnceLock<Weak<Engine>>>, keys: Arc<dyn StreamKeyStore>) -> OpenPublish {
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
                    "output `{output_id}` is of a kind the app can't publish yet"
                )));
            }
            None => {
                return Err(PublishError::new(format!(
                    "output `{output_id}` has no target: create it with `output.create`"
                )));
            }
        };
        let key: Secret = keys
            .get(output_id.as_str())
            .map_err(|error| PublishError::new(format!("could not read the stream key: {error}")))?
            .ok_or_else(|| {
                PublishError::new(format!(
                    "output `{output_id}` has no stream key: set one in its settings"
                ))
            })?;
        let publish: Box<dyn LivePublish> =
            Box::new(jstream_core::connect_rtmp(&url, key.expose())?);
        Ok(publish)
    })
}

#[cfg(test)]
mod tests {
    use super::{Host, VcamStatus, engine, new_token, vcam_status};
    use crate::ui::ORIGIN;
    use jstream_control::protocol::{Hello, PROTOCOL_VERSION};
    use jstream_control::{Control, WsConfig, WsControl};
    use jstream_core::AuditMemory;
    use jstream_crash::Secrets;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Arc;

    /// `--no-vcam` is off whatever the registry says; the UI reads these exact values.
    #[test]
    fn the_vcam_status_names_what_the_ui_reads() {
        assert_eq!(vcam_status(None, false), VcamStatus::Off);
        let names: Vec<&str> = [
            VcamStatus::Registered,
            VcamStatus::NotRegistered,
            VcamStatus::Outdated,
            VcamStatus::Off,
            VcamStatus::Unavailable,
        ]
        .into_iter()
        .map(VcamStatus::as_str)
        .collect();
        assert_eq!(
            names,
            [
                "registered",
                "not_registered",
                "outdated",
                "off",
                "unavailable"
            ]
        );
        #[cfg(not(windows))]
        assert_eq!(vcam_status(None, true), VcamStatus::Unavailable);
        #[cfg(windows)]
        assert_eq!(vcam_status(None, true), VcamStatus::NotRegistered);
    }

    #[test]
    fn tokens_are_fresh_lowercase_hex() {
        let (a, b) = (new_token().ok(), new_token().ok());
        let a = a.unwrap_or_default();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')));
        assert_ne!(Some(a), b);
    }

    /// The host serves its engine on localhost behind its token until it stops.
    #[test]
    fn serves_the_engine_behind_its_token_until_stopped() {
        let collection = std::env::temp_dir().join(format!(
            "jstream-desktop-collection-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&collection);
        let secrets = Secrets::new();
        let engine = engine(
            &collection,
            &std::env::temp_dir(),
            Arc::new(AuditMemory::new()),
            true,
            &secrets,
        )
        .unwrap_or_else(|e| panic!("an engine: {e:#}"));
        let host = Host::start(engine, "http://jstream.localhost", &secrets)
            .unwrap_or_else(|e| panic!("a host: {e:#}"));
        assert!(
            host.ws_url().starts_with("ws://127.0.0.1:"),
            "{}",
            host.ws_url()
        );
        assert!(host.ws_url().ends_with("/api/v1/ws"));
        assert!(
            secrets.is_registered(host.token()),
            "the token is redacted from crash reports (#353)"
        );
        let origin = host.ws_origin();
        assert!(host.ws_url().starts_with(&origin) && !origin.ends_with('/'));

        let client = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap_or_else(|e| panic!("{e}"));
        let hello = Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "desktop-test".into(),
        };
        let url = host.ws_url().to_owned();
        let token = host.token().to_owned();
        client.block_on(async {
            let wrong = WsControl::connect(WsConfig::new(&url, "wrong")).await;
            assert!(wrong.is_err(), "a wrong token is refused");
            let control = WsControl::connect(WsConfig::new(&url, &token))
                .await
                .unwrap_or_else(|e| panic!("connect: {e}"));
            let welcome = control
                .hello(hello)
                .await
                .unwrap_or_else(|e| panic!("hello: {e}"));
            assert_eq!(welcome.protocol, PROTOCOL_VERSION);
        });
        host.stop();
        let again = client.block_on(WsControl::connect(WsConfig::new(&url, &token)));
        assert!(again.is_err(), "stopped");
        assert!(collection.is_file(), "the collection file was created");
        let _ = std::fs::remove_file(&collection);
    }

    /// The status line of a WebSocket upgrade to `host`'s server with its token and
    /// `origin`, as the webview sends it.
    fn upgrade_status(host: &Host, origin: &str) -> String {
        let authority = host.ws_origin().replace("ws://", "");
        let mut stream = TcpStream::connect(&authority).unwrap_or_else(|e| panic!("{e}"));
        let request = format!(
            "GET /api/v1/ws HTTP/1.1\r\nHost: {authority}\r\nOrigin: {origin}\r\n\
             Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Authorization: Bearer {}\r\n\r\n",
            host.token()
        );
        stream
            .write_all(request.as_bytes())
            .unwrap_or_else(|e| panic!("{e}"));
        let mut head = [0_u8; 12];
        stream
            .read_exact(&mut head)
            .unwrap_or_else(|e| panic!("{e}"));
        String::from_utf8_lossy(&head).into_owned()
    }

    /// The webview's origin is let in; another page's is refused, token or not.
    #[test]
    fn only_the_webview_origin_is_accepted() {
        let collection = std::env::temp_dir().join(format!(
            "jstream-desktop-origin-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&collection);
        let engine = engine(
            &collection,
            &std::env::temp_dir(),
            Arc::new(AuditMemory::new()),
            false,
            &Secrets::new(),
        )
        .unwrap_or_else(|e| panic!("an engine: {e:#}"));
        let host = Host::start(engine, ORIGIN, &Secrets::new())
            .unwrap_or_else(|e| panic!("a host: {e:#}"));
        assert_eq!(upgrade_status(&host, ORIGIN), "HTTP/1.1 101");
        assert_eq!(upgrade_status(&host, "http://evil.example"), "HTTP/1.1 403");
        host.stop();
        let _ = std::fs::remove_file(&collection);
    }
}
