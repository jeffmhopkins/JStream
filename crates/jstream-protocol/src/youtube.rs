//! YouTube (ADR-0058): the account's sign-in state, broadcasts and streams as the engine
//! reports them, the health and audience of a live stream, the engine's estimate of the
//! day's API quota, and the results the YouTube actions return.
//!
//! JStream signs in to one Google account at a time, once, with the full `youtube`
//! scope (ADR-0041 §4, kept by ADR-0058). No token ever travels in the protocol (R-SEC-001): a sign-in
//! returns the consent URL, and the redirect it leads to completes the sign-in inside
//! the engine. A stream's key never travels either: starting a YouTube-bound output reads
//! it from the API and hands it straight to the RTMPS connection.
//!
//! Values YouTube reports (a broadcast's state, a stream's health) mirror the Live API.
//! A value this version doesn't know, from YouTube or from a newer server, is `other`.
//! Text YouTube returns (titles, health issue descriptions) is untrusted: clients show
//! it as plain text.

use crate::id::OutputId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Whether the engine is signed in to YouTube. Serialized as a snake_case string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum YoutubeAuth {
    /// The engine has no Google OAuth client (as in a build whose built-in client is
    /// still empty), so YouTube's API features are off. Clients hide YouTube's sign-in and
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

/// A running `youtube` output's audience, as YouTube last reported it (ADR-0041 §3,
/// 0.38): what the `youtube.stats` event sends and the `youtube.stats` query lists.
/// The engine reads it at a low fixed rate, about once a minute, while the output runs.
/// A count YouTube doesn't report (the owner hides it, or the broadcast has no viewers
/// now or has ended) is omitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AudienceStatsInfo {
    /// The output.
    pub output_id: OutputId,
    /// The broadcast it sends to.
    pub broadcast_id: String,
    /// People watching now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrent_viewers: Option<u64>,
    /// Likes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub likes: Option<u64>,
    /// Views.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub views: Option<u64>,
}

/// How the engine polls audience stats, by how much of the day's estimated YouTube
/// quota is left (ADR-0041 §5). Serialized as a snake_case string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StatsPolling {
    /// At the normal rate.
    #[default]
    Normal,
    /// Less often, because the quota is running low.
    Slowed,
    /// Not at all until the quota resets, because it is nearly or entirely used up.
    Stopped,
    /// A value this version doesn't know, from a newer server.
    #[serde(other)]
    Other,
}

/// The engine's estimate of the day's YouTube API quota (ADR-0041 §5). It adds up what
/// each call the engine makes costs by YouTube's published unit costs. YouTube's own
/// count, which other apps on the same Google project share, can differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QuotaInfo {
    /// Units used since the quota last reset, by the engine's count.
    pub used: u64,
    /// The units the engine assumes a day has.
    pub daily: u64,
    /// Whether YouTube refused a call since the quota last reset because it is used up.
    /// Omitted when it hasn't.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exhausted: bool,
    /// How audience stats are polled now.
    pub stats: StatsPolling,
}
