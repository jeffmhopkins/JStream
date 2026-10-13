//! The checks every upgrade request passes before a session starts (R-SEC-002,
//! ADR-0006): `Host`, then `Origin`, then the control token.
//!
//! - **`Host`** must name the server: a loopback name or the bound address with the bound
//!   port, or a configured extra. This defeats DNS rebinding, where a page on an
//!   attacker's domain resolves that domain to `127.0.0.1`: its `Host` is still the
//!   attacker's name. A `Host` without a port means the scheme's default port, 80 for
//!   `http` and `ws`, so it is accepted only when the server listens on port 80;
//!   otherwise it names another port and is refused.
//! - **`Origin`** must be present and be the server's own (`http://` plus the accepted
//!   `Host`, where an origin without a port means port 80 too) or a configured extra.
//!   Browsers always send it on WebSocket upgrades, so a
//!   foreign web page can't connect; native clients send the server's own.
//! - **`Authorization: Bearer <token>`** must carry the control token, compared in
//!   constant time.
//!
//! A refused `Host` or `Origin` is HTTP 403, a missing or wrong token 401. Each header
//! must appear exactly once.

use crate::config::{ServerConfig, Token};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::net::SocketAddr;

/// The default port of the `http` and `ws` schemes: a `Host` or `Origin` without a port
/// names it (RFC 9110 §7.2, RFC 6455 §3).
const DEFAULT_PORT: u16 = 80;

/// Why an upgrade request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// `Host` is missing, repeated or not one of the server's names.
    Host,
    /// `Origin` is missing, repeated or not allowed.
    Origin,
    /// The control token is missing or wrong.
    Token,
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
    use super::{Gate, Refusal};
    use crate::config::{ServerConfig, Token};
    use axum::http::{HeaderMap, HeaderName, HeaderValue, header};
    use std::net::SocketAddr;

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
    fn refuses_foreign_or_missing_origins() {
        let gate = gate(&[], &[]);
        for origin in [
            None,
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
