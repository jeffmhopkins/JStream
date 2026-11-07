//! The network listener (ADR-0104): a second listener, for devices on the network, that
//! speaks TLS only and admits device tokens only. The host turns it on
//! ([`ServerConfig::network`](crate::ServerConfig::network)); the loopback listener is
//! unchanged beside it.
//!
//! - **TLS.** rustls on the ring provider (R-DEP-006), TLS 1.3 and 1.2 with rustls'
//!   default cipher suites for them, no client certificates, ALPN `http/1.1` (the
//!   server speaks HTTP/1.1 only). The certificate is the host's [`Identity`].
//! - **Limits.** It has its own pool of open TCP connections, the size of the loopback
//!   listener's ([`ServerConfig::max_tcp_connections`](crate::ServerConfig::max_tcp_connections)),
//!   so connections from the network that never authenticate can't use up the host's own
//!   UI's. The header-read deadline covers the TLS handshake too: a peer that never
//!   finishes it is dropped at the deadline. The handshake runs on the connection's
//!   own task, never in the accept loop. The WebSocket connection and MCP session limits
//!   are shared with loopback, since only an admitted device reaches them.
//! - **Who gets in.** The gate's network side ([`Gate`](crate::gate::Gate)): `Host` must
//!   name the listener's port with a name the certificate carries or the address the
//!   connection arrived on; `Origin`, when present, is `https://` and that host; and only
//!   a device token, never the control token. `POST /api/v1/pair` and `GET /pair` are
//!   answered here too, sharing the open code's five attempts with the
//!   host's listener (`devices.rs`).

use crate::error::ServerError;
use crate::identity::Identity;
use crate::listener::{Guarded, Limited, Meta};
use axum::serve::Listener;
use rustls::pki_types::PrivateKeyDer;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

/// The network listener's settings (ADR-0104 §1): where it listens, and the key and
/// certificate it serves.
#[derive(Clone, Debug)]
pub struct NetworkConfig {
    /// The address to listen on: every interface (`0.0.0.0` or `[::]`) on a fixed port,
    /// or one interface's address. The host picks the port, and the person can change it.
    pub bind: SocketAddr,
    /// The certificate and key, made on the host ([`Identity::load_or_create`]). Its
    /// names are also the `Host` names the listener accepts.
    pub identity: Arc<Identity>,
}

/// The TLS setup for `identity` (ADR-0104 §2, #1129 N2).
///
/// # Errors
/// [`ServerError::Tls`] when rustls refuses the key or certificate, including a key that
/// doesn't match the certificate.
pub(crate) fn acceptor(identity: &Identity) -> Result<TlsAcceptor, ServerError> {
    let tls = |error: rustls::Error| ServerError::Tls(error.to_string());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(tls)?
        .with_no_client_auth()
        .with_single_cert(vec![identity.cert()], PrivateKeyDer::Pkcs8(identity.key()))
        .map_err(tls)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// The network listener: [`Limited`]'s accepted connections, each wrapped in TLS.
pub(crate) struct TlsLimited {
    inner: Limited,
    acceptor: TlsAcceptor,
}

impl TlsLimited {
    pub(crate) fn new(inner: Limited, acceptor: TlsAcceptor) -> TlsLimited {
        TlsLimited { inner, acceptor }
    }
}

impl Listener for TlsLimited {
    type Io = TlsIo;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (TlsIo, SocketAddr) {
        let (io, peer) = self.inner.accept().await;
        let meta = io.meta();
        // The handshake runs when the connection's task first reads or writes, so a slow
        // peer never holds up the accept loop.
        let io = TlsIo {
            meta,
            state: State::Handshake(Box::new(self.acceptor.accept(io))),
        };
        (io, peer)
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
}

/// One connection to the network listener: its TLS handshake, then the TLS stream.
pub(crate) struct TlsIo {
    meta: Meta,
    state: State,
}

enum State {
    Handshake(Box<tokio_rustls::Accept<Guarded>>),
    Ready(Box<TlsStream<Guarded>>),
    Failed,
}

impl TlsIo {
    /// What the handlers learn about this connection.
    pub(crate) fn meta(&self) -> &Meta {
        &self.meta
    }

    /// Drives the handshake: `Ready(Ok)` once the TLS stream is up.
    fn handshake(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.state {
            State::Ready(_) => Poll::Ready(Ok(())),
            State::Failed => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "the TLS handshake failed",
            ))),
            State::Handshake(accept) => match Pin::new(accept.as_mut()).poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Ok(stream)) => {
                    self.state = State::Ready(Box::new(stream));
                    Poll::Ready(Ok(()))
                }
                Poll::Ready(Err(error)) => {
                    tracing::debug!(
                        peer = %self.meta.peer,
                        error = %error,
                        "network connection dropped: TLS handshake failed"
                    );
                    self.state = State::Failed;
                    Poll::Ready(Err(error))
                }
            },
        }
    }

    fn stream(&mut self) -> &mut TlsStream<Guarded> {
        match &mut self.state {
            State::Ready(stream) => stream,
            // `handshake` returned `Ready(Ok)` first.
            State::Handshake(_) | State::Failed => unreachable!("the handshake is done"),
        }
    }
}

impl AsyncRead for TlsIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        std::task::ready!(this.handshake(cx))?;
        Pin::new(this.stream()).poll_read(cx, buf)
    }
}

impl AsyncWrite for TlsIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        std::task::ready!(this.handshake(cx))?;
        Pin::new(this.stream()).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        std::task::ready!(this.handshake(cx))?;
        Pin::new(this.stream()).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        match &self.state {
            State::Ready(stream) => stream.is_write_vectored(),
            State::Handshake(_) | State::Failed => false,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match &mut this.state {
            State::Ready(stream) => Pin::new(stream.as_mut()).poll_flush(cx),
            State::Handshake(_) | State::Failed => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match &mut this.state {
            State::Ready(stream) => Pin::new(stream.as_mut()).poll_shutdown(cx),
            State::Handshake(_) | State::Failed => Poll::Ready(Ok(())),
        }
    }
}
