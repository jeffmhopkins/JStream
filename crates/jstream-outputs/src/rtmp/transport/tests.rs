//! Transport tests over loopback TCP, with TLS certificates generated at test time
//! (docs/RULES.md#R-TEST-004).

use std::net::TcpListener;
use std::thread;

use rustls::CertificateError;
use rustls::version::{TLS12, TLS13};

use super::*;
use crate::rtmp::test_util::{TestPki, tls_server};

const TIMEOUT: Option<Duration> = Some(Duration::from_secs(10));

fn url(scheme: &str, host: &str, port: u16) -> RtmpUrl {
    RtmpUrl::parse(&format!("{scheme}://{host}:{port}/live2")).unwrap()
}

/// Echoes one line-sized message back over `io` and returns what it read.
fn echo_once(mut io: impl Read + Write) -> Vec<u8> {
    let mut buf = [0u8; 5];
    io.read_exact(&mut buf).unwrap();
    io.write_all(&buf).unwrap();
    io.flush().unwrap();
    buf.to_vec()
}

#[test]
fn rtmp_connects_over_plain_tcp() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || echo_once(listener.accept().unwrap().0));

    let mut t = connect(&url("rtmp", "127.0.0.1", port), TIMEOUT).unwrap();
    assert!(!t.is_tls());
    assert!(t.tcp().nodelay().unwrap());
    t.write_all(b"hello").unwrap();
    let mut back = [0u8; 5];
    t.read_exact(&mut back).unwrap();
    assert_eq!(&back, b"hello");
    assert_eq!(server.join().unwrap(), b"hello");
}

/// A TLS round trip with each protocol version the client offers.
#[test]
fn rtmps_round_trip_over_tls13_and_tls12() {
    let pki = TestPki::new(&["localhost"]);
    for version in [&TLS13, &TLS12] {
        let (port, server) = tls_server(pki.server_config(&[version]), |mut s| {
            let got = echo_once(&mut s);
            // The client's close_notify reads as a clean end of stream.
            let mut rest = Vec::new();
            s.read_to_end(&mut rest).unwrap();
            assert!(rest.is_empty());
            (got, s.conn.protocol_version())
        });
        let config = client_config(pki.roots.clone()).unwrap();
        let mut t = connect_tls(&url("rtmps", "localhost", port), TIMEOUT, config).unwrap();
        assert!(t.is_tls());
        // The handshake's timeouts are cleared again.
        assert_eq!(t.tcp().read_timeout().unwrap(), None);
        assert_eq!(t.tcp().write_timeout().unwrap(), None);
        t.write_all(b"hello").unwrap();
        t.flush().unwrap();
        let mut back = [0u8; 5];
        t.read_exact(&mut back).unwrap();
        assert_eq!(&back, b"hello");
        t.shutdown().unwrap();
        let (got, negotiated) = server.join().unwrap().unwrap();
        assert_eq!(got, b"hello");
        assert_eq!(negotiated, Some(version.version));
    }
}

#[test]
fn certificate_for_another_host_is_refused() {
    let pki = TestPki::new(&["ingest.example.invalid"]);
    let (port, server) = tls_server(pki.server_config(&[&TLS13]), |_| ());
    let config = client_config(pki.roots.clone()).unwrap();
    let err = connect_tls(&url("rtmps", "localhost", port), TIMEOUT, config).unwrap_err();
    assert!(
        matches!(
            &err,
            RtmpError::Tls(rustls::Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. }
            ))
        ),
        "{err:?}"
    );
    // The server sees the client's alert and fails its handshake.
    assert!(server.join().unwrap().is_err());
}

#[test]
fn certificate_from_an_untrusted_ca_is_refused() {
    let server_pki = TestPki::new(&["localhost"]);
    let other_pki = TestPki::with_ca_name("Another test CA", &["localhost"]);
    let (port, server) = tls_server(server_pki.server_config(&[&TLS13]), |_| ());
    let config = client_config(other_pki.roots.clone()).unwrap();
    let err = connect_tls(&url("rtmps", "localhost", port), TIMEOUT, config).unwrap_err();
    assert!(
        matches!(
            &err,
            RtmpError::Tls(rustls::Error::InvalidCertificate(
                CertificateError::UnknownIssuer
            ))
        ),
        "{err:?}"
    );
    assert!(server.join().unwrap().is_err());
}

#[test]
fn handshake_against_a_silent_server_times_out() {
    // Accepts TCP but never answers the ClientHello.
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut tcp, _) = listener.accept().unwrap();
        let mut sink = Vec::new();
        // Returns once the client gives up and closes the connection.
        let _ = tcp.read_to_end(&mut sink);
    });
    let pki = TestPki::new(&["localhost"]);
    let config = client_config(pki.roots.clone()).unwrap();
    let short = Some(Duration::from_millis(100));
    let err = connect_tls(&url("rtmps", "localhost", port), short, config).unwrap_err();
    assert!(
        matches!(&err, RtmpError::Io { during: "TLS handshake", source }
            if matches!(source.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)),
        "{err:?}"
    );
    server.join().unwrap();
}
