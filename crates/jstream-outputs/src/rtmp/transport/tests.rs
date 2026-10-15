//! Transport tests over loopback TCP, with TLS certificates generated at test time
//! (docs/RULES.md#R-TEST-004).

use std::net::{TcpListener, TcpStream};
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

/// Reads `tcp` a trickle at a time (64 KiB every 60 ms, raw bytes even under TLS)
/// until `stop` fires or the client hangs up.
fn trickle(tcp: &TcpStream, stop: &std::sync::mpsc::Receiver<()>) {
    tcp.set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let mut buf = vec![0u8; 64 * 1024];
    while stop.try_recv().is_err() {
        match (&*tcp).read(&mut buf) {
            Ok(0) => break,
            Ok(_) => thread::sleep(Duration::from_millis(60)),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
}

/// Writes 32 MiB with a write deadline 500 ms out to a peer that reads a trickle, and
/// returns how long the write took to fail. Without the deadline each socket write's
/// timeout restarts on every bit of progress, so this would run for half a minute.
fn write_past_a_deadline(t: &mut Transport) -> Duration {
    t.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
    let started = std::time::Instant::now();
    t.set_write_deadline(Some(started + Duration::from_millis(500)));
    let err = t
        .write_all(&vec![0u8; 32 * 1024 * 1024])
        .and_then(|()| t.flush())
        .expect_err("a trickle-reading peer took 32 MiB before the deadline");
    assert!(
        matches!(
            err.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ),
        "{err:?}"
    );
    started.elapsed()
}

/// #138: the write deadline bounds a whole large write, not each socket write.
#[test]
fn a_write_deadline_bounds_a_large_write_over_tcp() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, stopped) = std::sync::mpsc::channel();
    let server = thread::spawn(move || trickle(&listener.accept().unwrap().0, &stopped));

    let mut t = connect(&url("rtmp", "127.0.0.1", port), TIMEOUT).unwrap();
    let took = write_past_a_deadline(&mut t);
    let _ = stop.send(());
    drop(t);
    server.join().unwrap();
    assert!(
        took < Duration::from_secs(4),
        "the write failed after {took:?}"
    );
}

/// The same over TLS, where one write of the plaintext is many socket writes inside
/// rustls.
#[test]
fn a_write_deadline_bounds_a_large_write_over_tls() {
    let pki = TestPki::new(&["localhost"]);
    let (stop, stopped) = std::sync::mpsc::channel();
    let (port, server) = tls_server(pki.server_config(&[&TLS13]), move |s| {
        trickle(&s.sock, &stopped);
    });
    let config = client_config(pki.roots.clone()).unwrap();
    let mut t = connect_tls(&url("rtmps", "localhost", port), TIMEOUT, config).unwrap();
    let took = write_past_a_deadline(&mut t);
    let _ = stop.send(());
    drop(t);
    server.join().unwrap().unwrap();
    assert!(
        took < Duration::from_secs(4),
        "the write failed after {took:?}"
    );
}

/// #209: `shutdown`'s TLS flush goes through the write deadline too. Past the deadline
/// `close_notify` is not sent and the shutdown fails at once, where it used to be sent
/// with only the socket's write timeout to bound it. #236: the socket is still shut
/// down, so the server sees the stream end without `close_notify` while the client
/// still holds the transport. #243: that end is a FIN on every platform, though the
/// client has left the server's TLS 1.3 session tickets unread; a shutdown in both
/// directions makes Windows abort the connection instead.
#[test]
fn shutdown_over_tls_honours_the_write_deadline() {
    let pki = TestPki::new(&["localhost"]);
    let (ready, server_ready) = std::sync::mpsc::channel();
    let (port, server) = tls_server(pki.server_config(&[&TLS13]), move |mut s| {
        // A timeout, not a hang, if the client never shuts the socket down.
        s.sock
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let _ = ready.send(());
        let mut rest = Vec::new();
        s.read_to_end(&mut rest).map_err(|e| e.kind())
    });
    let config = client_config(pki.roots.clone()).unwrap();
    let mut t = connect_tls(&url("rtmps", "localhost", port), TIMEOUT, config).unwrap();
    // macOS refuses `setsockopt` with EINVAL once the peer has shut down, so the
    // server's timeout must be set before the client shuts down.
    server_ready.recv().unwrap();
    t.set_write_deadline(Some(std::time::Instant::now()));
    let err = t
        .shutdown()
        .expect_err("close_notify was sent past the deadline");
    assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err:?}");
    // Held until the server has read the end, so whatever end it sees came from the
    // shutdown, not from a drop.
    let ended = server.join().unwrap().unwrap();
    drop(t);
    // A clean TLS close reads as `Ok`, no shutdown as the read timeout, and an abort
    // as `ConnectionAborted` or `ConnectionReset`. The shutdown's FIN, with no
    // `close_notify`, is `UnexpectedEof`.
    assert_eq!(ended, Err(io::ErrorKind::UnexpectedEof));
}

/// #243: `shutdown` closes only the sending side, so the client can still read what the
/// server sends after the client's `close_notify`, here a last message and the
/// server's own `close_notify`. Shut down in both directions, the read finds the end
/// of stream at once (Linux, macOS) or fails (Windows), and the server's
/// `close_notify` never arrives.
#[test]
fn shutdown_leaves_the_server_s_reply_readable() {
    let pki = TestPki::new(&["localhost"]);
    let (port, server) = tls_server(pki.server_config(&[&TLS13]), |mut s| {
        s.sock
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut rest = Vec::new();
        let ended = s.read_to_end(&mut rest).map_err(|e| e.kind());
        // Lets the client reach its read first, so the reply arrives at a socket
        // already shut down, not one with the reply queued.
        thread::sleep(Duration::from_millis(100));
        let sent = s.write_all(b"bye").and_then(|()| {
            s.conn.send_close_notify();
            s.flush()
        });
        (ended, sent.map_err(|e| e.kind()))
    });
    let config = client_config(pki.roots.clone()).unwrap();
    let mut t = connect_tls(&url("rtmps", "localhost", port), TIMEOUT, config).unwrap();
    t.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    t.shutdown().unwrap();
    let mut reply = Vec::new();
    let read = t.read_to_end(&mut reply).map_err(|e| e.kind());
    // The server's reply, then its close_notify: a clean end of stream.
    assert_eq!(read, Ok(3), "the client read {reply:?}");
    assert_eq!(reply, b"bye");
    let (ended, sent) = server.join().unwrap().unwrap();
    // The client's close_notify, then its FIN: a clean end of stream too.
    assert_eq!(ended, Ok(0));
    // Linux resets a connection whose receiving side is shut down when more data
    // arrives, so with that side open the reply reaches the client whole.
    assert_eq!(sent, Ok(()));
}
