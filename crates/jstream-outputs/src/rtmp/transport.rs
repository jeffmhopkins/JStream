//! Opening the byte stream a session runs over: TCP for `rtmp://`, TLS over TCP for
//! `rtmps://` (rustls with the `ring` provider, docs/RULES.md#R-DEP-006).
//!
//! Docs: crates/jstream-outputs/README.md#purpose

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, Stream, StreamOwned};

use super::{RtmpError, RtmpUrl, Scheme};

/// The connected transport [`connect`] returns: plain TCP for `rtmp://`, or TLS over TCP
/// for `rtmps://`. It is the `S` of an [`RtmpPublisher`](super::RtmpPublisher).
///
/// Timeouts are set on the TCP socket underneath ([`set_read_timeout`](Self::set_read_timeout),
/// [`set_write_timeout`](Self::set_write_timeout)) and apply through TLS as well: a TLS
/// read that times out keeps any partly received record and resumes it on the next
/// read, so [`RtmpPublisher::poll_incoming`](super::RtmpPublisher::poll_incoming) works
/// the same over both.
///
/// A write timeout bounds each socket write, and writing one large message takes
/// many. [`set_write_deadline`](Self::set_write_deadline) bounds them all together.
#[derive(Debug)]
pub struct Transport {
    link: Link,
    /// Set by [`Transport::set_write_deadline`].
    write_deadline: Option<Instant>,
}

/// The byte stream itself.
#[derive(Debug)]
enum Link {
    /// `rtmp://`: the TCP stream itself.
    Tcp(TcpStream),
    /// `rtmps://`: a rustls client session over the TCP stream, handshake complete.
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Transport {
    fn new(link: Link) -> Self {
        Self {
            link,
            write_deadline: None,
        }
    }

    /// The TCP socket underneath, for example to read its peer address.
    pub fn tcp(&self) -> &TcpStream {
        match &self.link {
            Link::Tcp(s) => s,
            Link::Tls(s) => &s.sock,
        }
    }

    /// Whether this is a TLS (`rtmps://`) transport.
    pub fn is_tls(&self) -> bool {
        matches!(self.link, Link::Tls(_))
    }

    /// Bounds every later write, and the flush, by `deadline`: before each socket write
    /// (over TLS too) the write timeout is cut to the time left, and once it has passed
    /// a write fails with [`TimedOut`](io::ErrorKind::TimedOut) without touching the
    /// socket. So a peer that reads a few bytes at a time cannot stretch one message
    /// past it, as it can with a write timeout alone, which each partial write
    /// restarts. Each socket write also sends at most 16 KiB, because on macOS and the
    /// BSDs the timeout of one write restarts whenever the peer frees buffer space; there
    /// a trickling peer can still hold a write for as long as it takes to read 16 KiB.
    /// `None` stops cutting, and leaves the last timeout set in place; set
    /// another with [`set_write_timeout`](Self::set_write_timeout).
    pub fn set_write_deadline(&mut self, deadline: Option<Instant>) {
        self.write_deadline = deadline;
    }

    /// Sets the socket's read timeout. With a short one, a read that finds nothing
    /// pending fails with `WouldBlock` or `TimedOut`, which `poll_incoming` treats as
    /// "nothing to do".
    ///
    /// # Errors
    /// When the operating system refuses the value (for example a zero duration).
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.tcp().set_read_timeout(timeout)
    }

    /// Sets the socket's write timeout, so a stalled server surfaces as an error.
    ///
    /// # Errors
    /// When the operating system refuses the value (for example a zero duration).
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.tcp().set_write_timeout(timeout)
    }

    /// Closes the connection cleanly: for TLS, sends `close_notify` first so the server
    /// sees an orderly end rather than a truncated session; then shuts the socket down
    /// in both directions.
    ///
    /// A socket the peer has already disconnected or reset counts as shut down, at
    /// either step.
    ///
    /// # Errors
    /// Transport errors while sending `close_notify` or shutting the socket down.
    pub fn shutdown(&mut self) -> io::Result<()> {
        if let Link::Tls(s) = &mut self.link {
            s.conn.send_close_notify();
            match s.flush() {
                Err(e) if peer_gone(&e) => return Ok(()),
                result => result?,
            }
        }
        match self.tcp().shutdown(Shutdown::Both) {
            Err(e) if peer_gone(&e) => Ok(()),
            result => result,
        }
    }
}

/// Whether `e` says the peer already closed or reset the connection.
fn peer_gone(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotConnected
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}

impl Read for Transport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.link {
            Link::Tcp(s) => s.read(buf),
            Link::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match (&mut self.link, self.write_deadline) {
            (Link::Tcp(s), None) => s.write(buf),
            (Link::Tls(s), None) => s.write(buf),
            (Link::Tcp(s), Some(deadline)) => DeadlineSocket::new(s, deadline).write(buf),
            (Link::Tls(s), Some(deadline)) => {
                let s = &mut **s;
                // The same as `StreamOwned`'s write, over a socket that cuts each of
                // the writes rustls makes to the time left.
                Stream::new(&mut s.conn, &mut DeadlineSocket::new(&s.sock, deadline)).write(buf)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match (&mut self.link, self.write_deadline) {
            (Link::Tcp(s), _) => s.flush(),
            (Link::Tls(s), None) => s.flush(),
            (Link::Tls(s), Some(deadline)) => {
                let s = &mut **s;
                Stream::new(&mut s.conn, &mut DeadlineSocket::new(&s.sock, deadline)).flush()
            }
        }
    }
}

/// The most one socket write sends while a write deadline is set
/// ([`DeadlineSocket`]): one TLS record's worth.
const MAX_DEADLINE_WRITE: usize = 16 * 1024;

/// A TCP socket whose every write is bounded by `deadline`
/// ([`Transport::set_write_deadline`]). Reads pass straight through.
struct DeadlineSocket<'a> {
    tcp: &'a TcpStream,
    deadline: Instant,
}

impl<'a> DeadlineSocket<'a> {
    fn new(tcp: &'a TcpStream, deadline: Instant) -> Self {
        Self { tcp, deadline }
    }
}

impl Read for DeadlineSocket<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.tcp.read(buf)
    }
}

impl Write for DeadlineSocket<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the write deadline passed",
            ));
        }
        // A zero timeout means "none" to the OS, so never go below 1 ms.
        self.tcp
            .set_write_timeout(Some(left.max(Duration::from_millis(1))))?;
        // On macOS and the BSDs the timeout restarts each time the peer frees some
        // buffer space, so one large write to a trickle-reading peer can block far past
        // it. Capping each write bounds the overshoot to the time the peer takes to
        // read this much; Linux bounds the whole write by the timeout anyway.
        let len = buf.len().min(MAX_DEADLINE_WRITE);
        self.tcp.write(&buf[..len])
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}

/// Connects to `url`'s host and port: plain TCP for `rtmp://`, TLS for `rtmps://`.
/// Nagle's algorithm is off either way.
///
/// For TLS, the server name sent (SNI) and checked against the certificate is the URL's
/// host, and the certificate must chain to a root in the operating system's trust
/// store (loaded on each call). The TLS handshake completes before this returns, so a
/// certificate problem is reported here and not on the first RTMP message.
///
/// `timeout` bounds each TCP connection attempt (one per resolved address) and, for
/// TLS, each read and write of the handshake. The returned transport has no read or
/// write timeout; set them with [`Transport::set_read_timeout`] and
/// [`Transport::set_write_timeout`] if the session must not block indefinitely on a
/// stalled server.
///
/// # Errors
/// [`RtmpError::Io`] when name resolution or every connection attempt fails, or the
/// handshake times out; [`RtmpError::NoRootCertificates`] when the operating system's
/// trust store yields no usable root; [`RtmpError::InvalidUrl`] when the host is not a
/// valid TLS server name; [`RtmpError::Tls`] when the TLS handshake fails, for example
/// because the certificate does not match the host or chains to no trusted root.
pub fn connect(url: &RtmpUrl, timeout: Option<Duration>) -> Result<Transport, RtmpError> {
    match url.scheme() {
        Scheme::Rtmp => connect_tcp(url, timeout).map(|tcp| Transport::new(Link::Tcp(tcp))),
        Scheme::Rtmps => connect_tls(url, timeout, client_config(native_roots()?)?),
    }
}

/// A rustls client configuration on the `ring` provider, with its default (safe)
/// protocol versions, TLS 1.3 and 1.2, and no client certificate.
pub(crate) fn client_config(roots: RootCertStore) -> Result<Arc<ClientConfig>, RtmpError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(RtmpError::Tls)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// The roots in the operating system's trust store. Certificates that fail to parse are
/// skipped, as are files the store couldn't read, as long as at least one root remains.
///
/// Both are logged at `warn` (#67): each load error, and how many certificates could not
/// be parsed. An incomplete store otherwise shows up only later, as an `UnknownIssuer`
/// handshake failure with no hint why.
fn native_roots() -> Result<RootCertStore, RtmpError> {
    let loaded = rustls_native_certs::load_native_certs();
    for error in &loaded.errors {
        tracing::warn!(error = %error, "could not load OS root certificates");
    }
    let mut roots = RootCertStore::empty();
    let (added, ignored) = roots.add_parsable_certificates(loaded.certs);
    if ignored > 0 {
        tracing::warn!(
            ignored,
            added,
            "skipped OS root certificates that could not be parsed"
        );
    }
    tracing::debug!(added, "loaded OS root certificates");
    if roots.is_empty() {
        return Err(RtmpError::NoRootCertificates);
    }
    Ok(roots)
}

/// Opens TCP to `url`, then runs the TLS handshake with `config`, naming `url`'s host.
pub(crate) fn connect_tls(
    url: &RtmpUrl,
    timeout: Option<Duration>,
    config: Arc<ClientConfig>,
) -> Result<Transport, RtmpError> {
    let name = ServerName::try_from(url.host().to_owned()).map_err(|_| RtmpError::InvalidUrl {
        reason: "host is not a valid TLS server name",
    })?;
    let mut tcp = connect_tcp(url, timeout)?;
    let io = |e| RtmpError::io("starting TLS", e);
    tcp.set_read_timeout(timeout).map_err(io)?;
    tcp.set_write_timeout(timeout).map_err(io)?;
    let mut conn = ClientConnection::new(config, name).map_err(RtmpError::Tls)?;
    while conn.is_handshaking() {
        conn.complete_io(&mut tcp).map_err(tls_error)?;
    }
    tcp.set_read_timeout(None).map_err(io)?;
    tcp.set_write_timeout(None).map_err(io)?;
    Ok(Transport::new(Link::Tls(Box::new(StreamOwned::new(
        conn, tcp,
    )))))
}

/// A handshake I/O error: the rustls error inside it when there is one (a certificate
/// or protocol failure), otherwise the transport error.
fn tls_error(e: io::Error) -> RtmpError {
    match e
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
    {
        Some(tls) => RtmpError::Tls(tls.clone()),
        None => RtmpError::io("TLS handshake", e),
    }
}

/// Opens a TCP connection to `url`'s host and port, with Nagle's algorithm off.
fn connect_tcp(url: &RtmpUrl, timeout: Option<Duration>) -> Result<TcpStream, RtmpError> {
    let io = |e| RtmpError::io("connecting", e);
    let mut last = io::Error::new(io::ErrorKind::NotFound, "host resolved to no address");
    for addr in (url.host(), url.port()).to_socket_addrs().map_err(io)? {
        let attempt = match timeout {
            Some(t) => TcpStream::connect_timeout(&addr, t),
            None => TcpStream::connect(addr),
        };
        match attempt {
            Ok(stream) => {
                stream.set_nodelay(true).map_err(io)?;
                return Ok(stream);
            }
            Err(e) => last = e,
        }
    }
    Err(io(last))
}

#[cfg(test)]
mod tests;
