//! `GET /pair` ([`PAIR_PAGE_PATH`]): the page a browser pairs on (ADR-0047).
//!
//! - **Only on a TLS listener** (`ServerConfig::tls`). Without TLS a browser has no way
//!   to keep a device token (the cookie is `Secure`), so the path is 404, before any
//!   check.
//! - **Token-free.** It is answered without a token, after the `Host` and `Origin`
//!   checks (`Gate::admit_unauthenticated`), and refused like any request when they
//!   fail. It is a fixed page built into the server ([`PAGE`], `pair.html`), not part
//!   of the UI bundle: a code field, a device-name field and one inline script. It holds
//!   no engine data and no secret, and changes no state.
//! - **The script** posts the code and name to `POST /api/v1/pair` with
//!   `"browser": true`. The server answers with the device ID only and sets the device
//!   token as the `__Host-jstream_device` cookie, which no script can read (`pair.rs`).
//!   On success the page goes to `/`, where the cookie admits the browser.
//! - **Headers.** Those of a UI page (`ui.rs`): the same nonce `Content-Security-Policy`,
//!   whose nonce replaces trunk's placeholder in the page's `<script>` and `<style>`,
//!   `nosniff`, `no-cache` and `no-referrer`.
//! - **Methods.** `GET` and `HEAD`; any other is 405 with `Allow: GET, HEAD`.
//!
//! The UI's route fallback never serves `/pair` or a path below it (`ui.rs`).

use crate::App;
use crate::listener::Accepted;
use crate::ui::{NONCE_PLACEHOLDER, csp, nonce, set_page_headers};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

/// The path of the browser pairing page.
pub const PAIR_PAGE_PATH: &str = "/pair";

/// The page, with trunk's nonce placeholder where the nonce goes.
const PAGE: &str = include_str!("pair.html");

/// `GET /pair`.
pub(crate) async fn handle(
    State(app): State<Arc<App>>,
    ConnectInfo(accepted): ConnectInfo<Accepted>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    if !app.tls || !app.pairing {
        return (StatusCode::NOT_FOUND, "not found\n").into_response();
    }
    if let Err(refusal) = app.gate.admit_unauthenticated(&headers, accepted.peer) {
        return refusal.into_response();
    }
    if method != Method::GET && method != Method::HEAD {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "GET, HEAD")],
            "the pairing page is read-only: GET requests\n",
        )
            .into_response();
    }
    let Some(nonce) = nonce() else {
        tracing::warn!("no random source for a CSP nonce");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    // The gate accepted `Host`, so it is one of the server's own names.
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    let mut response = PAGE.replace(NONCE_PLACEHOLDER, &nonce).into_response();
    set_page_headers(
        &mut response,
        "text/html; charset=utf-8",
        &csp(host, Some(&nonce), true),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::PAGE;
    use crate::ui::NONCE_PLACEHOLDER;

    /// The page is small and fixed: one inline script and one style block, both for the
    /// nonce, posting the browser exchange; no secret, no external resource.
    #[test]
    fn the_page_is_one_form_and_one_nonced_script() {
        assert_eq!(PAGE.matches("<script").count(), 1);
        assert_eq!(PAGE.matches("<style").count(), 1);
        assert_eq!(PAGE.matches(NONCE_PLACEHOLDER).count(), 2);
        assert!(PAGE.contains("id=\"code\"") && PAGE.contains("id=\"name\""));
        assert!(PAGE.contains("\"/api/v1/pair\"") && PAGE.contains("browser: true"));
        for absent in [
            "http://",
            "https://",
            "src=",
            "localStorage",
            "document.cookie",
        ] {
            assert!(!PAGE.contains(absent), "{absent}");
        }
        assert!(PAGE.len() < 4096);
    }
}
