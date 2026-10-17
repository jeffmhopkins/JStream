use std::sync::Arc;
use std::time::{Duration, Instant};

use jstream_platform::Secret;

use super::{AudienceStats, CHAT_RATE_WINDOW, ChatRate, MAX_CHAT_POLLS, STATS_PARTS};
use crate::fake::FakeTransport;
use crate::{AccessToken, LiveClient, OAuthClient, Scope, YoutubeError};

const LIVE: &str = include_str!("../../fixtures/video_stats_live.json");
const ENDED: &str = include_str!("../../fixtures/video_stats_ended.json");
const QUOTA: &str = include_str!("../../fixtures/error_quota.json");

fn signed_in(transport: &FakeTransport, scopes: Vec<Scope>) -> LiveClient {
    let grant = crate::Grant {
        access: AccessToken::new(
            Secret::new("ya29.current"),
            Instant::now() + Duration::from_secs(3600),
            scopes,
        ),
        refresh: Secret::new("1//0grefresh"),
    };
    let oauth = OAuthClient::new("1234-fixture.apps.googleusercontent.com", None);
    LiveClient::from_grant(Arc::new(transport.clone()), oauth, grant)
}

#[test]
fn a_live_broadcast_reports_viewers_likes_and_views() {
    let transport = FakeTransport::new();
    transport.respond(200, LIVE);
    // JStream's one sign-in grants `youtube` (Manage), which covers the read.
    let mut client = signed_in(&transport, vec![Scope::Manage]);
    let stats = client.audience_stats("fixturebroadcast01").expect("stats");
    assert_eq!(
        stats,
        AudienceStats {
            concurrent_viewers: Some(1287),
            likes: Some(412),
            views: Some(5321),
            actual_start_time: Some("2026-10-17T18:00:04Z".into()),
            actual_end_time: None,
        }
    );
    let [request] = transport.requests().try_into().expect("one request");
    assert_eq!(request.endpoint, "videos.list");
    assert!(
        request
            .url
            .starts_with("https://www.googleapis.com/youtube/v3/videos?"),
        "{}",
        request.url
    );
    assert_eq!(request.query("part").as_deref(), Some(STATS_PARTS));
    assert_eq!(request.query("id").as_deref(), Some("fixturebroadcast01"));
    assert_eq!(request.bearer.as_deref(), Some("ya29.current"));
}

/// After the broadcast ends YouTube drops the viewer count, and a hidden like count is
/// absent: both are `None`, not 0.
#[test]
fn an_ended_broadcast_has_no_viewer_count_and_hidden_likes_are_none() {
    let transport = FakeTransport::new();
    transport.respond(200, ENDED);
    let mut client = signed_in(&transport, vec![Scope::Readonly]);
    let stats = client.audience_stats("fixturebroadcast02").expect("stats");
    assert_eq!(stats.concurrent_viewers, None);
    assert_eq!(stats.likes, None);
    assert_eq!(stats.views, Some(9001));
    assert_eq!(
        stats.actual_end_time.as_deref(),
        Some("2026-10-16T19:30:41Z")
    );
}

#[test]
fn numbers_parse_as_strings_or_numbers_and_missing_parts_are_none() {
    let transport = FakeTransport::new();
    transport.respond(
        200,
        r#"{"items":[{"id":"v","statistics":{"likeCount":7},"liveStreamingDetails":{"concurrentViewers":3}}]}"#,
    );
    transport.respond(200, r#"{"items":[{"id":"v"}]}"#);
    let mut client = signed_in(&transport, vec![Scope::Readonly]);
    let stats = client.audience_stats("v").expect("numbers");
    assert_eq!((stats.concurrent_viewers, stats.likes), (Some(3), Some(7)));
    assert_eq!(
        client.audience_stats("v").expect("bare"),
        AudienceStats::default()
    );
}

#[test]
fn an_unknown_video_is_not_found_and_an_empty_id_is_refused() {
    let transport = FakeTransport::new();
    transport.respond(200, r#"{"kind":"youtube#videoListResponse","items":[]}"#);
    let mut client = signed_in(&transport, vec![Scope::Readonly]);
    let err = client.audience_stats("gone").expect_err("no such video");
    assert!(
        matches!(&err, YoutubeError::NotFound { kind: "video", id } if id == "gone"),
        "{err:?}"
    );
    let err = client.audience_stats("").expect_err("empty");
    assert!(
        matches!(err, YoutubeError::InvalidArgument { .. }),
        "{err:?}"
    );
    assert_eq!(transport.requests().len(), 1, "the empty ID sent nothing");
}

#[test]
fn a_quota_error_is_an_api_error_with_its_reason() {
    let transport = FakeTransport::new();
    transport.respond(403, QUOTA);
    let mut client = signed_in(&transport, vec![Scope::Readonly]);
    let err = client
        .audience_stats("fixturebroadcast01")
        .expect_err("quota");
    assert!(
        matches!(&err, YoutubeError::Api { status: 403, reason: Some(r), .. } if r == "quotaExceeded"),
        "{err:?}"
    );
}

#[test]
fn chat_rate_is_none_without_polls_and_counts_messages_per_minute() {
    let start = Instant::now();
    let mut rate = ChatRate::new();
    assert_eq!(rate.per_minute(start), None);
    // 10 messages, then 20 more 30 s later: 30 messages over 30 s is 60 a minute.
    rate.record(start, 10);
    rate.record(start + Duration::from_secs(30), 20);
    let per_minute = rate
        .per_minute(start + Duration::from_secs(30))
        .expect("polls in the window");
    assert!((per_minute - 60.0).abs() < 1e-9, "{per_minute}");
    // A first poll alone counts over at least a second, not over zero time.
    let mut first = ChatRate::new();
    first.record(start, 5);
    assert_eq!(first.per_minute(start), Some(300.0));
}

#[test]
fn chat_rate_forgets_polls_older_than_the_window() {
    let start = Instant::now();
    let mut rate = ChatRate::new();
    rate.record(start, 100);
    rate.record(start + Duration::from_secs(50), 6);
    // At 70 s the first poll is past the window; 6 messages since 50 s, over 20 s.
    let per_minute = rate
        .per_minute(start + Duration::from_secs(70))
        .expect("one poll left");
    assert!((per_minute - 18.0).abs() < 1e-9, "{per_minute}");
    // Long after, nothing is left: no data, not a quiet chat.
    assert_eq!(rate.per_minute(start + CHAT_RATE_WINDOW * 3), None);
}

#[test]
fn chat_rate_keeps_a_bounded_number_of_polls() {
    let start = Instant::now();
    let mut rate = ChatRate::new();
    for i in 0..MAX_CHAT_POLLS + 10 {
        rate.record(start + Duration::from_millis(i as u64 * 100), 1);
    }
    assert_eq!(rate.polls.len(), MAX_CHAT_POLLS);
}
