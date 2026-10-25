use std::sync::Arc;
use std::time::{Duration, Instant};

use jstream_platform::Secret;

use super::*;
use crate::fake::FakeTransport;
use crate::http::Method;
use crate::{AccessToken, Grant, OAuthClient};

const PAGE: &str = include_str!("../../fixtures/chat_messages.json");
const OFFLINE: &str = include_str!("../../fixtures/chat_messages_offline.json");
const ENDED: &str = include_str!("../../fixtures/error_chat_ended.json");
const QUOTA: &str = include_str!("../../fixtures/error_quota.json");

fn signed_in(transport: &FakeTransport) -> LiveClient {
    let grant = Grant {
        access: AccessToken::new(
            Secret::new("ya29.current"),
            Instant::now() + Duration::from_secs(3600),
            vec![Scope::Manage],
        ),
        refresh: Secret::new("1//0grefresh"),
    };
    let oauth = OAuthClient::new("1234-fixture.apps.googleusercontent.com", None);
    LiveClient::from_grant(Arc::new(transport.clone()), oauth, grant)
}

#[test]
fn chat_messages_parses_a_page_as_plain_text() {
    let transport = FakeTransport::new();
    transport.respond(200, PAGE);
    let mut client = signed_in(&transport);

    let page = client
        .chat_messages("Cg0KC2ZpeHR1cmUtY2hhdA", None)
        .unwrap();

    let [request] = transport.requests().try_into().unwrap();
    assert_eq!(request.method, Method::Get);
    assert!(
        request
            .url
            .starts_with("https://www.googleapis.com/youtube/v3/liveChat/messages?")
    );
    assert_eq!(request.endpoint, "liveChatMessages.list");
    assert_eq!(
        request.query("liveChatId").as_deref(),
        Some("Cg0KC2ZpeHR1cmUtY2hhdA")
    );
    assert_eq!(
        request.query("part").as_deref(),
        Some("id,snippet,authorDetails")
    );
    assert_eq!(request.query("maxResults").as_deref(), Some("2000"));
    assert_eq!(request.query("pageToken"), None);

    assert_eq!(page.polling_interval, Some(Duration::from_millis(3500)));
    assert_eq!(
        page.next_page_token.as_deref(),
        Some("GO2tqPrW_IcDILe9y7LW_4cD")
    );
    assert_eq!(page.offline_at, None);
    let [text, sup, deleted, other] = page.messages.try_into().unwrap();

    assert_eq!(text.id, "LCC.text-1");
    assert_eq!(text.kind, ChatEvent::Text);
    // Control characters are replaced; markup stays as the literal text it is.
    assert_eq!(text.text, "hello  <b>world</b>");
    assert_eq!(text.author.name, "Viewer One");
    assert!(text.author.is_moderator && text.author.is_member);
    assert!(!text.author.is_owner && !text.author.is_verified);
    assert_eq!(
        text.published_at.as_deref(),
        Some("2026-10-15T15:00:01.000Z")
    );

    assert_eq!(
        sup.kind,
        ChatEvent::SuperChat(Paid {
            amount: "$5.00".into(),
            currency: "USD".into(),
            amount_micros: Some(5_000_000),
        })
    );
    assert!(sup.author.is_verified);

    assert_eq!(
        deleted.kind,
        ChatEvent::Deleted {
            message_id: "LCC.text-0".into()
        }
    );
    assert_eq!(other.kind, ChatEvent::Other("pollEvent".into()));
}

#[test]
fn long_text_and_names_are_cut() {
    let long = "x".repeat(MAX_MESSAGE_CHARS + 50);
    let name = "n".repeat(MAX_AUTHOR_CHARS + 50);
    let body = serde_json::json!({
        "items": [{
            "id": "LCC.long",
            "snippet": { "type": "textMessageEvent", "displayMessage": long },
            "authorDetails": { "displayName": name }
        }]
    });
    let transport = FakeTransport::new();
    transport.respond(200, body.to_string());
    let mut client = signed_in(&transport);

    let page = client.chat_messages("chat", Some("next")).unwrap();

    assert_eq!(
        transport.requests()[0].query("pageToken").as_deref(),
        Some("next")
    );
    assert_eq!(page.polling_interval, None);
    assert_eq!(page.messages[0].text.chars().count(), MAX_MESSAGE_CHARS);
    assert_eq!(
        page.messages[0].author.name.chars().count(),
        MAX_AUTHOR_CHARS
    );
}

#[test]
fn an_empty_chat_id_is_refused_before_sending() {
    let transport = FakeTransport::new();
    let mut client = signed_in(&transport);
    assert!(matches!(
        client.chat_messages("", None),
        Err(YoutubeError::InvalidArgument { .. })
    ));
    assert!(transport.requests().is_empty());
}

#[test]
fn the_poller_waits_the_interval_the_api_asks_for_and_follows_the_page_token() {
    let transport = FakeTransport::new();
    transport.respond(200, PAGE);
    transport.respond(200, r#"{"items":[],"pollingIntervalMillis":5000}"#);
    let mut client = signed_in(&transport);
    let start = Instant::now();
    let mut poller = ChatPoller::new("chat", start);
    assert_eq!(poller.due(), start);

    let Poll::Messages(first) = poller.poll(&mut client, start).unwrap() else {
        panic!("a page");
    };
    assert_eq!(first.len(), 4);
    assert_eq!(poller.due(), start + Duration::from_millis(3500));

    // Too soon: nothing is sent.
    let early = start + Duration::from_secs(1);
    assert_eq!(
        poller.poll(&mut client, early).unwrap(),
        Poll::NotYet(start + Duration::from_millis(3500))
    );
    assert_eq!(transport.requests().len(), 1);

    let later = start + Duration::from_secs(4);
    assert_eq!(
        poller.poll(&mut client, later).unwrap(),
        Poll::Messages(vec![])
    );
    let requests = transport.requests();
    assert_eq!(
        requests[1].query("pageToken").as_deref(),
        Some("GO2tqPrW_IcDILe9y7LW_4cD")
    );
    // An empty page keeps the token it had.
    assert_eq!(
        poller.page_token.as_deref(),
        Some("GO2tqPrW_IcDILe9y7LW_4cD")
    );
    assert_eq!(poller.due(), later + Duration::from_secs(5));
}

#[test]
fn a_zero_interval_still_waits_the_minimum() {
    let transport = FakeTransport::new();
    transport.respond(200, r#"{"items":[],"pollingIntervalMillis":0}"#);
    let mut client = signed_in(&transport);
    let now = Instant::now();
    let mut poller = ChatPoller::new("chat", now);
    poller.poll(&mut client, now).unwrap();
    assert_eq!(poller.due(), now + MIN_POLL_INTERVAL);
}

#[test]
fn quota_errors_back_off_doubling_up_to_the_cap() {
    let transport = FakeTransport::new();
    let mut client = signed_in(&transport);
    let mut now = Instant::now();
    let mut poller = ChatPoller::new("chat", now);
    let mut waits = Vec::new();
    for _ in 0..8 {
        transport.respond(403, QUOTA);
        let err = poller.poll(&mut client, now).unwrap_err();
        assert!(err.is_quota());
        waits.push(poller.due() - now);
        now = poller.due();
    }
    let minutes = |m: u64| Duration::from_secs(m * 60);
    assert_eq!(
        waits,
        [
            minutes(1),
            minutes(2),
            minutes(4),
            minutes(8),
            minutes(16),
            minutes(30),
            minutes(30),
            minutes(30)
        ]
    );

    // A page resets the backoff.
    transport.respond(200, r#"{"items":[],"pollingIntervalMillis":2000}"#);
    poller.poll(&mut client, now).unwrap();
    transport.respond(500, "oops");
    let then = poller.due();
    assert!(poller.poll(&mut client, then).is_err());
    assert_eq!(poller.due() - then, ERROR_BACKOFF);
}

#[test]
fn other_errors_back_off_from_five_seconds() {
    let transport = FakeTransport::new();
    let mut client = signed_in(&transport);
    let now = Instant::now();
    let mut poller = ChatPoller::new("chat", now);
    // Nothing queued: a transport error.
    assert!(matches!(
        poller.poll(&mut client, now),
        Err(YoutubeError::Transport { .. })
    ));
    assert_eq!(poller.due() - now, ERROR_BACKOFF);
    let next = poller.due();
    assert!(poller.poll(&mut client, next).is_err());
    assert_eq!(poller.due() - next, ERROR_BACKOFF * 2);
}

#[test]
fn an_ended_chat_stops_polling() {
    let transport = FakeTransport::new();
    transport.respond(403, ENDED);
    let mut client = signed_in(&transport);
    let now = Instant::now();
    let mut poller = ChatPoller::new("chat", now);
    assert_eq!(poller.poll(&mut client, now).unwrap(), Poll::Ended);
    assert!(poller.is_ended());
    assert_eq!(
        poller
            .poll(&mut client, now + Duration::from_secs(60))
            .unwrap(),
        Poll::Ended
    );
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn an_offline_page_ends_the_chat() {
    let transport = FakeTransport::new();
    transport.respond(200, OFFLINE);
    let mut client = signed_in(&transport);
    let now = Instant::now();
    let mut poller = ChatPoller::new("chat", now);
    assert_eq!(poller.poll(&mut client, now).unwrap(), Poll::Ended);
    assert!(poller.is_ended());
}

/// Each class of error keeps its own ladder: a quota error after transport errors
/// waits the first quota step, not a step doubled by the other errors (review N2 on
/// #363).
#[test]
fn a_quota_error_after_other_errors_starts_at_the_first_quota_step() {
    let transport = FakeTransport::new();
    let mut client = signed_in(&transport);
    let mut now = Instant::now();
    let mut poller = ChatPoller::new("chat", now);
    for _ in 0..3 {
        // Nothing queued: a transport error.
        assert!(poller.poll(&mut client, now).is_err());
        now = poller.due();
    }
    transport.respond(403, QUOTA);
    assert!(poller.poll(&mut client, now).unwrap_err().is_quota());
    assert_eq!(poller.due() - now, QUOTA_BACKOFF);
}
