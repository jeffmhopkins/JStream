//! Google OAuth 2.0 for installed apps: the authorization-code flow with PKCE (RFC 7636,
//! `S256`) and a loopback redirect, token refresh and revocation.
//!
//! The flow, with [`LoopbackRedirect`](crate::LoopbackRedirect) receiving the code:
//! 1. [`OAuthClient::authorize`] builds the consent URL and a [`PendingAuthorization`]
//!    holding the PKCE verifier and `state`. The host opens the URL in the browser.
//! 2. The browser comes back to the loopback redirect with a code.
//! 3. [`OAuthClient::exchange`] trades the code and verifier for a [`Grant`]: an access
//!    token and the refresh token.
//! 4. [`OAuthClient::refresh`] later trades the refresh token for a new access token.
//!
//! Every token, code and the client secret travel as [`Secret`]s (R-SEC-001,
//! docs/RULES.md#R-SEC-001). Keeping the refresh token in the OS credential store is the
//! host's job: this crate hands it over and takes it back, and never writes it anywhere.

use std::fmt;
use std::time::{Duration, Instant};

use jstream_platform::Secret;
use serde::Deserialize;

use crate::http::{Body, HttpRequest, HttpTransport, Method, base64url, encode_pairs};
use crate::{YoutubeError, error::remote_text};

/// Google's authorization endpoint.
pub const AUTHORIZATION_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// Google's token endpoint.
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
/// Google's revocation endpoint.
pub const REVOCATION_ENDPOINT: &str = "https://oauth2.googleapis.com/revoke";

/// An access token is refreshed once less than this much of its life is left, so a call
/// never starts with a token about to expire.
pub const REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// A YouTube OAuth scope (ADR-0041 §4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// `youtube.readonly`: listing broadcasts and streams, and stream health.
    Readonly,
    /// `youtube`: everything `Readonly` allows, plus creating broadcasts and streams,
    /// binding and transitioning. Asked for only when broadcast control is first used.
    Manage,
}

impl Scope {
    /// The scope's URL, as the authorization request names it.
    pub fn url(self) -> &'static str {
        match self {
            Self::Readonly => "https://www.googleapis.com/auth/youtube.readonly",
            Self::Manage => "https://www.googleapis.com/auth/youtube",
        }
    }

    /// The scopes a token's space-separated `scope` string grants. `youtube.force-ssl`
    /// counts as [`Manage`](Self::Manage): the Live API methods accept either. Other
    /// scopes are ignored.
    pub fn parse_granted(scopes: &str) -> Vec<Self> {
        let mut granted: Vec<Self> = scopes
            .split_ascii_whitespace()
            .filter_map(|s| match s {
                "https://www.googleapis.com/auth/youtube.readonly" => Some(Self::Readonly),
                "https://www.googleapis.com/auth/youtube"
                | "https://www.googleapis.com/auth/youtube.force-ssl" => Some(Self::Manage),
                _ => None,
            })
            .collect();
        granted.sort();
        granted.dedup();
        granted
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Readonly => "youtube.readonly",
            Self::Manage => "youtube",
        })
    }
}

/// The app's OAuth client, as registered in Google Cloud ("Desktop app" type).
///
/// Google issues installed apps a client secret too. It can't be kept secret in a
/// shipped app, but it is still handled as a [`Secret`] so it never prints.
pub struct OAuthClient {
    client_id: String,
    client_secret: Option<Secret>,
}

impl fmt::Debug for OAuthClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthClient")
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret)
            .finish_non_exhaustive()
    }
}

impl OAuthClient {
    /// The client with ID `client_id` and, if it has one, its secret.
    pub fn new(client_id: impl Into<String>, client_secret: Option<Secret>) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret,
        }
    }

    /// The client ID.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Starts a sign-in: the consent URL to open, and the PKCE verifier and `state` to
    /// finish it with. `redirect_uri` is the loopback listener's
    /// ([`LoopbackRedirect::redirect_uri`](crate::LoopbackRedirect::redirect_uri)).
    ///
    /// Google doesn't support incremental authorization for installed apps, so to add
    /// [`Scope::Manage`] later, sign in again asking for every scope wanted.
    ///
    /// # Errors
    /// [`YoutubeError::InvalidArgument`] when `scopes` is empty;
    /// [`YoutubeError::Transport`] when the OS random source fails.
    pub fn authorize(
        &self,
        redirect_uri: &str,
        scopes: &[Scope],
    ) -> Result<PendingAuthorization, YoutubeError> {
        if scopes.is_empty() {
            return Err(YoutubeError::InvalidArgument {
                reason: "a sign-in needs at least one scope",
            });
        }
        let verifier = Secret::new(base64url(&random_bytes::<32>()?));
        let state = base64url(&random_bytes::<16>()?);
        let challenge = pkce_challenge(&verifier);
        let mut scopes = scopes.to_vec();
        scopes.sort();
        scopes.dedup();
        let scope = scopes.iter().map(|s| s.url()).collect::<Vec<_>>().join(" ");
        let query = encode_pairs([
            ("client_id", self.client_id.as_str()),
            ("redirect_uri", redirect_uri),
            ("response_type", "code"),
            ("scope", scope.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
        ]);
        Ok(PendingAuthorization {
            url: format!("{AUTHORIZATION_ENDPOINT}?{query}"),
            redirect_uri: redirect_uri.to_owned(),
            state,
            verifier,
            scopes,
        })
    }

    /// Finishes a sign-in: trades the code the redirect brought for tokens.
    ///
    /// # Errors
    /// [`YoutubeError::OAuth`] when the token endpoint refuses (an `invalid_grant` here
    /// means the code or verifier is wrong or used); [`YoutubeError::InvalidResponse`]
    /// when no refresh token came back; transport errors.
    pub fn exchange(
        &self,
        transport: &dyn HttpTransport,
        pending: PendingAuthorization,
        code: Secret,
    ) -> Result<Grant, YoutubeError> {
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("client_id", self.client_id.as_str()),
            ("code", code.expose()),
            ("code_verifier", pending.verifier.expose()),
            ("redirect_uri", pending.redirect_uri.as_str()),
        ];
        if let Some(secret) = &self.client_secret {
            form.push(("client_secret", secret.expose()));
        }
        let response = self.token_request(transport, &form, false)?;
        let refresh = response
            .refresh_token
            .ok_or(YoutubeError::InvalidResponse {
                endpoint: "token",
                what: "the code exchange returned no refresh token".into(),
            })?;
        Ok(Grant {
            access: response.access,
            refresh,
        })
    }

    /// A new access token for `refresh_token`.
    ///
    /// Google usually keeps the refresh token; if it sends a new one, it is in
    /// [`Refreshed::rotated_refresh_token`] and replaces the stored one.
    ///
    /// # Errors
    /// [`YoutubeError::SignInRequired`] when the refresh token was revoked or expired
    /// (`invalid_grant`); [`YoutubeError::OAuth`] for other refusals; transport errors.
    pub fn refresh(
        &self,
        transport: &dyn HttpTransport,
        refresh_token: &Secret,
    ) -> Result<Refreshed, YoutubeError> {
        let mut form = vec![
            ("grant_type", "refresh_token"),
            ("client_id", self.client_id.as_str()),
            ("refresh_token", refresh_token.expose()),
        ];
        if let Some(secret) = &self.client_secret {
            form.push(("client_secret", secret.expose()));
        }
        let response = self.token_request(transport, &form, true)?;
        Ok(Refreshed {
            access: response.access,
            rotated_refresh_token: response.refresh_token,
        })
    }

    /// Revokes `token` (a refresh token revokes the whole grant), for signing out.
    ///
    /// # Errors
    /// [`YoutubeError::OAuth`] when Google refuses (for example, the token is already
    /// invalid); transport errors.
    pub fn revoke(
        &self,
        transport: &dyn HttpTransport,
        token: &Secret,
    ) -> Result<(), YoutubeError> {
        let body = Secret::new(encode_pairs([("token", token.expose())]));
        let response = transport.send(HttpRequest {
            method: Method::Post,
            url: REVOCATION_ENDPOINT.to_owned(),
            bearer: None,
            body: Body::Form(body),
            endpoint: "revoke",
        })?;
        if response.is_success() {
            Ok(())
        } else {
            Err(oauth_error(response.status, response.body()))
        }
    }

    /// Posts `form` to the token endpoint and parses a token response.
    fn token_request(
        &self,
        transport: &dyn HttpTransport,
        form: &[(&str, &str)],
        refreshing: bool,
    ) -> Result<TokenResponse, YoutubeError> {
        let body = Secret::new(encode_pairs(form.iter().copied()));
        let requested_at = Instant::now();
        let response = transport.send(HttpRequest {
            method: Method::Post,
            url: TOKEN_ENDPOINT.to_owned(),
            bearer: None,
            body: Body::Form(body),
            endpoint: "token",
        })?;
        if !response.is_success() {
            let err = oauth_error(response.status, response.body());
            return Err(match err {
                YoutubeError::OAuth { error, .. } if refreshing && error == "invalid_grant" => {
                    YoutubeError::SignInRequired
                }
                err => err,
            });
        }
        let raw: RawToken =
            serde_json::from_slice(response.body()).map_err(|e| YoutubeError::json("token", &e))?;
        if !raw.token_type.eq_ignore_ascii_case("bearer") {
            return Err(YoutubeError::InvalidResponse {
                endpoint: "token",
                what: "the token type is not Bearer".into(),
            });
        }
        // Timed from when the request was sent, so the token is never thought to live
        // longer than it does.
        let expires_at = requested_at
            .checked_add(Duration::from_secs(raw.expires_in))
            .ok_or(YoutubeError::InvalidResponse {
                endpoint: "token",
                what: "the token's lifetime is out of range".into(),
            })?;
        Ok(TokenResponse {
            access: AccessToken {
                secret: Secret::new(raw.access_token),
                expires_at,
                scopes: Scope::parse_granted(raw.scope.as_deref().unwrap_or_default()),
            },
            refresh_token: raw.refresh_token.map(Secret::new),
        })
    }
}

/// A sign-in in progress: the consent URL, and what finishing it needs.
pub struct PendingAuthorization {
    url: String,
    redirect_uri: String,
    state: String,
    verifier: Secret,
    scopes: Vec<Scope>,
}

impl fmt::Debug for PendingAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingAuthorization")
            .field("redirect_uri", &self.redirect_uri)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

impl PendingAuthorization {
    /// The consent URL for the user's browser. It holds the PKCE challenge, never the
    /// verifier.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The `state` the redirect must bring back.
    pub fn state(&self) -> &str {
        &self.state
    }

    /// The redirect URI the sign-in was started with.
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// The scopes asked for.
    pub fn scopes(&self) -> &[Scope] {
        &self.scopes
    }
}

/// An OAuth access token, with when it expires and what it grants.
pub struct AccessToken {
    secret: Secret,
    expires_at: Instant,
    scopes: Vec<Scope>,
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessToken")
            .field("secret", &self.secret)
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .finish()
    }
}

impl AccessToken {
    /// A token that expires at `expires_at` and grants `scopes`.
    pub fn new(secret: Secret, expires_at: Instant, scopes: Vec<Scope>) -> Self {
        Self {
            secret,
            expires_at,
            scopes,
        }
    }

    /// The token itself, for the `Authorization` header.
    pub fn secret(&self) -> &Secret {
        &self.secret
    }

    /// When it expires.
    pub fn expires_at(&self) -> Instant {
        self.expires_at
    }

    /// The scopes the grant holds.
    pub fn scopes(&self) -> &[Scope] {
        &self.scopes
    }

    /// Whether the grant allows what `needed` allows. [`Scope::Manage`] covers reads.
    pub fn allows(&self, needed: Scope) -> bool {
        self.scopes.contains(&needed) || self.scopes.contains(&Scope::Manage)
    }

    /// Whether it should be refreshed before use at `now`: less than
    /// [`REFRESH_MARGIN`] of its life is left.
    pub fn is_stale(&self, now: Instant) -> bool {
        now + REFRESH_MARGIN >= self.expires_at
    }
}

/// What a finished sign-in yields. The host stores the refresh token in the OS
/// credential store.
#[derive(Debug)]
pub struct Grant {
    /// The first access token.
    pub access: AccessToken,
    /// The refresh token, which keeps the grant alive across restarts.
    pub refresh: Secret,
}

/// What a refresh yields.
#[derive(Debug)]
pub struct Refreshed {
    /// The new access token.
    pub access: AccessToken,
    /// A new refresh token, when Google rotated it. It replaces the stored one.
    pub rotated_refresh_token: Option<Secret>,
}

struct TokenResponse {
    access: AccessToken,
    refresh_token: Option<Secret>,
}

/// The token endpoint's JSON. The token strings move into [`Secret`]s unchanged.
#[derive(Deserialize)]
struct RawToken {
    access_token: String,
    expires_in: u64,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    token_type: String,
}

#[derive(Deserialize)]
struct RawOAuthError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// An error from the token or revocation endpoint: its OAuth `error` and description,
/// or the bare status when the body isn't an OAuth error.
fn oauth_error(status: u16, body: &[u8]) -> YoutubeError {
    match serde_json::from_slice::<RawOAuthError>(body) {
        Ok(raw) => YoutubeError::OAuth {
            error: remote_text(&raw.error),
            description: raw.error_description.as_deref().map(remote_text),
        },
        Err(_) => YoutubeError::OAuth {
            error: format!("http_{status}"),
            description: None,
        },
    }
}

/// The PKCE `S256` challenge: base64url(SHA-256(verifier)), RFC 7636 §4.2.
pub(crate) fn pkce_challenge(verifier: &Secret) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, verifier.expose().as_bytes());
    base64url(digest.as_ref())
}

/// `N` bytes from the operating system's random source.
fn random_bytes<const N: usize>() -> Result<[u8; N], YoutubeError> {
    use ring::rand::SecureRandom;
    let mut bytes = [0; N];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| {
            YoutubeError::transport(
                "reading the OS random source",
                std::io::Error::other("the OS random source failed"),
            )
        })?;
    Ok(bytes)
}

#[cfg(test)]
mod tests;
