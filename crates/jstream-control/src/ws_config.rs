//! [`WsConfig`] and the pieces of the WebSocket handshake that both clients share: the
//! native one (`ws.rs`) and the browser one (`ws_web.rs`).
//!
//! Docs: crates/jstream-control/README.md#design, docs/adr/0006-mcp-full-access.md

use crate::error::ControlError;
use std::fmt;

/// Where and how to connect.
#[derive(Clone)]
pub struct WsConfig {
    /// The engine's WebSocket URL, `ws://<host>:<port>[/path]`. Without a path (or with
    /// `/`) the API path `/api/v1/ws` is used. The browser client also takes `wss://`.
    pub url: String,
    /// The control token: sent as `Authorization: Bearer <token>` by the native client,
    /// and as the `jstream.token.<token>` subprotocol by the browser client. The browser
    /// client sends nothing when it is empty, relying on credentials the browser already
    /// holds for the engine's origin.
    pub token: String,
    /// The `Origin` to send. `None` sends the engine's own origin (`http://<host>:<port>`
    /// of the URL), which the server always accepts. The browser client ignores it: the
    /// browser sets `Origin` to the page's own.
    pub origin: Option<String>,
}

impl WsConfig {
    /// A configuration for `url` with `token` and the default `Origin`.
    pub fn new(url: impl Into<String>, token: impl Into<String>) -> Self {
        WsConfig {
            url: url.into(),
            token: token.into(),
            origin: None,
        }
    }
}

impl fmt::Debug for WsConfig {
    /// Never prints the token.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WsConfig")
            .field("url", &self.url)
            .field("token", &"***")
            .field("origin", &self.origin)
            .finish()
    }
}

/// `Disconnected` with `reason`.
pub(crate) fn disconnected(reason: impl Into<String>) -> ControlError {
    ControlError::Disconnected {
        reason: reason.into(),
    }
}

/// The URL the browser client opens: `url` with the API path when it has none. Only
/// `ws://` and `wss://` URLs with a host are accepted; the browser checks the rest.
// Only the wasm32 client uses it; native builds compile it for the tests.
#[cfg(any(test, target_arch = "wasm32"))]
pub(crate) fn browser_url(url: &str) -> Result<String, ControlError> {
    let invalid = |why: &str| disconnected(format!("invalid engine URL `{url}`: {why}"));
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| invalid("the scheme must be `ws://` or `wss://`"))?;
    if !scheme.eq_ignore_ascii_case("ws") && !scheme.eq_ignore_ascii_case("wss") {
        return Err(invalid("the scheme must be `ws://` or `wss://`"));
    }
    let (authority, path) = match rest.find(['/', '?', '#']) {
        Some(at) => rest.split_at(at),
        None => (rest, ""),
    };
    if authority.is_empty() {
        return Err(invalid("it has no host"));
    }
    if path.starts_with('#') {
        return Err(invalid("a WebSocket URL can't have a fragment"));
    }
    let path = match path {
        "" | "/" => crate::WS_PATH,
        path if path.starts_with('?') => {
            return Ok(format!("{scheme}://{authority}{}{path}", crate::WS_PATH));
        }
        path => path,
    };
    Ok(format!("{scheme}://{authority}{path}"))
}

/// The subprotocol that carries `token`: `jstream.token.<token>`. A subprotocol must be
/// an HTTP token (RFC 6455 §4.1), so a token with other characters is refused; the
/// daemon's tokens are lowercase hex.
// Only the wasm32 client uses it; native builds compile it for the tests.
#[cfg(any(test, target_arch = "wasm32"))]
pub(crate) fn token_protocol(token: &str) -> Result<String, ControlError> {
    // RFC 7230 `tchar`.
    let tchar = |b: u8| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b);
    if token.is_empty() || !token.bytes().all(tchar) {
        return Err(disconnected(
            "the control token contains characters a subprotocol can't carry",
        ));
    }
    Ok(format!("{}{token}", crate::WS_TOKEN_PROTOCOL_PREFIX))
}

/// How the browser's socket ended: an `error` event or a `close` event.
// Only the wasm32 client uses it; native builds compile it for the tests.
#[cfg(any(test, target_arch = "wasm32"))]
#[derive(Clone, Copy, Debug)]
pub(crate) enum SocketEnd<'a> {
    /// An `error` event. Browsers usually follow it with `close`, but not always: when
    /// the page's Content Security Policy refuses the URL, Chromium fires `error`, moves
    /// the socket to `CLOSED` and sends no `close`, so `error` must end the session too.
    Error,
    /// A `close` event, with the reason the engine gave (empty when it gave none).
    Close { reason: &'a str },
}

/// Why the browser client's session ended, given how the socket ended and whether the
/// opening handshake was still in progress. While connecting, every ending reads as a
/// failed connect: a browser doesn't tell the page why, so the reason covers them all.
// Only the wasm32 client uses it; native builds compile it for the tests.
#[cfg(any(test, target_arch = "wasm32"))]
pub(crate) fn end_reason(url: &str, connecting: bool, end: SocketEnd<'_>) -> String {
    if connecting {
        return format!(
            "could not connect to the engine at `{url}`: it is unreachable, or it refused \
             the token or this page's origin (or the page's Content Security Policy blocked it)"
        );
    }
    match end {
        SocketEnd::Error => "the connection to the engine failed".to_owned(),
        SocketEnd::Close { reason: "" } => "the engine closed the connection".to_owned(),
        SocketEnd::Close { reason } => format!("the engine closed the connection: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{SocketEnd, WsConfig, browser_url, end_reason, token_protocol};
    use crate::WS_PROTOCOL;

    #[test]
    fn browser_urls_get_the_api_path_when_they_have_none() {
        for (url, want) in [
            ("ws://127.0.0.1:4455", "ws://127.0.0.1:4455/api/v1/ws"),
            ("ws://127.0.0.1:4455/", "ws://127.0.0.1:4455/api/v1/ws"),
            (
                "wss://studio.example:443",
                "wss://studio.example:443/api/v1/ws",
            ),
            ("ws://[::1]:9000/custom/ws", "ws://[::1]:9000/custom/ws"),
            ("ws://host:1?x=1", "ws://host:1/api/v1/ws?x=1"),
        ] {
            assert_eq!(browser_url(url).ok().as_deref(), Some(want), "{url}");
        }
    }

    #[test]
    fn browser_urls_need_a_websocket_scheme_and_a_host() {
        for url in [
            "http://host:1",
            "host:1",
            "ws://",
            "ws:///api",
            "ws://host#frag",
        ] {
            let error = browser_url(url).err().map(|e| e.to_string());
            assert!(
                error
                    .as_deref()
                    .is_some_and(|e| e.contains("invalid engine URL")),
                "{url}: {error:?}"
            );
        }
    }

    #[test]
    fn the_token_rides_in_its_own_subprotocol() {
        assert_eq!(
            token_protocol("0a1b2c").ok().as_deref(),
            Some("jstream.token.0a1b2c")
        );
        assert_ne!(token_protocol("0a").ok().as_deref(), Some(WS_PROTOCOL));
        for bad in ["", "a b", "a,b", "a/b", "é"] {
            assert!(token_protocol(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn debug_never_prints_the_token() {
        let config = WsConfig::new("ws://h:1", "secret-token");
        assert!(!format!("{config:?}").contains("secret"));
    }

    /// Issue #305: a CSP-blocked socket fires `error` and never `close`, so an `error`
    /// while connecting must fail the connect with the same reason a `close` gives.
    #[test]
    fn an_error_while_connecting_fails_the_connect_like_a_close() {
        let url = "ws://127.0.0.1:4455/api/v1/ws";
        let on_error = end_reason(url, true, SocketEnd::Error);
        let on_close = end_reason(url, true, SocketEnd::Close { reason: "" });
        assert!(on_error.contains("could not connect"), "{on_error}");
        assert!(on_error.contains(url), "{on_error}");
        assert_eq!(on_error, on_close);
    }

    #[test]
    fn an_ending_after_open_says_how_the_session_ended() {
        let url = "ws://h:1/api/v1/ws";
        let failed = end_reason(url, false, SocketEnd::Error);
        assert!(!failed.contains("could not connect"), "{failed}");
        assert!(failed.contains("failed"), "{failed}");
        assert_eq!(
            end_reason(url, false, SocketEnd::Close { reason: "" }),
            "the engine closed the connection"
        );
        assert_eq!(
            end_reason(
                url,
                false,
                SocketEnd::Close {
                    reason: "shutting down"
                }
            ),
            "the engine closed the connection: shutting down"
        );
    }
}
