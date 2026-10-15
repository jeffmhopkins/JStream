//! The checks every request passes before a session starts (R-SEC-002, ADR-0006,
//! ADR-0044): `Host`, then `Origin`, then the control token or a device token.
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
//! - **`Authorization`** must carry the control token: `Bearer <token>`, or `Basic`
//!   credentials whose password is the token (the user name is ignored), which is how a
//!   browser presents it to the UI's paths and then to the UI's upgrade. Without
//!   `Authorization`, an upgrade may instead offer the subprotocol `jstream.token.<token>`
//!   beside `jstream`, which is how `jstream-control`'s browser client sends it; the
//!   server selects `jstream`, never the token's subprotocol. The server keeps
//!   only the token's BLAKE3 digest; the presented value is hashed too and the two
//!   digests are compared in constant time, so the compare reveals nothing about the
//!   token (see `Token::matches`).
//! - **Device tokens** (ADR-0044) are presented the same three ways, and checked against
//!   the paired devices' digests the same way (`Devices::admit`). The request is then
//!   admitted as that device ([`Credential::Device`]). A device token travels only over
//!   loopback or TLS, and the server has no TLS yet, so one is accepted only while the
//!   server listens on a loopback address; bound anywhere else, only the control token
//!   gets in.
//! - **`POST /api/v1/pair`** is the one request answered without a token: it passes the
//!   `Host` and `Origin` checks alone ([`Gate::admit_unauthenticated`]), and the open
//!   pairing code is its gate (`pair.rs`).
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
use crate::devices::Devices;
use crate::error::ServerError;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use jstream_protocol::DeviceId;
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// The server already has as many MCP sessions open as it allows.
    McpSessions,
    /// `POST /api/v1/pair` while no pairing code is open.
    NoPairingCode,
    /// `POST /api/v1/pair` with a code that isn't the open one.
    WrongPairingCode,
}

/// What admitted a request: the control token, or a paired device's token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Credential {
    /// The control token (ADR-0006).
    Control,
    /// A device token (ADR-0044).
    Device {
        /// The device.
        id: DeviceId,
        /// Its name, which its sessions are known by in the audit log
        /// (`device:<name>`).
        name: String,
    },
}

impl Credential {
    /// The device, for a device token.
    pub(crate) fn device(&self) -> Option<&DeviceId> {
        match self {
            Credential::Control => None,
            Credential::Device { id, .. } => Some(id),
        }
    }
}

impl Refusal {
    /// The reason, as logged.
    fn reason(self) -> &'static str {
        match self {
            Refusal::Host => "Host not allowed",
            Refusal::Origin => "Origin not allowed",
            Refusal::Token => "missing or wrong control token",
            Refusal::Full => "too many connections",
            Refusal::McpSessions => "too many MCP sessions",
            Refusal::NoPairingCode => "pairing attempted while no pairing code is open",
            Refusal::WrongPairingCode => "wrong pairing code",
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
            Refusal::McpSessions => {
                (StatusCode::SERVICE_UNAVAILABLE, "too many MCP sessions\n").into_response()
            }
            Refusal::NoPairingCode => (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer")],
                "no pairing code is open: start pairing on the host first\n",
            )
                .into_response(),
            Refusal::WrongPairingCode => {
                (StatusCode::FORBIDDEN, "wrong pairing code\n").into_response()
            }
        }
    }
}

/// The accepted hosts and origins and the token, fixed when the server binds, and the
/// paired devices.
pub(crate) struct Gate {
    token: Token,
    devices: Arc<Devices>,
    /// Whether device tokens are accepted: only while the server listens on loopback.
    devices_allowed: bool,
    /// The port the server listens on.
    port: u16,
    /// Lowercase `name:port` values.
    hosts: Vec<String>,
    /// Lowercase `scheme://host[:port]` values, besides `http://<accepted host>`.
    extra_origins: Vec<String>,
}

impl Gate {
    /// The gate for a server configured by `config`, listening on `local`, with these
    /// paired devices.
    pub(crate) fn new(config: &ServerConfig, local: SocketAddr, devices: Arc<Devices>) -> Gate {
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
            devices,
            devices_allowed: local.ip().is_loopback(),
            port,
            hosts,
            extra_origins,
        }
    }

    /// Checks an upgrade request's headers, as [`check`](Gate::check) does, and logs a
    /// refusal with its reason and `peer`, the address it came from. Nothing from the
    /// headers is logged: not the token, nor the `Host` or `Origin` values.
    pub(crate) fn admit(
        &self,
        headers: &HeaderMap,
        peer: SocketAddr,
    ) -> Result<Credential, Refusal> {
        let result = self.check(headers);
        if let Err(refusal) = &result {
            refusal.log(peer);
        }
        result
    }

    /// Checks only `Host` and `Origin`, as for `POST /api/v1/pair`, the one request
    /// answered without a token, and logs a refusal as [`admit`](Gate::admit) does.
    pub(crate) fn admit_unauthenticated(
        &self,
        headers: &HeaderMap,
        peer: SocketAddr,
    ) -> Result<(), Refusal> {
        let result = self.check_host_and_origin(headers);
        if let Err(refusal) = result {
            refusal.log(peer);
        }
        result
    }

    /// Checks a request's headers: `Host`, `Origin`, then the token, and says which
    /// credential the token is.
    pub(crate) fn check(&self, headers: &HeaderMap) -> Result<Credential, Refusal> {
        self.check_host_and_origin(headers)?;
        let token = if headers.contains_key(header::AUTHORIZATION) {
            single(headers, &header::AUTHORIZATION).and_then(credentials)
        } else {
            subprotocol_token(headers)
        }
        .ok_or(Refusal::Token)?;
        if self.token.matches(&token) {
            return Ok(Credential::Control);
        }
        if !self.devices_allowed {
            return Err(Refusal::Token);
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        match self.devices.admit(&token, now) {
            Some((id, name)) => Ok(Credential::Device { id, name }),
            None => Err(Refusal::Token),
        }
    }

    /// Checks `Host`, then `Origin`.
    fn check_host_and_origin(&self, headers: &HeaderMap) -> Result<(), Refusal> {
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
        Ok(())
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

/// The subprotocol a browser's WebSocket client offers, and the server selects, when it
/// carries the token in `Sec-WebSocket-Protocol` (`jstream_control`'s `WS_PROTOCOL`).
pub(crate) const WS_PROTOCOL: &str = "jstream";

/// The prefix of the offered subprotocol that carries the token, `jstream.token.<token>`
/// (`jstream_control`'s `WS_TOKEN_PROTOCOL_PREFIX`). It is never selected, so never echoed.
const TOKEN_PROTOCOL_PREFIX: &str = "jstream.token.";

/// The token a browser offers as the subprotocol `jstream.token.<token>`, used only when
/// the request has no `Authorization`. Every `Sec-WebSocket-Protocol` line counts, and
/// exactly one offered subprotocol may carry a token.
fn subprotocol_token(headers: &HeaderMap) -> Option<Vec<u8>> {
    let mut tokens = headers
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|offered| offered.trim().strip_prefix(TOKEN_PROTOCOL_PREFIX));
    let token = tokens.next()?;
    tokens.next().is_none().then(|| token.as_bytes().to_vec())
}

/// The token an `Authorization` value presents: a `Bearer` token, or the password of
/// `Basic` credentials, which is how a browser presents it (the user name is ignored).
/// Schemes are case-insensitive.
fn credentials(authorization: &str) -> Option<Vec<u8>> {
    let (scheme, credentials) = authorization.split_once(' ')?;
    let credentials = credentials.trim();
    if scheme.eq_ignore_ascii_case("bearer") {
        Some(credentials.as_bytes().to_vec())
    } else if scheme.eq_ignore_ascii_case("basic") {
        let decoded = base64_decode(credentials)?;
        let colon = decoded.iter().position(|&b| b == b':')?;
        Some(decoded[colon + 1..].to_vec())
    } else {
        None
    }
}

/// Decodes standard base64 (RFC 4648 §4, with `=` padding), as `Basic` credentials are
/// written (RFC 7617). `None` for anything else.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    fn sextet(b: u8) -> Option<u32> {
        Some(u32::from(match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        }))
    }
    let bytes = text.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let quads = bytes.len() / 4;
    for (i, quad) in bytes.chunks_exact(4).enumerate() {
        let padding = quad.iter().rev().take_while(|&&b| b == b'=').count();
        if padding > 2 || (padding > 0 && i + 1 != quads) {
            return None;
        }
        let mut n = 0u32;
        for &b in &quad[..4 - padding] {
            n = (n << 6) | sextet(b)?;
        }
        n <<= 6 * padding;
        let [_, a, b, c] = n.to_be_bytes();
        out.extend_from_slice(&[a, b, c][..3 - padding]);
    }
    Some(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{Credential, Gate, Refusal, check_extra_hosts};
    use crate::config::{ServerConfig, Token};
    use crate::devices::{Devices, Now};
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
        Gate::new(&config, SocketAddr::from(([127, 0, 0, 1], port)), devices())
    }

    fn devices() -> Arc<Devices> {
        match Devices::load(None) {
            Ok(devices) => Arc::new(devices),
            Err(error) => panic!("devices: {error}"),
        }
    }

    /// A device paired on `devices`, and its token.
    fn paired(devices: &Devices, name: &str) -> String {
        let now = Now::current();
        let code = devices.start_pairing(now).expect("random");
        devices.pair(&code, name, now).expect("paired").1
    }

    /// ADR-0044: a device token gets in the same three ways as the control token, as
    /// that device, and only while the server listens on loopback.
    #[test]
    fn device_tokens_admit_their_device_on_loopback_only() {
        let Ok(token) = Token::new("t0ken") else {
            panic!("a valid token");
        };
        let config = ServerConfig::new(token);
        let devices = devices();
        let device = paired(&devices, "Tablet");
        let loopback = Gate::new(
            &config,
            SocketAddr::from(([127, 0, 0, 1], 4455)),
            Arc::clone(&devices),
        );
        let bearer = format!("Bearer {device}");
        let admitted = loopback.check(&with(header::AUTHORIZATION, Some(bearer.leak())));
        let Ok(Credential::Device { name, .. }) = admitted else {
            panic!("admitted as a device: {admitted:?}");
        };
        assert_eq!(name, "Tablet");
        let protocols = format!("jstream, jstream.token.{device}");
        let mut offered = with(header::AUTHORIZATION, None);
        offered.append(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_str(&protocols).expect("a header value"),
        );
        assert!(matches!(
            loopback.check(&offered),
            Ok(Credential::Device { .. })
        ));
        // Bound beyond loopback, with no TLS, a device token is refused.
        let mut wide = config.clone();
        wide.extra_hosts = vec!["127.0.0.1:4455".into()];
        let open = Gate::new(&wide, SocketAddr::from(([0, 0, 0, 0], 4455)), devices);
        let bearer = format!("Bearer {device}");
        assert_eq!(
            open.check(&with(header::AUTHORIZATION, Some(bearer.leak()))),
            Err(Refusal::Token)
        );
        assert_eq!(open.check(&headers(&good())), Ok(Credential::Control));
    }

    /// `POST /api/v1/pair` passes `Host` and `Origin` alone, and still needs both.
    #[test]
    fn the_pairing_request_needs_host_and_origin_but_no_token() {
        let gate = gate(&[], &[]);
        let peer = SocketAddr::from(([127, 0, 0, 1], 50_000));
        assert_eq!(
            gate.admit_unauthenticated(&with(header::AUTHORIZATION, None), peer),
            Ok(())
        );
        assert_eq!(
            gate.admit_unauthenticated(&with(header::ORIGIN, Some("http://evil.example")), peer),
            Err(Refusal::Origin)
        );
        assert_eq!(
            gate.admit_unauthenticated(&with(header::HOST, Some("evil.example:4455")), peer),
            Err(Refusal::Host)
        );
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
        assert_eq!(gate.check(&headers(&good())), Ok(Credential::Control));
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
            assert_eq!(gate.check(&map), Ok(Credential::Control), "{host} {origin}");
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
        assert_eq!(gate.check(&map), Ok(Credential::Control));
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
                Ok(Credential::Control),
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
    pub(crate) struct Capture(Arc<Mutex<Vec<String>>>);

    impl Capture {
        fn push(&self, text: String) {
            self.0.lock().expect("not poisoned").push(text);
        }

        pub(crate) fn text(&self) -> String {
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
            assert_eq!(gate.admit(&headers(&good()), peer), Ok(Credential::Control));
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
        assert_eq!(gate.check(&map), Ok(Credential::Control));
        assert_eq!(
            gate.check(&with(header::ORIGIN, Some("https://UI.example"))),
            Ok(Credential::Control)
        );
    }

    /// A browser presents the token as the password of `Basic` credentials; the user
    /// name is ignored, and anything that isn't `user:token` in base64 is refused.
    #[test]
    fn basic_credentials_carry_the_token_as_the_password() {
        let gate = gate(&[], &[]);
        let basic = |value: &str| {
            headers(&[
                (header::HOST, "127.0.0.1:4455"),
                (header::ORIGIN, "http://127.0.0.1:4455"),
                (header::AUTHORIZATION, value),
            ])
        };
        // "jstream:t0ken", ":t0ken", "any one:t0ken".
        for accepted in [
            "Basic anN0cmVhbTp0MGtlbg==",
            "basic OnQwa2Vu",
            "BASIC YW55IG9uZTp0MGtlbg==",
        ] {
            assert_eq!(
                gate.check(&basic(accepted)),
                Ok(Credential::Control),
                "{accepted}"
            );
        }
        // "t0ken" (no colon), "t0ken:x" (the token as the user name), "jstream:t0ke",
        // malformed base64, padding in the middle, empty.
        for refused in [
            "Basic dDBrZW4=",
            "Basic dDBrZW46eA==",
            "Basic anN0cmVhbTp0MGtl",
            "Basic anN0cmVhbTp0MGtlbg",
            "Basic anN0=mVhbTp0MGtlbg==",
            "Basic !!!!",
            "Basic ",
        ] {
            assert_eq!(
                gate.check(&basic(refused)),
                Err(Refusal::Token),
                "{refused}"
            );
        }
    }

    #[test]
    fn base64_decodes_rfc_4648_vectors() {
        use super::base64_decode;
        for (text, plain) in [
            ("Zg==", "f"),
            ("Zm8=", "fo"),
            ("Zm9v", "foo"),
            ("Zm9vYg==", "foob"),
            ("Zm9vYmE=", "fooba"),
            ("Zm9vYmFy", "foobar"),
        ] {
            assert_eq!(
                base64_decode(text).as_deref(),
                Some(plain.as_bytes()),
                "{text}"
            );
        }
        for bad in ["", "Zg", "Zg=", "Z===", "Zg==Zg==", "Zm9v-A==", "Zm 9"] {
            assert_eq!(base64_decode(bad), None, "{bad}");
        }
    }

    /// Without `Authorization`, the token may come as the offered subprotocol
    /// `jstream.token.<token>`, on any `Sec-WebSocket-Protocol` line, once.
    #[test]
    fn the_token_may_ride_in_the_offered_subprotocols() {
        let gate = gate(&[], &[]);
        let offer = |lines: &[&str], authorization: Option<&str>| {
            let mut pairs = vec![
                (header::HOST, "127.0.0.1:4455"),
                (header::ORIGIN, "http://127.0.0.1:4455"),
            ];
            pairs.extend(lines.iter().map(|l| (header::SEC_WEBSOCKET_PROTOCOL, *l)));
            if let Some(value) = authorization {
                pairs.push((header::AUTHORIZATION, value));
            }
            gate.check(&headers(&pairs))
        };
        assert_eq!(
            offer(&["jstream, jstream.token.t0ken"], None),
            Ok(Credential::Control)
        );
        assert_eq!(
            offer(&["jstream", "jstream.token.t0ken"], None),
            Ok(Credential::Control)
        );
        assert_eq!(
            offer(&["jstream.token.wr0ng, jstream"], None),
            Err(Refusal::Token)
        );
        assert_eq!(offer(&["jstream"], None), Err(Refusal::Token));
        assert_eq!(
            offer(&["jstream.token.t0ken, jstream.token.t0ken"], None),
            Err(Refusal::Token),
            "only one token"
        );
        // `Authorization`, when present, is the one that counts.
        assert_eq!(
            offer(&["jstream, jstream.token.t0ken"], Some("Bearer wrong")),
            Err(Refusal::Token)
        );
        assert_eq!(
            offer(&["jstream, jstream.token.wr0ng"], Some("Bearer t0ken")),
            Ok(Credential::Control)
        );
    }
}
