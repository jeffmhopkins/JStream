//! The web UI's files: a built bundle (`trunk build`'s `dist` directory) served from
//! [`ServerConfig::ui_dir`](crate::ServerConfig::ui_dir) at every path the API doesn't
//! use, behind the same `Host`, `Origin` and token checks (R-SEC-002).
//!
//! - **Credentials.** A browser can't attach a bearer token to a page load, so a refused
//!   token is answered with a `Basic` challenge: the browser asks for a user name and
//!   password, and the token is the password (the user name is ignored). The browser
//!   then sends those credentials with every request to this origin, the UI's WebSocket
//!   upgrade included, so the UI itself never handles the token. On a TLS listener the
//!   refusal's body is a short page linking to the pairing page ([`PAIR_PAGE_PATH`]),
//!   where a browser pairs into the device cookie instead (ADR-0047); a request the
//!   cookie admitted, when its use renews the device's lapse, gets the cookie re-set.
//! - **Paths.** The path is percent-decoded and split into segments. A segment that is
//!   empty, `.` or `..`, or holds `\`, `:` or NUL is refused (404), and the file found is
//!   canonicalized and must still be inside the canonical root, so neither `..` nor a
//!   symbolic link reaches outside it. A directory serves its `index.html`. A missing
//!   path whose last segment has no `.` serves the root's `index.html`, for the UI's own
//!   routes; any other missing path is 404. Paths under `/api` and `/mcp` belong to the
//!   API and are 404 here (with MCP off, or a version the server doesn't serve), and so
//!   are paths under `/pair`, which is reserved for the pairing page.
//! - **Headers.** `Content-Type` from the extension ([`mime`]), `X-Content-Type-Options:
//!   nosniff`, `Cache-Control: no-cache`, `Referrer-Policy: no-referrer`, and a
//!   `Content-Security-Policy` ([`csp`]). In an HTML file, trunk's nonce placeholder
//!   ([`NONCE_PLACEHOLDER`]) is replaced by a fresh random nonce, which the policy names.
//! - **Methods.** `GET` and `HEAD`; any other is 405 with `Allow: GET, HEAD`.

use crate::App;
use crate::error::ServerError;
use crate::gate;
use crate::listener::Accepted;
use crate::pair_page::PAIR_PAGE_PATH;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What trunk writes in `nonce` attributes with `create_nonce = true`, to be replaced by
/// the server with a fresh nonce on every response.
pub(crate) const NONCE_PLACEHOLDER: &str = "{{__TRUNK NONCE__}}";

/// The realm of the `Basic` challenge, which browsers show in their sign-in prompt.
const CHALLENGE: &str = "Basic realm=\"JStream\", charset=\"UTF-8\"";

/// The `401` body on a TLS listener: a page that links to the pairing page.
const PAIR_LINK_PAGE: &str = "<!doctype html>\n<meta charset=\"utf-8\">\n\
<title>JStream</title>\n\
<p>A paired browser or the control token is required. \
Enter the control token as the password, or \
<a href=\"/pair\">pair this browser</a> with a code from the host.</p>\n";

/// The `Content-Security-Policy` of [`PAIR_LINK_PAGE`]: nothing loads, nothing runs.
const PAIR_LINK_POLICY: &str =
    "default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// The largest file served, in bytes. A UI bundle is a few MiB.
const MAX_FILE_BYTES: u64 = 64 << 20;

/// The UI's root directory, canonicalized when the server binds.
#[derive(Debug)]
pub(crate) struct Ui {
    root: PathBuf,
}

impl Ui {
    /// The UI served from `dir`, which must be a directory.
    pub(crate) fn new(dir: &Path) -> Result<Ui, ServerError> {
        let invalid = |source| ServerError::UiDir {
            dir: dir.to_path_buf(),
            source,
        };
        let root = dir.canonicalize().map_err(invalid)?;
        if !root.is_dir() {
            return Err(invalid(io::Error::new(
                io::ErrorKind::NotADirectory,
                "not a directory",
            )));
        }
        Ok(Ui { root })
    }

    /// The file a request path names, or `None` when there is none to serve.
    fn resolve(&self, path: &str) -> Option<PathBuf> {
        let segments = segments(path)?;
        let reserved = PAIR_PAGE_PATH.trim_start_matches('/');
        if matches!(segments.first().map(String::as_str), Some(first) if first == "api" || first == "mcp" || first == reserved)
        {
            return None;
        }
        let mut candidate = self.root.clone();
        candidate.extend(&segments);
        match self.inside(&candidate) {
            Some(found) if found.is_dir() => self.inside(&found.join("index.html")),
            Some(found) => Some(found),
            None => {
                let last = segments.last().map(String::as_str).unwrap_or_default();
                if last.contains('.') {
                    None
                } else {
                    self.inside(&self.root.join("index.html"))
                }
            }
        }
    }

    /// `path` canonicalized, if it exists and is inside the root.
    fn inside(&self, path: &Path) -> Option<PathBuf> {
        let found = path.canonicalize().ok()?;
        found.starts_with(&self.root).then_some(found)
    }
}

/// A request path's percent-decoded segments, or `None` if one may not name a file
/// below the root: empty (`//`, or a trailing `/` other than the root's), `.` or `..`,
/// or holding `\`, `:` or NUL, or not UTF-8 once decoded.
fn segments(path: &str) -> Option<Vec<String>> {
    let path = path.strip_prefix('/')?;
    if path.is_empty() {
        return Some(Vec::new());
    }
    let path = path.strip_suffix('/').unwrap_or(path);
    path.split('/')
        .map(|raw| {
            let segment = percent_decode(raw)?;
            let refused = segment.is_empty()
                || segment == "."
                || segment == ".."
                || segment.contains(['\\', ':', '\0', '/']);
            (!refused).then_some(segment)
        })
        .collect()
}

/// Decodes `%XX` escapes; `None` for a broken escape or a result that isn't UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The `Content-Type` for a file name's extension; `application/octet-stream` for one
/// not listed. `application/wasm` matters: browsers compile a module while it streams
/// only with that type.
pub(crate) fn mime(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match extension.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The `Content-Security-Policy` of every UI response, and of the pairing page. Scripts,
/// styles, fetches and WebSockets come only from this origin (the WebSocket named by
/// `host` too, `wss://` on a TLS listener, since older browsers don't let `'self'` match
/// `ws:`), WebAssembly may be compiled, and the page can't be framed, post forms, change
/// its base URL or embed plugins. With a `nonce`, scripts and styles carrying it may be
/// inline, as trunk's loader and the pairing page's script are.
pub(crate) fn csp(host: &str, nonce: Option<&str>, tls: bool) -> String {
    let nonce = nonce.map(|n| format!(" 'nonce-{n}'")).unwrap_or_default();
    let ws = if tls { "wss" } else { "ws" };
    format!(
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'{nonce}; \
         style-src 'self'{nonce}; img-src 'self' data: blob:; media-src 'self' blob:; \
         font-src 'self'; connect-src 'self' {ws}://{host}; worker-src 'self' blob:; \
         object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    )
}

/// Sets the headers every page the server serves carries: its `Content-Type`,
/// `X-Content-Type-Options: nosniff`, `Cache-Control: no-cache`,
/// `Referrer-Policy: no-referrer` and `policy` as its `Content-Security-Policy`.
pub(crate) fn set_page_headers(response: &mut Response, content_type: &'static str, policy: &str) {
    let set = response.headers_mut();
    set.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    set.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    set.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    set.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    if let Ok(policy) = HeaderValue::from_str(policy) {
        set.insert(header::CONTENT_SECURITY_POLICY, policy);
    }
}

/// A fresh nonce: 16 bytes from the OS's random source, as 32 hex digits.
pub(crate) fn nonce() -> Option<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Any path the API doesn't use, any method: checks the request (R-SEC-002), then
/// serves the file it names.
///
/// A request that passes the checks disarms its connection's header-read deadline, as
/// on `/mcp`, so a browser can fetch the bundle's files over one kept-alive connection.
pub(crate) async fn handle(
    State(app): State<Arc<App>>,
    ConnectInfo(accepted): ConnectInfo<Accepted>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let Some(ui) = &app.ui else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let credential = match app.gate.admit(&headers, accepted.peer) {
        Ok(credential) => credential,
        Err(refusal) if refusal.is_credential() => return challenge(app.tls && app.pairing),
        Err(refusal) => return refusal.into_response(),
    };
    accepted.request_read();
    if method != Method::GET && method != Method::HEAD {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "GET, HEAD")],
            "the UI is read-only: GET requests\n",
        )
            .into_response();
    }
    let Some(file) = ui.resolve(uri.path()) else {
        return (StatusCode::NOT_FOUND, "not found\n").into_response();
    };
    // The gate accepted `Host`, so it is one of the server's own names.
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let read = tokio::task::spawn_blocking(move || read(&file).map(|body| (file, body))).await;
    let (file, body) = match read {
        Ok(Ok(found)) => found,
        Ok(Err(error)) => {
            tracing::warn!(error = %error, "could not read a UI file");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not read the file\n",
            )
                .into_response();
        }
        Err(error) => {
            tracing::warn!(error = %error, "a UI file read failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let content_type = mime(&file);
    let (body, policy) = if content_type.starts_with("text/html") {
        let Some(nonce) = nonce() else {
            tracing::warn!("no random source for a CSP nonce");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        let body = match String::from_utf8(body) {
            Ok(text) => text.replace(NONCE_PLACEHOLDER, &nonce).into_bytes(),
            Err(error) => error.into_bytes(),
        };
        (body, csp(&host, Some(&nonce), app.tls))
    } else {
        (body, csp(&host, None, app.tls))
    };
    let mut response = body.into_response();
    set_page_headers(&mut response, content_type, &policy);
    gate::renew_cookie(&credential, &headers, &mut response);
    response
}

/// The answer to a UI path refused for a missing or refused credential: the `Basic`
/// challenge, and on a TLS listener a page that links to the pairing page.
fn challenge(tls: bool) -> Response {
    if !tls {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, CHALLENGE)],
            "the control token is required: enter it as the password\n",
        )
            .into_response();
    }
    let mut response = (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, CHALLENGE)],
        PAIR_LINK_PAGE,
    )
        .into_response();
    set_page_headers(&mut response, "text/html; charset=utf-8", PAIR_LINK_POLICY);
    response
}

/// Reads a whole file, refusing one over [`MAX_FILE_BYTES`].
fn read(file: &Path) -> io::Result<Vec<u8>> {
    let length = std::fs::metadata(file)?.len();
    if length > MAX_FILE_BYTES {
        return Err(io::Error::other("the file is too large to serve"));
    }
    std::fs::read(file)
}

#[cfg(test)]
mod tests {
    use super::{Ui, csp, mime, percent_decode, segments};
    use std::path::Path;

    #[test]
    fn segments_refuse_anything_that_could_leave_the_root() {
        assert_eq!(segments("/"), Some(vec![]));
        assert_eq!(
            segments("/assets/app.js"),
            Some(vec!["assets".into(), "app.js".into()])
        );
        assert_eq!(segments("/scenes/"), Some(vec!["scenes".into()]));
        assert_eq!(segments("/a%20b.css"), Some(vec!["a b.css".into()]));
        for refused in [
            "",
            "/..",
            "/a/../b",
            "/%2e%2e/secret",
            "/.",
            "//etc/passwd",
            "/a//b",
            "/a%2fb",
            "/..%5csecret",
            "/C:/Windows",
            "/a%00b",
            "/%zz",
            "/%ff",
        ] {
            assert_eq!(segments(refused), None, "{refused}");
        }
    }

    #[test]
    fn percent_decoding_is_strict() {
        assert_eq!(percent_decode("a%41%62").as_deref(), Some("aAb"));
        assert_eq!(percent_decode("%e2%9c%93").as_deref(), Some("\u{2713}"));
        assert_eq!(percent_decode("%4"), None);
        assert_eq!(percent_decode("%c3"), None);
    }

    #[test]
    fn types_follow_the_extension() {
        for (name, expected) in [
            ("index.html", "text/html; charset=utf-8"),
            ("app.JS", "text/javascript; charset=utf-8"),
            ("app_bg.wasm", "application/wasm"),
            ("style.css", "text/css; charset=utf-8"),
            ("icon.svg", "image/svg+xml"),
            ("font.woff2", "font/woff2"),
            ("data.bin", "application/octet-stream"),
            ("LICENSE", "application/octet-stream"),
        ] {
            assert_eq!(mime(Path::new(name)), expected, "{name}");
        }
    }

    #[test]
    fn the_policy_allows_only_this_origin_and_wasm() {
        let policy = csp("127.0.0.1:4455", Some("abc"), false);
        assert!(policy.starts_with("default-src 'self'; "), "{policy}");
        assert!(
            policy.contains("script-src 'self' 'wasm-unsafe-eval' 'nonce-abc';"),
            "{policy}"
        );
        assert!(policy.contains("style-src 'self' 'nonce-abc';"), "{policy}");
        assert!(
            policy.contains("connect-src 'self' ws://127.0.0.1:4455;"),
            "{policy}"
        );
        assert!(policy.contains("frame-ancestors 'none'"), "{policy}");
        assert!(!policy.contains("unsafe-inline"), "{policy}");
        assert!(!csp("localhost:1", None, false).contains("nonce"));
        assert!(
            csp("studio.lan:4455", None, true)
                .contains("connect-src 'self' wss://studio.lan:4455;")
        );
    }

    #[test]
    fn a_ui_dir_must_be_a_directory() {
        assert!(Ui::new(Path::new("/definitely/not/here")).is_err());
        let file = std::env::temp_dir().join(format!("jstream-ui-file-{}", std::process::id()));
        assert!(std::fs::write(&file, "x").is_ok());
        assert!(Ui::new(&file).is_err());
        let _ = std::fs::remove_file(&file);
    }
}
