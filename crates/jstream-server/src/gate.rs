//! The checks every upgrade request passes before a session starts (R-SEC-002,
//! ADR-0006): `Host`, then `Origin`, then the control token.
//!
//! - **`Host`** must name the server: a loopback name or the bound address with the bound
//!   port, or a configured extra. This defeats DNS rebinding, where a page on an
//!   attacker's domain resolves that domain to `127.0.0.1`: its `Host` is still the
//!   attacker's name. A `Host` without a port means the scheme's default port, 80 for
//!   `http` and `ws`, so it is accepted only when the server listens on port 80;
//!   otherwise it names another port and is refused.
//! - **`Origin`**, when present, must be the server's own (`http://` plus the accepted
//!   `Host`, where an origin without a port means port 80 too) or a configured extra.
//!   Browsers always send it on WebSocket upgrades, so a foreign web page can't connect.
//!   A request without one is a native client (the CLI, a script, an MCP proxy) and is
//!   let through to the token check, which is the gate (#44, R-SEC-002).
//! - **`Authorization: Bearer <token>`** must carry the control token. The server keeps
//!   only the token's BLAKE3 digest; the presented value is hashed too and the two
//!   digests are compared in constant time, so the compare reveals nothing about the
//!   token (see `Token::matches`).
//!
//! Configured extra hosts must be `name:port`, with an IPv6 address in brackets
//! (`[::1]:port`) and exactly one port: `check_extra_hosts` refuses any other form
//! before the server binds, since it could never match.
//!
//! A refused `Host` or `Origin` is HTTP 403, a missing or wrong token 401. `Host` and
//! the token must appear exactly once, and `Origin` at most once. `Gate::admit` logs each refusal at `warn` with its reason
//! and the peer address, and never a header's value, so never the token (R-SEC-001).
//!
//! A request that passes the gate can still be refused because the server already has
//! `ServerConfig::max_connections` connections open: HTTP 503 (`Refusal::Full`), logged
//! the same way.

use crate::config::{ServerConfig, Token};
use crate::error::ServerError;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::net::{Ipv6Addr, SocketAddr};

/// The default port of the `http` and `ws` schemes: a `Host` or `Origin` without a port
/// names it (RFC 9110 §7.2, RFC 6455 §3).
const DEFAULT_PORT: u16 = 80;

/// Why an upgrade request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// `Host` is missing, repeated or not one of the server's names.
    Host,
    /// `Origin` is repeated or not allowed (a missing one is left to the token).
    Origin,
    /// The control token is missing or wrong.
    Token,
    /// The server already has as many connections open as it allows.
    Full,
}

impl Refusal {
    /// The reason, as logged.
    fn reason(self) -> &'static str {
        match self {
            Refusal::Host => "Host not allowed",
            Refusal::Origin => "Origin not allowed",
            Refusal::Token => "missing or wrong control token",
            Refusal::Full => "too many connections",
        }
    }

    /// Logs the refusal at `warn` with its reason and `peer`, the address it came from.
    /// Nothing from the request is logged.
    pub(crate) fn log(self, peer: SocketAddr) {
        tracing::warn!(peer = %peer, reason = self.reason(), "connection refused");
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        match self {
            Refusal::Host => (StatusCode::FORBIDDEN, "Host not allowed\n").into_response(),
            Refusal::Origin => (StatusCode::FORBIDDEN, "Origin not allowed\n").into_response(),
            Refusal::Token => (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer")],
                "a valid control token is required\n",
            )
                .into_response(),
            Refusal::Full => {
                (StatusCode::SERVICE_UNAVAILABLE, "too many connections\n").into_response()
            }
        }
    }
}

/// The accepted hosts and origins and the token, fixed when the server binds.
#[derive(Debug)]
pub(crate) struct Gate {
    token: Token,
    /// The port the server listens on.
    port: u16,
    /// Lowercase `name:port` values.
    hosts: Vec<String>,
    /// Lowercase `scheme://host[:port]` values, besides `http://<accepted host>`.
    extra_origins: Vec<String>,
}

impl Gate {
    /// The gate for a server configured by `config` and listening on `local`.
    pub(crate) fn new(config: &ServerConfig, local: SocketAddr) -> Gate {
        let port = local.port();
        let mut hosts = vec![
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
        ];
        if !local.ip().is_unspecified() {
            hosts.push(local.to_string());
        }
        hosts.extend(config.extra_hosts.iter().cloned());
        for host in &mut hosts {
            host.make_ascii_lowercase();
        }
        hosts.dedup();
        let extra_origins = config
            .extra_origins
            .iter()
            .map(|origin| origin.to_ascii_lowercase())
            .collect();
        Gate {
            token: config.token.clone(),
            port,
            hosts,
            extra_origins,
        }
    }

    /// Checks an upgrade request's headers, as [`check`](Gate::check) does, and logs a
    /// refusal with its reason and `peer`, the address it came from. Nothing from the
    /// headers is logged: not the token, nor the `Host` or `Origin` values.
    pub(crate) fn admit(&self, headers: &HeaderMap, peer: SocketAddr) -> Result<(), Refusal> {
        let result = self.check(headers);
        if let Err(refusal) = result {
            refusal.log(peer);
        }
        result
    }

    /// Checks an upgrade request's headers.
    pub(crate) fn check(&self, headers: &HeaderMap) -> Result<(), Refusal> {
        let host = single(headers, &header::HOST)
            .map(|value| value.to_ascii_lowercase())
            .ok_or(Refusal::Host)?;
        // A `Host` without a port names port 80: it can only be this server on port 80.
        let host = if has_port(&host) {
            host
        } else if self.port == DEFAULT_PORT {
            format!("{host}:{DEFAULT_PORT}")
        } else {
            return Err(Refusal::Host);
        };
        if !self.hosts.contains(&host) {
            return Err(Refusal::Host);
        }
        // No `Origin` at all is a native client: the token is the gate (#44). One that is
        // present, even the opaque `null`, must be allowed, and it may appear only once.
        if headers.contains_key(header::ORIGIN) {
            let origin = single(headers, &header::ORIGIN)
                .map(|value| value.to_ascii_lowercase())
                .ok_or(Refusal::Origin)?;
            let own = origin.strip_prefix("http://").is_some_and(|rest| {
                if has_port(rest) {
                    rest == host
                } else {
                    format!("{rest}:{DEFAULT_PORT}") == host
                }
            });
            if !own && !self.extra_origins.contains(&origin) {
                return Err(Refusal::Origin);
            }
        }
        let token = single(headers, &header::AUTHORIZATION)
            .and_then(bearer)
            .ok_or(Refusal::Token)?;
        if self.token.matches(token.as_bytes()) {
            Ok(())
        } else {
            Err(Refusal::Token)
        }
    }
}

/// The header's value as text, if it appears exactly once and is visible ASCII.
fn single<'h>(headers: &'h HeaderMap, name: &HeaderName) -> Option<&'h str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    HeaderValue::to_str(value).ok()
}

/// Checks that every configured extra host is `name:port` with a decimal port from 1 to
/// 65535, where an IPv6 name is a bracketed literal (`[::1]:4455`). The gate compares
/// hosts with their port, and a request's `Host` always brackets an IPv6 address, so an
/// entry without a port, with more than one, with an unbracketed IPv6 address or with a
/// port the server can't listen on would never match: it is refused here instead of
/// being ignored (#44, #72).
pub(crate) fn check_extra_hosts(extra_hosts: &[String]) -> Result<(), ServerError> {
    for host in extra_hosts {
        let invalid = |reason| ServerError::InvalidHost {
            host: host.clone(),
            reason,
        };
        let port = extra_host_port(host).map_err(invalid)?;
        let port_ok = !port.is_empty()
            && port.bytes().all(|b| b.is_ascii_digit())
            && port.parse::<u16>().is_ok_and(|p| p != 0);
        if !port_ok {
            return Err(invalid("its port must be a number from 1 to 65535"));
        }
    }
    Ok(())
}

/// The port of an extra host written `name:port` or `[ipv6]:port`, or why the entry
/// can't be split that way.
fn extra_host_port(host: &str) -> Result<&str, &'static str> {
    const NO_PORT: &str = "it has no port; write it as `name:port`";
    const NO_NAME: &str = "it has no name before the port";
    if let Some(bracketed) = host.strip_prefix('[') {
        let Some((address, after)) = bracketed.split_once(']') else {
            return Err("its `[` has no matching `]`");
        };
        if address.is_empty() {
            return Err(NO_NAME);
        }
        if address.parse::<Ipv6Addr>().is_err() {
            return Err("only an IPv6 address goes in brackets, as `[addr]:port`");
        }
        return match after.strip_prefix(':') {
            Some(port) => Ok(port),
            None if after.is_empty() => Err(NO_PORT),
            None => Err("only `:port` may follow the `]`"),
        };
    }
    let Some((name, port)) = host.split_once(':') else {
        return Err(NO_PORT);
    };
    if port.contains(':') {
        return Err(if host.parse::<Ipv6Addr>().is_ok() {
            "an IPv6 address must be in brackets, as `[addr]:port`"
        } else {
            "it has more than one port"
        });
    }
    if name.contains(['[', ']']) {
        return Err("only an IPv6 address goes in brackets, as `[addr]:port`");
    }
    if name.is_empty() {
        return Err(NO_NAME);
    }
    Ok(port)
}

/// Whether a `host[:port]` authority names a port. An IPv6 literal (`[::1]`) has colons
/// of its own, so only one after its closing bracket counts.
fn has_port(authority: &str) -> bool {
    match authority.rfind(']') {
        Some(end) => authority[end + 1..].starts_with(':'),
        None => authority.contains(':'),
    }
}

/// The credentials of a `Bearer` authorization (the scheme is case-insensitive).
fn bearer(authorization: &str) -> Option<&str> {
    let (scheme, credentials) = authorization.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| credentials.trim())
}

#[cfg(test)]
mod tests {
    use super::{Gate, Refusal, check_extra_hosts};
    use crate::config::{ServerConfig, Token};
    use crate::error::ServerError;
    use axum::http::{HeaderMap, HeaderName, HeaderValue, header};
    use std::fmt;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Metadata, Subscriber};

    fn gate(extra_hosts: &[&str], extra_origins: &[&str]) -> Gate {
        gate_on(4455, extra_hosts, extra_origins)
    }

    fn gate_on(port: u16, extra_hosts: &[&str], extra_origins: &[&str]) -> Gate {
        let Ok(token) = Token::new("t0ken") else {
            panic!("a valid token");
        };
        let mut config = ServerConfig::new(token);
        config.extra_hosts = extra_hosts.iter().map(|h| (*h).to_owned()).collect();
        config.extra_origins = extra_origins.iter().map(|o| (*o).to_owned()).collect();
        Gate::new(&config, SocketAddr::from(([127, 0, 0, 1], port)))
    }

    fn request(host: &str, origin: &str) -> HeaderMap {
        headers(&[
            (header::HOST, host),
            (header::ORIGIN, origin),
            (header::AUTHORIZATION, "Bearer t0ken"),
        ])
    }

    fn headers(pairs: &[(HeaderName, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                name.clone(),
                HeaderValue::from_str(value)
                    .unwrap_or_else(|_| panic!("a valid header value: {value}")),
            );
        }
        map
    }

    fn good() -> Vec<(HeaderName, &'static str)> {
        vec![
            (header::HOST, "127.0.0.1:4455"),
            (header::ORIGIN, "http://127.0.0.1:4455"),
            (header::AUTHORIZATION, "Bearer t0ken"),
        ]
    }

    fn with(name: HeaderName, value: Option<&'static str>) -> HeaderMap {
        let mut pairs: Vec<_> = good().into_iter().filter(|(n, _)| *n != name).collect();
        if let Some(value) = value {
            pairs.push((name, value));
        }
        headers(&pairs)
    }

    #[test]
    fn accepts_its_own_names_and_origins() {
        let gate = gate(&[], &[]);
        assert_eq!(gate.check(&headers(&good())), Ok(()));
        for (host, origin) in [
            ("localhost:4455", "http://localhost:4455"),
            ("LOCALHOST:4455", "http://localhost:4455"),
            ("[::1]:4455", "http://[::1]:4455"),
        ] {
            let map = headers(&[
                (header::HOST, host),
                (header::ORIGIN, origin),
                (header::AUTHORIZATION, "bearer t0ken"),
            ]);
            assert_eq!(gate.check(&map), Ok(()), "{host} {origin}");
        }
    }

    #[test]
    fn refuses_foreign_or_missing_hosts() {
        let gate = gate(&[], &[]);
        for host in [
            None,
            Some("evil.example:4455"),
            Some("127.0.0.1:9999"),
            Some("127.0.0.1"),
        ] {
            assert_eq!(
                gate.check(&with(header::HOST, host)),
                Err(Refusal::Host),
                "{host:?}"
            );
        }
        let mut twice = headers(&good());
        twice.append(header::HOST, HeaderValue::from_static("127.0.0.1:4455"));
        assert_eq!(gate.check(&twice), Err(Refusal::Host));
    }

    #[test]
    fn refuses_foreign_origins() {
        let gate = gate(&[], &[]);
        for origin in [
            Some("null"),
            Some("http://evil.example"),
            Some("https://127.0.0.1:4455"),
            Some("http://localhost:4455"),
        ] {
            assert_eq!(
                gate.check(&with(header::ORIGIN, origin)),
                Err(Refusal::Origin),
                "{origin:?}"
            );
        }
    }

    /// #44: a request without `Origin` (a native client) passes to the token check,
    /// which still refuses it without the token; a repeated `Origin` is refused.
    #[test]
    fn a_missing_origin_is_left_to_the_token() {
        let gate = gate(&[], &[]);
        let mut map = with(header::ORIGIN, None);
        assert_eq!(gate.check(&map), Ok(()));
        map.remove(header::AUTHORIZATION);
        assert_eq!(gate.check(&map), Err(Refusal::Token));
        let mut twice = headers(&good());
        twice.append(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:4455"),
        );
        assert_eq!(gate.check(&twice), Err(Refusal::Origin));
    }

    #[test]
    fn refuses_missing_or_wrong_tokens() {
        let gate = gate(&[], &[]);
        for auth in [
            None,
            Some("Bearer nope"),
            Some("Basic t0ken"),
            Some("t0ken"),
            Some("Bearer"),
        ] {
            assert_eq!(
                gate.check(&with(header::AUTHORIZATION, auth)),
                Err(Refusal::Token),
                "{auth:?}"
            );
        }
    }

    /// #44: on port 80, the scheme's default, a `Host` or `Origin` without a port names
    /// the server, as it does with `:80`.
    #[test]
    fn on_the_default_port_a_host_without_a_port_is_accepted() {
        let gate = gate_on(80, &["studio.lan:80"], &[]);
        for (host, origin) in [
            ("127.0.0.1", "http://127.0.0.1"),
            ("localhost", "http://localhost"),
            ("LOCALHOST", "http://localhost"),
            ("[::1]", "http://[::1]"),
            ("studio.lan", "http://studio.lan"),
            ("localhost:80", "http://localhost"),
            ("localhost", "http://localhost:80"),
            ("localhost:80", "http://localhost:80"),
        ] {
            assert_eq!(
                gate.check(&request(host, origin)),
                Ok(()),
                "{host} {origin}"
            );
        }
        for (host, origin, refusal) in [
            ("evil.example", "http://evil.example", Refusal::Host),
            ("localhost:8080", "http://localhost:8080", Refusal::Host),
            ("localhost", "http://localhost:8080", Refusal::Origin),
            ("localhost", "http://127.0.0.1", Refusal::Origin),
            ("localhost", "https://localhost", Refusal::Origin),
        ] {
            assert_eq!(
                gate.check(&request(host, origin)),
                Err(refusal),
                "{host} {origin}"
            );
        }
    }

    /// #44: on any other port, a `Host` without a port names port 80, not this server, so
    /// it is refused, as is an `Origin` without a port.
    #[test]
    fn on_another_port_a_host_without_a_port_is_refused() {
        let gate = gate(&["studio.lan:4455"], &[]);
        for host in ["127.0.0.1", "localhost", "[::1]", "studio.lan"] {
            let origin = format!("http://{host}");
            let map = headers(&[
                (header::HOST, host),
                (header::ORIGIN, &origin),
                (header::AUTHORIZATION, "Bearer t0ken"),
            ]);
            assert_eq!(gate.check(&map), Err(Refusal::Host), "{host}");
        }
        assert_eq!(
            gate.check(&request("localhost:4455", "http://localhost")),
            Err(Refusal::Origin)
        );
    }

    /// #44: an extra host without a port (or with a bad one) could never match, so it is
    /// refused when the server is configured instead of being silently ignored.
    #[test]
    fn extra_hosts_must_name_a_port() {
        let check = |hosts: &[&str]| {
            let hosts: Vec<String> = hosts.iter().map(|h| (*h).to_owned()).collect();
            check_extra_hosts(&hosts)
        };
        for good in [
            "studio.lan:4455",
            "STUDIO.lan:80",
            "[::1]:4455",
            "[fe80::1]:65535",
            "10.0.0.2:1",
        ] {
            assert!(check(&[good]).is_ok(), "{good}");
        }
        assert!(check(&[]).is_ok());
        for bad in [
            "studio.lan",
            "[::1]",
            "[fe80::1]",
            "studio.lan:",
            ":4455",
            "studio.lan:0",
            "studio.lan:65536",
            "studio.lan:+80",
            "studio.lan:http",
            // #72 N1: an unbracketed IPv6 address, or a second port, used to pass as
            // `name:port` and then never match.
            "fe80::1",
            "::1:4455",
            "studio.lan:80:80",
            "[::1]:80:80",
            "[::1",
            "[]:4455",
            "[studio.lan]:4455",
            "[::1]x:4455",
            "studio[1]:4455",
        ] {
            match check(&["localhost:4455", bad]) {
                Err(ServerError::InvalidHost { host, .. }) => assert_eq!(host, bad),
                other => panic!("{bad}: {other:?}"),
            }
        }
        let Err(error) = check(&["studio.lan"]) else {
            panic!("a host without a port is refused");
        };
        assert_eq!(
            error.to_string(),
            "invalid extra host `studio.lan`: it has no port; write it as `name:port`"
        );
        let reason = |host: &str| match check(&[host]) {
            Err(ServerError::InvalidHost { reason, .. }) => reason,
            other => panic!("{host}: {other:?}"),
        };
        assert_eq!(
            reason("fe80::1"),
            "an IPv6 address must be in brackets, as `[addr]:port`"
        );
        assert_eq!(reason("studio.lan:80:80"), "it has more than one port");
        assert_eq!(
            reason("[::1]:80:80"),
            "its port must be a number from 1 to 65535"
        );
    }

    /// A minimal subscriber that keeps every span's and event's fields as text, so a
    /// test can check what would be logged.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<String>>>);

    impl Capture {
        fn push(&self, text: String) {
            self.0.lock().expect("not poisoned").push(text);
        }

        fn text(&self) -> String {
            self.0.lock().expect("not poisoned").join("\n")
        }
    }

    /// The fields of one span or event, as `name=value` pairs.
    #[derive(Default)]
    struct Fields(String);

    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.0.push_str(&format!("{}={value:?} ", field.name()));
        }
    }

    impl Subscriber for Capture {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, span: &Attributes<'_>) -> Id {
            let mut fields = Fields::default();
            span.record(&mut fields);
            self.push(format!("span {} {}", span.metadata().name(), fields.0));
            Id::from_u64(1)
        }

        fn record(&self, _: &Id, values: &Record<'_>) {
            let mut fields = Fields::default();
            values.record(&mut fields);
            self.push(format!("record {}", fields.0));
        }

        fn record_follows_from(&self, _: &Id, _: &Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut fields = Fields::default();
            event.record(&mut fields);
            self.push(format!("event {} {}", event.metadata().level(), fields.0));
        }

        fn enter(&self, _: &Id) {}

        fn exit(&self, _: &Id) {}
    }

    /// R-SEC-001: a refused upgrade is logged with its reason and the peer, and neither
    /// the presented token nor the real one appears in what is logged.
    #[test]
    fn refusals_are_logged_without_the_token() {
        let gate = gate(&[], &[]);
        let peer = SocketAddr::from(([127, 0, 0, 1], 50_000));
        let capture = Capture::default();
        tracing::subscriber::with_default(capture.clone(), || {
            assert_eq!(
                gate.admit(
                    &with(header::AUTHORIZATION, Some("Bearer wr0ng-s3cret")),
                    peer
                ),
                Err(Refusal::Token)
            );
            assert_eq!(
                gate.admit(&with(header::ORIGIN, Some("http://evil.example")), peer),
                Err(Refusal::Origin)
            );
            assert_eq!(gate.admit(&headers(&good()), peer), Ok(()));
            Refusal::Full.log(peer);
        });
        let logged = capture.text();
        assert!(
            logged.contains("connection refused")
                && logged.contains("missing or wrong control token")
                && logged.contains("Origin not allowed")
                && logged.contains("too many connections")
                && logged.contains("127.0.0.1:50000"),
            "{logged}"
        );
        assert_eq!(logged.lines().count(), 3, "only the refusals: {logged}");
        assert!(
            logged.lines().all(|line| line.starts_with("event WARN ")),
            "refusals are warnings: {logged}"
        );
        for secret in ["wr0ng-s3cret", "t0ken", "evil.example"] {
            assert!(!logged.contains(secret), "{secret} logged: {logged}");
        }
    }

    #[test]
    fn extras_extend_what_is_accepted() {
        let gate = gate(&["studio.lan:4455"], &["https://ui.example"]);
        let map = headers(&[
            (header::HOST, "studio.lan:4455"),
            (header::ORIGIN, "http://studio.lan:4455"),
            (header::AUTHORIZATION, "Bearer t0ken"),
        ]);
        assert_eq!(gate.check(&map), Ok(()));
        assert_eq!(
            gate.check(&with(header::ORIGIN, Some("https://UI.example"))),
            Ok(())
        );
    }
}
