//! Opening and closing the network listener while the server runs (ADR-0234): the
//! desktop app's "Allow devices on my network" applies at once, without a restart.
//!
//! - **Open** binds the listener, sets the gate's network side and what `pairing.start`
//!   answers ([`NetworkState`]), and, once the server serves, serves it on its own task
//!   with the server's router, and announces it on mDNS when asked (ADR-0233). One
//!   opened before [`Server::serve_with_shutdown`](crate::Server::serve_with_shutdown)
//!   (the host's `ServerConfig::network`) starts serving with it.
//! - **Close** clears the state first, so a request still arriving fails the gate,
//!   then stops the listener (graceful: no new connection), its announcement (a
//!   goodbye), and every WebSocket session admitted on it, which closes with 1001 as at
//!   shutdown. A paired device stays paired; it is admitted again once reopened.
//! - **Shutdown** of the server stops it the same way. The server's router is dropped
//!   when serving ends, so the shutdown's wait for its sessions isn't held up by it.

use crate::announce;
use crate::error::ServerError;
use crate::gate::NetworkSide;
use crate::identity::Identity;
use crate::listener::{Accepted, Limited};
use crate::network::{self, NetworkConfig, TlsLimited};
use crate::network_state::NetworkState;
use axum::Router;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Opens and closes the server's network listener while it runs. Cheap to clone; every
/// clone switches the same listener. Got from [`Server::network_switch`](crate::Server::network_switch).
#[derive(Clone)]
pub struct NetworkSwitch(Arc<Switch>);

impl std::fmt::Debug for NetworkSwitch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkSwitch")
            .field("open", &self.info())
            .finish()
    }
}

/// Where the network listener is open, for the host to show the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkInfo {
    /// The address it listens on.
    pub addr: SocketAddr,
    /// Its URLs, one per name or address the certificate carries.
    pub urls: Vec<String>,
    /// The certificate's SHA-256 fingerprint, `AB:CD:…`.
    pub fingerprint: String,
}

struct Switch {
    state: Arc<NetworkState>,
    /// What serving needs, from when the server starts serving until it stops.
    serving: Mutex<Option<Serving>>,
    /// The open listener, if any. Opening and closing hold it across their awaits, so
    /// two switches never interleave.
    open: tokio::sync::Mutex<Option<Running>>,
    /// Where it is open, kept apart from `open` so [`NetworkSwitch::info`] never reads
    /// `None` just because a switch, or serving's start, holds `open` at that moment.
    info: Mutex<Option<NetworkInfo>>,
}

/// What a listener is served with.
#[derive(Clone)]
pub(crate) struct Serving {
    pub(crate) router: Router,
    pub(crate) max_tcp_connections: usize,
    pub(crate) header_read_timeout: Duration,
    /// The server's shutdown, which stops the listener too.
    pub(crate) shutdown: watch::Receiver<bool>,
}

/// An open listener.
struct Running {
    local_addr: SocketAddr,
    identity: Arc<Identity>,
    /// Turns true to stop the listener and the sessions admitted on it.
    stop: Arc<watch::Sender<bool>>,
    /// Bound and not yet served (opened before serving began), or served.
    listener: Option<(TcpListener, tokio_rustls::TlsAcceptor)>,
    task: Option<JoinHandle<()>>,
    /// The mDNS announcement, while on: its own stop and task.
    announcing: Option<(watch::Sender<bool>, JoinHandle<()>)>,
    announce: bool,
}

impl NetworkSwitch {
    pub(crate) fn new(state: Arc<NetworkState>) -> NetworkSwitch {
        NetworkSwitch(Arc::new(Switch {
            state,
            serving: Mutex::new(None),
            open: tokio::sync::Mutex::new(None),
            info: Mutex::new(None),
        }))
    }

    /// Where the listener is open, or `None` while it is closed.
    pub fn info(&self) -> Option<NetworkInfo> {
        self.0
            .info
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_info(&self, info: Option<NetworkInfo>) {
        *self.0.info.lock().unwrap_or_else(PoisonError::into_inner) = info;
    }

    /// Opens the listener with `config`, closing one already open first. Answers where
    /// it listens.
    ///
    /// # Errors
    /// [`ServerError::Bind`] when its address can't be bound (another program has the
    /// port), [`ServerError::Tls`] when rustls refuses the certificate. The listener is
    /// then closed.
    pub async fn open(&self, config: NetworkConfig) -> Result<NetworkInfo, ServerError> {
        let mut open = self.0.open.lock().await;
        if let Some(running) = open.take() {
            self.stop(running).await;
        }
        let acceptor = network::acceptor(&config.identity)?;
        let bind_error = |source| ServerError::Bind {
            addr: config.bind,
            source,
        };
        let listener = TcpListener::bind(config.bind).await.map_err(bind_error)?;
        let local_addr = listener.local_addr().map_err(bind_error)?;
        let (stop, stopping) = watch::channel(false);
        let mut running = Running {
            local_addr,
            identity: Arc::clone(&config.identity),
            stop: Arc::new(stop),
            listener: Some((listener, acceptor)),
            task: None,
            announcing: None,
            announce: config.announce,
        };
        let info = running.info();
        self.0.state.open(
            NetworkSide::new(local_addr.port(), config.identity.names()),
            Some(jstream_protocol::NetworkAccess {
                urls: info.urls.clone(),
                fingerprint: info.fingerprint.clone(),
            }),
            stopping,
        );
        tracing::info!(
            addr = %local_addr,
            fingerprint = info.fingerprint,
            "network listener on (TLS, device tokens only)"
        );
        let serving = self.serving();
        if let Some(serving) = serving {
            running.serve(&serving);
        }
        *open = Some(running);
        self.set_info(Some(info.clone()));
        Ok(info)
    }

    /// Closes the listener, if open: no new connection, the sessions admitted on it
    /// close, and its announcement says goodbye. Returns once it stopped accepting.
    pub async fn close(&self) {
        let mut open = self.0.open.lock().await;
        if let Some(running) = open.take() {
            self.stop(running).await;
            tracing::info!("network listener off");
        }
    }

    /// Turns the listener's mDNS announcement on or off (ADR-0233), now if it is open
    /// and served, and for the next time it opens otherwise.
    pub async fn set_announce(&self, announce: bool) {
        let mut open = self.0.open.lock().await;
        if let Some(running) = open.as_mut() {
            running.announce = announce;
            if announce {
                if running.task.is_some() {
                    running.start_announcing();
                }
            } else {
                running.stop_announcing().await;
            }
        }
    }

    async fn stop(&self, mut running: Running) {
        self.set_info(None);
        self.0.state.close();
        running.stop.send_replace(true);
        running.stop_announcing().await;
        if let Some(task) = running.task.take() {
            let _ = task.await;
        }
    }

    fn serving(&self) -> Option<Serving> {
        self.0
            .serving
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Serving begins: a listener opened before now starts serving.
    pub(crate) async fn begin(&self, serving: Serving) {
        *self
            .0
            .serving
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(serving.clone());
        let mut open = self.0.open.lock().await;
        if let Some(running) = open.as_mut() {
            running.serve(&serving);
        }
    }

    /// Serving ended: the listener stops, and the router is let go.
    pub(crate) async fn end(&self) {
        self.close().await;
        self.0
            .serving
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }
}

impl Running {
    fn info(&self) -> NetworkInfo {
        NetworkInfo {
            addr: self.local_addr,
            urls: crate::network_urls(&self.identity, self.local_addr.port()),
            fingerprint: self.identity.fingerprint().to_owned(),
        }
    }

    /// Serves the bound listener on its own task until it closes or the server stops.
    fn serve(&mut self, serving: &Serving) {
        let Some((listener, acceptor)) = self.listener.take() else {
            return;
        };
        // Its own pool of open connections, the size of the host listener's, so the
        // network can't use up the host's (#1129 N1).
        let listener = TlsLimited::new(
            Limited::new(
                listener,
                serving.max_tcp_connections,
                serving.header_read_timeout,
            ),
            acceptor,
        );
        let service = serving
            .router
            .clone()
            .into_make_service_with_connect_info::<Accepted>();
        let mut stopping = self.stop.subscribe();
        let stop = Arc::clone(&self.stop);
        let mut shutdown = serving.shutdown.clone();
        // The server's shutdown closes this listener and its sessions too. The receiver
        // goes as soon as either happens, so the shutdown's wait isn't held up.
        tokio::spawn(async move {
            tokio::select! {
                _ = shutdown.wait_for(|stop| *stop) => { stop.send_replace(true); }
                () = async { let _ = stop.subscribe().wait_for(|stop| *stop).await; } => {}
            }
        });
        self.task = Some(tokio::spawn(async move {
            let stop = async move {
                let _ = stopping.wait_for(|stop| *stop).await;
            };
            if let Err(error) = axum::serve(listener, service)
                .with_graceful_shutdown(stop)
                .await
            {
                tracing::error!(%error, "the network listener failed");
            }
        }));
        if self.announce {
            self.start_announcing();
        }
    }

    fn start_announcing(&mut self) {
        if self.announcing.is_some() {
            return;
        }
        // The certificate's first name is the host name, unless the OS gave none.
        let host = self
            .identity
            .names()
            .iter()
            .find(|name| name.parse::<IpAddr>().is_err())
            .map_or("", String::as_str);
        let what = announce::Announcement::new(host, self.local_addr.port());
        let (own, mut own_stop) = watch::channel(false);
        let mut closing = self.stop.subscribe();
        let stop = async move {
            tokio::select! {
                _ = own_stop.wait_for(|stop| *stop) => {}
                _ = closing.wait_for(|stop| *stop) => {}
            }
        };
        self.announcing = Some((own, tokio::spawn(announce::run(what, stop))));
    }

    async fn stop_announcing(&mut self) {
        if let Some((own, task)) = self.announcing.take() {
            own.send_replace(true);
            let _ = task.await;
        }
    }
}
