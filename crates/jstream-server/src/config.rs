//! The server's configuration: where it listens, the control token, and which `Host` and
//! `Origin` values it accepts beyond its own.

use crate::error::ServerError;
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};

/// The control token every connection must present (ADR-0006).
///
/// Its `Debug` form never shows the value, and it is compared in time that reveals neither
/// the value nor its length.
#[derive(Clone)]
pub struct Token(String);

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
        Ok(Token(value))
    }

    /// Whether `presented` is this token, in time that depends only on `presented`'s
    /// length: neither where the first difference is nor the token's own length shows.
    ///
    /// Every presented byte is compared with the token repeated to that length (a token
    /// is never empty), and the difference of the two lengths is folded in, so a value
    /// that is the token repeated (`abcabc` for `abc`) still fails.
    pub(crate) fn matches(&self, presented: &[u8]) -> bool {
        let expected = self.0.as_bytes();
        let lengths = expected.len() ^ presented.len();
        let bytes = expected
            .iter()
            .cycle()
            .zip(presented)
            .fold(0u8, |diff, (a, b)| diff | (a ^ b));
        (lengths | usize::from(bytes)) == 0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(***)")
    }
}

/// How the server listens and whom it lets in.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// The address to listen on. [`ServerConfig::new`] uses `127.0.0.1` and an ephemeral
    /// port; listening beyond localhost is an explicit choice (R-SEC-002).
    pub bind: SocketAddr,
    /// The control token.
    pub token: Token,
    /// `Host` values (`name:port`) accepted besides the loopback names
    /// (`127.0.0.1`, `localhost`, `[::1]`) and the bound address, each with the bound
    /// port. Compared without regard to ASCII case. A `Host` without a port means port 80,
    /// so it matches `name:80` when the server listens on port 80, and nothing otherwise.
    pub extra_hosts: Vec<String>,
    /// `Origin` values (`scheme://host[:port]`) accepted besides the server's own
    /// (`http://` plus an accepted `Host`). Compared without regard to ASCII case.
    pub extra_origins: Vec<String>,
}

impl ServerConfig {
    /// Listens on `127.0.0.1` with an ephemeral port, accepting only its own hosts and
    /// origins.
    pub fn new(token: Token) -> Self {
        ServerConfig {
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            token,
            extra_hosts: Vec::new(),
            extra_origins: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Token;

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
