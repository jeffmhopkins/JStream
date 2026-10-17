//! Audience stats (ADR-0041 §3, kept by ADR-0058): a live broadcast's concurrent viewers
//! and likes, read with `videos.list`, and the chat rate, derived from the chat the
//! engine already polls.
//!
//! These are reads only. The engine polls [`LiveClient::audience_stats`] at
//! [`STATS_INTERVAL`] (a low fixed rate, ADR-0041 §5) and reports the result; this
//! crate keeps no timer. [`ChatRate`] counts the messages each chat poll returns and
//! gives messages per minute over the last [`CHAT_RATE_WINDOW`].
//!
//! Docs: crates/jstream-youtube/README.md#purpose, docs/adr/0058-youtube-sign-in-with-a-jstream-client.md

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::http::{Body, Method};
use crate::live::{Page, first, non_empty, u64_or_string};
use crate::{LiveClient, Scope, YoutubeError};

/// How often the engine polls [`LiveClient::audience_stats`]: a low fixed rate
/// (ADR-0041 §5). Each call costs one unit of the day's quota.
pub const STATS_INTERVAL: Duration = Duration::from_secs(60);

/// The window [`ChatRate`] counts messages over.
pub const CHAT_RATE_WINDOW: Duration = Duration::from_secs(60);

/// The most chat polls [`ChatRate`] keeps. Polls come at the interval the API asks for
/// (`pollingIntervalMillis`), assumed here to be at least half a second, so a minute's
/// window fits; if polls come faster, the cap only shortens the window.
const MAX_CHAT_POLLS: usize = 128;

/// The `part`s `videos.list` is asked for: the live details for the viewer count, and
/// the statistics for the like and view counts.
const STATS_PARTS: &str = "id,liveStreamingDetails,statistics";

/// A broadcast's audience, as `videos.list` reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudienceStats {
    /// People watching now. `None` when the broadcast has no current viewers, its owner
    /// hides the count, or it has ended (YouTube stops counting then).
    pub concurrent_viewers: Option<u64>,
    /// Likes. `None` when the owner hides them.
    pub likes: Option<u64>,
    /// Views. `None` when the owner hides them.
    pub views: Option<u64>,
    /// When the broadcast started, RFC 3339, once it has.
    pub actual_start_time: Option<String>,
    /// When the broadcast ended, RFC 3339, once it has.
    pub actual_end_time: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Video {
    #[serde(default)]
    live_streaming_details: Option<LiveDetails>,
    #[serde(default)]
    statistics: Option<Statistics>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LiveDetails {
    #[serde(default, deserialize_with = "u64_or_string")]
    concurrent_viewers: Option<u64>,
    #[serde(default)]
    actual_start_time: Option<String>,
    #[serde(default)]
    actual_end_time: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Statistics {
    #[serde(default, deserialize_with = "u64_or_string")]
    like_count: Option<u64>,
    #[serde(default, deserialize_with = "u64_or_string")]
    view_count: Option<u64>,
}

impl LiveClient {
    /// The audience of broadcast `broadcast_id` (a broadcast's ID is its video's), with
    /// `videos.list`, for the engine to poll at [`STATS_INTERVAL`] while live. Needs
    /// [`Scope::Readonly`], which JStream's one `youtube` sign-in covers (ADR-0041 §4).
    ///
    /// # Errors
    /// [`YoutubeError::NotFound`] when there is no video with that ID;
    /// [`YoutubeError::InvalidArgument`] for an empty ID; API (quota included), OAuth
    /// and transport errors.
    pub fn audience_stats(&mut self, broadcast_id: &str) -> Result<AudienceStats, YoutubeError> {
        let query = [
            ("part", STATS_PARTS.to_owned()),
            ("id", non_empty(broadcast_id)?),
        ];
        let page: Page<Video> = self.call(
            Method::Get,
            "videos",
            &query,
            Body::Empty,
            "videos.list",
            Scope::Readonly,
        )?;
        let video = first(page, "video", broadcast_id)?;
        let live = video.live_streaming_details.unwrap_or_default();
        let statistics = video.statistics.unwrap_or_default();
        Ok(AudienceStats {
            concurrent_viewers: live.concurrent_viewers,
            likes: statistics.like_count,
            views: statistics.view_count,
            actual_start_time: live.actual_start_time,
            actual_end_time: live.actual_end_time,
        })
    }
}

/// The chat rate: messages per minute over the last [`CHAT_RATE_WINDOW`], from the
/// number of messages each chat poll returned. It holds counts, never messages.
#[derive(Clone, Debug, Default)]
pub struct ChatRate {
    /// Each poll's time and message count, oldest first.
    polls: VecDeque<(Instant, usize)>,
}

impl ChatRate {
    /// No polls yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a chat poll at `at` that returned `messages` new messages. Polls older
    /// than the window are dropped; so is the oldest when [`MAX_CHAT_POLLS`] are kept.
    pub fn record(&mut self, at: Instant, messages: usize) {
        self.forget_before(at);
        if self.polls.len() == MAX_CHAT_POLLS {
            self.polls.pop_front();
        }
        self.polls.push_back((at, messages));
    }

    /// Messages per minute at `now`, over the polls of the last [`CHAT_RATE_WINDOW`].
    /// `None` until two polls are in the window, so "no data" isn't shown as a quiet
    /// chat.
    ///
    /// A poll returns the messages that arrived since the poll before it, so the oldest
    /// poll kept is only the window's start: the rate is the messages of the polls after
    /// it, over the time from it to the newest. A first poll's backlog is that start, not
    /// a burst.
    pub fn per_minute(&mut self, now: Instant) -> Option<f64> {
        self.forget_before(now);
        let (oldest, _) = *self.polls.front()?;
        let (newest, _) = *self.polls.back()?;
        let span = newest.saturating_duration_since(oldest);
        if span.is_zero() {
            return None;
        }
        let messages: usize = self.polls.iter().skip(1).map(|(_, n)| n).sum();
        Some(messages as f64 * 60.0 / span.as_secs_f64())
    }

    fn forget_before(&mut self, now: Instant) {
        while let Some((at, _)) = self.polls.front() {
            if now.saturating_duration_since(*at) > CHAT_RATE_WINDOW {
                self.polls.pop_front();
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests;
