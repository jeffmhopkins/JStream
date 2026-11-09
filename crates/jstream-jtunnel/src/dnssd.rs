//! Finding JTunnel senders: the mDNS / DNS-SD query and the answers to it, and the list
//! of senders seen.
//!
//! A sender announces itself every 10 s, and answers queries, as
//! `<tunnel name>._jtunnel._udp.local.` with an SRV record giving its port and TXT
//! records `id` (its claim's owner, hex) and `since` (when it claimed the name, ms on
//! its wall clock), with a TTL of 30 s. A goodbye is the same announcement with a TTL
//! of 0. The sender's address is where its announcement came from (the SRV target is a
//! placeholder), so the socket code passes the datagram's source address in.
//!
//! This module builds and parses the datagrams; it opens no socket.

use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddrV4;

use crate::wire::MAX_TUNNEL_NAME;

/// The DNS-SD service JTunnel senders announce.
pub const SERVICE: &str = "_jtunnel._udp.local";
/// mDNS's IPv4 multicast group.
pub const MDNS_GROUP: [u8; 4] = [224, 0, 0, 251];
/// mDNS's port.
pub const MDNS_PORT: u16 = 5353;
/// A sender not heard from in this long has gone (it announces every 10 s, with a TTL
/// of 30 s).
pub const FORGET_AFTER_MS: u64 = 35_000;

const TYPE_PTR: u16 = 12;
const TYPE_TXT: u16 = 16;
const TYPE_SRV: u16 = 33;
const CLASS_IN: u16 = 1;
/// The most labels a name may have, and the most compression pointers followed, so a
/// hostile packet can't loop.
const MAX_LABELS: usize = 128;
/// The most questions and records a message may declare. A sender's answer has a
/// handful; the bound keeps the steps one hostile datagram costs to a few thousand.
const MAX_RECORDS: u32 = 32;

/// Why a datagram isn't a DNS message this module can read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DnsError {
    /// It ends before a field it declares.
    Truncated,
    /// A name is malformed: a bad label length, or compression that loops.
    BadName,
    /// It declares more questions and records than an answer would have.
    TooManyRecords,
}

impl fmt::Display for DnsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("DNS message cut short"),
            Self::BadName => f.write_str("malformed DNS name"),
            Self::TooManyRecords => f.write_str("more DNS records than an answer has"),
        }
    }
}

impl std::error::Error for DnsError {}

/// Writes the PTR query for [`SERVICE`] into `out`, and gives its length, or `None` if
/// `out` is too small.
#[must_use]
pub fn write_query(out: &mut [u8]) -> Option<usize> {
    let mut msg = Vec::with_capacity(48);
    // ID 0 and no flags, as mDNS queries are; one question.
    msg.extend_from_slice(&[0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in SERVICE.split('.') {
        msg.push(label.len() as u8);
        msg.extend_from_slice(label.as_bytes());
    }
    msg.push(0);
    msg.extend_from_slice(&TYPE_PTR.to_be_bytes());
    msg.extend_from_slice(&CLASS_IN.to_be_bytes());
    out.get_mut(..msg.len())?.copy_from_slice(&msg);
    Some(msg.len())
}

/// One sender's announcement, from [`parse_announcements`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announcement {
    /// The tunnel's name: the instance name before `._jtunnel._udp.local`.
    pub name: String,
    /// The sender's port.
    pub port: u16,
    /// The record's TTL in seconds: 0 is a goodbye.
    pub ttl: u32,
    /// The claim's owner id, from the `id` TXT record.
    pub owner: Option<u64>,
    /// When the sender claimed the name, from the `since` TXT record.
    pub since: Option<u64>,
}

struct Rr {
    /// Where its name starts in the message.
    name_at: usize,
    kind: u16,
    ttl: u32,
    rdata_at: usize,
    rdata_len: usize,
}

fn be16(msg: &[u8], at: usize) -> Result<u16, DnsError> {
    let b = msg.get(at..at + 2).ok_or(DnsError::Truncated)?;
    Ok(u16::from_be_bytes([b[0], b[1]]))
}

/// Walks the name at `at`, following compression pointers, handing each label to
/// `label` in order, and gives where the name ends in the message. It allocates
/// nothing, so a hostile datagram costs at most [`MAX_LABELS`] steps a name.
fn walk_name<'m>(
    msg: &'m [u8],
    mut at: usize,
    mut label: impl FnMut(&'m [u8]),
) -> Result<usize, DnsError> {
    let mut end = None;
    for _ in 0..MAX_LABELS {
        let len = *msg.get(at).ok_or(DnsError::Truncated)?;
        match len {
            0 => return Ok(end.unwrap_or(at + 1)),
            0xC0..=0xFF => {
                let low = *msg.get(at + 1).ok_or(DnsError::Truncated)?;
                end.get_or_insert(at + 2);
                at = usize::from(len & 0x3F) << 8 | usize::from(low);
            }
            0x40..=0xBF => return Err(DnsError::BadName),
            _ => {
                let bytes = msg
                    .get(at + 1..at + 1 + usize::from(len))
                    .ok_or(DnsError::Truncated)?;
                label(bytes);
                at += 1 + usize::from(len);
            }
        }
    }
    Err(DnsError::BadName)
}

/// Hands each SRV and TXT record of `msg` to `each`, in order, allocating nothing.
fn records(msg: &[u8], mut each: impl FnMut(&Rr)) -> Result<(), DnsError> {
    let questions = be16(msg, 4)?;
    let total = u32::from(be16(msg, 6)?) + u32::from(be16(msg, 8)?) + u32::from(be16(msg, 10)?);
    if u32::from(questions) + total > MAX_RECORDS {
        return Err(DnsError::TooManyRecords);
    }
    let mut at = 12;
    for _ in 0..questions {
        at = walk_name(msg, at, |_| {})? + 4;
    }
    for _ in 0..total {
        let name_at = at;
        let end = walk_name(msg, at, |_| {})?;
        let kind = be16(msg, end)?;
        let ttl_bytes = msg.get(end + 4..end + 8).ok_or(DnsError::Truncated)?;
        let ttl = u32::from_be_bytes([ttl_bytes[0], ttl_bytes[1], ttl_bytes[2], ttl_bytes[3]]);
        let rdata_len = usize::from(be16(msg, end + 8)?);
        let rdata_at = end + 10;
        if rdata_at + rdata_len > msg.len() {
            return Err(DnsError::Truncated);
        }
        if matches!(kind, TYPE_SRV | TYPE_TXT) {
            each(&Rr {
                name_at,
                kind,
                ttl,
                rdata_at,
                rdata_len,
            });
        }
        at = rdata_at + rdata_len;
    }
    Ok(())
}

/// The most labels an instance name and the service's take: a tunnel name of at most
/// [`MAX_TUNNEL_NAME`] bytes has at most half as many labels, plus one, and the service
/// three.
const MAX_INSTANCE_LABELS: usize = MAX_TUNNEL_NAME / 2 + 1 + 3;

/// The tunnel name of the instance name at `at`, `<name>._jtunnel._udp.local`, compared
/// without regard to ASCII case. A name with dots in it is several labels, joined back
/// with dots, as JTunnel reads them. The labels are compared where they lie; only a name
/// that matches is copied.
fn tunnel_name(msg: &[u8], at: usize) -> Option<String> {
    let mut spans: [&[u8]; MAX_INSTANCE_LABELS] = [&[]; MAX_INSTANCE_LABELS];
    let mut count = 0;
    let mut too_many = false;
    walk_name(msg, at, |label| {
        if count < MAX_INSTANCE_LABELS {
            spans[count] = label;
            count += 1;
        } else {
            too_many = true;
        }
    })
    .ok()?;
    let service_labels = SERVICE.split('.').count();
    if too_many || count <= service_labels {
        return None;
    }
    let (instance, tail) = spans[..count].split_at(count - service_labels);
    if !tail
        .iter()
        .zip(SERVICE.split('.'))
        .all(|(l, s)| l.eq_ignore_ascii_case(s.as_bytes()))
    {
        return None;
    }
    let joined_len = instance.iter().map(|l| l.len()).sum::<usize>() + instance.len() - 1;
    if joined_len > MAX_TUNNEL_NAME {
        return None;
    }
    let joined = instance.join(&b'.');
    String::from_utf8(joined).ok()
}

/// The JTunnel announcements in one mDNS datagram: answers, authority and additional
/// records alike. A query, or a datagram about other services, gives none.
///
/// # Errors
///
/// A [`DnsError`] for a datagram that isn't a well-formed DNS message.
pub fn parse_announcements(datagram: &[u8]) -> Result<Vec<Announcement>, DnsError> {
    // Only a record that names a JTunnel instance allocates (its name, once copied), so
    // a datagram about other services, or one that repeats a long name, costs none.
    let mut srv: Vec<(String, u16, u32)> = Vec::new();
    let mut txt: BTreeMap<String, (Option<u64>, Option<u64>)> = BTreeMap::new();
    records(datagram, |rr| {
        let Some(name) = tunnel_name(datagram, rr.name_at) else {
            return;
        };
        let rdata = &datagram[rr.rdata_at..rr.rdata_at + rr.rdata_len];
        match rr.kind {
            TYPE_SRV if rdata.len() >= 6 => {
                let port = u16::from_be_bytes([rdata[4], rdata[5]]);
                if port != 0 {
                    srv.push((name, port, rr.ttl));
                }
            }
            TYPE_TXT => {
                let entry = txt.entry(name.to_ascii_lowercase()).or_default();
                let mut rest = rdata;
                while let Some((&len, tail)) = rest.split_first() {
                    let Some(s) = tail.get(..usize::from(len)) else {
                        break;
                    };
                    rest = &tail[usize::from(len)..];
                    let s = String::from_utf8_lossy(s);
                    match s.split_once('=') {
                        Some(("id", v)) => entry.0 = u64::from_str_radix(v, 16).ok(),
                        Some(("since", v)) => entry.1 = v.parse().ok(),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    })?;
    Ok(srv
        .into_iter()
        .map(|(name, port, ttl)| {
            let (owner, since) = txt
                .get(&name.to_ascii_lowercase())
                .copied()
                .unwrap_or_default();
            Announcement {
                name,
                port,
                ttl,
                owner,
                since,
            }
        })
        .collect())
}

/// A sender in the [`Directory`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sender {
    /// The tunnel's name.
    pub name: String,
    /// Where it sends from, and where to subscribe.
    pub address: SocketAddrV4,
    /// When it was last heard, in the caller's milliseconds.
    pub last_seen_ms: u64,
}

/// The JTunnel senders heard on the network, as JTunnel's own list keeps them: a sender
/// is added or refreshed by an announcement, removed by its goodbye, and forgotten
/// [`FORGET_AFTER_MS`] after it was last heard.
#[derive(Clone, Debug, Default)]
pub struct Directory {
    senders: Vec<Sender>,
}

impl Directory {
    /// An empty list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes the announcements of one datagram that came from `source`'s IP address,
    /// heard at `now_ms`.
    pub fn observe(
        &mut self,
        source: std::net::Ipv4Addr,
        announcements: &[Announcement],
        now_ms: u64,
    ) {
        for a in announcements {
            let address = SocketAddrV4::new(source, a.port);
            let found = self
                .senders
                .iter()
                .position(|s| s.address == address && s.name.eq_ignore_ascii_case(&a.name));
            match (found, a.ttl) {
                (Some(i), 0) => {
                    self.senders.remove(i);
                }
                (None, 0) => {}
                (Some(i), _) => self.senders[i].last_seen_ms = now_ms,
                (None, _) => self.senders.push(Sender {
                    name: a.name.clone(),
                    address,
                    last_seen_ms: now_ms,
                }),
            }
        }
    }

    /// Forgets senders not heard from in [`FORGET_AFTER_MS`].
    pub fn expire(&mut self, now_ms: u64) {
        self.senders
            .retain(|s| now_ms.saturating_sub(s.last_seen_ms) <= FORGET_AFTER_MS);
    }

    /// How many senders it lists.
    #[must_use]
    pub fn len(&self) -> usize {
        self.senders.len()
    }

    /// Whether it lists none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.senders.is_empty()
    }

    /// The senders, sorted by name, then address.
    #[must_use]
    pub fn senders(&self) -> Vec<Sender> {
        let mut out = self.senders.clone();
        out.sort_by(|a, b| a.name.cmp(&b.name).then(a.address.cmp(&b.address)));
        out
    }

    /// Where the tunnel `name` is (compared without regard to ASCII case): the one
    /// heard most recently, if two addresses claim it.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<SocketAddrV4> {
        self.senders
            .iter()
            .filter(|s| s.name.eq_ignore_ascii_case(name))
            .max_by_key(|s| s.last_seen_ms)
            .map(|s| s.address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    /// An announcement as JTunnel's mDNS library writes it: an answer PTR, then SRV and
    /// TXT records as additional records, with the service name compressed.
    fn announcement(name: &str, port: u16, ttl: u32) -> Vec<u8> {
        let mut m = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 3];
        let service_at = m.len();
        for l in SERVICE.split('.') {
            m.push(l.len() as u8);
            m.extend_from_slice(l.as_bytes());
        }
        m.push(0);
        let ptr =
            |m: &mut Vec<u8>, at: usize| m.extend_from_slice(&[0xC0 | (at >> 8) as u8, at as u8]);
        m.extend_from_slice(&TYPE_PTR.to_be_bytes());
        m.extend_from_slice(&CLASS_IN.to_be_bytes());
        m.extend_from_slice(&ttl.to_be_bytes());
        let rdata_len_at = m.len();
        m.extend_from_slice(&[0, 0]);
        let instance_at = m.len();
        m.push(name.len() as u8);
        m.extend_from_slice(name.as_bytes());
        ptr(&mut m, service_at);
        let len = (m.len() - instance_at) as u16;
        m[rdata_len_at..rdata_len_at + 2].copy_from_slice(&len.to_be_bytes());
        // SRV
        ptr(&mut m, instance_at);
        m.extend_from_slice(&TYPE_SRV.to_be_bytes());
        m.extend_from_slice(&(CLASS_IN | 0x8000).to_be_bytes());
        m.extend_from_slice(&ttl.to_be_bytes());
        let host = b"\x07jtunnel\x05local\x00";
        m.extend_from_slice(&((6 + host.len()) as u16).to_be_bytes());
        m.extend_from_slice(&[0, 0, 0, 0]);
        m.extend_from_slice(&port.to_be_bytes());
        m.extend_from_slice(host);
        // Two TXT records, one per key, as JTunnel writes them.
        for kv in ["id=1f00000000a", "since=1730000000000"] {
            ptr(&mut m, instance_at);
            m.extend_from_slice(&TYPE_TXT.to_be_bytes());
            m.extend_from_slice(&(CLASS_IN | 0x8000).to_be_bytes());
            m.extend_from_slice(&ttl.to_be_bytes());
            m.extend_from_slice(&((kv.len() + 1) as u16).to_be_bytes());
            m.push(kv.len() as u8);
            m.extend_from_slice(kv.as_bytes());
        }
        m
    }

    #[test]
    fn the_query_asks_for_the_service_pointer() {
        let mut buf = [0; 64];
        let n = write_query(&mut buf).unwrap_or(0);
        assert_eq!(&buf[12..21], b"\x08_jtunnel");
        assert_eq!(&buf[n - 4..n], &[0, 12, 0, 1]);
        // Our own query, heard back, has no announcements in it.
        assert_eq!(parse_announcements(&buf[..n]), Ok(Vec::new()));
        assert_eq!(write_query(&mut [0; 8]), None);
    }

    #[test]
    fn reads_an_announcement_with_its_claim() {
        let got = parse_announcements(&announcement("Studio Mac 1", 47_000, 30));
        assert_eq!(
            got,
            Ok(vec![Announcement {
                name: "Studio Mac 1".into(),
                port: 47_000,
                ttl: 30,
                owner: Some(0x1f0_0000_000a),
                since: Some(1_730_000_000_000),
            }])
        );
    }

    #[test]
    fn other_services_and_bad_names_are_ignored_and_broken_packets_refused() {
        let mut other = announcement("x", 1, 30);
        // Rename the service label `_jtunnel` to `_jtunnex`.
        other[20] = b'x';
        assert_eq!(parse_announcements(&other), Ok(Vec::new()));
        let long = "n".repeat(MAX_TUNNEL_NAME + 1);
        assert_eq!(
            parse_announcements(&announcement(&long, 1, 30)),
            Ok(Vec::new())
        );
        let good = announcement("ok", 1, 30);
        for cut in 0..good.len() {
            assert!(parse_announcements(&good[..cut]).is_err(), "cut at {cut}");
        }
        // A compression pointer to itself.
        let mut looped = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        looped.extend_from_slice(&[0xC0, 12]);
        assert_eq!(parse_announcements(&looped), Err(DnsError::BadName));
    }

    #[test]
    fn the_directory_adds_refreshes_says_goodbye_and_forgets() {
        let mut d = Directory::new();
        let ip = Ipv4Addr::new(192, 168, 1, 20);
        let a = |name: &str, ttl| Announcement {
            name: name.into(),
            port: 47_000,
            ttl,
            owner: None,
            since: None,
        };
        d.observe(ip, &[a("Bitwig", 30), a("Live", 30)], 0);
        assert_eq!(d.resolve("bitwig"), Some(SocketAddrV4::new(ip, 47_000)));
        d.observe(ip, &[a("BITWIG", 30)], 30_000);
        d.expire(40_000);
        assert_eq!(
            d.senders()
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["Bitwig"]
        );
        d.observe(ip, &[a("Bitwig", 0)], 41_000);
        assert_eq!(d.resolve("Bitwig"), None);
        assert!(d.senders().is_empty());
    }
}
