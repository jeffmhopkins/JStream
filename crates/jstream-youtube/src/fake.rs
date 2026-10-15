//! An in-memory [`HttpTransport`] for tests: it replays queued responses in order and
//! records every request, so tests run on recorded JSON with no network
//! (R-TEST-004, docs/RULES.md#R-TEST-004).
//!
//! The recording exposes bearer tokens and form bodies on purpose, so tests can check
//! them. Use it only in tests.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::YoutubeError;
use crate::http::{HttpRequest, HttpResponse, HttpTransport, Method};

/// A request as [`FakeTransport`] saw it, secrets included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedRequest {
    /// The method.
    pub method: Method,
    /// The full URL.
    pub url: String,
    /// The bearer token, exposed.
    pub bearer: Option<String>,
    /// The body as text, exposed.
    pub body: String,
    /// The request's endpoint name.
    pub endpoint: &'static str,
}

impl RecordedRequest {
    /// The decoded value of query parameter `name`, if the URL has it.
    pub fn query(&self, name: &str) -> Option<String> {
        pairs(self.url.split_once('?')?.1, name)
    }

    /// The decoded value of form field `name`, if the body has it.
    pub fn form(&self, name: &str) -> Option<String> {
        pairs(&self.body, name)
    }
}

fn pairs(encoded: &str, name: &str) -> Option<String> {
    encoded
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(n, _)| *n == name)
        .and_then(|(_, v)| crate::http::decode_component(v))
}

/// Replays queued responses; a request with nothing queued fails as a transport error.
/// Clones share the queue and the recording.
#[derive(Clone, Debug, Default)]
pub struct FakeTransport {
    state: Arc<Mutex<State>>,
}

#[derive(Debug, Default)]
struct State {
    responses: VecDeque<(u16, Vec<u8>)>,
    requests: Vec<RecordedRequest>,
}

impl FakeTransport {
    /// A transport with nothing queued.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues a response with `status` and `body`.
    pub fn respond(&self, status: u16, body: impl Into<Vec<u8>>) -> &Self {
        lock(&self.state).responses.push_back((status, body.into()));
        self
    }

    /// Every request so far, in order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        lock(&self.state).requests.clone()
    }

    /// How many queued responses are left.
    pub fn pending(&self) -> usize {
        lock(&self.state).responses.len()
    }
}

impl HttpTransport for FakeTransport {
    fn send(&self, request: HttpRequest<'_>) -> Result<HttpResponse, YoutubeError> {
        let mut state = lock(&self.state);
        state.requests.push(RecordedRequest {
            method: request.method,
            url: request.url.clone(),
            bearer: request.bearer.map(|s| s.expose().to_owned()),
            body: String::from_utf8_lossy(request.body.bytes()).into_owned(),
            endpoint: request.endpoint,
        });
        let (status, body) = state.responses.pop_front().ok_or_else(|| {
            YoutubeError::transport(
                "sending to the fake transport",
                io::Error::new(io::ErrorKind::ConnectionRefused, "no response queued"),
            )
        })?;
        Ok(HttpResponse::new(status, body))
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}
