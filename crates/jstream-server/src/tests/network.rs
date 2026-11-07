//! The network listener (ADR-0104) end to end: TLS on a loopback port the gate treats as
//! the network's, a client that pins the certificate's fingerprint as a native client
//! does, and the pairing exchange over it. Hermetic: loopback sockets only (R-TEST-004).

use super::{Harness, HttpReply, TOKEN, hello, http_to, ok, start_with};
use crate::identity::Identity;
use crate::{NetworkConfig, ServerConfig};
use jstream_control::Control;
use jstream_control::MockEngine;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

/// The name the test certificate carries besides the loopback address.
const NAME: &str = "studio-test.local";

/// A pin, as a native client keeps one: the certificate's SHA-256 fingerprint. It also
/// records the fingerprint it was offered, as such a client shows it before pairing.
#[derive(Debug)]
struct Pin {
    expected: String,
    offered: Mutex<Option<String>>,
}

impl ServerCertVerifier for Pin {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let digest = ring::digest::digest(&ring::digest::SHA256, end_entity.as_ref());
        let offered = digest
            .as_ref()
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":");
        let matches = offered == self.expected;
        *self
            .offered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(offered);
        if matches {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "the certificate is not the pinned one".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// A server with the network listener on `127.0.0.1:0`, and its identity.
fn start_network(configure: impl FnOnce(&mut ServerConfig)) -> (Harness, Arc<Identity>) {
    let dir = std::env::temp_dir().join(format!(
        "jstream-network-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let names = vec![NAME.to_owned(), "127.0.0.1".to_owned()];
    let (identity, _) = Identity::load_or_create(&dir.join("token.tls"), &names).expect("identity");
    let identity = Arc::new(identity);
    let network = NetworkConfig {
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        identity: Arc::clone(&identity),
    };
    let harness = start_with(&MockEngine::new(), move |config| {
        config.network = Some(network);
        configure(config);
    });
    (harness, identity)
}

impl Harness {
    fn network_addr(&self) -> SocketAddr {
        self.network.expect("the network listener is on")
    }

    /// One HTTP/1.1 request to the network listener over TLS, pinning `pin`, with `Host`
    /// set to the listener's address unless `headers` sets one.
    fn tls_request(
        &self,
        pin: &str,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Result<HttpReply, String> {
        let addr = self.network_addr();
        let host = addr.to_string();
        let mut all: Vec<(&str, &str)> = Vec::new();
        if !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("host")) {
            all.push(("Host", &host));
        }
        all.extend_from_slice(headers);
        let verifier = Arc::new(Pin {
            expected: pin.to_owned(),
            offered: Mutex::new(None),
        });
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("versions")
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        self.run(async {
            let tcp = tokio::net::TcpStream::connect(addr)
                .await
                .map_err(|e| e.to_string())?;
            let name = ServerName::try_from(NAME.to_owned()).map_err(|e| e.to_string())?;
            let mut tls = connector
                .connect(name, tcp)
                .await
                .map_err(|e| e.to_string())?;
            Ok(http_to(&mut tls, method, path, &all, body).await)
        })
    }
}

fn upgrade_headers(authorization: &str) -> Vec<(&str, &str)> {
    vec![
        ("Authorization", authorization),
        ("Connection", "Upgrade"),
        ("Upgrade", "websocket"),
        ("Sec-WebSocket-Version", "13"),
        ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ("Sec-WebSocket-Protocol", "jstream"),
    ]
}

/// ADR-0104 §2 and §3: the network listener speaks TLS only, serves the certificate the
/// client pinned, refuses the control token, and admits a device paired over it.
#[test]
fn the_network_listener_is_tls_only_and_takes_device_tokens_only() {
    let (harness, identity) = start_network(|_| {});
    let pin = identity.fingerprint();

    // Plain HTTP gets no HTTP answer: the listener only speaks TLS.
    let plain = harness.run(async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut tcp = tokio::net::TcpStream::connect(harness.network_addr())
            .await
            .expect("connect");
        let request = format!(
            "GET /pair HTTP/1.1\r\nHost: {}\r\n\r\n",
            harness.network_addr()
        );
        let _ = tcp.write_all(request.as_bytes()).await;
        let mut buf = Vec::new();
        let _ = tcp.read_to_end(&mut buf).await;
        buf
    });
    assert!(!plain.starts_with(b"HTTP/"), "answered plain HTTP");

    // Another certificate fails the pin before anything is sent.
    let other = "00:".repeat(31) + "00";
    assert!(
        harness
            .tls_request(&other, "GET", "/pair", &[], "")
            .is_err()
    );

    // The control token is refused here, though it is right.
    let bearer = format!("Bearer {TOKEN}");
    let reply = harness
        .tls_request(pin, "GET", crate::WS_PATH, &upgrade_headers(&bearer), "")
        .expect("tls");
    assert_eq!(reply.status, 401, "{}", reply.body);
    assert!(reply.body.contains("paired device"), "{}", reply.body);

    // A foreign Host is refused; the certificate's names and the address are not.
    let reply = harness
        .tls_request(pin, "GET", "/pair", &[("Host", "evil.example")], "")
        .expect("tls");
    assert_eq!(reply.status, 403);
    let named = format!("{NAME}:{}", harness.network_addr().port());
    for host in [named.as_str(), &harness.network_addr().to_string()] {
        let reply = harness
            .tls_request(pin, "GET", "/pair", &[("Host", host)], "")
            .expect("tls");
        assert_eq!(reply.status, 200, "{host}: {}", reply.body);
    }

    // Pair over the network: the code comes from the host.
    let control = ok(harness.connect());
    ok(harness.run(control.hello(hello())));
    let code = harness.start_pairing(&control);
    let body = serde_json::json!({"code": code, "name": "Stream deck"}).to_string();
    let origin = format!("https://{}", harness.network_addr());
    let reply = harness
        .tls_request(
            pin,
            "POST",
            crate::PAIR_PATH,
            &[("Content-Type", "application/json"), ("Origin", &origin)],
            &body,
        )
        .expect("tls");
    assert_eq!(reply.status, 200, "{}", reply.body);
    let token = reply.json()["token"].as_str().expect("token").to_owned();
    let device = format!("Bearer {token}");
    let reply = harness
        .tls_request(pin, "GET", crate::WS_PATH, &upgrade_headers(&device), "")
        .expect("tls");
    assert_eq!(reply.status, 101, "{}", reply.body);
}

/// #1129 N1: a network peer that guesses wrong five times is refused for that code,
/// and the code stays open for the host and for other devices.
#[test]
fn a_network_peer_cant_void_the_hosts_code() {
    let (harness, identity) = start_network(|_| {});
    let pin = identity.fingerprint();
    let control = ok(harness.connect());
    ok(harness.run(control.hello(hello())));
    let code = harness.start_pairing(&control);
    let wrong = if code.ends_with('0') {
        format!("{}1", &code[..8])
    } else {
        format!("{}0", &code[..8])
    };
    let post = |code: &str| {
        let body = serde_json::json!({"code": code, "name": "Guess"}).to_string();
        harness
            .tls_request(
                pin,
                "POST",
                crate::PAIR_PATH,
                &[("Content-Type", "application/json")],
                &body,
            )
            .expect("tls")
            .status
    };
    for _ in 0..crate::devices::MAX_WRONG_PER_PEER {
        assert_eq!(post(&wrong), 403);
    }
    assert_eq!(post(&code), 429);
    // The host's own listener still pairs with the same code.
    harness.pair(&code, "Host browser");
}

/// ADR-0047 on the network listener: the pairing page is served there, though the
/// host's own listener is plain HTTP and serves none.
#[test]
fn the_pairing_page_is_served_on_the_network_listener() {
    let (harness, identity) = start_network(|_| {});
    let reply = harness
        .tls_request(
            identity.fingerprint(),
            "GET",
            crate::PAIR_PAGE_PATH,
            &[],
            "",
        )
        .expect("tls");
    assert_eq!(reply.status, 200);
    assert!(reply.body.contains("fingerprint"), "{}", reply.body);
    let local = harness.http("GET", &[], "");
    assert_ne!(local.status, 200);
}
