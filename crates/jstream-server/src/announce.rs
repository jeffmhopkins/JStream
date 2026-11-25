//! Announcing the network listener on the local network (ADR-0233): mDNS / DNS-SD, so a
//! device such as a Stream Deck plugin finds JStream without typing its address.
//!
//! - **What.** One service instance, `<host name>._jstream._tcp.local`: a PTR record from
//!   the service to it, an SRV record with the network listener's port and the host's
//!   `.local` name, and a TXT record `v=1`. No address records: a device connects to the
//!   address the answer came from, which the gate accepts as the address the connection
//!   arrived on. The answer says nothing a device couldn't learn by trying the port.
//! - **When.** Only while the network listener is on and the host asked to announce it
//!   ([`NetworkConfig::announce`](crate::NetworkConfig::announce)). It announces three
//!   times a second apart when it starts and once a minute after (TTL 120 s), answers
//!   queries for the service or the instance, and says goodbye (TTL 0) when the server
//!   stops.
//! - **The socket.** UDP port 5353 on every IPv4 interface, shared with the system's own
//!   responder (address reuse), joined to `224.0.0.251`, as the JTunnel receiver does.
//!   Only a query from the local network is answered (a private, link-local or loopback
//!   source, as `jstream_core::lan::is_lan` decides for JTunnel), so a host beyond the
//!   link that can reach port 5353 learns nothing. Answers go to the group, at most once
//!   a second (RFC 6762 §6); a query from a port other than 5353 (a one-shot "legacy"
//!   query) is also answered to its sender, repeating its question (§6.7). A query is
//!   read with bounds on its size, its questions and the labels walked, so a hostile
//!   datagram costs a few steps.
//! - **Trust.** Finding JStream admits no one: pairing still needs the code and the
//!   certificate fingerprint the host shows (ADR-0104, ADR-0044).

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

/// The DNS-SD service the network listener is announced as.
pub const SERVICE: &str = "_jstream._tcp.local";
/// mDNS's IPv4 group and port.
const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const PORT: u16 = 5353;
/// How long a device may keep the answer.
const TTL_SECS: u32 = 120;
/// How often the instance is announced after the first three.
const EVERY: Duration = Duration::from_secs(60);
/// The largest datagram read or written.
const MAX_DATAGRAM: usize = 1500;
/// The most questions read from one query, and labels walked in one name.
const MAX_QUESTIONS: u16 = 32;
const MAX_LABELS: usize = 64;

const TYPE_PTR: u16 = 12;
const TYPE_TXT: u16 = 16;
const TYPE_SRV: u16 = 33;
const TYPE_ANY: u16 = 255;
const CLASS_IN: u16 = 1;
/// The cache-flush bit on a unique record's class.
const FLUSH: u16 = 0x8000;

/// The least time between two multicast answers (RFC 6762 §6).
const THROTTLE: Duration = Duration::from_secs(1);

/// A query that asks for JStream: its ID and the question that matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    /// The query's ID.
    pub id: u16,
    /// The name asked for, dotted.
    pub name: String,
    /// The record type asked for.
    pub kind: u16,
}

/// Whether `ip` is on the local network: private or link-local IPv4, or loopback (as
/// `jstream_core::lan::is_lan`, which this crate doesn't depend on). Only such a source
/// is answered.
pub fn on_link(ip: IpAddr) -> bool {
    let v4 = match ip {
        IpAddr::V4(v4) => v4,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4,
            None => return v6.is_loopback(),
        },
    };
    v4.is_loopback() || v4.is_private() || v4.is_link_local()
}

/// What is announced: the instance's name and where its listener is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announcement {
    /// The instance's readable name: the host name, as a device lists it.
    pub instance: String,
    /// The host's `.local` name, the SRV record's target.
    pub target: String,
    /// The network listener's port.
    pub port: u16,
}

impl Announcement {
    /// The announcement for `host_name` (without `.local`) and `port`. The instance name is
    /// cut to 63 bytes, a DNS label's limit, on a character boundary.
    pub fn new(host_name: &str, port: u16) -> Announcement {
        let mut instance = host_name.trim().trim_end_matches(".local").to_owned();
        if instance.is_empty() {
            instance = "JStream".to_owned();
        }
        while instance.len() > 63 {
            instance.pop();
        }
        let host: String = instance
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        Announcement {
            target: format!("{host}.local"),
            instance,
            port,
        }
    }

    fn instance_name(&self) -> Vec<&str> {
        let mut labels = vec![self.instance.as_str()];
        labels.extend(SERVICE.split('.'));
        labels
    }

    /// The response datagram: PTR, SRV and TXT, each with `ttl` (0 says goodbye), with
    /// the query's `id` (0 for an unsolicited one).
    pub fn response(&self, id: u16, ttl: u32) -> Vec<u8> {
        self.answer(id, ttl, None)
    }

    /// The answer to a one-shot ("legacy") query, sent to its sender: its ID, and its
    /// question repeated (RFC 6762 §6.7).
    pub fn legacy_response(&self, asked: &Asked) -> Vec<u8> {
        self.answer(asked.id, TTL_SECS.min(10), Some(asked))
    }

    fn answer(&self, id: u16, ttl: u32, question: Option<&Asked>) -> Vec<u8> {
        let mut out = Vec::with_capacity(256);
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&0x8400u16.to_be_bytes()); // response, authoritative
        out.extend_from_slice(&u16::from(question.is_some()).to_be_bytes()); // questions
        out.extend_from_slice(&3u16.to_be_bytes()); // answers
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        if let Some(question) = question {
            let labels: Vec<&str> = question.name.split('.').collect();
            name(&mut out, &labels);
            out.extend_from_slice(&question.kind.to_be_bytes());
            out.extend_from_slice(&CLASS_IN.to_be_bytes());
        }
        let service: Vec<&str> = SERVICE.split('.').collect();
        let instance = self.instance_name();
        // PTR: shared, so no cache-flush bit.
        record(&mut out, &service, TYPE_PTR, CLASS_IN, ttl, |data| {
            name(data, &instance)
        });
        record(
            &mut out,
            &instance,
            TYPE_SRV,
            CLASS_IN | FLUSH,
            ttl,
            |data| {
                data.extend_from_slice(&0u16.to_be_bytes()); // priority
                data.extend_from_slice(&0u16.to_be_bytes()); // weight
                data.extend_from_slice(&self.port.to_be_bytes());
                let target: Vec<&str> = self.target.split('.').collect();
                name(data, &target);
            },
        );
        record(
            &mut out,
            &instance,
            TYPE_TXT,
            CLASS_IN | FLUSH,
            ttl,
            |data| {
                data.push(3);
                data.extend_from_slice(b"v=1");
            },
        );
        out
    }

    /// Whether `query` asks for this service or this instance: its ID and the question.
    /// `None` for a response, a malformed datagram or a query for anything else.
    pub fn asked(&self, query: &[u8]) -> Option<Asked> {
        if query.len() < 12 || query.len() > MAX_DATAGRAM {
            return None;
        }
        let id = u16::from_be_bytes([query[0], query[1]]);
        let flags = u16::from_be_bytes([query[2], query[3]]);
        if flags & 0x8000 != 0 {
            return None; // a response
        }
        let questions = u16::from_be_bytes([query[4], query[5]]).min(MAX_QUESTIONS);
        let instance = self.instance_name().join(".");
        let mut at = 12;
        for _ in 0..questions {
            let (asked, next) = read_name(query, at)?;
            let kind = u16::from_be_bytes([*query.get(next)?, *query.get(next + 1)?]);
            at = next + 4;
            if at > query.len() {
                return None;
            }
            let service =
                asked.eq_ignore_ascii_case(SERVICE) && (kind == TYPE_PTR || kind == TYPE_ANY);
            let mine = asked.eq_ignore_ascii_case(&instance)
                && matches!(kind, TYPE_SRV | TYPE_TXT | TYPE_ANY);
            if service || mine {
                return Some(Asked {
                    id,
                    name: asked,
                    kind,
                });
            }
        }
        None
    }
}

fn name(out: &mut Vec<u8>, labels: &[&str]) {
    for label in labels {
        let bytes = label.as_bytes();
        let len = bytes.len().min(63);
        // `len` is at most 63, so it fits a byte.
        out.push(u8::try_from(len).unwrap_or(63));
        out.extend_from_slice(&bytes[..len]);
    }
    out.push(0);
}

fn record(
    out: &mut Vec<u8>,
    owner: &[&str],
    kind: u16,
    class: u16,
    ttl: u32,
    data: impl FnOnce(&mut Vec<u8>),
) {
    name(out, owner);
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(&class.to_be_bytes());
    out.extend_from_slice(&ttl.to_be_bytes());
    let mut rdata = Vec::new();
    data(&mut rdata);
    let len = u16::try_from(rdata.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&rdata);
}

/// Reads a name at `at`, following compression pointers backwards only, and returns it
/// dotted with the offset after it in place.
fn read_name(msg: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut end = None;
    for _ in 0..MAX_LABELS {
        let len = *msg.get(at)?;
        if len == 0 {
            return Some((labels.join("."), end.unwrap_or(at + 1)));
        }
        if len & 0xC0 == 0xC0 {
            let pointer = usize::from(u16::from_be_bytes([len & 0x3F, *msg.get(at + 1)?]));
            if pointer >= at {
                return None;
            }
            end.get_or_insert(at + 2);
            at = pointer;
            continue;
        }
        if len & 0xC0 != 0 {
            return None;
        }
        let label = msg.get(at + 1..at + 1 + usize::from(len))?;
        labels.push(String::from_utf8_lossy(label).into_owned());
        at += 1 + usize::from(len);
    }
    None
}

/// Opens the shared mDNS socket: port 5353 with address reuse, joined to the group.
fn open() -> std::io::Result<tokio::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    socket.set_reuse_port(true)?;
    socket.bind(&SocketAddr::from(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT)).into())?;
    socket.join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED)?;
    socket.set_multicast_loop_v4(true)?;
    socket.set_multicast_ttl_v4(255)?;
    socket.set_nonblocking(true)?;
    tokio::net::UdpSocket::from_std(socket.into())
}

/// Announces `what` until `stopping` completes, then says goodbye. Logs and returns if
/// the socket can't be opened: the listener works without it, by its address.
pub(crate) async fn run(what: Announcement, stopping: impl Future<Output = ()> + Send) {
    let socket = match open() {
        Ok(socket) => socket,
        Err(error) => {
            tracing::warn!(%error, "could not open mDNS port 5353; devices must type JStream's address");
            return;
        }
    };
    let group = SocketAddr::from(SocketAddrV4::new(GROUP, PORT));
    let hello = what.response(0, TTL_SECS);
    tracing::info!(instance = %what.instance, port = what.port, "announcing the network listener on mDNS as {SERVICE}");
    let mut stopping = std::pin::pin!(stopping);
    let mut buf = vec![0u8; MAX_DATAGRAM];
    let mut sent = 0u32;
    let mut next = tokio::time::Instant::now();
    // When the group last heard the answer, for the throttle.
    let mut answered: Option<tokio::time::Instant> = None;
    loop {
        tokio::select! {
            () = &mut stopping => break,
            () = tokio::time::sleep_until(next) => {
                if let Err(error) = socket.send_to(&hello, group).await {
                    tracing::debug!(%error, "mDNS announcement not sent");
                }
                answered = Some(tokio::time::Instant::now());
                sent = sent.saturating_add(1);
                next += if sent < 3 { Duration::from_secs(1) } else { EVERY };
            }
            received = socket.recv_from(&mut buf) => {
                let Ok((len, from)) = received else { continue };
                // Beyond the link, nothing: not the group answer, not a unicast one.
                if !on_link(from.ip()) {
                    continue;
                }
                let Some(asked) = what.asked(&buf[..len]) else { continue };
                if from.port() != PORT {
                    let _ = socket.send_to(&what.legacy_response(&asked), from).await;
                    continue;
                }
                let now = tokio::time::Instant::now();
                if answered.is_some_and(|at| now.duration_since(at) < THROTTLE) {
                    continue;
                }
                answered = Some(now);
                let _ = socket.send_to(&hello, group).await;
            }
        }
    }
    let _ = socket.send_to(&what.response(0, 0), group).await;
    tracing::info!("mDNS announcement withdrawn");
}

#[cfg(test)]
mod tests {
    use super::{Announcement, SERVICE, on_link, read_name};

    fn query(name: &str, kind: u16, id: u16) -> Vec<u8> {
        let mut q = Vec::new();
        q.extend_from_slice(&id.to_be_bytes());
        q.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        for label in name.split('.') {
            q.push(u8::try_from(label.len()).expect("short"));
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&kind.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        q
    }

    /// A query for the service or the instance is answered; others, and responses, aren't.
    #[test]
    fn answers_only_queries_for_jstream() {
        let a = Announcement::new("Studio PC", 4456);
        let id = |q: &[u8]| a.asked(q).map(|asked| asked.id);
        assert_eq!(id(&query(SERVICE, 12, 7)), Some(7));
        assert_eq!(id(&query("_JSTREAM._tcp.local", 255, 8)), Some(8));
        assert_eq!(id(&query(&format!("Studio PC.{SERVICE}"), 33, 9)), Some(9));
        assert_eq!(a.asked(&query("_http._tcp.local", 12, 1)), None);
        assert_eq!(
            a.asked(&query(SERVICE, 1, 1)),
            None,
            "an A query for the service"
        );
        let mut response = query(SERVICE, 12, 1);
        response[2] = 0x84;
        assert_eq!(a.asked(&response), None, "a response is not a query");
        assert_eq!(a.asked(&[0; 5]), None);
    }

    /// The response names the instance, the port and the target, and parses back.
    #[test]
    fn the_response_carries_ptr_srv_and_txt() {
        let a = Announcement::new("Studio PC", 4456);
        assert_eq!(a.target, "Studio-PC.local");
        let r = a.response(0, 120);
        assert_eq!(&r[6..8], &[0, 3], "three answers");
        let (ptr_owner, after) = read_name(&r, 12).expect("owner");
        assert_eq!(ptr_owner, SERVICE);
        let rdata = after + 10;
        let (instance, _) = read_name(&r, rdata).expect("instance");
        assert_eq!(instance, format!("Studio PC.{SERVICE}"));
        let port = 4456u16.to_be_bytes();
        assert!(r.windows(2).any(|w| w == port), "the port");
        assert!(r.windows(3).any(|w| w == b"v=1"));
        let goodbye = a.response(0, 0);
        assert_eq!(
            &goodbye[after + 4..after + 8],
            &[0, 0, 0, 0],
            "TTL 0 says goodbye"
        );
    }

    /// A hostile name can't loop or read past the end.
    #[test]
    fn a_hostile_name_is_refused() {
        let a = Announcement::new("x", 1);
        // A pointer to itself.
        let mut q = vec![0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0xC0, 12, 0, 12, 0, 1];
        assert_eq!(a.asked(&q), None);
        // A label longer than the datagram.
        q.truncate(12);
        q.extend_from_slice(&[40, b'a']);
        assert_eq!(a.asked(&q), None);
    }

    /// A long or odd host name still makes a valid label and target.
    #[test]
    fn names_are_cut_to_a_label() {
        let a = Announcement::new(&"é".repeat(40), 1);
        assert!(a.instance.len() <= 63);
        assert!(a.target.ends_with(".local"));
        assert_eq!(Announcement::new("  ", 1).instance, "JStream");
        assert_eq!(Announcement::new("studio.local", 1).instance, "studio");
    }

    /// Only the local network is answered: an off-link source learns nothing (#1799 B1).
    #[test]
    fn only_the_local_network_is_answered() {
        for lan in [
            "192.168.1.20",
            "10.0.0.5",
            "172.16.3.4",
            "169.254.9.9",
            "127.0.0.1",
            "::1",
        ] {
            assert!(on_link(lan.parse().expect("ip")), "{lan}");
        }
        for off in [
            "8.8.8.8",
            "203.0.113.7",
            "100.64.0.1",
            "2001:db8::1",
            "fe80::1",
        ] {
            assert!(!on_link(off.parse().expect("ip")), "{off}");
        }
        assert!(on_link("::ffff:192.168.1.20".parse().expect("ip")));
    }

    /// A one-shot query's answer carries its ID and repeats its question (RFC 6762 §6.7).
    #[test]
    fn a_legacy_answer_repeats_the_question() {
        let a = Announcement::new("Studio PC", 4456);
        let asked = a.asked(&query(SERVICE, 12, 0x1234)).expect("asked");
        let r = a.legacy_response(&asked);
        assert_eq!(&r[0..2], &[0x12, 0x34]);
        assert_eq!(&r[4..6], &[0, 1], "one question");
        assert_eq!(&r[6..8], &[0, 3], "three answers");
        let (question, after) = read_name(&r, 12).expect("question");
        assert_eq!(question, SERVICE);
        assert_eq!(&r[after..after + 4], &[0, 12, 0, 1], "PTR, IN");
        let (owner, _) = read_name(&r, after + 4).expect("first answer");
        assert_eq!(owner, SERVICE);
    }
}
