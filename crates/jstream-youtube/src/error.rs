//! The crate's one error type. No variant ever carries a token, an authorization code,
//! a stream key or a response body (R-SEC-001, docs/RULES.md#R-SEC-001).

use std::io;

/// Everything that can go wrong signing in or calling the YouTube Live API.
///
/// Text that comes from Google (an API error's `message`, an OAuth error's
/// `error_description`) is untrusted: it is kept as plain text, cut to
/// [`MAX_REMOTE_TEXT`] characters, and never holds a secret, because Google's errors
/// don't echo tokens. A response that fails to parse is reported by position only, since
/// a JSON parser's message can quote the value it choked on.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum YoutubeError {
    /// The request never got an HTTP response: name resolution, connecting, TLS, a
    /// timeout, or a malformed response.
    #[error("HTTP transport failed while {context}: {source}")]
    Transport {
        /// What the transport was doing.
        context: &'static str,
        /// The underlying error. TLS errors arrive wrapped in an `io::Error`.
        #[source]
        source: io::Error,
    },

    /// The API answered with an error status (YouTube's JSON error, when it sent one).
    #[error("YouTube API error {status}{}: {message}", reason.as_deref().map(|r| format!(" ({r})")).unwrap_or_default())]
    Api {
        /// The HTTP status.
        status: u16,
        /// The first error's `reason`, such as `quotaExceeded` or `invalidTransition`.
        reason: Option<String>,
        /// The error's `message`, cut to [`MAX_REMOTE_TEXT`] characters.
        message: String,
    },

    /// The token endpoint refused a request, other than a refresh token that no longer
    /// works (that is [`SignInRequired`](Self::SignInRequired)).
    #[error("OAuth error {error}{}", description.as_deref().map(|d| format!(": {d}")).unwrap_or_default())]
    OAuth {
        /// The OAuth `error` code, such as `invalid_request` or `redirect_uri_mismatch`.
        error: String,
        /// Google's `error_description`, cut to [`MAX_REMOTE_TEXT`] characters.
        description: Option<String>,
    },

    /// The user has to sign in again: the refresh token was revoked or has expired
    /// (`invalid_grant`), or the API refused a freshly refreshed access token.
    #[error("YouTube sign-in is needed again")]
    SignInRequired,

    /// The signed-in grant lacks the scope a call needs. Broadcast control needs
    /// [`Scope::Manage`](crate::Scope::Manage) (ADR-0041 §4, kept by ADR-0051).
    #[error("the YouTube grant lacks the {needed} scope")]
    MissingScope {
        /// The scope the call needs.
        needed: crate::Scope,
    },

    /// The user, or Google, ended the sign-in in the browser with an error, such as
    /// `access_denied`.
    #[error("sign-in was not completed: {error}")]
    AuthorizationDenied {
        /// The `error` the redirect carried, reduced to `[a-z_]` characters.
        error: String,
    },

    /// No redirect with the expected `state` reached the loopback listener in time.
    #[error("timed out waiting for the sign-in redirect")]
    AuthorizationTimedOut,

    /// A response arrived but wasn't what the API documents.
    #[error("unexpected response from {endpoint}: {what}")]
    InvalidResponse {
        /// The endpoint, such as `liveBroadcasts.list` or `token`.
        endpoint: &'static str,
        /// What was wrong, without quoting the response.
        what: String,
    },

    /// The API returned no resource with the ID asked for.
    #[error("no {kind} with ID {id:?}")]
    NotFound {
        /// `"broadcast"` or `"stream"`.
        kind: &'static str,
        /// The ID that was asked for.
        id: String,
    },

    /// An argument was refused before any request was sent.
    #[error("invalid argument: {reason}")]
    InvalidArgument {
        /// Why, as a fixed string.
        reason: &'static str,
    },
}

/// The most characters of Google-supplied error text an error keeps.
pub const MAX_REMOTE_TEXT: usize = 512;

/// API error reasons that mean the project's quota or a rate limit ran out
/// (ADR-0041 §5, kept by ADR-0051: quota errors are surfaced as protocol errors).
const QUOTA_REASONS: &[&str] = &[
    "quotaExceeded",
    "dailyLimitExceeded",
    "rateLimitExceeded",
    "userRateLimitExceeded",
    "userRequestsExceedRateLimit",
];

impl YoutubeError {
    /// Whether this is the API reporting that quota or a rate limit ran out.
    pub fn is_quota(&self) -> bool {
        matches!(self, Self::Api { reason: Some(r), .. } if QUOTA_REASONS.contains(&r.as_str()))
    }

    pub(crate) fn transport(context: &'static str, source: io::Error) -> Self {
        Self::Transport { context, source }
    }

    /// A JSON parse failure, reported by position only.
    pub(crate) fn json(endpoint: &'static str, err: &serde_json::Error) -> Self {
        let kind = match err.classify() {
            serde_json::error::Category::Io => "I/O error",
            serde_json::error::Category::Syntax => "malformed JSON",
            serde_json::error::Category::Data => "JSON of the wrong shape",
            serde_json::error::Category::Eof => "truncated JSON",
        };
        Self::InvalidResponse {
            endpoint,
            what: format!("{kind} at line {}, column {}", err.line(), err.column()),
        }
    }
}

/// `text` cut to [`MAX_REMOTE_TEXT`] characters, with control characters replaced, so
/// untrusted remote text can't forge log lines.
pub(crate) fn remote_text(text: &str) -> String {
    text.chars()
        .take(MAX_REMOTE_TEXT)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
