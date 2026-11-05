//! The desktop app's check for a newer release (docs/security/threat-model.md#assets).
//!
//! Help › Check for updates, and the launch check when that setting is on, ask the page
//! for [`PATH`]. The page cannot choose an address: this answers with one `GET` of
//! GitHub's public releases list, or a short error, and nothing the page sent is
//! forwarded. The request carries no body, no token and no version. The user agent is
//! the word `JStream`. Pre-releases are included. GitHub returns the newest releases
//! first, so `per_page=20` is the recent page and stays under the body cap. The page
//! decides which is newer.
//!
//! Docs: apps/jstream-desktop/README.md#design

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use crate::ui::Reply;

/// The path the page fetches. The same path is named in `jstream-ui`'s `updates.rs`.
pub const PATH: &str = "/jstream-update-check";

/// GitHub's public releases list for this repository, pre-releases included.
/// Registered in `docs/external.toml` (`gh-releases-api`).
pub const RELEASES_URL: &str =
    "https://api.github.com/repos/jeffmhopkins/JStream/releases?per_page=20";

/// Sent as `User-Agent`. It does not include the running version.
const USER_AGENT: &str = "JStream";

/// How long connecting, and each read and write, may take.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The largest releases list kept. A page of releases is far smaller.
const MAX_BODY: usize = 1 << 20;

/// The largest status line plus headers read.
const MAX_HEAD: usize = 64 << 10;

const OFFLINE: &str = "Couldn't reach GitHub. Check the connection and try again.";
const UNREAD: &str = "GitHub's reply couldn't be read.";
const WAIT: &str = "GitHub asked us to wait before checking again.";

/// Whether `method` and `path` are the update check. Anything else is a file.
pub fn handles(method: &str, path: &str) -> bool {
    method == "GET" && path == PATH
}

/// The releases list, or `{"error": "…"}` when it couldn't be read. Always JSON.
pub fn reply() -> Reply {
    let body = match fetch() {
        Ok((status, body)) => answer(status, &body),
        Err(message) => {
            tracing::warn!(error = %message, "update check failed");
            error_json(&message)
        }
    };
    json_reply(body)
}

/// What the page is given for a GitHub response: the list when it is one, otherwise an
/// error and never the body (a rate limit, an HTML page, a redirect).
fn answer(status: u16, body: &[u8]) -> Vec<u8> {
    if status == 200 && is_json_array(body) && body.len() <= MAX_BODY {
        return body.to_vec();
    }
    let message = if status == 403 || status == 429 {
        WAIT
    } else {
        UNREAD
    };
    error_json(message)
}

fn is_json_array(body: &[u8]) -> bool {
    body.iter()
        .find(|byte| !byte.is_ascii_whitespace())
        .copied()
        == Some(b'[')
}

fn error_json(message: &str) -> Vec<u8> {
    debug_assert!(
        message
            .chars()
            .all(|c| c.is_ascii() && c != '"' && c != '\\' && !c.is_control()),
        "the error text is placed in JSON without escaping"
    );
    format!(r#"{{"error":"{message}"}}"#).into_bytes()
}

fn json_reply(body: Vec<u8>) -> Reply {
    Reply {
        status: 200,
        headers: vec![
            ("Content-Type", "application/json; charset=utf-8".to_owned()),
            ("X-Content-Type-Options", "nosniff".to_owned()),
            ("Cache-Control", "no-store".to_owned()),
            ("Content-Security-Policy", "default-src 'none'".to_owned()),
        ],
        body,
    }
}

/// `GET` [`RELEASES_URL`]. The error is text the page can show.
fn fetch() -> Result<(u16, Vec<u8>), String> {
    let (host, path) = target(RELEASES_URL).ok_or(UNREAD)?;
    let name = ServerName::try_from(host.to_owned()).map_err(|_| OFFLINE.to_owned())?;
    let tcp = connect(host)?;
    let config = tls_config()?;
    let conn = ClientConnection::new(config, name).map_err(|_| OFFLINE.to_owned())?;
    let mut stream = StreamOwned::new(conn, tcp);
    let head = request_bytes(host, &path);
    stream
        .write_all(&head)
        .and_then(|()| stream.flush())
        .map_err(|_| OFFLINE.to_owned())?;
    let response = read_response(&mut BufReader::new(&mut stream)).map_err(|message| {
        if message == UNREAD {
            UNREAD.to_owned()
        } else {
            OFFLINE.to_owned()
        }
    });
    stream.conn.send_close_notify();
    let _ = stream.flush();
    response
}

/// `host` and the path-and-query of an `https://host/path` URL with no port, user or
/// space. The path is returned with its leading `/`.
fn target(url: &str) -> Option<(&str, String)> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    if host.is_empty()
        || host.contains(['@', ':', ' ', '/', '\\', '?'])
        || path.contains([' ', '\r', '\n'])
    {
        return None;
    }
    Some((host, format!("/{path}")))
}

/// The request line and headers. There is no body.
fn request_bytes(host: &str, path: &str) -> Vec<u8> {
    format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         User-Agent: {USER_AGENT}\r\n\
         Accept: application/vnd.github+json\r\n\
         X-GitHub-Api-Version: 2022-11-28\r\n\
         Accept-Encoding: identity\r\n\
         Connection: close\r\n\
         \r\n"
    )
    .into_bytes()
}

fn tls_config() -> Result<Arc<ClientConfig>, String> {
    let loaded = rustls_native_certs::load_native_certs();
    for error in &loaded.errors {
        tracing::warn!(error = %error, "could not load an OS root certificate");
    }
    let mut roots = RootCertStore::empty();
    let (added, _) = roots.add_parsable_certificates(loaded.certs);
    if added == 0 {
        return Err(OFFLINE.to_owned());
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| OFFLINE.to_owned())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

fn connect(host: &str) -> Result<TcpStream, String> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "the host resolved to no address");
    let addrs = (host, 443_u16)
        .to_socket_addrs()
        .map_err(|_| OFFLINE.to_owned())?;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, TIMEOUT) {
            Ok(stream) => {
                stream
                    .set_read_timeout(Some(TIMEOUT))
                    .map_err(|_| OFFLINE.to_owned())?;
                stream
                    .set_write_timeout(Some(TIMEOUT))
                    .map_err(|_| OFFLINE.to_owned())?;
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(error) => last = error,
        }
    }
    let _ = last;
    Err(OFFLINE.to_owned())
}

/// One HTTP/1.1 response. `Err(UNREAD)` is a malformed message; any other error is a
/// broken connection, which the caller reports as offline.
fn read_response(reader: &mut impl BufRead) -> Result<(u16, Vec<u8>), &'static str> {
    let mut head_bytes = 0_usize;
    let mut line = || -> Result<String, &'static str> {
        let mut buf = Vec::new();
        let limit = MAX_HEAD.saturating_sub(head_bytes) as u64;
        reader
            .by_ref()
            .take(limit)
            .read_until(b'\n', &mut buf)
            .map_err(|_| OFFLINE)?;
        head_bytes = head_bytes.saturating_add(buf.len());
        if !buf.ends_with(b"\n") {
            return Err(UNREAD);
        }
        String::from_utf8(buf)
            .map(|text| text.trim_end_matches(['\r', '\n']).to_owned())
            .map_err(|_| UNREAD)
    };

    let status_line = line()?;
    let mut parts = status_line.splitn(3, ' ');
    let status = match (parts.next(), parts.next()) {
        (Some(version), Some(code)) if version.starts_with("HTTP/1.") => code
            .parse::<u16>()
            .ok()
            .filter(|code| (100..=599).contains(code))
            .ok_or(UNREAD)?,
        _ => return Err(UNREAD),
    };

    let mut content_length = None;
    let mut chunked = false;
    loop {
        let header = line()?;
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            return Err(UNREAD);
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(value.parse::<usize>().map_err(|_| UNREAD)?);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            let last = value.rsplit(',').next().unwrap_or("").trim();
            if last.eq_ignore_ascii_case("chunked") {
                chunked = true;
            } else if !last.is_empty() {
                return Err(UNREAD);
            }
        }
    }

    let body = if status == 204 || status == 304 || (100..200).contains(&status) {
        Vec::new()
    } else if chunked {
        read_chunked(reader)?
    } else if let Some(len) = content_length {
        if len > MAX_BODY {
            return Err(UNREAD);
        }
        let mut body = vec![0; len];
        reader.read_exact(&mut body).map_err(|_| OFFLINE)?;
        body
    } else {
        read_to_end(reader)?
    };
    Ok((status, body))
}

fn read_chunked(reader: &mut impl BufRead) -> Result<Vec<u8>, &'static str> {
    let mut body = Vec::new();
    let mut size_line = Vec::new();
    loop {
        size_line.clear();
        reader
            .by_ref()
            .take(1024)
            .read_until(b'\n', &mut size_line)
            .map_err(|_| OFFLINE)?;
        if !size_line.ends_with(b"\n") {
            return Err(UNREAD);
        }
        let text = std::str::from_utf8(&size_line).map_err(|_| UNREAD)?;
        let size_text = text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| UNREAD)?;
        if size == 0 {
            loop {
                size_line.clear();
                let n = reader
                    .by_ref()
                    .take(1024)
                    .read_until(b'\n', &mut size_line)
                    .map_err(|_| OFFLINE)?;
                if n == 0 || size_line == b"\r\n" || size_line == b"\n" {
                    return Ok(body);
                }
            }
        }
        if size > MAX_BODY.saturating_sub(body.len()) {
            return Err(UNREAD);
        }
        let start = body.len();
        body.resize(start + size, 0);
        let mut crlf = [0; 2];
        reader
            .read_exact(&mut body[start..])
            .and_then(|()| reader.read_exact(&mut crlf))
            .map_err(|_| OFFLINE)?;
        if crlf != *b"\r\n" {
            return Err(UNREAD);
        }
    }
}

fn read_to_end(reader: &mut impl BufRead) -> Result<Vec<u8>, &'static str> {
    let mut body = Vec::new();
    let read = reader
        .by_ref()
        .take(MAX_BODY as u64 + 1)
        .read_to_end(&mut body);
    if body.len() > MAX_BODY {
        return Err(UNREAD);
    }
    match read {
        Ok(_) => Ok(body),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(body),
        Err(_) => Err(OFFLINE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn only_the_check_path_is_the_update_check() {
        assert!(handles("GET", PATH));
        assert!(!handles("POST", PATH));
        assert!(!handles("GET", "/"));
        assert!(!handles("GET", "/jstream-update-check/extra"));
    }

    #[test]
    fn the_request_is_a_get_of_the_releases_list_and_names_no_version() {
        let (host, path) = target(RELEASES_URL).expect("the releases URL");
        assert_eq!(host, "api.github.com");
        assert_eq!(path, "/repos/jeffmhopkins/JStream/releases?per_page=20");
        let bytes = request_bytes(host, &path);
        let text = String::from_utf8(bytes).expect("ascii");
        assert!(
            text.starts_with("GET /repos/jeffmhopkins/JStream/releases?per_page=20 HTTP/1.1\r\n")
        );
        assert!(text.contains("User-Agent: JStream\r\n"));
        assert!(!text.contains("0.1.0"), "the running version is not sent");
        assert!(!text.contains("Authorization"));
        assert!(text.ends_with("\r\n\r\n"));
        assert_eq!(target("http://api.github.com/repos"), None);
        assert_eq!(target("https://user:pw@api.github.com/x"), None);
        assert_eq!(target("https://api.github.com"), None);
    }

    #[test]
    fn a_releases_list_is_passed_through_and_anything_else_is_an_error() {
        let list = br#"[{"tag_name":"v0.1.0-alpha.5","draft":false}]"#;
        assert_eq!(answer(200, list), list);
        let wait = answer(403, b"slow down");
        assert_eq!(
            std::str::from_utf8(&wait).expect("utf-8"),
            r#"{"error":"GitHub asked us to wait before checking again."}"#
        );
        assert_eq!(answer(429, b""), wait);
        let unread = answer(200, b"not a list");
        assert!(
            std::str::from_utf8(&unread)
                .expect("utf-8")
                .contains(UNREAD)
        );
        assert_eq!(answer(301, list), unread, "a redirect is not followed");
        assert_eq!(answer(500, b"<html>"), unread);
        for message in [OFFLINE, UNREAD, WAIT] {
            assert!(
                message
                    .chars()
                    .all(|c| c.is_ascii() && c != '"' && c != '\\' && !c.is_control())
            );
        }
    }

    #[test]
    fn responses_are_read_by_length_by_chunks_and_to_the_close() {
        let by_length = read_response(&mut Cursor::new(
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello extra",
        ))
        .expect("length");
        assert_eq!(by_length, (200, b"hello".to_vec()));

        let chunked = read_response(&mut Cursor::new(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
        ))
        .expect("chunked");
        assert_eq!(chunked, (200, b"hello".to_vec()));

        let closed = read_response(&mut Cursor::new("HTTP/1.1 200 OK\r\n\r\nbye")).expect("close");
        assert_eq!(closed, (200, b"bye".to_vec()));

        assert_eq!(
            read_response(&mut Cursor::new("not http\r\n\r\n")),
            Err(UNREAD)
        );
    }
}
