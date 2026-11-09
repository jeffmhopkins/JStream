//! The listener: accepts TCP connections, bounds how many are open at once
//! (`ServerConfig::max_tcp_connections`), and bounds how long each may take to send an
//! upgrade request the server accepts (`ServerConfig::header_read_timeout`). The network
//! listener (`network.rs`) wraps one of its own in TLS, with its own pool of permits.
//! Each connection's [`Meta`] tells the handlers its peer, the address it arrived on
//! and which listener took it.
//!
//! **Open connections.** Every accepted stream holds a permit from one semaphore until
//! it is dropped, whatever it carries: a request not yet authenticated, an HTTP
//! keep-alive connection to `/mcp` or the UI, or an upgraded WebSocket. While none is
//! free, a newly accepted connection is closed at once, unanswered, so connections that
//! never authenticate can't exhaust the process's sockets or memory, and neither can
//! authenticated keep-alive ones, which the header-read deadline no longer bounds (#354).
//! The first such drop is logged at `warn`, and later ones are not, until a connection
//! is accepted again.
//!
//! hyper's own header-read timeout isn't reachable through `axum::serve`, so the bound
//! is kept on the socket instead: each accepted stream is wrapped in a [`Guarded`] that
//! fails every read and write once its deadline has passed, which ends the connection.
//! The upgrade handler disarms it ([`Accepted::request_read`]) once it has accepted a
//! request, and from then on the session's own limits apply. A connection that sends its
//! headers too slowly, sends none, or keeps the connection open after a refusal never
//! gets disarmed, so it is dropped at the deadline.

use axum::extract::connect_info::Connected;
use axum::serve::{IncomingStream, Listener};
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, Sleep};

/// A TCP listener that keeps at most `max_tcp_connections` connections open, each of
/// which must send an accepted upgrade request within `header_read_timeout`.
pub(crate) struct Limited {
    inner: TcpListener,
    /// The listener's own address, for a connection whose local address can't be read.
    local: SocketAddr,
    header_read_timeout: Duration,
    /// One permit per open connection.
    open: Arc<Semaphore>,
    /// Whether a connection was dropped for want of a permit since the last one was
    /// accepted: only the first drop of a run is logged.
    full: bool,
}

impl Limited {
    pub(crate) fn new(
        inner: TcpListener,
        max_tcp_connections: usize,
        header_read_timeout: Duration,
    ) -> Limited {
        let local = inner
            .local_addr()
            .unwrap_or(SocketAddr::from(([127, 0, 0, 1], 0)));
        Limited {
            inner,
            local,
            header_read_timeout,
            open: Arc::new(Semaphore::new(
                max_tcp_connections.min(Semaphore::MAX_PERMITS),
            )),
            full: false,
        }
    }
}

impl Listener for Limited {
    type Io = Guarded;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Guarded, SocketAddr) {
        loop {
            // axum's `TcpListener` impl retries (and logs) failed accepts.
            let (stream, peer) = Listener::accept(&mut self.inner).await;
            let Ok(permit) = Arc::clone(&self.open).try_acquire_owned() else {
                if !self.full {
                    self.full = true;
                    tracing::warn!(
                        peer = %peer,
                        "connection dropped: too many open connections"
                    );
                }
                // Dropping the stream closes it.
                continue;
            };
            self.full = false;
            // Replies are small writes, often several in a row (a response, then its
            // event): with Nagle's algorithm on, each after the first waited for the
            // client's delayed ACK, about 40 ms on Linux (#1440).
            if let Err(error) = stream.set_nodelay(true) {
                tracing::debug!(peer = %peer, %error, "could not turn off Nagle's algorithm");
            }
            let at = Instant::now();
            let local = stream.local_addr().unwrap_or(self.local);
            let guarded = Guarded {
                io: stream,
                peer,
                local,
                at,
                deadline: Box::pin(tokio::time::sleep_until(at + self.header_read_timeout)),
                request_read: Arc::new(AtomicBool::new(false)),
                _open: permit,
            };
            return (guarded, peer);
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
}

/// An accepted TCP stream that fails every read and write once its deadline has passed,
/// until the upgrade handler disarms it.
pub(crate) struct Guarded {
    io: TcpStream,
    peer: SocketAddr,
    /// The address the connection arrived on.
    local: SocketAddr,
    /// When the connection was accepted.
    at: Instant,
    deadline: Pin<Box<Sleep>>,
    /// Set by the upgrade handler once it has accepted the request: the deadline no
    /// longer applies.
    request_read: Arc<AtomicBool>,
    /// This connection's place among the open ones, freed when it is dropped.
    _open: OwnedSemaphorePermit,
}

impl Guarded {
    /// What the handlers learn about this connection, on the network listener when
    /// `network`.
    pub(crate) fn meta_on(&self, network: bool) -> Meta {
        Meta {
            peer: self.peer,
            local: self.local,
            at: self.at,
            request_read: Arc::clone(&self.request_read),
            network,
        }
    }

    /// [`meta_on`](Guarded::meta_on) the network listener, which wraps it in TLS.
    #[cfg(feature = "tls")]
    pub(crate) fn meta(&self) -> Meta {
        self.meta_on(true)
    }

    /// An error if the deadline has passed and the request hasn't been accepted.
    /// Otherwise the deadline's timer is registered with `cx`, so a read or write that
    /// waits is woken when it passes.
    fn check(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if self.request_read.load(Ordering::Acquire) {
            return Ok(());
        }
        match self.deadline.as_mut().poll(cx) {
            Poll::Pending => Ok(()),
            Poll::Ready(()) => {
                tracing::debug!(
                    peer = %self.peer,
                    "connection dropped: no accepted upgrade request within the header-read timeout"
                );
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "no accepted upgrade request in time",
                ))
            }
        }
    }
}

impl AsyncRead for Guarded {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.check(cx)?;
        Pin::new(&mut this.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for Guarded {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.check(cx)?;
        Pin::new(&mut this.io).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.check(cx)?;
        Pin::new(&mut this.io).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.check(cx)?;
        Pin::new(&mut this.io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
    }
}

/// What the handlers learn about a connection: axum's `ConnectInfo`.
pub(crate) type Accepted = Meta;

/// What the handlers learn about their connection (axum's `ConnectInfo`).
#[derive(Clone, Debug)]
pub(crate) struct Meta {
    /// The peer's address. The gate decides on the headers; the network listener also
    /// counts pairing attempts by it.
    pub(crate) peer: SocketAddr,
    /// The address the connection arrived on: on the network listener, a `Host` naming
    /// it is accepted.
    pub(crate) local: SocketAddr,
    /// When the connection was accepted: the handshake timeout counts from here.
    pub(crate) at: Instant,
    request_read: Arc<AtomicBool>,
    /// Whether it came to the network listener (ADR-0104) rather than the host's own.
    pub(crate) network: bool,
}

impl Meta {
    /// Disarms the connection's header-read deadline: its request was accepted.
    pub(crate) fn request_read(&self) {
        self.request_read.store(true, Ordering::Release);
    }

    /// Which side of the gate the connection is on.
    pub(crate) fn on(&self) -> On {
        if self.network {
            On::Network(self.local)
        } else {
            On::Host
        }
    }
}

/// Which listener a request came to, for the gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum On {
    /// The host's own listener: loopback, or wherever `ServerConfig::bind` says.
    Host,
    /// The network listener, on this local address.
    Network(SocketAddr),
}

impl Connected<IncomingStream<'_, Limited>> for Meta {
    fn connect_info(stream: IncomingStream<'_, Limited>) -> Self {
        stream.io().meta_on(false)
    }
}

#[cfg(feature = "tls")]
impl Connected<IncomingStream<'_, crate::network::TlsLimited>> for Meta {
    fn connect_info(stream: IncomingStream<'_, crate::network::TlsLimited>) -> Self {
        stream.io().meta().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1440: an accepted connection has Nagle's algorithm off, so a reply written
    /// right after another doesn't wait for the client's delayed ACK.
    #[tokio::test]
    async fn an_accepted_connection_sends_without_delay() {
        let inner = match TcpListener::bind("127.0.0.1:0").await {
            Ok(inner) => inner,
            Err(error) => panic!("bind: {error}"),
        };
        let mut limited = Limited::new(inner, 4, Duration::from_secs(10));
        let addr = match limited.local_addr() {
            Ok(addr) => addr,
            Err(error) => panic!("local_addr: {error}"),
        };
        let (client, (accepted, _)) = tokio::join!(TcpStream::connect(addr), limited.accept());
        let _client = match client {
            Ok(client) => client,
            Err(error) => panic!("connect: {error}"),
        };
        match accepted.io.nodelay() {
            Ok(nodelay) => assert!(nodelay, "Nagle's algorithm is still on"),
            Err(error) => panic!("nodelay: {error}"),
        }
    }
}
