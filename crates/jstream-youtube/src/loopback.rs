//! The loopback redirect: a one-shot HTTP listener on `127.0.0.1` that receives the
//! browser's redirect at the end of sign-in and takes the authorization code from it.
//!
//! Google redirects the user's browser, so the listener can't ask for JStream's control
//! token (R-SEC-002 covers the control listeners). What protects it instead:
//! - it binds only `127.0.0.1`, on a port the OS picks, and lives only until the code
//!   arrives or the wait times out;
//! - a request must name that address in its `Host` header, which stops DNS rebinding;
//! - the redirect must carry the `state` of the sign-in in progress (compared in
//!   constant time), which stops a page from injecting its own code;
//! - the code is useless without the PKCE verifier, which never leaves this process.
//!
//! Requests that fail a check (a favicon request, a stray tab) get an error page, and the
//! listener keeps waiting.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use jstream_platform::Secret;

use crate::http::decode_component;
use crate::{PendingAuthorization, YoutubeError};

/// How often an idle wait checks for a connection.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How long one connection may take to send its whole request head.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The largest request head read.
const MAX_REQUEST_BYTES: u64 = 8 << 10;

/// A loopback listener for one sign-in.
#[derive(Debug)]
pub struct LoopbackRedirect {
    listener: TcpListener,
    addr: SocketAddr,
}

impl LoopbackRedirect {
    /// Binds `127.0.0.1` on a port the OS picks.
    ///
    /// # Errors
    /// [`YoutubeError::Transport`] when the socket can't be bound.
    pub fn bind() -> Result<Self, YoutubeError> {
        let io = |e| YoutubeError::transport("binding the loopback redirect", e);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(io)?;
        listener.set_nonblocking(true).map_err(io)?;
        let addr = listener.local_addr().map_err(io)?;
        Ok(Self { listener, addr })
    }

    /// The redirect URI to register the sign-in with: `http://127.0.0.1:<port>`.
    pub fn redirect_uri(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Waits up to `timeout` for the redirect of `pending`, answers the browser, and
    /// returns the authorization code.
    ///
    /// # Errors
    /// [`YoutubeError::AuthorizationDenied`] when the redirect carries an error (the
    /// user declined); [`YoutubeError::AuthorizationTimedOut`] when no valid redirect
    /// came in time; [`YoutubeError::Transport`] when the listener fails.
    pub fn wait(
        &self,
        pending: &PendingAuthorization,
        timeout: Duration,
    ) -> Result<Secret, YoutubeError> {
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() >= deadline {
                return Err(YoutubeError::AuthorizationTimedOut);
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    // A connection that fails, stalls or sends garbage is answered (if it
                    // can be) and dropped; the wait goes on. It gets REQUEST_TIMEOUT in
                    // all, and never past the wait's own deadline.
                    let request_deadline = deadline.min(Instant::now() + REQUEST_TIMEOUT);
                    if let Some(outcome) = self.handle(stream, pending, request_deadline) {
                        return outcome;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(YoutubeError::AuthorizationTimedOut);
                    }
                    std::thread::sleep(POLL_INTERVAL.min(deadline - now));
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(YoutubeError::transport("accepting the redirect", e)),
            }
        }
    }

    /// Reads one request and answers it. `Some` ends the wait.
    fn handle(
        &self,
        mut stream: TcpStream,
        pending: &PendingAuthorization,
        deadline: Instant,
    ) -> Option<Result<Secret, YoutubeError>> {
        let setup = stream
            .set_nonblocking(false)
            .and_then(|()| stream.set_write_timeout(Some(REQUEST_TIMEOUT)));
        if setup.is_err() {
            return None;
        }
        let request = match read_request(&stream, deadline) {
            Ok(request) => request,
            Err(_) => {
                respond(&mut stream, 400, "The request could not be read.");
                return None;
            }
        };
        let (outcome, status, page) = check(&request, &self.addr, pending.state());
        respond(&mut stream, status, page);
        outcome
    }
}

/// A request head: the target of its request line and its `Host` header.
#[derive(Debug, Default)]
struct Request {
    method: String,
    target: String,
    host: Option<String>,
}

/// A stream whose reads all end by one deadline, so a client trickling bytes can't hold
/// the listener past it.
struct Deadlined<'s> {
    stream: &'s TcpStream,
    deadline: Instant,
}

impl Read for Deadlined<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buf)
    }
}

fn read_request(stream: &TcpStream, deadline: Instant) -> io::Result<Request> {
    let mut reader = BufReader::new(Deadlined { stream, deadline }).take(MAX_REQUEST_BYTES);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_ascii_whitespace();
    let mut request = Request {
        method: parts.next().unwrap_or_default().to_owned(),
        target: parts.next().unwrap_or_default().to_owned(),
        host: None,
    };
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            return Ok(request);
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("host")
        {
            request.host = Some(value.trim().to_owned());
        }
    }
}

/// Checks a request against the listener's address and the expected `state`: the
/// outcome (`None` to keep waiting), and the status and page to answer with.
fn check(
    request: &Request,
    addr: &SocketAddr,
    state: &str,
) -> (Option<Result<Secret, YoutubeError>>, u16, &'static str) {
    const NOT_THIS: &str = "This is not the JStream sign-in page.";
    if request.method != "GET" || request.host.as_deref() != Some(addr.to_string().as_str()) {
        return (None, 400, NOT_THIS);
    }
    let (path, query) = request
        .target
        .split_once('?')
        .unwrap_or((request.target.as_str(), ""));
    if path != "/" {
        return (None, 404, NOT_THIS);
    }

    let mut got_state = None;
    let mut code = None;
    let mut error = None;
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        match name {
            "state" => got_state = decode_component(value),
            "code" => code = decode_component(value).map(Secret::new),
            "error" => error = decode_component(value),
            _ => {}
        }
    }
    if !got_state.is_some_and(|s| constant_time_eq(s.as_bytes(), state.as_bytes())) {
        return (
            None,
            400,
            "This sign-in link has expired. Start the sign-in again from JStream.",
        );
    }
    if let Some(error) = error {
        let error: String = error
            .chars()
            .filter(|c| c.is_ascii_lowercase() || *c == '_')
            .take(64)
            .collect();
        return (
            Some(Err(YoutubeError::AuthorizationDenied { error })),
            200,
            "Sign-in was cancelled. You can close this tab.",
        );
    }
    match code {
        Some(code) if !code.expose().is_empty() => (
            Some(Ok(code)),
            200,
            "JStream is signed in to YouTube. You can close this tab.",
        ),
        _ => (None, 400, NOT_THIS),
    }
}

fn respond(stream: &mut TcpStream, status: u16, message: &str) {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Bad Request",
    };
    let body =
        format!("<!doctype html><meta charset=\"utf-8\"><title>JStream</title><p>{message}</p>\n");
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // The browser may already have gone; the outcome doesn't depend on the answer.
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Compares without an early exit, so the time taken doesn't reveal the `state`.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests;
