//! The network listener's certificate (ADR-0104 §4 to §6): an ECDSA P-256 key and a
//! self-signed certificate, made on the host the first time network access is on, and
//! kept until the person renews it.
//!
//! - **The file.** Key and certificate live in one user-only file (beside the token
//!   file, [`identity_file`]), checked and written as the token and device files are:
//!   mode 0600 on Unix, a protected DACL on Windows, and one other users may access is
//!   refused. It is JSON: `{"version": 1, "names": […], "cert": "<hex DER>", "key":
//!   "<hex PKCS#8 DER>"}`. The key never leaves the file and the process: it is in no
//!   log, event, result or snapshot, and the [`Identity`]'s `Debug` form leaves it out
//!   (R-SEC-001).
//! - **Names.** The certificate names the machine's host name, its `.local` name and
//!   the addresses it is reached on, as the host gives them ([`machine_names`]). They
//!   are fixed when it is made. [`Identity::missing`] says which of today's names it
//!   doesn't carry, so the host can say the addresses changed; nothing renews it
//!   silently.
//! - **Trust.** Clients trust the certificate by its SHA-256 fingerprint
//!   ([`Identity::fingerprint`]), written as a browser's certificate viewer writes it:
//!   32 upper-case hex pairs joined by colons. A renewal ([`Identity::renew`]) makes a
//!   new key, so a new fingerprint, and every client that pinned the old one has to
//!   pair again.
//! - **Lifetime.** The certificate is valid from 2025 to the end of 2049: it is trusted
//!   by its pin, not by its dates, and is replaced only by a renewal.

use crate::devices::{read_private, write_private};
use crate::error::ServerError;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use serde_json::{Value, json};
use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

/// The identity file's format version.
const FILE_VERSION: u64 = 1;

/// The certificate's common name. Clients go by its fingerprint, not by this.
const COMMON_NAME: &str = "JStream on this computer";

/// The network listener's key and certificate.
pub struct Identity {
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
    names: Vec<String>,
    fingerprint: String,
}

/// Whether [`Identity::load_or_create`] read an existing file or made a new one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Made {
    /// A new key and certificate were made and written.
    Created,
    /// The file's key and certificate were read.
    Existing,
}

/// The file beside the token file that keeps the network listener's key and
/// certificate: the token file's name with `.tls` added (`token` gives `token.tls`).
pub fn identity_file(token_file: &Path) -> PathBuf {
    let mut name = token_file.as_os_str().to_owned();
    name.push(".tls");
    PathBuf::from(name)
}

impl Identity {
    /// Reads the identity in `path`, or makes one naming `names` and writes it there if
    /// there is none. An existing one is kept whatever names it carries: compare them
    /// with [`missing`](Identity::missing).
    ///
    /// # Errors
    /// [`ServerError::Identity`] when the file can't be read, is broken or other users
    /// can access it, or a new one can't be made or written.
    pub fn load_or_create(path: &Path, names: &[String]) -> Result<(Identity, Made), ServerError> {
        let unusable = |reason: String| ServerError::Identity {
            path: path.to_path_buf(),
            reason,
        };
        match read_private(path).map_err(unusable)? {
            Some(text) => Ok((parse(&text).map_err(unusable)?, Made::Existing)),
            None => Ok((Identity::renew(path, names)?, Made::Created)),
        }
    }

    /// Makes a new key and certificate naming `names`, and writes them to `path` in
    /// place of what is there (ADR-0104 §6). Every client that pinned the old
    /// fingerprint has to pair again.
    ///
    /// # Errors
    /// [`ServerError::Identity`] when it can't be made or written. The old file is then
    /// left as it was.
    pub fn renew(path: &Path, names: &[String]) -> Result<Identity, ServerError> {
        let failed = |reason: String| ServerError::Identity {
            path: path.to_path_buf(),
            reason,
        };
        let identity = make(names).map_err(failed)?;
        write_private(path, &render(&identity))
            .map_err(|error| failed(format!("could not write it: {error}")))?;
        Ok(identity)
    }

    /// The certificate's SHA-256 fingerprint, as `AB:CD:…` (32 pairs).
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// The names and addresses the certificate carries.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Which of `names` the certificate doesn't carry (compared without regard to ASCII
    /// case): when the machine's names or addresses changed since it was made.
    pub fn missing(&self, names: &[String]) -> Vec<String> {
        names
            .iter()
            .filter(|name| !self.names.iter().any(|own| own.eq_ignore_ascii_case(name)))
            .cloned()
            .collect()
    }

    /// The certificate, for the TLS listener.
    pub(crate) fn cert(&self) -> CertificateDer<'static> {
        self.cert.clone()
    }

    /// The private key, for the TLS listener.
    pub(crate) fn key(&self) -> PrivatePkcs8KeyDer<'static> {
        self.key.clone_key()
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("names", &self.names)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

/// The names a certificate for this machine carries, and that the network listener's
/// `Host` check accepts: the host name and its `.local` name, then the addresses: `bind`
/// when the listener binds one address, else the machine's preferred outgoing IPv4 and
/// IPv6 addresses (what the OS would send from to reach the internet; no packet is
/// sent). Best effort: a name the OS doesn't give is left out.
pub fn machine_names(bind: IpAddr) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(host) = host_name() {
        let lower = host.to_ascii_lowercase();
        if !lower.contains('.') {
            names.push(format!("{lower}.local"));
        }
        names.insert(0, lower);
    }
    let addresses = if bind.is_unspecified() {
        outgoing_addresses()
    } else {
        vec![bind]
    };
    names.extend(addresses.iter().map(IpAddr::to_string));
    names.dedup();
    names
}

/// The machine's host name: `COMPUTERNAME` on Windows, the kernel's on Linux, else what
/// the `hostname` command prints (macOS and the rest).
fn host_name() -> Option<String> {
    let valid = |name: String| {
        let name = name.trim().to_owned();
        let ok = !name.is_empty()
            && name.len() <= 253
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
        ok.then_some(name)
    };
    if cfg!(windows)
        && let Some(name) = std::env::var("COMPUTERNAME").ok().and_then(valid)
    {
        return Some(name);
    }
    if let Some(name) = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .and_then(valid)
    {
        return Some(name);
    }
    let output = std::process::Command::new("hostname")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
        .and_then(valid)
}

/// The addresses the OS would send from to reach the internet, IPv4 then IPv6: a UDP
/// socket "connected" to a documentation address (RFC 5737, RFC 3849) learns its local
/// address from the routing table, and sends nothing.
fn outgoing_addresses() -> Vec<IpAddr> {
    let probe = |bind: &str, to: &str| {
        let socket = std::net::UdpSocket::bind(bind).ok()?;
        socket.connect(to).ok()?;
        let ip = socket.local_addr().ok()?.ip();
        (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
    };
    [
        probe("0.0.0.0:0", "192.0.2.1:9"),
        probe("[::]:0", "[2001:db8::1]:9"),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// A new key and a self-signed certificate naming `names`.
fn make(names: &[String]) -> Result<Identity, String> {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .map_err(|error| format!("could not make a key: {error}"))?;
    let mut params = rcgen::CertificateParams::new(names.to_vec())
        .map_err(|error| format!("could not name the certificate: {error}"))?;
    let mut subject = rcgen::DistinguishedName::new();
    subject.push(rcgen::DnType::CommonName, COMMON_NAME);
    params.distinguished_name = subject;
    params.not_before = rcgen::date_time_ymd(2025, 1, 1);
    params.not_after = rcgen::date_time_ymd(2049, 12, 31);
    let cert = params
        .self_signed(&key)
        .map_err(|error| format!("could not sign the certificate: {error}"))?;
    let cert = cert.der().clone();
    Ok(Identity {
        fingerprint: fingerprint(&cert),
        cert,
        key: PrivatePkcs8KeyDer::from(key.serialize_der()),
        names: names.to_vec(),
    })
}

/// SHA-256 of the certificate's DER, as `AB:CD:…`.
fn fingerprint(cert: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, cert);
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn render(identity: &Identity) -> String {
    let mut text = json!({
        "version": FILE_VERSION,
        "names": identity.names,
        "cert": hex(identity.cert.as_ref()),
        "key": hex(identity.key.secret_pkcs8_der()),
    })
    .to_string();
    text.push('\n');
    text
}

fn parse(text: &str) -> Result<Identity, String> {
    let value: Value = serde_json::from_str(text).map_err(|_| "it is not JSON".to_owned())?;
    if value.get("version").and_then(Value::as_u64) != Some(FILE_VERSION) {
        return Err(format!("it is not a version {FILE_VERSION} identity file"));
    }
    let broken = || "it is incomplete or malformed".to_owned();
    let bytes = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .and_then(unhex)
            .ok_or_else(broken)
    };
    let names = value
        .get("names")
        .and_then(Value::as_array)
        .ok_or_else(broken)?
        .iter()
        .map(|name| name.as_str().map(str::to_owned).ok_or_else(broken))
        .collect::<Result<Vec<_>, _>>()?;
    let cert = CertificateDer::from(bytes("cert")?);
    let key = PrivatePkcs8KeyDer::from(bytes("key")?);
    // A key that can't be read would fail every handshake: refuse it now, with the file
    // named. The listener checks that it matches the certificate (`network::acceptor`).
    rcgen::KeyPair::try_from(key.secret_pkcs8_der())
        .map_err(|_| "its key can't be read".to_owned())?;
    Ok(Identity {
        fingerprint: fingerprint(&cert),
        cert,
        key,
        names,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Identity, Made, identity_file, machine_names, unhex};
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("jstream-identity-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("token.tls")
    }

    fn names() -> Vec<String> {
        vec![
            "studio".into(),
            "studio.local".into(),
            "192.168.1.20".into(),
        ]
    }

    #[test]
    fn the_identity_file_sits_beside_the_token_file() {
        assert_eq!(
            identity_file(Path::new("/data/token")),
            PathBuf::from("/data/token.tls")
        );
    }

    /// ADR-0104 §4 and §6: made once, user-only, then reused with the same fingerprint
    /// until a renewal makes a new one.
    #[test]
    fn it_is_made_once_and_kept_until_renewed() {
        let path = scratch("kept");
        let (made, how) = Identity::load_or_create(&path, &names()).expect("made");
        assert_eq!(how, Made::Created);
        assert_eq!(made.names(), names().as_slice());
        let pairs: Vec<&str> = made.fingerprint().split(':').collect();
        assert_eq!(pairs.len(), 32);
        assert!(pairs.iter().all(|pair| {
            pair.len() == 2
                && pair
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
        }));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("file").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "user-only");
        }

        let (again, how) = Identity::load_or_create(&path, &["elsewhere".into()]).expect("read");
        assert_eq!(how, Made::Existing);
        assert_eq!(again.fingerprint(), made.fingerprint());
        assert_eq!(
            again.missing(&["Studio".into(), "10.0.0.5".into()]),
            ["10.0.0.5"]
        );

        let renewed = Identity::renew(&path, &["10.0.0.5".into()]).expect("renewed");
        assert_ne!(renewed.fingerprint(), made.fingerprint());
        let (read, _) = Identity::load_or_create(&path, &[]).expect("read");
        assert_eq!(read.fingerprint(), renewed.fingerprint());
    }

    /// R-SEC-001: the key is in no `Debug` form.
    #[test]
    fn debug_leaves_the_key_out() {
        let path = scratch("debug");
        let (made, _) = Identity::load_or_create(&path, &names()).expect("made");
        let shown = format!("{made:?}");
        let text = std::fs::read_to_string(&path).expect("file");
        let value: serde_json::Value = serde_json::from_str(&text).expect("json");
        let key = value["key"].as_str().expect("key");
        assert!(!shown.contains(key));
        assert!(shown.contains(made.fingerprint()));
    }

    #[test]
    fn broken_or_shared_files_are_refused() {
        let path = scratch("broken");
        let write = |text: &str| {
            let _ = std::fs::remove_file(&path);
            crate::devices::write_private(&path, text).expect("written");
        };
        for text in [
            "not json",
            r#"{"version": 2}"#,
            r#"{"version": 1, "names": [], "cert": "zz", "key": "00"}"#,
            r#"{"version": 1, "names": [], "cert": "00", "key": "0001"}"#,
        ] {
            write(text);
            assert!(Identity::load_or_create(&path, &names()).is_err(), "{text}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::remove_file(&path);
            Identity::load_or_create(&path, &names()).expect("made");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
            let error = Identity::load_or_create(&path, &names()).expect_err("shared");
            assert!(error.to_string().contains("other users"), "{error}");
        }
    }

    #[test]
    fn a_bound_address_is_the_one_named() {
        let names = machine_names(IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)));
        assert_eq!(names.last().map(String::as_str), Some("10.1.2.3"));
        assert!(!names.iter().any(|name| name == "0.0.0.0"));
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(unhex("00ff10"), Some(vec![0, 255, 16]));
        assert_eq!(unhex("0"), None);
        assert_eq!(unhex("zz"), None);
    }
}
