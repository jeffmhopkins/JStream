use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

use super::*;
use crate::{OAuthClient, Scope};

fn pending(redirect: &LoopbackRedirect) -> PendingAuthorization {
    OAuthClient::new("id", None)
        .authorize(&redirect.redirect_uri(), &[Scope::Readonly])
        .unwrap()
}

/// Sends one raw request to the listener and returns the status line of the answer.
fn send(addr: SocketAddr, request: String) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    answer.lines().next().unwrap_or_default().to_owned()
}

fn get(addr: SocketAddr, target: &str) -> String {
    send(
        addr,
        format!("GET {target} HTTP/1.1\r\nHost: {addr}\r\n\r\n"),
    )
}

#[test]
fn the_redirect_uri_is_loopback_on_an_os_port() {
    let redirect = LoopbackRedirect::bind().unwrap();
    let uri = redirect.redirect_uri();
    assert!(uri.starts_with("http://127.0.0.1:"), "{uri}");
    assert_ne!(redirect.addr.port(), 0);
}

#[test]
fn the_code_is_taken_after_strays_and_forgeries_are_turned_away() {
    let redirect = LoopbackRedirect::bind().unwrap();
    let pending = pending(&redirect);
    let addr = redirect.addr;
    let state = pending.state().to_owned();
    let browser = thread::spawn(move || {
        [
            get(addr, "/favicon.ico"),
            get(addr, "/?state=forged&code=attacker"),
            get(addr, "/?code=no-state"),
            // DNS rebinding: right path and state, wrong Host.
            send(
                addr,
                format!("GET /?state={state}&code=x HTTP/1.1\r\nHost: evil.example:80\r\n\r\n"),
            ),
            send(
                addr,
                format!("POST /?state={state}&code=x HTTP/1.1\r\nHost: {addr}\r\n\r\n"),
            ),
            get(addr, &format!("/?state={state}&code=")),
            get(
                addr,
                &format!("/?state={state}&code=4%2F0Afixture-code&scope=x"),
            ),
        ]
    });
    let code = redirect.wait(&pending, Duration::from_secs(10)).unwrap();
    assert_eq!(code.expose(), "4/0Afixture-code");
    let statuses = browser.join().unwrap();
    assert_eq!(
        statuses.iter().map(|s| &s[9..12]).collect::<Vec<_>>(),
        ["404", "400", "400", "400", "400", "400", "200"]
    );
}

#[test]
fn a_cancelled_sign_in_ends_the_wait_with_the_error() {
    let redirect = LoopbackRedirect::bind().unwrap();
    let pending = pending(&redirect);
    let addr = redirect.addr;
    let target = format!(
        "/?error=access_denied%3Cscript%3E&state={}",
        pending.state()
    );
    let browser = thread::spawn(move || get(addr, &target));
    let err = redirect
        .wait(&pending, Duration::from_secs(10))
        .unwrap_err();
    match err {
        YoutubeError::AuthorizationDenied { error } => assert_eq!(error, "access_deniedscript"),
        other => panic!("{other}"),
    }
    assert!(browser.join().unwrap().contains("200"));
}

#[test]
fn the_wait_times_out() {
    let redirect = LoopbackRedirect::bind().unwrap();
    let pending = pending(&redirect);
    let err = redirect
        .wait(&pending, Duration::from_millis(50))
        .unwrap_err();
    assert!(matches!(err, YoutubeError::AuthorizationTimedOut), "{err}");
}

#[test]
fn a_trickling_client_cannot_hold_the_wait_past_its_deadline() {
    let redirect = LoopbackRedirect::bind().unwrap();
    let pending = pending(&redirect);
    let addr = redirect.addr;
    let client = thread::spawn(move || {
        let mut stream = TcpStream::connect(addr).unwrap();
        // One byte at a time for about 5 s, each well inside a per-read timeout.
        for b in b"GET /?state=0123456789abcdefghijklmnopqrstuvwxyz HTTP/1.1" {
            if stream.write_all(&[*b]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    });
    let started = std::time::Instant::now();
    let err = redirect
        .wait(&pending, Duration::from_millis(300))
        .unwrap_err();
    assert!(matches!(err, YoutubeError::AuthorizationTimedOut), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    client.join().unwrap();
}

#[test]
fn state_comparison_is_exact() {
    assert!(constant_time_eq(b"abc", b"abc"));
    assert!(!constant_time_eq(b"abc", b"abd"));
    assert!(!constant_time_eq(b"abc", b"abcd"));
}
