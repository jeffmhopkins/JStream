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
    let (network, identity) = network_config();
    let harness = start_with(&MockEngine::new(), move |config| {
        config.network = Some(network);
        configure(config);
    });
    (harness, identity)
}

/// A network listener's configuration on a free loopback port, with a fresh certificate
/// for [`NAME`] and the loopback address.
fn network_config() -> (NetworkConfig, Arc<Identity>) {
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
        announce: false,
    };
    (network, identity)
}

impl Harness {
    fn network_addr(&self) -> SocketAddr {
        self.switch.info().expect("the network listener is on").addr
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
        self.run(tls_request_to(
            self.network_addr(),
            pin,
            method,
            path,
            headers,
            body,
        ))
    }

    /// A WebSocket session on the network listener over TLS, pinning `pin`, with
    /// `authorization`; the upgrade's status when it is refused.
    fn tls_session(&self, pin: &str, authorization: &str) -> Result<TlsSocket, u16> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::http::HeaderValue;
        let addr = self.network_addr();
        let connector = pinned(pin);
        let mut request = format!("wss://{addr}{}", crate::WS_PATH)
            .into_client_request()
            .expect("request");
        let headers = request.headers_mut();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(authorization).expect("header"),
        );
        headers.insert(
            "Sec-WebSocket-Protocol",
            HeaderValue::from_static("jstream"),
        );
        self.run(async {
            let tcp = tokio::net::TcpStream::connect(addr).await.expect("connect");
            let name = ServerName::try_from(NAME.to_owned()).expect("name");
            let tls = connector.connect(name, tcp).await.expect("tls");
            match tokio_tungstenite::client_async(request, tls).await {
                Ok((socket, _)) => Ok(socket),
                Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                    Err(response.status().as_u16())
                }
                Err(error) => panic!("upgrade: {error}"),
            }
        })
    }

    /// Pairs a device over the network listener and answers its `Authorization`.
    fn pair_over_network(&self, pin: &str) -> String {
        let control = ok(self.connect());
        ok(self.run(control.hello(hello())));
        let code = self.start_pairing(&control);
        let body = serde_json::json!({"code": code, "name": "Stream deck"}).to_string();
        let origin = format!("https://{}", self.network_addr());
        let reply = self
            .tls_request(
                pin,
                "POST",
                crate::PAIR_PATH,
                &[("Content-Type", "application/json"), ("Origin", &origin)],
                &body,
            )
            .expect("tls");
        assert_eq!(reply.status, 200, "{}", reply.body);
        format!("Bearer {}", reply.json()["token"].as_str().expect("token"))
    }
}

/// One HTTP/1.1 request to the network listener at `addr` over TLS, pinning `pin`, with
/// `Host` set to `addr` unless `headers` sets one.
async fn tls_request_to(
    addr: SocketAddr,
    pin: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Result<HttpReply, String> {
    let host = addr.to_string();
    let mut all: Vec<(&str, &str)> = Vec::new();
    if !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("host")) {
        all.push(("Host", &host));
    }
    all.extend_from_slice(headers);
    let connector = pinned(pin);
    let tcp = tokio::net::TcpStream::connect(addr)
        .await
        .map_err(|e| e.to_string())?;
    let name = ServerName::try_from(NAME.to_owned()).map_err(|e| e.to_string())?;
    let mut tls = connector
        .connect(name, tcp)
        .await
        .map_err(|e| e.to_string())?;
    Ok(http_to(&mut tls, method, path, &all, body).await)
}

type TlsSocket =
    tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

/// A TLS connector that pins `pin`, as a native client does.
fn pinned(pin: &str) -> tokio_rustls::TlsConnector {
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
    tokio_rustls::TlsConnector::from(Arc::new(config))
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

/// The host UI's own token (ADR-0131 §2) is the host's alone: the network listener
/// refuses it as it refuses the control token.
#[test]
fn the_network_listener_refuses_the_host_uis_token() {
    const UI: &str = "ui-token-network-1";
    let (harness, identity) = start_network(|config| {
        config.ui_token = match crate::Token::new(UI) {
            Ok(token) => Some(token),
            Err(error) => panic!("ui token: {error}"),
        };
    });
    let bearer = format!("Bearer {UI}");
    let reply = harness
        .tls_request(
            identity.fingerprint(),
            "GET",
            crate::WS_PATH,
            &upgrade_headers(&bearer),
            "",
        )
        .expect("tls");
    assert_eq!(reply.status, 401, "{}", reply.body);
    assert!(reply.body.contains("paired device"), "{}", reply.body);
}

/// ADR-0044's five wrong attempts hold across both listeners: four from the host and
/// one from the network void the code, so the network adds no guesses at it.
#[test]
fn wrong_codes_from_both_listeners_share_the_five_attempts() {
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
    let origin = harness.own_origin();
    let body = serde_json::json!({"code": wrong, "name": "Guess"}).to_string();
    for _ in 1..crate::devices::MAX_WRONG_ATTEMPTS {
        let reply = harness.post_pair(&[("Origin", &origin)], &body);
        assert_eq!(reply.status, 403, "{}", reply.body);
    }
    assert_eq!(post(&wrong), 403);
    // That was the fifth: the right code no longer pairs, from either listener.
    assert_eq!(post(&code), 401);
    let body = serde_json::json!({"code": code, "name": "Host browser"}).to_string();
    let reply = harness.post_pair(&[("Origin", &origin)], &body);
    assert_eq!(reply.status, 401, "{}", reply.body);
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

/// ADR-0234 decision 3: with `ServerConfig::pairing` off, as the desktop app sets it, the
/// server pairs while the network listener is open, so its `/pair` page is served there
/// too, and the UI's challenge points a browser to it (#1745 B1).
#[test]
fn the_pairing_page_is_served_while_the_listener_is_open_with_pairing_off() {
    let (harness, identity) = start_network(|config| config.pairing = false);
    let reply = harness
        .tls_request(
            identity.fingerprint(),
            "GET",
            crate::PAIR_PAGE_PATH,
            &[],
            "",
        )
        .expect("tls");
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(reply.body.contains("fingerprint"), "{}", reply.body);
}

/// 0.97: `pairing.start` names where a device on the network reaches the listener, and
/// the certificate's fingerprint, so the host can show them beside the code; without the
/// network listener it names nothing.
#[test]
fn pairing_start_names_the_network_listener_and_its_fingerprint() {
    let (harness, identity) = start_network(|_| {});
    let control = ok(harness.connect());
    ok(harness.run(control.hello(hello())));
    let (_, network) = harness.start_pairing_with_network(&control);
    let network = network.expect("the network listener is on");
    let port = harness.network_addr().port();
    assert_eq!(
        network.urls,
        vec![
            format!("https://{NAME}:{port}/"),
            format!("https://127.0.0.1:{port}/")
        ]
    );
    assert_eq!(network.fingerprint, identity.fingerprint());

    let harness = start_with(&MockEngine::new(), |_| {});
    let control = ok(harness.connect());
    ok(harness.run(control.hello(hello())));
    assert_eq!(harness.start_pairing_with_network(&control).1, None);
}

/// ADR-0234: the host opens and closes the network listener while the server runs. Closed,
/// nothing connects there and the host pairs no device (pairing off, as the desktop app
/// sets it); opened, a device pairs and connects; closed again, its session ends and it
/// can't connect; reopened, the same device is admitted again.
#[test]
fn the_network_listener_opens_and_closes_while_the_server_runs() {
    let harness = start_with(&MockEngine::new(), |config| config.pairing = false);
    let switch = harness.switch.clone();
    assert_eq!(switch.info(), None);
    let control = ok(harness.connect());
    ok(harness.run(control.hello(hello())));
    let refused = harness.run(control.call(jstream_control::protocol::Call::Action {
        action: jstream_control::protocol::Action::PairingStart,
    }));
    assert!(
        refused.is_err(),
        "paired with the listener closed: {refused:?}"
    );

    let (network, identity) = network_config();
    let pin = identity.fingerprint();
    let info = harness
        .run(switch.open(network.clone()))
        .expect("the listener opens");
    assert_eq!(info.addr, harness.network_addr());
    assert_eq!(info.fingerprint, pin);
    let device = harness.pair_over_network(pin);
    let mut session = harness.tls_session(pin, &device).expect("admitted");

    // Closed: the session ends, and nothing connects there.
    let addr = harness.network_addr();
    harness.run(switch.close());
    assert_eq!(switch.info(), None);
    let ended = harness.run(async {
        use futures_util::StreamExt;
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                match session.next().await {
                    None | Some(Err(_)) => return None,
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(frame))) => {
                        return frame.map(|frame| u16::from(frame.code));
                    }
                    Some(Ok(_)) => {}
                }
            }
        })
        .await
    });
    assert_eq!(
        ended,
        Ok(Some(1001)),
        "the session didn't close as at shutdown"
    );
    assert!(
        harness.run(tokio::net::TcpStream::connect(addr)).is_err(),
        "the closed listener still accepts"
    );
    let refused = harness.run(control.call(jstream_control::protocol::Call::Action {
        action: jstream_control::protocol::Action::PairingStart,
    }));
    assert!(
        refused.is_err(),
        "paired with the listener closed: {refused:?}"
    );

    // Reopened: the paired device is admitted again, and the host pairs again.
    harness
        .run(switch.open(network))
        .expect("the listener reopens");
    assert!(harness.tls_session(pin, &device).is_ok());
    let _ = harness.pair_over_network(pin);
}

/// ADR-0235: a device asks to pair over the network listener; every session hears
/// `pairing.asked` with its name and address; allowed, the held request gets a device
/// token that the listener admits; denied, it gets `403`. Each ask ends with
/// `pairing.ask_ended`, and an ask no one holds can't be answered.
#[test]
fn a_device_that_asks_is_paired_when_the_person_allows_it() {
    use futures_util::StreamExt;
    use jstream_control::protocol::{Action, Call, Event, PairingAskOutcome};
    let (harness, identity) = start_network(|config| config.pairing = false);
    let pin = identity.fingerprint();
    let addr = harness.network_addr();
    let origin = format!("https://{addr}");
    let control = ok(harness.connect());
    ok(harness.run(control.hello(hello())));
    let mut events = control.events();
    let body = serde_json::json!({"name": "Stream Deck"}).to_string();
    let headers = [
        ("Content-Type", "application/json"),
        ("Origin", origin.as_str()),
    ];

    let mut ask = |allow: bool| {
        harness.run(async {
            let asking = tls_request_to(addr, pin, "POST", crate::PAIR_ASK_PATH, &headers, &body);
            let answering = async {
                let ask = loop {
                    match events.next().await {
                        Some(Event::PairingAsked { ask }) => break ask,
                        Some(_) => {}
                        None => panic!("the events ended"),
                    }
                };
                assert_eq!(ask.name, "Stream Deck");
                assert_eq!(ask.address, "127.0.0.1");
                ok(control
                    .call(Call::Action {
                        action: Action::PairingAnswer {
                            ask_id: ask.ask_id.clone(),
                            allow,
                        },
                    })
                    .await);
                loop {
                    match events.next().await {
                        Some(Event::PairingAskEnded { ask_id, outcome }) => {
                            assert_eq!(ask_id, ask.ask_id);
                            break (ask.ask_id, outcome);
                        }
                        Some(_) => {}
                        None => panic!("the events ended"),
                    }
                }
            };
            let (reply, ended) = tokio::join!(asking, answering);
            (reply.expect("tls"), ended)
        })
    };

    let (reply, (ask_id, outcome)) = ask(true);
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(outcome, PairingAskOutcome::Allowed);
    let token = format!("Bearer {}", reply.json()["token"].as_str().expect("token"));
    assert!(
        harness.tls_session(pin, &token).is_ok(),
        "the token isn't admitted"
    );
    let late = harness.run(control.call(Call::Action {
        action: Action::PairingAnswer {
            ask_id,
            allow: true,
        },
    }));
    assert!(late.is_err(), "answered an ask that ended: {late:?}");

    let (reply, (_, outcome)) = ask(false);
    assert_eq!(reply.status, 403, "{}", reply.body);
    assert_eq!(outcome, PairingAskOutcome::Denied);
}
