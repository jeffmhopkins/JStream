use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use jstream_platform::Secret;
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair};
use rustls::pki_types::PrivateKeyDer;
use rustls::{RootCertStore, ServerConfig, ServerConnection, StreamOwned};

use super::*;
use crate::http::{Body, Method};

fn read(raw: &[u8]) -> Result<HttpResponse, YoutubeError> {
    read_response(&mut Cursor::new(raw.to_vec()))
}

#[test]
fn targets_parse_and_anything_but_https_is_refused() {
    assert_eq!(
        Target::parse("https://www.googleapis.com/youtube/v3/liveStreams?part=id").unwrap(),
        Target {
            host: "www.googleapis.com",
            port: 443,
            path: "/youtube/v3/liveStreams?part=id"
        }
    );
    assert_eq!(
        Target::parse("https://localhost:8443").unwrap(),
        Target {
            host: "localhost",
            port: 8443,
            path: "/"
        }
    );
    for bad in [
        "http://oauth2.googleapis.com/token",
        "https://user:pw@host/",
        "https://host:port/",
        "https:///path",
        "https://host?x=1",
        "https://host/a b",
        "https://host/a\r\nX: y",
    ] {
        assert!(Target::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn responses_are_framed_by_length_chunks_or_the_end_of_the_stream() {
    let r = read(
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}extra",
    )
    .unwrap();
    assert_eq!((r.status, r.body()), (200, &b"{}"[..]));

    let r = read(
        b"HTTP/1.1 403 Forbidden\r\nTransfer-Encoding: chunked\r\n\r\n4;ext=1\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\nX-Trailer: t\r\n\r\n",
    )
    .unwrap();
    assert_eq!((r.status, r.body()), (403, &b"{\"a\":1}"[..]));

    let r = read(b"HTTP/1.0 200 OK\r\n\r\nto the end").unwrap();
    assert_eq!(r.body(), b"to the end");

    let r = read(b"HTTP/1.1 204 No Content\r\nContent-Length: 5\r\n\r\n").unwrap();
    assert!(r.body().is_empty());
}

#[test]
fn malformed_or_oversized_responses_are_transport_errors() {
    let too_big = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
        MAX_BODY_BYTES + 1
    );
    let huge_chunk = format!(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
        MAX_BODY_BYTES + 1
    );
    let overflowing_chunk =
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffff\r\n";
    let long_head = format!(
        "HTTP/1.1 200 OK\r\nX: {}\r\n\r\n",
        "a".repeat(MAX_HEAD_BYTES)
    );
    let cases: [&[u8]; 10] = [
        b"",
        b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort",
        b"SPDY/3 200 OK\r\n\r\n",
        b"HTTP/1.1 abc OK\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nno colon\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n",
        too_big.as_bytes(),
        huge_chunk.as_bytes(),
        overflowing_chunk.as_bytes(),
        long_head.as_bytes(),
    ];
    for raw in cases {
        let err = read(raw).unwrap_err();
        assert!(matches!(err, YoutubeError::Transport { .. }), "{err}");
    }
    let mut endless = Cursor::new(b"HTTP/1.1 200 OK\r\n\r\n".to_vec()).chain(std::io::repeat(b'x'));
    let err = read_response(&mut BufReader::new(&mut endless)).unwrap_err();
    assert!(matches!(err, YoutubeError::Transport { .. }), "{err}");
}

/// A CA, a server certificate for `localhost`, and a server config presenting it.
fn pki() -> (RootCertStore, Arc<ServerConfig>) {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "jstream-youtube test CA");
    let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().unwrap()).unwrap();
    let leaf_key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["localhost".to_owned()])
        .unwrap()
        .signed_by(&leaf_key, &ca)
        .unwrap();
    let mut roots = RootCertStore::empty();
    roots.add(ca.der().clone()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone(), ca.der().clone()],
            PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
        )
        .unwrap();
    (roots, Arc::new(config))
}

#[test]
fn a_request_round_trips_over_tls_with_its_bearer_and_body() {
    let (roots, config) = pki();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (tcp, _) = listener.accept().unwrap();
        let mut tls = StreamOwned::new(ServerConnection::new(config).unwrap(), tcp);
        let mut reader = BufReader::new(&mut tls);
        let mut head = Vec::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            head.push(line.trim_end().to_owned());
        }
        let mut body = vec![0; 13];
        reader.read_exact(&mut body).unwrap();
        tls.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n\
              7\r\n{\"ok\":1\r\n1\r\n}\r\n0\r\n\r\n",
        )
        .unwrap();
        tls.conn.send_close_notify();
        tls.flush().unwrap();
        (head, body)
    });

    let transport = HttpsTransport::with_roots(roots)
        .unwrap()
        .with_timeout(Duration::from_secs(10));
    let token = Secret::new("ya29.over-tls");
    let response = transport
        .send(HttpRequest {
            method: Method::Post,
            url: format!("https://localhost:{port}/youtube/v3/liveBroadcasts?part=id"),
            bearer: Some(&token),
            body: Body::Json(br#"{"snippet":1}"#.to_vec()),
            endpoint: "liveBroadcasts.insert",
        })
        .unwrap();
    assert_eq!(
        (response.status, response.body()),
        (200, &b"{\"ok\":1}"[..])
    );

    let (head, body) = server.join().unwrap();
    assert_eq!(head[0], "POST /youtube/v3/liveBroadcasts?part=id HTTP/1.1");
    for want in [
        format!("Host: localhost:{port}"),
        "Authorization: Bearer ya29.over-tls".to_owned(),
        "Content-Type: application/json".to_owned(),
        "Content-Length: 13".to_owned(),
        "Connection: close".to_owned(),
    ] {
        assert!(head.contains(&want), "{want} not in {head:?}");
    }
    assert_eq!(body, br#"{"snippet":1}"#);
}

#[test]
fn a_certificate_from_an_unknown_authority_is_refused() {
    let (_, config) = pki();
    let (other_roots, _) = pki();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut tcp, _) = listener.accept().unwrap();
        let mut conn = ServerConnection::new(config).unwrap();
        // The client aborts the handshake; the server's side of it fails.
        while conn.is_handshaking() {
            if conn.complete_io(&mut tcp).is_err() {
                break;
            }
        }
    });
    let transport = HttpsTransport::with_roots(other_roots)
        .unwrap()
        .with_timeout(Duration::from_secs(10));
    let err = transport
        .send(HttpRequest {
            method: Method::Get,
            url: format!("https://localhost:{port}/"),
            bearer: None,
            body: Body::Empty,
            endpoint: "test",
        })
        .unwrap_err();
    assert!(matches!(err, YoutubeError::Transport { .. }), "{err}");
    server.join().unwrap();
}
