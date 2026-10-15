//! YouTube for JStream: Google OAuth 2.0 for installed apps (a loopback redirect with
//! PKCE, [`oauth`] and [`LoopbackRedirect`]), token refresh, and the YouTube Live API
//! calls for broadcasts and streams ([`LiveClient`]): list, create, bind, transition and
//! stream health. [`chat`] reads a broadcast's live chat and [`ChatPoller`] polls it at
//! the pace the API asks for. Audience stats come later.
//!
//! Every request goes through the [`HttpTransport`] seam: [`HttpsTransport`] (rustls
//! with `ring`) in the app, and the `fake` feature's `FakeTransport` with recorded JSON
//! in tests. Tokens, codes and stream keys only ever travel as
//! `jstream_platform::Secret`s.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-youtube/README.md#purpose, docs/adr/0041-youtube-interaction-scope-v3.md

#![forbid(unsafe_code)]

pub mod chat;
mod error;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
pub mod http;
mod https;
pub mod live;
mod loopback;
pub mod oauth;

pub use chat::ChatPoller;
pub use error::{MAX_REMOTE_TEXT, YoutubeError};
pub use http::HttpTransport;
pub use https::{DEFAULT_TIMEOUT, HttpsTransport, MAX_BODY_BYTES};
pub use live::LiveClient;
pub use loopback::LoopbackRedirect;
pub use oauth::{AccessToken, Grant, OAuthClient, PendingAuthorization, Refreshed, Scope};
