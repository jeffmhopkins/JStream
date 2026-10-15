//! The HTTP seam: the [`HttpTransport`] trait every request goes through, and the
//! request and response types it carries.
//!
//! [`HttpsTransport`](crate::HttpsTransport) is the real one. Tests use
//! `fake::FakeTransport` (feature `fake`), which replays recorded JSON, so no
//! test touches the network (R-TEST-004, docs/RULES.md#R-TEST-004).
//!
//! Secrets never sit in a request's URL or headers as plain strings: the bearer token and
//! a form body (which carries an authorization code, a refresh token or the client
//! secret) are [`Secret`]s, which a transport exposes only while it writes them. A
//! response body may hold a token or a stream key, so it is zeroed when the response is
//! dropped, and neither type's `Debug` shows a body.

use std::fmt;
use std::sync::atomic::{Ordering, compiler_fence};

use jstream_platform::Secret;

/// The HTTP methods the Live API and the token endpoint use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// `GET`, with no body.
    Get,
    /// `POST`, with a body or an empty one.
    Post,
}

impl Method {
    /// The method's name on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// A request body.
pub enum Body {
    /// No body. A `POST` sends `Content-Length: 0`.
    Empty,
    /// `application/json`. Live API resource bodies hold no secrets.
    Json(Vec<u8>),
    /// `application/x-www-form-urlencoded`, already encoded. Token-endpoint bodies always
    /// hold a secret, so the whole body is one.
    Form(Secret),
}

impl Body {
    /// The `Content-Type`, or `None` for an empty body.
    pub fn content_type(&self) -> Option<&'static str> {
        match self {
            Self::Empty => None,
            Self::Json(_) => Some("application/json"),
            Self::Form(_) => Some("application/x-www-form-urlencoded"),
        }
    }

    /// The bytes to send. For a form, this exposes the secret: write it and drop it.
    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Empty => &[],
            Self::Json(b) => b,
            Self::Form(s) => s.expose().as_bytes(),
        }
    }
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("Empty"),
            Self::Json(b) => write!(f, "Json({} bytes)", b.len()),
            Self::Form(_) => f.write_str("Form(***)"),
        }
    }
}

/// One HTTPS request.
#[derive(Debug)]
pub struct HttpRequest<'a> {
    /// The method.
    pub method: Method,
    /// The absolute `https://` URL, query included. It never holds a secret.
    pub url: String,
    /// The OAuth access token, sent as `Authorization: Bearer …`.
    pub bearer: Option<&'a Secret>,
    /// The body.
    pub body: Body,
    /// A short name for logs and errors, such as `liveBroadcasts.list` or `token`.
    pub endpoint: &'static str,
}

/// An HTTP response: its status and its body, read in full.
///
/// The body is zeroed when the response is dropped, because a token response holds
/// tokens and a stream resource holds its stream key.
pub struct HttpResponse {
    /// The status code.
    pub status: u16,
    body: Vec<u8>,
}

impl HttpResponse {
    /// A response with `status` and `body`.
    pub fn new(status: u16, body: Vec<u8>) -> Self {
        Self { status, body }
    }

    /// The body.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Whether the status is 2xx.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

impl fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("body", &format_args!("{} bytes", self.body.len()))
            .finish()
    }
}

impl Drop for HttpResponse {
    fn drop(&mut self) {
        wipe(&mut self.body);
    }
}

/// Sends one HTTPS request and reads the whole response.
///
/// This is the crate's one seam to the network (R-RS-007: a transport). Calls block;
/// callers run them off any real-time thread. An implementation must not log or put in
/// an error the bearer token, a form body or a response body.
pub trait HttpTransport: Send + Sync {
    /// Sends `request` and returns the response, whatever its status.
    ///
    /// # Errors
    /// [`YoutubeError::Transport`](crate::YoutubeError::Transport) when no complete
    /// response arrived.
    fn send(&self, request: HttpRequest<'_>) -> Result<HttpResponse, crate::YoutubeError>;
}

/// Overwrites `bytes` with zeros. `black_box` and the fence keep the writes from being
/// dropped as dead stores.
pub(crate) fn wipe(bytes: &mut [u8]) {
    bytes.iter_mut().for_each(|b| *b = 0);
    std::hint::black_box(&bytes);
    compiler_fence(Ordering::SeqCst);
}

/// Percent-encodes `value` for a query string or form body: everything but RFC 3986's
/// unreserved characters (`A–Z a–z 0–9 - . _ ~`) becomes `%XX`.
pub(crate) fn encode_component(value: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in value.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 0xF)]));
        }
    }
}

/// `name=value&…`, each part percent-encoded.
pub(crate) fn encode_pairs<'v>(pairs: impl IntoIterator<Item = (&'v str, &'v str)>) -> String {
    let mut out = String::new();
    for (name, value) in pairs {
        if !out.is_empty() {
            out.push('&');
        }
        encode_component(name, &mut out);
        out.push('=');
        encode_component(value, &mut out);
    }
    out
}

/// Decodes one percent-encoded query or form component (`+` is a space). `None` when an
/// escape is malformed or the result isn't UTF-8.
pub(crate) fn decode_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = bytes.get(i + 1..i + 3)?;
                let hex = std::str::from_utf8(hex).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// Base64url without padding (RFC 4648 §5), as PKCE and `state` use.
pub(crate) fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            let index = (n >> (18 - 6 * i)) & 0x3F;
            out.push(char::from(ALPHABET[index as usize]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_matches_rfc_4648_vectors_without_padding() {
        let cases: [(&[u8], &str); 7] = [
            (b"", ""),
            (b"f", "Zg"),
            (b"fo", "Zm8"),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg"),
            (b"fooba", "Zm9vYmE"),
            (b"foobar", "Zm9vYmFy"),
        ];
        for (input, want) in cases {
            assert_eq!(base64url(input), want);
        }
        assert_eq!(base64url(&[0xFB, 0xFF]), "-_8");
    }

    #[test]
    fn components_round_trip_through_percent_encoding() {
        let value = "a b&c=d/é~._-";
        let mut encoded = String::new();
        encode_component(value, &mut encoded);
        assert_eq!(encoded, "a%20b%26c%3Dd%2F%C3%A9~._-");
        assert_eq!(decode_component(&encoded).as_deref(), Some(value));
        assert_eq!(decode_component("a+b").as_deref(), Some("a b"));
        assert_eq!(decode_component("%4"), None);
        assert_eq!(decode_component("%zz"), None);
        assert_eq!(decode_component("%FF"), None);
    }

    #[test]
    fn debug_never_shows_a_secret_or_a_body() {
        let token = Secret::new("ya29.token");
        let request = HttpRequest {
            method: Method::Post,
            url: "https://oauth2.googleapis.com/token".into(),
            bearer: Some(&token),
            body: Body::Form(Secret::new("refresh_token=1%2F%2Fsecret")),
            endpoint: "token",
        };
        let response = HttpResponse::new(200, br#"{"access_token":"ya29.new"}"#.to_vec());
        let shown = format!("{request:?} {response:?}");
        assert!(
            !shown.contains("ya29") && !shown.contains("secret"),
            "{shown}"
        );
    }
}
