//! Ingest URLs (`rtmp(s)://host[:port]/app[/...]`) and stream keys.

use std::fmt;

use super::RtmpError;

/// The URL scheme, which selects plain TCP or TLS.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scheme {
    /// `rtmp://`, plain TCP, default port 1935.
    Rtmp,
    /// `rtmps://`, RTMP over TLS, default port 443.
    Rtmps,
}

impl Scheme {
    /// The port used when the URL names none.
    pub fn default_port(self) -> u16 {
        match self {
            Self::Rtmp => 1935,
            Self::Rtmps => 443,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Rtmp => "rtmp",
            Self::Rtmps => "rtmps",
        }
    }
}

/// A parsed ingest URL such as `rtmp://a.rtmp.youtube.com/live2`.
///
/// The application name is the whole path after the host (`live2`, or `app/instance`),
/// which is how streaming services' "server URL" fields are meant: the stream key is
/// supplied separately as a [`StreamKey`].
///
/// ```
/// use jstream_outputs::rtmp::{RtmpUrl, Scheme};
///
/// let url = RtmpUrl::parse("rtmp://a.rtmp.youtube.com/live2")?;
/// assert_eq!(url.scheme(), Scheme::Rtmp);
/// assert_eq!(url.host(), "a.rtmp.youtube.com");
/// assert_eq!(url.port(), 1935);
/// assert_eq!(url.app(), "live2");
/// assert_eq!(url.tc_url(), "rtmp://a.rtmp.youtube.com/live2");
/// # Ok::<(), jstream_outputs::rtmp::RtmpError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RtmpUrl {
    scheme: Scheme,
    host: String,
    port: u16,
    app: String,
    tc_url: String,
}

impl RtmpUrl {
    /// Parses `rtmp://host[:port]/app[/...]` or `rtmps://…`. The scheme is
    /// case-insensitive; IPv6 hosts are written in brackets (`rtmp://[::1]/live`).
    ///
    /// # Errors
    /// [`RtmpError::InvalidUrl`] when the scheme is not `rtmp` or `rtmps`, the host or
    /// application is missing, the port is not a non-zero number, the URL carries
    /// `user:password@` credentials, or it contains whitespace or control characters.
    pub fn parse(url: &str) -> Result<Self, RtmpError> {
        let invalid = |reason| RtmpError::InvalidUrl { reason };
        if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(invalid("contains whitespace or control characters"));
        }
        let (scheme, rest) = url
            .split_once("://")
            .ok_or(invalid("missing `rtmp://` or `rtmps://`"))?;
        let scheme = if scheme.eq_ignore_ascii_case("rtmp") {
            Scheme::Rtmp
        } else if scheme.eq_ignore_ascii_case("rtmps") {
            Scheme::Rtmps
        } else {
            return Err(invalid("scheme is not `rtmp` or `rtmps`"));
        };
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        if authority.contains('@') {
            return Err(invalid("credentials in the URL are not supported"));
        }
        let (host, port) = split_host_port(authority)?;
        let port = match port {
            None => scheme.default_port(),
            Some(p) => match p.parse::<u16>() {
                Ok(p) if p != 0 => p,
                _ => return Err(invalid("port is not a number in 1..=65535")),
            },
        };
        let app = path.trim_matches('/');
        if app.is_empty() {
            return Err(invalid(
                "missing application name (the path after the host)",
            ));
        }
        let tc_url = format!("{}://{authority}/{app}", scheme.as_str());
        Ok(Self {
            scheme,
            host: host.to_owned(),
            port,
            app: app.to_owned(),
            tc_url,
        })
    }

    /// `rtmp` or `rtmps`.
    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// The host name or IP address, without brackets for IPv6.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Whether publishing here sends the stream key across the network unencrypted:
    /// plain `rtmp://` to a host other than this machine. `localhost` and loopback
    /// addresses (`127.0.0.0/8`, `::1`, and IPv4 loopback mapped into IPv6) don't count,
    /// so a local relay raises no warning (#353).
    pub fn sends_key_in_cleartext(&self) -> bool {
        self.scheme == Scheme::Rtmp && !is_loopback(&self.host)
    }

    /// The port, explicit or the scheme's default.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The application name sent in `connect` (the path after the host).
    pub fn app(&self) -> &str {
        &self.app
    }

    /// The `tcUrl` sent in `connect`: the URL without a trailing slash.
    pub fn tc_url(&self) -> &str {
        &self.tc_url
    }
}

impl fmt::Display for RtmpUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.tc_url)
    }
}

/// Splits `host[:port]` or `[v6]:port`.
fn split_host_port(authority: &str) -> Result<(&str, Option<&str>), RtmpError> {
    let invalid = |reason| RtmpError::InvalidUrl { reason };
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (host, after) = v6
            .split_once(']')
            .ok_or(invalid("unclosed `[` in IPv6 host"))?;
        match after {
            "" => (host, None),
            _ => (
                host,
                Some(
                    after
                        .strip_prefix(':')
                        .ok_or(invalid("unexpected text after IPv6 host"))?,
                ),
            ),
        }
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    if host.is_empty() {
        return Err(invalid("missing host"));
    }
    Ok((host, port))
}

/// A stream key: the secret that names (and authorizes) the stream being published.
///
/// Its `Debug` output is `"***"` and it has no `Display`, so it does not leak into logs
/// (docs/RULES.md#R-SEC-001). Where keys are stored is the caller's concern; this type
/// only carries one to the server.
#[derive(Clone, PartialEq, Eq)]
pub struct StreamKey(String);

impl StreamKey {
    /// Wraps `key`.
    ///
    /// # Errors
    /// [`RtmpError::EmptyStreamKey`] when `key` is empty.
    pub fn new(key: impl Into<String>) -> Result<Self, RtmpError> {
        let key = key.into();
        if key.is_empty() {
            return Err(RtmpError::EmptyStreamKey);
        }
        Ok(Self(key))
    }

    /// The key itself, as sent to the server. Never log it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for StreamKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StreamKey(\"***\")")
    }
}

/// Whether `host` names this machine: `localhost` or a loopback address.
fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.to_canonical().is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plain `rtmp://` to a remote host sends the key in the clear; `rtmps://` and
    /// loopback don't (#353).
    #[test]
    fn only_plain_rtmp_to_a_remote_host_sends_the_key_in_cleartext() {
        let clear = |url: &str| RtmpUrl::parse(url).unwrap().sends_key_in_cleartext();
        assert!(clear("rtmp://a.rtmp.youtube.com/live2"));
        assert!(clear("RTMP://192.168.1.20:1935/live"));
        assert!(clear("rtmp://[2001:db8::1]/live"));
        assert!(clear("rtmp://localhost.example.com/live"));
        assert!(!clear("rtmps://a.rtmps.youtube.com/live2"));
        assert!(!clear("rtmp://localhost/live"));
        assert!(!clear("rtmp://LOCALHOST:1936/live"));
        assert!(!clear("rtmp://127.0.0.1/live"));
        assert!(!clear("rtmp://127.8.9.10/live"));
        assert!(!clear("rtmp://[::1]:1936/live"));
        assert!(!clear("rtmp://[::ffff:127.0.0.1]/live"));
    }

    #[test]
    fn parses_youtube_style_url() {
        let u = RtmpUrl::parse("rtmp://a.rtmp.youtube.com/live2/").unwrap();
        assert_eq!(u.scheme(), Scheme::Rtmp);
        assert_eq!(u.host(), "a.rtmp.youtube.com");
        assert_eq!(u.port(), 1935);
        assert_eq!(u.app(), "live2");
        assert_eq!(u.tc_url(), "rtmp://a.rtmp.youtube.com/live2");
    }

    #[test]
    fn parses_rtmps_port_and_nested_app() {
        let u = RtmpUrl::parse("RTMPS://a.rtmps.youtube.com:8443/live2/inst").unwrap();
        assert_eq!(u.scheme(), Scheme::Rtmps);
        assert_eq!(u.port(), 8443);
        assert_eq!(u.app(), "live2/inst");
        assert_eq!(u.tc_url(), "rtmps://a.rtmps.youtube.com:8443/live2/inst");
        assert_eq!(
            RtmpUrl::parse("rtmps://h/app").unwrap().port(),
            Scheme::Rtmps.default_port()
        );
    }

    #[test]
    fn parses_ipv6_host() {
        let u = RtmpUrl::parse("rtmp://[::1]:1936/live").unwrap();
        assert_eq!(u.host(), "::1");
        assert_eq!(u.port(), 1936);
        assert_eq!(RtmpUrl::parse("rtmp://[::1]/live").unwrap().port(), 1935);
    }

    #[test]
    fn rejects_malformed_urls() {
        for bad in [
            "a.rtmp.youtube.com/live2",
            "http://host/app",
            "rtmp://host",
            "rtmp://host/",
            "rtmp:///app",
            "rtmp://host:0/app",
            "rtmp://host:99999/app",
            "rtmp://host:x/app",
            "rtmp://user:pw@host/app",
            "rtmp://[::1/app",
            "rtmp://[::1]x/app",
            "rtmp://host/a pp",
        ] {
            assert!(
                matches!(RtmpUrl::parse(bad), Err(RtmpError::InvalidUrl { .. })),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn stream_key_is_redacted_and_non_empty() {
        let k = StreamKey::new("abcd-efgh-ijkl").unwrap();
        assert_eq!(format!("{k:?}"), "StreamKey(\"***\")");
        assert_eq!(k.expose(), "abcd-efgh-ijkl");
        assert!(matches!(StreamKey::new(""), Err(RtmpError::EmptyStreamKey)));
    }
}
