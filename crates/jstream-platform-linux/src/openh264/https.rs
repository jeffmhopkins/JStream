//! [`HttpsFetch`]: the real [`Fetch`], one blocking `GET` over rustls with the `ring`
//! provider (R-DEP-006), certificates checked against the operating system's trust store.
//!
//! It is all the OpenH264 download needs: one request per connection, a response framed
//! by `Content-Length`, chunked coding or the end of the connection, and no redirects. A
//! redirect would mean fetching from somewhere other than the pinned address on Cisco's
//! server, so it is refused. The body's integrity doesn't rest on this client: the store
//! checks its SHA-256 against the pin.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use jstream_platform::PlatformError;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use super::Fetch;

/// The bound on connecting and on each read and write.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The largest status line plus headers read.
const MAX_HEAD_BYTES: usize = 64 << 10;

/// HTTPS `GET` over rustls.
#[derive(Clone)]
pub struct HttpsFetch {
    config: Arc<ClientConfig>,
}

impl std::fmt::Debug for HttpsFetch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpsFetch").finish_non_exhaustive()
    }
}

fn os(call: &'static str, e: impl std::fmt::Display) -> PlatformError {
    PlatformError::Os {
        call,
        code: None,
        message: e.to_string(),
    }
}

impl HttpsFetch {
    /// A client that trusts the roots in the operating system's store.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the store yields no usable root, or rustls refuses the
    /// configuration.
    pub fn new() -> Result<Self, PlatformError> {
        let loaded = rustls_native_certs::load_native_certs();
        let mut roots = RootCertStore::empty();
        roots.add_parsable_certificates(loaded.certs);
        if roots.is_empty() {
            return Err(os(
                "loading root certificates",
                "the OS trust store has no usable root",
            ));
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| os("configuring TLS", e))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self {
            config: Arc::new(config),
        })
    }
}

impl Fetch for HttpsFetch {
    fn get(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>, PlatformError> {
        let (host, path) = parse(url)?;
        let name =
            ServerName::try_from(host.to_owned()).map_err(|e| os("downloading OpenH264", e))?;
        let tcp = connect(host)?;
        let conn =
            ClientConnection::new(self.config.clone(), name).map_err(|e| os("starting TLS", e))?;
        let mut stream = StreamOwned::new(conn, tcp);
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: JStream\r\n\
             Accept-Encoding: identity\r\nConnection: close\r\n\r\n"
        );
        stream
            .write_all(head.as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|e| os("sending the OpenH264 request", e))?;
        let body = read_response(&mut BufReader::new(&mut stream), max_bytes);
        stream.conn.send_close_notify();
        // The response is read; a failed close_notify changes nothing.
        let _ = stream.flush();
        body
    }
}

/// The host and path of an `https://host/path` URL. No port, user information or query:
/// the pinned address has none.
fn parse(url: &str) -> Result<(&str, &str), PlatformError> {
    let bad = || {
        os(
            "downloading OpenH264",
            format!("not a plain https:// URL: {url}"),
        )
    };
    let rest = url.strip_prefix("https://").ok_or_else(bad)?;
    let slash = rest.find('/').ok_or_else(bad)?;
    let (host, path) = rest.split_at(slash);
    if host.is_empty() || host.contains([':', '@']) || path.contains(['?', '#', ' ', '\r', '\n']) {
        return Err(bad());
    }
    Ok((host, path))
}

fn connect(host: &str) -> Result<TcpStream, PlatformError> {
    let io = |e: io::Error| os("connecting to Cisco's server", e);
    let mut last = io::Error::new(io::ErrorKind::NotFound, "the host resolved to no address");
    for addr in (host, 443).to_socket_addrs().map_err(io)? {
        match TcpStream::connect_timeout(&addr, TIMEOUT) {
            Ok(stream) => {
                stream.set_read_timeout(Some(TIMEOUT)).map_err(io)?;
                stream.set_write_timeout(Some(TIMEOUT)).map_err(io)?;
                return Ok(stream);
            }
            Err(e) => last = e,
        }
    }
    Err(io(last))
}

/// Reads one HTTP/1.1 response and returns its body, which must come with status 200 and
/// be at most `max_bytes`.
pub(super) fn read_response(
    reader: &mut impl BufRead,
    max_bytes: usize,
) -> Result<Vec<u8>, PlatformError> {
    let call = "reading the OpenH264 download";
    let mut head_bytes = 0;
    let mut line = || -> Result<String, PlatformError> {
        let mut buf = Vec::new();
        reader
            .by_ref()
            .take((MAX_HEAD_BYTES - head_bytes) as u64)
            .read_until(b'\n', &mut buf)
            .map_err(|e| os(call, e))?;
        head_bytes += buf.len();
        if !buf.ends_with(b"\n") {
            return Err(os(call, "the response head is truncated or too long"));
        }
        let text = String::from_utf8(buf).map_err(|_| os(call, "the response head isn't UTF-8"))?;
        Ok(text.trim_end_matches(['\r', '\n']).to_owned())
    };

    let status_line = line()?;
    let mut parts = status_line.splitn(3, ' ');
    let status = match (parts.next(), parts.next()) {
        (Some(version), Some(code)) if version.starts_with("HTTP/1.") => code,
        _ => return Err(os(call, "the status line is malformed")),
    };
    let mut content_length = None;
    let mut chunked = false;
    let mut location = None;
    loop {
        let header = line()?;
        if header.is_empty() {
            break;
        }
        let (name, value) = header
            .split_once(':')
            .ok_or_else(|| os(call, "a header line is malformed"))?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| os(call, "Content-Length is invalid"))?,
            );
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.eq_ignore_ascii_case("chunked");
            if !chunked {
                return Err(os(call, "the transfer coding is not supported"));
            }
        } else if name.eq_ignore_ascii_case("location") {
            location = Some(value.to_owned());
        }
    }
    if status != "200" {
        let message = match location {
            Some(to) if status.starts_with('3') => format!(
                "Cisco's server answered {status}, redirecting to {to}; JStream downloads \
                 only from the pinned address"
            ),
            _ => format!("Cisco's server answered {status}"),
        };
        return Err(os(call, message));
    }
    let too_large = || {
        os(
            call,
            format!("the download is larger than {max_bytes} bytes"),
        )
    };
    if chunked {
        let mut body = Vec::new();
        loop {
            let size_line = line_bounded(reader, call)?;
            let size = size_line.split(';').next().unwrap_or_default().trim();
            let size =
                usize::from_str_radix(size, 16).map_err(|_| os(call, "a chunk size is invalid"))?;
            if size == 0 {
                // Trailers, then the blank line that ends the message.
                while !line_bounded(reader, call)?.is_empty() {}
                return Ok(body);
            }
            if size > max_bytes - body.len() {
                return Err(too_large());
            }
            let start = body.len();
            body.resize(start + size, 0);
            let mut crlf = [0; 2];
            reader
                .read_exact(&mut body[start..])
                .and_then(|()| reader.read_exact(&mut crlf))
                .map_err(|e| os(call, e))?;
            if crlf != *b"\r\n" {
                return Err(os(call, "a chunk is not followed by CRLF"));
            }
        }
    } else if let Some(n) = content_length {
        if n > max_bytes {
            return Err(too_large());
        }
        let mut body = vec![0; n];
        reader.read_exact(&mut body).map_err(|e| os(call, e))?;
        Ok(body)
    } else {
        // Framed by the close. A cut-short body fails the SHA-256 check.
        let mut body = Vec::new();
        let read = reader
            .by_ref()
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut body);
        if body.len() > max_bytes {
            return Err(too_large());
        }
        match read {
            Ok(_) => Ok(body),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(body),
            Err(e) => Err(os(call, e)),
        }
    }
}

/// One line of at most 1 KiB, without its line ending.
fn line_bounded(reader: &mut impl BufRead, call: &'static str) -> Result<String, PlatformError> {
    let mut buf = Vec::new();
    reader
        .by_ref()
        .take(1024)
        .read_until(b'\n', &mut buf)
        .map_err(|e| os(call, e))?;
    if !buf.ends_with(b"\n") {
        return Err(os(call, "a chunk line is truncated or too long"));
    }
    let text = std::str::from_utf8(&buf).map_err(|_| os(call, "a chunk line isn't UTF-8"))?;
    Ok(text.trim_end_matches(['\r', '\n']).to_owned())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{parse, read_response};

    #[test]
    fn only_plain_https_urls() {
        assert_eq!(
            parse("https://ciscobinary.openh264.org/libopenh264-2.6.0-linux64.8.so.bz2")
                .expect("parses"),
            (
                "ciscobinary.openh264.org",
                "/libopenh264-2.6.0-linux64.8.so.bz2"
            )
        );
        for bad in [
            "http://ciscobinary.openh264.org/x.bz2",
            "https://ciscobinary.openh264.org",
            "https://user@host/x",
            "https://host:8443/x",
            "https://host/x?y",
            "https:///x",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn bodies_by_length_chunks_or_close() {
        let read = |s: &[u8], max| read_response(&mut Cursor::new(s.to_vec()), max);
        assert_eq!(
            read(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabcdef", 10).expect("body"),
            b"abc"
        );
        assert_eq!(
            read(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nab\r\n1;x\r\nc\r\n0\r\n\r\n",
                10
            )
            .expect("body"),
            b"abc"
        );
        assert_eq!(
            read(b"HTTP/1.0 200 OK\r\n\r\nabc", 10).expect("body"),
            b"abc"
        );
    }

    #[test]
    fn refusals() {
        let read = |s: &[u8], max| read_response(&mut Cursor::new(s.to_vec()), max);
        // Too large, however it is framed.
        assert!(read(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n", 10).is_err());
        assert!(
            read(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nb\r\n",
                10
            )
            .is_err()
        );
        assert!(read(b"HTTP/1.1 200 OK\r\n\r\n01234567890", 10).is_err());
        // Not 200, a redirect included.
        let err = read(
            b"HTTP/1.1 302 Found\r\nLocation: https://elsewhere/x\r\n\r\n",
            10,
        )
        .expect_err("redirect");
        assert!(
            err.to_string().contains("only from the pinned address"),
            "{err}"
        );
        assert!(read(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n", 10).is_err());
        // Truncated.
        assert!(read(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nab", 10).is_err());
        assert!(read(b"HTTP/1.1 200 OK\r\n", 10).is_err());
        assert!(read(b"garbage\r\n\r\n", 10).is_err());
    }
}
