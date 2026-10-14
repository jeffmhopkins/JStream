//! Hermetic test transports (docs/RULES.md#R-TEST-004): in-memory pipes, and TLS over
//! loopback TCP with certificates generated at test time.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle};

use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{
    RootCertStore, ServerConfig, ServerConnection, StreamOwned, SupportedProtocolVersion,
};

/// Replays fixed input and records everything written.
pub(crate) struct Scripted {
    input: io::Cursor<Vec<u8>>,
    output: Vec<u8>,
}

impl Scripted {
    pub(crate) fn new(input: Vec<u8>) -> Self {
        Self {
            input: io::Cursor::new(input),
            output: Vec::new(),
        }
    }

    pub(crate) fn written(&self) -> &[u8] {
        &self.output
    }

    pub(crate) fn fully_read(&self) -> bool {
        self.input.position() as usize == self.input.get_ref().len()
    }
}

impl Read for Scripted {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.input.read(buf)
    }
}

impl Write for Scripted {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One end of an in-memory, blocking, full-duplex byte pipe.
///
/// Reads block until the other end writes or is dropped (then they return end of
/// stream). With `nonblocking`, a read with nothing pending fails with `WouldBlock`,
/// like a socket with a read timeout.
pub(crate) struct Duplex {
    tx: Sender<Vec<u8>>,
    rx: Receiver<Vec<u8>>,
    pending: Vec<u8>,
    pub(crate) nonblocking: bool,
}

/// Two connected [`Duplex`] ends.
pub(crate) fn duplex() -> (Duplex, Duplex) {
    let (a_tx, b_rx) = channel();
    let (b_tx, a_rx) = channel();
    let end = |tx, rx| Duplex {
        tx,
        rx,
        pending: Vec::new(),
        nonblocking: false,
    };
    (end(a_tx, a_rx), end(b_tx, b_rx))
}

impl Read for Duplex {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pending.is_empty() {
            let next = if self.nonblocking {
                match self.rx.try_recv() {
                    Ok(v) => Some(v),
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        return Err(io::ErrorKind::WouldBlock.into());
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
                }
            } else {
                self.rx.recv().ok()
            };
            match next {
                Some(v) => self.pending = v,
                None => return Ok(0),
            }
        }
        let n = buf.len().min(self.pending.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }
}

impl Write for Duplex {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tx
            .send(buf.to_vec())
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A TLS test authority generated at test time: a CA and a server certificate it signed.
/// Nothing is read from the operating system's trust store.
pub(crate) struct TestPki {
    /// Roots that trust the CA, for the client.
    pub(crate) roots: RootCertStore,
    /// The server certificate chain (leaf, then CA).
    chain: Vec<CertificateDer<'static>>,
    /// The server certificate's key, PKCS#8 DER.
    key_der: Vec<u8>,
}

impl TestPki {
    /// A fresh CA, and a server certificate from it for the DNS names `names`.
    pub(crate) fn new(names: &[&str]) -> Self {
        Self::with_ca_name("JStream test CA", names)
    }

    /// [`new`](Self::new) with the CA's common name `ca_name`. Two authorities need
    /// different names, or a verifier looking for one finds the other's certificate.
    pub(crate) fn with_ca_name(ca_name: &str, names: &[&str]) -> Self {
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, ca_name);
        let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().unwrap()).unwrap();
        let leaf_key = KeyPair::generate().unwrap();
        let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
        let leaf = CertificateParams::new(names)
            .unwrap()
            .signed_by(&leaf_key, &ca)
            .unwrap();
        let mut roots = RootCertStore::empty();
        roots.add(ca.der().clone()).unwrap();
        Self {
            roots,
            chain: vec![leaf.der().clone(), ca.der().clone()],
            key_der: leaf_key.serialize_der(),
        }
    }

    /// A server configuration on the `ring` provider that presents the server
    /// certificate and speaks only `versions`.
    pub(crate) fn server_config(
        &self,
        versions: &[&'static SupportedProtocolVersion],
    ) -> Arc<ServerConfig> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let key = PrivateKeyDer::Pkcs8(self.key_der.clone().into());
        let config = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(versions)
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(self.chain.clone(), key)
            .unwrap();
        Arc::new(config)
    }
}

/// A TLS server stream over TCP, as a test server sees it.
pub(crate) type TlsServerStream = StreamOwned<ServerConnection, TcpStream>;

/// Listens on a loopback port and, on another thread, accepts one connection, runs the
/// TLS handshake with `config` and hands the stream to `serve`. Returns the port and the
/// thread, whose result is `serve`'s, or the accept or handshake error.
pub(crate) fn tls_server<T: Send + 'static>(
    config: Arc<ServerConfig>,
    serve: impl FnOnce(TlsServerStream) -> T + Send + 'static,
) -> (u16, JoinHandle<io::Result<T>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (mut tcp, _) = listener.accept()?;
        let mut conn = ServerConnection::new(config).map_err(io::Error::other)?;
        while conn.is_handshaking() {
            conn.complete_io(&mut tcp)?;
        }
        Ok(serve(StreamOwned::new(conn, tcp)))
    });
    (port, handle)
}
