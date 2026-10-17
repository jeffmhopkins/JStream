//! YouTube for JStream: Google OAuth 2.0 for installed apps (a loopback redirect with
//! PKCE, [`oauth`] and [`LoopbackRedirect`]), token refresh, and the YouTube Live API
//! calls for broadcasts and streams ([`LiveClient`]): list, create, bind, transition and
//! stream health, and audience stats ([`stats`]). Chat comes later.
//!
//! Every request goes through the [`HttpTransport`] seam: [`HttpsTransport`] (rustls
//! with `ring`) in the app, and the `fake` feature's `FakeTransport` with recorded JSON
//! in tests. Tokens, codes and stream keys only ever travel as
//! `jstream_platform::Secret`s.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-youtube/README.md#purpose, docs/adr/0058-youtube-sign-in-with-a-jstream-client.md

#![forbid(unsafe_code)]

mod error;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
pub mod http;
mod https;
pub mod live;
mod loopback;
pub mod oauth;
pub mod stats;

pub use error::{MAX_REMOTE_TEXT, YoutubeError};
pub use http::HttpTransport;
pub use https::{DEFAULT_TIMEOUT, HttpsTransport, MAX_BODY_BYTES};
pub use live::LiveClient;
pub use loopback::LoopbackRedirect;
pub use oauth::{
    AccessToken, BUILT_IN_CLIENT_ID, BUILT_IN_CLIENT_SECRET, Grant, OAuthClient,
    PendingAuthorization, Refreshed, Scope,
};
pub use stats::{AudienceStats, ChatRate};
