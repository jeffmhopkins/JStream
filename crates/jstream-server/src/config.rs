//! The server's configuration: where it listens, the control token, which `Host` and
//! `Origin` values it accepts beyond its own, the connection limits, the MCP transport, and
//! the web UI's directory.

use crate::error::ServerError;
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

/// The default [`ServerConfig::max_connections`]: a handful of UIs, CLIs and MCP
/// proxies, with room to spare.
const MAX_CONNECTIONS: usize = 32;

/// The default [`ServerConfig::handshake_timeout`].
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The default [`ServerConfig::header_read_timeout`].
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The default [`ServerConfig::max_mcp_sessions`].
const MAX_MCP_SESSIONS: usize = 16;

/// The default [`ServerConfig::mcp_idle_timeout`].
const MCP_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The control token every connection must present (ADR-0006).
///
/// Only its BLAKE3 digest is kept, so its `Debug` form can't show the value, and a
/// presented token is compared in time that reveals neither the value nor its length.
#[derive(Clone)]
pub struct Token(blake3::Hash);

impl Token {
    /// Wraps a token. It must be non-empty visible ASCII (`!` to `~`), so it fits an
    /// `Authorization` header unchanged.
    pub fn new(value: impl Into<String>) -> Result<Token, ServerError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ServerError::InvalidToken {
                reason: "it is empty",
            });
        }
        if !value.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(ServerError::InvalidToken {
                reason: "it must be visible ASCII, with no spaces",
            });
        }
        Ok(Token(blake3::hash(value.as_bytes())))
    }

    /// Whether `presented` is this token, in time that depends only on `presented`'s
    /// length: neither where the first difference is nor the token's own length shows.
    ///
    /// `presented` is hashed to the same fixed 32 bytes as the token (whose digest was
    /// taken once, in [`Token::new`]), and the two digests are compared with
    /// `blake3::Hash`'s equality, which is constant-time: it always looks at all 32
    /// bytes. Hashing takes time that depends only on the input's length.
    pub(crate) fn matches(&self, presented: &[u8]) -> bool {
        blake3::hash(presented) == self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(***)")
    }
}

/// How the server listens and whom it lets in.
///
/// Every connection limit and timeout must be greater than zero:
/// [`Server::bind`](crate::Server::bind) refuses a zero one
/// ([`ServerError::InvalidLimit`]).
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// The address to listen on. [`ServerConfig::new`] uses `127.0.0.1` and an ephemeral
    /// port; listening beyond localhost is an explicit choice (R-SEC-002).
    pub bind: SocketAddr,
    /// The control token.
    pub token: Token,
    /// `Host` values (`name:port`) accepted besides the loopback names
    /// (`127.0.0.1`, `localhost`, `[::1]`) and the bound address, each with the bound
    /// port. Compared without regard to ASCII case. Each entry is `name:port` with
    /// exactly one port, and an IPv6 address goes in brackets (`[fd00::1]:4455`), as a
    /// request's `Host` writes it. [`Server::bind`](crate::Server::bind) refuses an entry
    /// without a port, with more than one, or with an unbracketed IPv6 address
    /// ([`ServerError::InvalidHost`]), since none would ever match. A request's `Host`
    /// without a port means port 80, so it matches `name:80` when the server listens on
    /// port 80, and nothing otherwise.
    pub extra_hosts: Vec<String>,
    /// `Origin` values (`scheme://host[:port]`) accepted besides the server's own
    /// (`http://` plus an accepted `Host`). Compared without regard to ASCII case.
    pub extra_origins: Vec<String>,
    /// The most WebSocket connections open at once. An upgrade beyond it is refused with
    /// HTTP 503 before a session is opened. [`ServerConfig::new`] sets 32.
    pub max_connections: usize,
    /// How long a connection has, from being accepted, to complete the protocol
    /// handshake (`hello` answered by `welcome`). One that hasn't is closed (close code
    /// 1008). [`ServerConfig::new`] sets 10 s.
    pub handshake_timeout: Duration,
    /// How long a connection has, from being accepted, to send a complete upgrade
    /// request that is accepted. One that hasn't (it sent its headers too slowly, sent
    /// none, or was refused and kept the connection open) is dropped.
    /// [`ServerConfig::new`] sets 10 s.
    pub header_read_timeout: Duration,
    /// Whether MCP's streamable-HTTP transport is served at
    /// [`MCP_PATH`](crate::MCP_PATH), behind the same checks. [`ServerConfig::new`]
    /// leaves it off; the host turns it on.
    pub mcp: bool,
    /// The most MCP sessions (one per `initialize`) open at once. An `initialize` beyond
    /// it is refused with HTTP 503. [`ServerConfig::new`] sets 16.
    pub max_mcp_sessions: usize,
    /// How long an MCP session may go unused before it is ended, and its `Control`
    /// session with it. [`ServerConfig::new`] sets 30 minutes.
    pub mcp_idle_timeout: Duration,
    /// The web UI's built bundle (`trunk build`'s `dist` directory), served at every
    /// path the API doesn't use, behind the same checks, with a browser's `Basic`
    /// credentials carrying the token. [`Server::bind`](crate::Server::bind) refuses one
    /// that isn't a directory ([`ServerError::UiDir`]). [`ServerConfig::new`] leaves it
    /// unset: no UI, and those paths are 404.
    pub ui_dir: Option<PathBuf>,
}

impl ServerConfig {
    /// Listens on `127.0.0.1` with an ephemeral port, accepting only its own hosts and
    /// origins, with the default connection limits and without MCP.
    pub fn new(token: Token) -> Self {
        ServerConfig {
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            token,
            extra_hosts: Vec::new(),
            extra_origins: Vec::new(),
            max_connections: MAX_CONNECTIONS,
            handshake_timeout: HANDSHAKE_TIMEOUT,
            header_read_timeout: HEADER_READ_TIMEOUT,
            mcp: false,
            max_mcp_sessions: MAX_MCP_SESSIONS,
            mcp_idle_timeout: MCP_IDLE_TIMEOUT,
            ui_dir: None,
        }
    }

    /// Refuses a zero connection limit or timeout ([`ServerError::InvalidLimit`]): the
    /// server would refuse or drop every connection or MCP session. Checked by
    /// [`Server::bind`](crate::Server::bind) before anything is opened.
    pub(crate) fn check_limits(&self) -> Result<(), ServerError> {
        let zero = [
            ("max_connections", self.max_connections == 0),
            ("handshake_timeout", self.handshake_timeout.is_zero()),
            ("header_read_timeout", self.header_read_timeout.is_zero()),
            ("max_mcp_sessions", self.max_mcp_sessions == 0),
            ("mcp_idle_timeout", self.mcp_idle_timeout.is_zero()),
        ];
        match zero.into_iter().find(|&(_, is_zero)| is_zero) {
            Some((field, _)) => Err(ServerError::InvalidLimit { field }),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ServerConfig, Token};
    use crate::error::ServerError;
    use std::time::Duration;

    /// Sets one field of a config to zero.
    type Zero = fn(&mut ServerConfig);

    /// #140 N3: a zero limit or timeout is refused, naming the field; the defaults pass.
    #[test]
    fn zero_limits_and_timeouts_are_refused() {
        let Ok(token) = Token::new("abc") else {
            panic!("a valid token");
        };
        let config = ServerConfig::new(token);
        assert!(config.check_limits().is_ok(), "the defaults");
        let cases: [(&str, Zero); 5] = [
            ("max_connections", |c| c.max_connections = 0),
            ("handshake_timeout", |c| {
                c.handshake_timeout = Duration::ZERO
            }),
            ("header_read_timeout", |c| {
                c.header_read_timeout = Duration::ZERO
            }),
            ("max_mcp_sessions", |c| c.max_mcp_sessions = 0),
            ("mcp_idle_timeout", |c| c.mcp_idle_timeout = Duration::ZERO),
        ];
        for (name, zero) in cases {
            let mut bad = config.clone();
            zero(&mut bad);
            match bad.check_limits() {
                Err(ServerError::InvalidLimit { field }) => assert_eq!(field, name),
                other => panic!("{name}: expected InvalidLimit, got {other:?}"),
            }
        }
        let mut small = config;
        small.max_connections = 1;
        small.handshake_timeout = Duration::from_millis(1);
        assert!(small.check_limits().is_ok(), "small but nonzero");
    }

    #[test]
    fn tokens_must_fit_a_header_and_never_print() {
        assert!(Token::new("").is_err());
        assert!(Token::new("has space").is_err());
        assert!(Token::new("tab\t").is_err());
        assert!(Token::new("caf\u{e9}").is_err());
        let token = Token::new("s3cret-T0ken").ok();
        assert_eq!(format!("{token:?}"), "Some(Token(***))");
    }

    #[test]
    fn tokens_match_exactly() {
        let Ok(token) = Token::new("abc") else {
            panic!("a valid token");
        };
        assert!(token.matches(b"abc"));
        assert!(!token.matches(b"abd"));
        assert!(!token.matches(b"ABC"));
        assert!(!token.matches(b"ab"));
        assert!(!token.matches(b"abcd"));
        assert!(!token.matches(b"abcabc"));
        assert!(!token.matches(b""));
    }

    /// The compare doesn't stop early on a length mismatch (#44), and a wrong token of
    /// the same length, of another length, or an empty one still fails.
    #[test]
    fn wrong_tokens_of_any_length_fail() {
        let Ok(token) = Token::new("s3cret-T0ken") else {
            panic!("a valid token");
        };
        assert!(token.matches(b"s3cret-T0ken"), "the token");
        assert!(!token.matches(b"s3cret-T0kem"), "same length");
        assert!(!token.matches(b"s3cret-T0ke"), "shorter");
        assert!(!token.matches(b"s3cret-T0ken-"), "longer");
        assert!(!token.matches(b"s3cret-T0kens3cret-T0ken"), "repeated");
        assert!(!token.matches(b""), "empty");
    }
}
