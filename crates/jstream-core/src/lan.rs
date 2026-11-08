//! The LAN ranges JStream's sockets outside the TLS listener may talk to (ADR-0123 §2,
//! ADR-0138 §8).

use std::net::IpAddr;

/// Whether `ip` is on the LAN as ADR-0123 counts it: loopback, the private IPv4 ranges
/// (10/8, 172.16/12, 192.168/16) and IPv4 link-local (169.254/16), and their IPv6
/// counterparts (`::1`, unique local `fc00::/7` and link-local `fe80::/10`). An IPv4
/// address mapped into IPv6 counts as itself.
#[must_use]
pub fn is_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_lan(IpAddr::V4(v4)),
            None => {
                let first = v6.segments()[0];
                v6.is_loopback() || first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::is_lan;

    #[test]
    fn only_lan_addresses_count() {
        for lan in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.1.40",
            "169.254.0.9",
            "::1",
            "fd12:3456::1",
            "fe80::1",
            "::ffff:192.168.1.2",
        ] {
            assert!(is_lan(lan.parse().unwrap()), "{lan}");
        }
        for wan in [
            "8.8.8.8",
            "172.32.0.1",
            "192.169.0.1",
            "2001:db8::1",
            "::ffff:8.8.8.8",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.251",
        ] {
            assert!(!is_lan(wan.parse().unwrap()), "{wan}");
        }
    }
}
