//! YouTube (ADR-0041): the account's sign-in state, broadcasts and streams as the engine
//! reports them, the health of a live stream, and the results the YouTube actions return.
//!
//! JStream signs in to one Google account at a time, once, with the full `youtube`
//! scope (ADR-0041 §4). No token ever travels in the protocol (R-SEC-001): a sign-in
//! returns the consent URL, and the redirect it leads to completes the sign-in inside
//! the engine. A stream's key never travels either: starting a YouTube-bound output reads
//! it from the API and hands it straight to the RTMPS connection.
//!
//! Values YouTube reports (a broadcast's state, a stream's health) mirror the Live API.
//! A value this version doesn't know, from YouTube or from a newer server, is `other`.
//! Text YouTube returns (titles, health issue descriptions) is untrusted: clients show
//! it as plain text.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Whether the engine is signed in to YouTube. Serialized as a snake_case string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum YoutubeAuth {
    /// No Google OAuth client is configured, so YouTube's API features are off (JStream
    /// ships none; the user brings their own). Clients hide YouTube's sign-in and
    /// broadcast controls; the YouTube actions and queries fail with `action_failed`.
    Unavailable,
    /// Not signed in. `youtube.sign_in` starts a sign-in.
    #[default]
    SignedOut,
    /// A sign-in is waiting for the user to finish it in the browser.
    SigningIn,
    /// Signed in. The engine holds the account's refresh token in the OS credential
    /// store and uses it for every YouTube call.
    SignedIn,
    /// A state this version of the protocol doesn't know, from a newer server.
    #[serde(other)]
    Other,
}

/// Where a broadcast is in its lifecycle (`status.lifeCycleStatus`). Serialized as a
/// snake_case string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BroadcastLifeCycle {
    /// Created, but missing settings it needs to go live.
    Created,
    /// Ready to go live once a stream is bound and sending.
    Ready,
    /// Moving to `testing`.
    TestStarting,
    /// Visible only through its monitor stream.
    Testing,
    /// Moving to `live`.
    LiveStarting,
    /// Visible to viewers.
    Live,
    /// Ended.
    Complete,
    /// Removed by YouTube.
    Revoked,
    /// A state this version doesn't know, from YouTube or a newer server.
    #[serde(other)]
    Other,
}

/// Who can watch a broadcast. Serialized as a snake_case string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BroadcastPrivacy {
    /// Only the channel.
    #[default]
    Private,
    /// Anyone.
    Public,
    /// Anyone with the link.
    Unlisted,
    /// A value this version doesn't know, from YouTube or a newer server. Refused in
    /// `youtube.create_broadcast` with `invalid_argument`.
    #[serde(other)]
    Other,
}

/// Which way `youtube.transition_broadcast` moves a broadcast. Serialized as a
/// snake_case string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BroadcastTransition {
    /// Visible only through the monitor stream. The bound stream must be sending.
    Testing,
    /// Visible to viewers. The bound stream must be sending.
    Live,
    /// Ended. A complete broadcast can't go live again.
    Complete,
}

/// Which broadcasts `youtube.list_broadcasts` returns, by state. Serialized as a
/// snake_case string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BroadcastListStatus {
    /// Every broadcast.
    #[default]
    All,
    /// Not started yet.
    Upcoming,
    /// Live now (or testing).
    Active,
    /// Ended.
    Completed,
}

/// Which broadcasts `youtube.list_broadcasts` returns, by type. Serialized as a
/// snake_case string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BroadcastType {
    /// Both kinds.
    #[default]
    All,
    /// Scheduled events.
    Event,
    /// The channel's persistent ("Stream now") broadcast.
    Persistent,
}

/// One broadcast, as the YouTube queries and actions report it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BroadcastInfo {
    /// YouTube's ID for it: what a `youtube` output target names.
    pub broadcast_id: String,
    /// Its title.
    pub title: String,
    /// Its description. Omitted when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// When it is scheduled to start, RFC 3339, as YouTube reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_start_time: Option<String>,
    /// Where it is in its lifecycle.
    pub life_cycle: BroadcastLifeCycle,
    /// Who can watch it.
    pub privacy: BroadcastPrivacy,
    /// The stream bound to it, if any. A `youtube` output can start only once one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_stream_id: Option<String>,
}

/// What YouTube says a stream is doing (`status.streamStatus`). Serialized as a
/// snake_case string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StreamStatus {
    /// Receiving data.
    Active,
    /// Created, with settings still missing.
    Created,
    /// Something is wrong with it.
    Error,
    /// Not receiving data.
    Inactive,
    /// Ready to receive data.
    Ready,
    /// A value this version doesn't know, from YouTube or a newer server.
    #[serde(other)]
    Other,
}

/// How YouTube rates what a stream receives (`status.healthStatus.status`). Serialized
/// as a snake_case string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StreamHealth {
    /// No problems.
    Good,
    /// Minor problems.
    Ok,
    /// Problems that hurt the stream.
    Bad,
    /// YouTube has no data yet.
    NoData,
    /// A value this version doesn't know, from YouTube or a newer server.
    #[serde(other)]
    Other,
}

/// How serious a [`HealthIssue`] is. Serialized as a snake_case string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum IssueSeverity {
    /// For information.
    Info,
    /// Worth fixing.
    Warning,
    /// Hurts the stream.
    Error,
    /// A value this version doesn't know, from YouTube or a newer server.
    #[serde(other)]
    Other,
}

/// One problem YouTube found with a stream's settings (`configurationIssues`). Its text
/// is YouTube's, cut to 512 characters with control characters replaced: show it as
/// plain text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HealthIssue {
    /// YouTube's name for the problem (`gopSizeLong`, `bitrateLow`, …).
    pub issue: String,
    /// How serious it is.
    pub severity: IssueSeverity,
    /// A short reason.
    pub reason: String,
    /// A longer explanation.
    pub description: String,
}

/// One stream, as `youtube.list_streams` reports it. Its key is never reported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StreamInfo {
    /// YouTube's ID for it: what `youtube.bind_broadcast` takes.
    pub stream_id: String,
    /// Its title.
    pub title: String,
    /// What it is doing.
    pub status: StreamStatus,
    /// How healthy what it receives is.
    pub health: StreamHealth,
}

/// What an action returns beyond being accepted. Serialized with an internal `type`
/// tag repeating the action's name; most actions return none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum ActionResult {
    /// The result of `youtube.sign_in`.
    #[serde(rename = "youtube.sign_in")]
    YoutubeSignIn {
        /// Google's consent page. The client opens it in a browser; once the user
        /// agrees, Google redirects to the engine, which finishes the sign-in and emits
        /// `youtube.status_changed`.
        url: String,
    },
    /// The result of `youtube.create_broadcast`: the new broadcast.
    #[serde(rename = "youtube.create_broadcast")]
    YoutubeCreateBroadcast {
        /// The broadcast, as YouTube created it.
        broadcast: BroadcastInfo,
    },
    /// The result of `youtube.bind_broadcast`: the broadcast with its new stream.
    #[serde(rename = "youtube.bind_broadcast")]
    YoutubeBindBroadcast {
        /// The broadcast after the bind.
        broadcast: BroadcastInfo,
    },
    /// The result of `youtube.transition_broadcast`.
    #[serde(rename = "youtube.transition_broadcast")]
    YoutubeTransitionBroadcast {
        /// The broadcast after the transition. Its lifecycle may still be moving
        /// (`test_starting`, `live_starting`).
        broadcast: BroadcastInfo,
    },
    /// A result this version of the protocol doesn't know, from a newer server.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}
