//! `POST /api/v1/pair` ([`PAIR_PATH`]): a device trades the open pairing code for its own
//! device token (ADR-0044). It is answered without a token: it passes the `Host` and
//! `Origin` checks, and the open code is its gate (R-SEC-002).
//!
//! - **Request.** `Content-Type: application/json`, a body of at most 4 KiB:
//!   `{"code": "XXXX-XXXX", "name": "Tablet"}`. The code is read loosely (case, the
//!   dash and spaces don't matter, and `O`, `I` and `L` read as `0`, `1` and `1`). The
//!   name is trimmed and must be 1 to 64 characters, none of them a control character.
//!   The pairing page adds `"browser": true` (ADR-0047), which only a TLS listener
//!   accepts; `false` or no `browser` is a native client.
//! - **Answers.** `200` with `{"device_id": "dev_…", "token": "…"}` and
//!   `Cache-Control: no-store`: the device presents the token from then on, as a client
//!   presents the control token. To a browser exchange the `200` is
//!   `{"device_id": "dev_…"}` alone, with the token set as the cookie
//!   `__Host-jstream_device=<token>; Secure; HttpOnly; SameSite=Strict; Path=/;
//!   Max-Age=2592000`, so no script, the page's own included, ever sees it. `401` while
//!   no code is open (none was started, or it
//!   expired, was used or was voided), `403` for a wrong code (the fifth voids it),
//!   `400` for a body that isn't that object, a name that isn't allowed, a `browser`
//!   that isn't a boolean, or `"browser": true` without TLS (none of which uses up an
//!   attempt), `415` for another content type, `405` for another method, and `500`
//!   when the device file can't be written (nothing is paired then).
//! - **Logs.** Refusals are logged at `warn` with their reason and the peer address,
//!   a voided code at `warn`, and a paired device at `info` with its ID and name. The
//!   code, the token and the cookie are never logged (R-SEC-001).

use crate::App;
use crate::asks::{ASK_TIMEOUT, AskRefused};
use crate::devices::{Now, PairError, check_name};
use crate::gate::{Refusal, device_cookie_header};
use crate::listener::Accepted;
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use jstream_protocol::PairingAskOutcome;
use serde_json::{Value, json};
use std::sync::Arc;

/// The path a device posts the pairing code to.
pub const PAIR_PATH: &str = "/api/v1/pair";

/// The path a device posts its name to, to ask the person to pair it (ADR-0235).
pub const PAIR_ASK_PATH: &str = "/api/v1/pair/ask";

/// The largest body accepted, in bytes: a code and a short name.
const MAX_BODY_BYTES: usize = 4096;

/// `POST /api/v1/pair`.
pub(crate) async fn handle(
    State(app): State<Arc<App>>,
    ConnectInfo(accepted): ConnectInfo<Accepted>,
    method: Method,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let peer = accepted.peer;
    if let Err(refusal) = app
        .gate
        .admit_unauthenticated(&headers, peer, accepted.on())
    {
        return refusal.into_response();
    }
    if method != Method::POST {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "POST")],
            "post the pairing code\n",
        )
            .into_response();
    }
    // Checked before the body is read, so a request with no code open costs nothing.
    if !app.devices.pairing_open(Now::current()) {
        Refusal::NoPairingCode.log(peer);
        return Refusal::NoPairingCode.into_response();
    }
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"));
    if !json {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "the body must be JSON (`Content-Type: application/json`)\n",
        )
            .into_response();
    }
    let Ok(bytes) = axum::body::to_bytes(body, MAX_BODY_BYTES).await else {
        return bad_request("the body is over 4 KiB, or could not be read");
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return bad_request("the body must be `{\"code\": …, \"name\": …}`");
    };
    let (Some(code), Some(name)) = (
        value.get("code").and_then(Value::as_str),
        value.get("name").and_then(Value::as_str),
    ) else {
        return bad_request("the body must be `{\"code\": …, \"name\": …}`");
    };
    let name = match check_name(name) {
        Ok(name) => name,
        Err(reason) => return bad_request(reason),
    };
    let browser = match value.get("browser") {
        None => false,
        Some(Value::Bool(browser)) => *browser,
        Some(_) => return bad_request("`browser` must be true or false"),
    };
    if browser && !app.tls(&accepted) {
        return bad_request("a browser pairs only on a TLS listener");
    }
    match app.devices.pair(code, name, Now::current()) {
        Ok((device_id, token)) if browser => {
            let Some(cookie) = device_cookie_header(token.as_bytes()) else {
                // The server's tokens are hex digits, so this can't happen; the device
                // can't be reached without its token, so it is not kept.
                let _ = app.devices.revoke(&device_id);
                return (StatusCode::INTERNAL_SERVER_ERROR, "could not pair\n").into_response();
            };
            let mut response = (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                json!({"device_id": device_id.as_str()}).to_string(),
            )
                .into_response();
            response.headers_mut().insert(header::SET_COOKIE, cookie);
            response
        }
        Ok((device_id, token)) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            json!({"device_id": device_id.as_str(), "token": token}).to_string(),
        )
            .into_response(),
        Err(PairError::NoCode) => {
            Refusal::NoPairingCode.log(peer);
            Refusal::NoPairingCode.into_response()
        }
        Err(PairError::Wrong { voided }) => {
            Refusal::WrongPairingCode.log(peer);
            if voided {
                (
                    StatusCode::FORBIDDEN,
                    "wrong pairing code; that was the last attempt, so start pairing again\n",
                )
                    .into_response()
            } else {
                Refusal::WrongPairingCode.into_response()
            }
        }
        Err(PairError::Random(error)) => {
            tracing::warn!(error = %error, "could not draw a device token");
            (StatusCode::INTERNAL_SERVER_ERROR, "could not pair\n").into_response()
        }
        Err(PairError::Store(error)) => {
            tracing::warn!(error = %error, "could not write the device file; nothing paired");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not save the device\n",
            )
                .into_response()
        }
    }
}

/// `POST /api/v1/pair/ask` ([`PAIR_ASK_PATH`], ADR-0235): a native device asks the person
/// to pair it, with `{"name": "Stream Deck"}`, and the request is held until they answer.
///
/// - **Answers.** `200` with `{"device_id": "dev_…", "token": "…"}` and
///   `Cache-Control: no-store` when the person allows it, as a code would; `403` when
///   they deny it, `408` when no one answered in 2 minutes, `429` while 4 asks are open
///   or one from this address is, `404` while the server pairs no device (its network
///   listener closed and `ServerConfig::pairing` off), `400` for a body that isn't that
///   object or a name that isn't allowed, `415` for another content type, `405` for
///   another method, and `500` when the device file can't be written.
/// - **Gone.** A device that closes its connection ends its ask (`closed`).
pub(crate) async fn ask(
    State(app): State<Arc<App>>,
    ConnectInfo(accepted): ConnectInfo<Accepted>,
    method: Method,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let peer = accepted.peer;
    if let Err(refusal) = app
        .gate
        .admit_unauthenticated(&headers, peer, accepted.on())
    {
        return refusal.into_response();
    }
    if method != Method::POST {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "POST")],
            "post the device's name\n",
        )
            .into_response();
    }
    if !app.pairs() || !app.pairing_offered && !accepted.network {
        return (StatusCode::NOT_FOUND, "this server pairs no device\n").into_response();
    }
    // The ask ends when its listener closes or the server stops, so a held ask never
    // holds up either, and nothing pairs once the person turned network access off.
    let mut network_stopping = if accepted.network {
        match app.network.stopping() {
            Some(stopping) if !*stopping.borrow() => Some(stopping),
            _ => return closed(),
        }
    } else {
        None
    };
    let mut shutdown = app.shutdown.clone();
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"));
    if !json {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "the body must be JSON (`Content-Type: application/json`)\n",
        )
            .into_response();
    }
    let Ok(bytes) = axum::body::to_bytes(body, MAX_BODY_BYTES).await else {
        return bad_request("the body is over 4 KiB, or could not be read");
    };
    let Some(name) = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|value| value.get("name").and_then(Value::as_str).map(str::to_owned))
    else {
        return bad_request("the body must be `{\"name\": …}`");
    };
    let name = match check_name(&name) {
        Ok(name) => name.to_owned(),
        Err(reason) => return bad_request(reason),
    };
    let mut pending = match app.asks.open(&name, peer.ip()) {
        Ok(pending) => pending,
        Err(AskRefused::Busy) => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                "another ask is waiting for an answer; try again in a moment\n",
            )
                .into_response();
        }
    };
    // Dropped with the request when the device goes away, which ends the ask.
    let answer = tokio::select! {
        answer = pending.answer(ASK_TIMEOUT) => answer,
        () = stopped(network_stopping.as_mut()) => {
            pending.finish(PairingAskOutcome::Closed);
            return closed();
        }
        () = stopped(Some(&mut shutdown)) => {
            pending.finish(PairingAskOutcome::Closed);
            return closed();
        }
    };
    if network_stopping
        .as_ref()
        .is_some_and(|stopping| *stopping.borrow())
        || *shutdown.borrow()
    {
        // Answered as the listener closed: pair nothing.
        pending.finish(PairingAskOutcome::Closed);
        return closed();
    }
    match answer {
        None => (StatusCode::REQUEST_TIMEOUT, "no one answered in JStream\n").into_response(),
        Some(false) => {
            pending.finish(PairingAskOutcome::Denied);
            tracing::warn!(%peer, "a device's ask to pair was denied");
            (StatusCode::FORBIDDEN, "denied in JStream\n").into_response()
        }
        Some(true) => match app.devices.add(&name, Now::current()) {
            Ok((device_id, token)) => {
                pending.finish(PairingAskOutcome::Allowed);
                (
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, "application/json"),
                        (header::CACHE_CONTROL, "no-store"),
                    ],
                    json!({"device_id": device_id.as_str(), "token": token}).to_string(),
                )
                    .into_response()
            }
            Err(error) => {
                pending.finish(PairingAskOutcome::Denied);
                tracing::warn!(?error, "could not pair an allowed device");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "could not save the device\n",
                )
                    .into_response()
            }
        },
    }
}

/// Completes once `stopping` turns true or its sender goes; never for `None`.
async fn stopped(stopping: Option<&mut tokio::sync::watch::Receiver<bool>>) {
    match stopping {
        Some(stopping) => {
            let _ = stopping.wait_for(|stopping| *stopping).await;
        }
        None => std::future::pending().await,
    }
}

/// The answer to an ask whose listener closed, or whose server stopped, first.
fn closed() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "network access was turned off\n",
    )
        .into_response()
}

fn bad_request(reason: &str) -> Response {
    (StatusCode::BAD_REQUEST, format!("{reason}\n")).into_response()
}
