//! The listener: accepts TCP connections and bounds how long each may take to send an
//! upgrade request the server accepts (`ServerConfig::header_read_timeout`).
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
use tokio::time::{Instant, Sleep};

/// A TCP listener whose connections must send an accepted upgrade request within
/// `header_read_timeout`.
pub(crate) struct Limited {
    inner: TcpListener,
    header_read_timeout: Duration,
}

impl Limited {
    pub(crate) fn new(inner: TcpListener, header_read_timeout: Duration) -> Limited {
        Limited {
            inner,
            header_read_timeout,
        }
    }
}

impl Listener for Limited {
    type Io = Guarded;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Guarded, SocketAddr) {
        // axum's `TcpListener` impl retries (and logs) failed accepts.
        let (stream, peer) = Listener::accept(&mut self.inner).await;
        let at = Instant::now();
        let guarded = Guarded {
            io: stream,
            peer,
            at,
            deadline: Box::pin(tokio::time::sleep_until(at + self.header_read_timeout)),
            request_read: Arc::new(AtomicBool::new(false)),
        };
        (guarded, peer)
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
    /// When the connection was accepted.
    at: Instant,
    deadline: Pin<Box<Sleep>>,
    /// Set by the upgrade handler once it has accepted the request: the deadline no
    /// longer applies.
    request_read: Arc<AtomicBool>,
}

impl Guarded {
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

/// What the upgrade handler learns about its connection (axum's `ConnectInfo`).
#[derive(Clone, Debug)]
pub(crate) struct Accepted {
    /// The peer's address, for logging only.
    pub(crate) peer: SocketAddr,
    /// When the connection was accepted: the handshake timeout counts from here.
    pub(crate) at: Instant,
    request_read: Arc<AtomicBool>,
}

impl Accepted {
    /// Disarms the connection's header-read deadline: its request was accepted.
    pub(crate) fn request_read(&self) {
        self.request_read.store(true, Ordering::Release);
    }
}

impl Connected<IncomingStream<'_, Limited>> for Accepted {
    fn connect_info(stream: IncomingStream<'_, Limited>) -> Self {
        let io = stream.io();
        Accepted {
            peer: io.peer,
            at: io.at,
            request_read: Arc::clone(&io.request_read),
        }
    }
}
