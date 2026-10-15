//! [`HttpsTransport`]: the real [`HttpTransport`], a small blocking HTTP/1.1 client over
//! rustls with the `ring` provider (docs/RULES.md#R-DEP-006).
//!
//! It sends one request per connection (`Connection: close`) and reads a response
//! framed by `Content-Length`, by chunked transfer coding, or by the end of the
//! connection. That is all Google's token endpoint and the Live API need, and it keeps
//! the crate free of an HTTP stack.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use crate::YoutubeError;
use crate::http::{HttpRequest, HttpResponse, HttpTransport, wipe};

/// The default bound on connecting and on each read and write.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The largest response body read. Live API pages are a few kilobytes.
pub const MAX_BODY_BYTES: usize = 4 << 20;

/// The largest status line plus headers read.
const MAX_HEAD_BYTES: usize = 64 << 10;

/// HTTPS over rustls, certificates checked against the operating system's trust store
/// (or the roots given to [`with_roots`](Self::with_roots)).
#[derive(Clone)]
pub struct HttpsTransport {
    config: Arc<ClientConfig>,
    timeout: Duration,
}

impl std::fmt::Debug for HttpsTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpsTransport")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl HttpsTransport {
    /// A transport that trusts the roots in the operating system's store.
    ///
    /// # Errors
    /// [`YoutubeError::Transport`] when the store yields no usable root, or rustls
    /// refuses the configuration.
    pub fn new() -> Result<Self, YoutubeError> {
        Self::with_roots(native_roots()?)
    }

    /// A transport that trusts only `roots`.
    ///
    /// # Errors
    /// [`YoutubeError::Transport`] when rustls refuses the configuration.
    pub fn with_roots(roots: RootCertStore) -> Result<Self, YoutubeError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| YoutubeError::transport("configuring TLS", io::Error::other(e)))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self {
            config: Arc::new(config),
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// The same transport with `timeout` bounding each connection attempt, read and
    /// write instead of [`DEFAULT_TIMEOUT`].
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl HttpTransport for HttpsTransport {
    fn send(&self, request: HttpRequest<'_>) -> Result<HttpResponse, YoutubeError> {
        let target = Target::parse(&request.url)?;
        tracing::debug!(
            endpoint = request.endpoint,
            method = request.method.as_str(),
            host = target.host,
            "YouTube request"
        );
        let name = ServerName::try_from(target.host.to_owned()).map_err(|_| {
            YoutubeError::InvalidArgument {
                reason: "the URL's host is not a valid TLS server name",
            }
        })?;
        let tcp = connect_tcp(target.host, target.port, self.timeout)?;
        let conn = ClientConnection::new(self.config.clone(), name)
            .map_err(|e| YoutubeError::transport("starting TLS", io::Error::other(e)))?;
        let mut stream = StreamOwned::new(conn, tcp);

        let mut head = write_head(&request, &target);
        let written = stream
            .write_all(&head)
            .and_then(|()| stream.write_all(request.body.bytes()))
            .and_then(|()| stream.flush());
        wipe(&mut head);
        written.map_err(|e| YoutubeError::transport("sending the request", e))?;

        let response = read_response(&mut BufReader::new(&mut stream));
        stream.conn.send_close_notify();
        // The response is complete; a failed close_notify changes nothing.
        let _ = stream.flush();
        response
    }
}

/// The parts of an `https://host[:port]/path?query` URL a request needs.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Target<'u> {
    pub(crate) host: &'u str,
    pub(crate) port: u16,
    /// The path and query, starting with `/`.
    pub(crate) path: &'u str,
}

impl<'u> Target<'u> {
    pub(crate) fn parse(url: &'u str) -> Result<Self, YoutubeError> {
        let bad = |reason| YoutubeError::InvalidArgument { reason };
        let rest = url
            .strip_prefix("https://")
            .ok_or(bad("only https:// URLs are sent"))?;
        let (authority, path) = match rest.find(['/', '?']) {
            Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], &rest[i..]),
            Some(_) => return Err(bad("the URL has no path")),
            None => (rest, "/"),
        };
        if authority.contains('@') {
            return Err(bad("the URL holds user information"));
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (
                host,
                port.parse().map_err(|_| bad("the URL's port is invalid"))?,
            ),
            None => (authority, 443),
        };
        if host.is_empty() || path.contains(['\r', '\n', ' ']) {
            return Err(bad("the URL is malformed"));
        }
        Ok(Self { host, port, path })
    }
}

/// The request line and headers. The buffer holds the bearer token, so the caller wipes
/// it once it is written.
fn write_head(request: &HttpRequest<'_>, target: &Target<'_>) -> Vec<u8> {
    let mut head = Vec::with_capacity(512);
    let host = if target.port == 443 {
        target.host.to_owned()
    } else {
        format!("{}:{}", target.host, target.port)
    };
    let _ = write!(
        head,
        "{} {} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: jstream-youtube\r\n\
         Accept: application/json\r\nAccept-Encoding: identity\r\nConnection: close\r\n",
        request.method.as_str(),
        target.path,
    );
    if let Some(token) = request.bearer {
        head.extend_from_slice(b"Authorization: Bearer ");
        head.extend_from_slice(token.expose().as_bytes());
        head.extend_from_slice(b"\r\n");
    }
    if let Some(content_type) = request.body.content_type() {
        let _ = write!(head, "Content-Type: {content_type}\r\n");
    }
    let _ = write!(
        head,
        "Content-Length: {}\r\n\r\n",
        request.body.bytes().len()
    );
    head
}

/// Reads one HTTP/1.1 response: the status line, the headers, and a body framed by
/// chunked coding, `Content-Length`, or the end of the stream.
pub(crate) fn read_response(reader: &mut impl BufRead) -> Result<HttpResponse, YoutubeError> {
    let io = |e| YoutubeError::transport("reading the response", e);
    let bad = |what: &str| io(io::Error::new(io::ErrorKind::InvalidData, what.to_owned()));

    let mut head_bytes = 0;
    let mut line = || -> Result<String, YoutubeError> {
        let mut buf = Vec::new();
        let limit = (MAX_HEAD_BYTES - head_bytes) as u64;
        reader
            .by_ref()
            .take(limit)
            .read_until(b'\n', &mut buf)
            .map_err(io)?;
        head_bytes += buf.len();
        if !buf.ends_with(b"\n") {
            return Err(bad("the response head is truncated or too long"));
        }
        let text = String::from_utf8(buf).map_err(|_| bad("the response head isn't UTF-8"))?;
        Ok(text.trim_end_matches(['\r', '\n']).to_owned())
    };

    let status_line = line()?;
    let mut parts = status_line.splitn(3, ' ');
    let status: u16 = match (parts.next(), parts.next()) {
        (Some(version), Some(code)) if version.starts_with("HTTP/1.") => code
            .parse()
            .ok()
            .filter(|c| (100..=599).contains(c))
            .ok_or_else(|| bad("the status code is invalid"))?,
        _ => return Err(bad("the status line is malformed")),
    };

    let mut content_length = None;
    let mut chunked = false;
    loop {
        let header = line()?;
        if header.is_empty() {
            break;
        }
        let (name, value) = header
            .split_once(':')
            .ok_or_else(|| bad("a header line is malformed"))?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            let n: usize = value
                .parse()
                .map_err(|_| bad("Content-Length is invalid"))?;
            content_length = Some(n);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value
                .rsplit(',')
                .next()
                .is_some_and(|last| last.trim().eq_ignore_ascii_case("chunked"));
            if !chunked {
                return Err(bad("the transfer coding is not supported"));
            }
        }
    }

    let body = if status == 204 || status == 304 || (100..200).contains(&status) {
        Vec::new()
    } else if chunked {
        read_chunked(reader)?
    } else if let Some(n) = content_length {
        if n > MAX_BODY_BYTES {
            return Err(bad("the response body is too large"));
        }
        let mut body = vec![0; n];
        reader.read_exact(&mut body).map_err(io)?;
        body
    } else {
        read_to_end(reader)?
    };
    Ok(HttpResponse::new(status, body))
}

/// A chunked body, up to [`MAX_BODY_BYTES`]. Trailers are read and ignored.
fn read_chunked(reader: &mut impl BufRead) -> Result<Vec<u8>, YoutubeError> {
    let io = |e| YoutubeError::transport("reading the response", e);
    let bad = |what: &str| io(io::Error::new(io::ErrorKind::InvalidData, what.to_owned()));
    let mut body = Vec::new();
    let mut size_line = Vec::new();
    loop {
        size_line.clear();
        reader
            .by_ref()
            .take(1024)
            .read_until(b'\n', &mut size_line)
            .map_err(io)?;
        let text = std::str::from_utf8(&size_line).map_err(|_| bad("a chunk size is invalid"))?;
        let size = text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size, 16).map_err(|_| bad("a chunk size is invalid"))?;
        if size == 0 {
            // Trailer lines, then the blank line that ends the message.
            loop {
                size_line.clear();
                let n = reader
                    .by_ref()
                    .take(1024)
                    .read_until(b'\n', &mut size_line)
                    .map_err(io)?;
                if n == 0 || size_line == b"\r\n" || size_line == b"\n" {
                    return Ok(body);
                }
            }
        }
        if size > MAX_BODY_BYTES - body.len() {
            wipe(&mut body);
            return Err(bad("the response body is too large"));
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader.read_exact(&mut body[start..]).map_err(io)?;
        let mut crlf = [0; 2];
        reader.read_exact(&mut crlf).map_err(io)?;
        if crlf != *b"\r\n" {
            wipe(&mut body);
            return Err(bad("a chunk is not followed by CRLF"));
        }
    }
}

/// A body that runs to the end of the connection, up to [`MAX_BODY_BYTES`]. A peer that
/// closes without TLS `close_notify` ends the body too: the framing is the close itself.
fn read_to_end(reader: &mut impl BufRead) -> Result<Vec<u8>, YoutubeError> {
    let mut body = Vec::new();
    let read = reader
        .by_ref()
        .take(MAX_BODY_BYTES as u64 + 1)
        .read_to_end(&mut body);
    match read {
        Ok(_) | Err(_) if body.len() > MAX_BODY_BYTES => {
            wipe(&mut body);
            Err(YoutubeError::transport(
                "reading the response",
                io::Error::new(io::ErrorKind::InvalidData, "the response body is too large"),
            ))
        }
        Ok(_) => Ok(body),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(body),
        Err(e) => {
            wipe(&mut body);
            Err(YoutubeError::transport("reading the response", e))
        }
    }
}

/// Connects to `host:port`, trying each resolved address in turn.
fn connect_tcp(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, YoutubeError> {
    let io = |e| YoutubeError::transport("connecting", e);
    let mut last = io::Error::new(io::ErrorKind::NotFound, "host resolved to no address");
    for addr in (host, port).to_socket_addrs().map_err(io)? {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(stream) => {
                stream.set_read_timeout(Some(timeout)).map_err(io)?;
                stream.set_write_timeout(Some(timeout)).map_err(io)?;
                stream.set_nodelay(true).map_err(io)?;
                return Ok(stream);
            }
            Err(e) => last = e,
        }
    }
    Err(io(last))
}

/// The roots in the operating system's trust store, skipping certificates that fail to
/// parse as long as one remains. Load errors are logged at `warn`, as the RTMPS client
/// does.
fn native_roots() -> Result<RootCertStore, YoutubeError> {
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
    if roots.is_empty() {
        return Err(YoutubeError::transport(
            "loading root certificates",
            io::Error::new(
                io::ErrorKind::NotFound,
                "the OS trust store has no usable root",
            ),
        ));
    }
    Ok(roots)
}

#[cfg(test)]
mod tests;
