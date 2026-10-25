use std::sync::Arc;
use std::time::{Duration, Instant};

use jstream_platform::Secret;

use super::*;
use crate::fake::{FakeTransport, RecordedRequest};
use crate::http::Method;

const REFRESH: &str = include_str!("../../fixtures/token_refresh.json");
const INVALID_GRANT: &str = include_str!("../../fixtures/token_invalid_grant.json");
const BROADCASTS: &str = include_str!("../../fixtures/broadcasts_list.json");
const INSERTED: &str = include_str!("../../fixtures/broadcast_insert.json");
const BOUND: &str = include_str!("../../fixtures/broadcast_bind.json");
const LIVE: &str = include_str!("../../fixtures/broadcast_transition_live.json");
const STREAMS: &str = include_str!("../../fixtures/streams_list.json");
const NEW_STREAM: &str = include_str!("../../fixtures/stream_insert.json");
const HEALTH: &str = include_str!("../../fixtures/stream_health.json");
const QUOTA: &str = include_str!("../../fixtures/error_quota.json");
const REDUNDANT: &str = include_str!("../../fixtures/error_redundant_transition.json");
const UNAUTHORIZED: &str = include_str!("../../fixtures/error_unauthorized.json");

fn oauth() -> OAuthClient {
    OAuthClient::new("1234-fixture.apps.googleusercontent.com", None)
}

/// A client whose access token is fresh and grants `scopes`.
fn signed_in(transport: &FakeTransport, scopes: Vec<Scope>) -> LiveClient {
    let grant = crate::Grant {
        access: AccessToken::new(
            Secret::new("ya29.current"),
            Instant::now() + Duration::from_secs(3600),
            scopes,
        ),
        refresh: Secret::new("1//0grefresh"),
    };
    LiveClient::from_grant(Arc::new(transport.clone()), oauth(), grant)
}

fn only(transport: &FakeTransport) -> RecordedRequest {
    let [request] = transport.requests().try_into().unwrap();
    request
}

#[test]
fn list_broadcasts_parses_a_page_and_tolerates_unknown_values() {
    let transport = FakeTransport::new();
    transport.respond(200, BROADCASTS);
    let mut client = signed_in(&transport, vec![Scope::Readonly]);

    let page = client
        .list_broadcasts(
            &BroadcastFilter::Status(BroadcastStatusFilter::Upcoming),
            &PageRequest {
                max_results: 2,
                page_token: Some("CAAQAA".into()),
            },
        )
        .unwrap();

    let request = only(&transport);
    assert_eq!(request.method, Method::Get);
    assert!(
        request
            .url
            .starts_with("https://www.googleapis.com/youtube/v3/liveBroadcasts?")
    );
    assert_eq!(request.endpoint, "liveBroadcasts.list");
    assert_eq!(request.bearer.as_deref(), Some("ya29.current"));
    assert_eq!(
        request.query("part").as_deref(),
        Some("id,snippet,contentDetails,status")
    );
    assert_eq!(
        request.query("broadcastStatus").as_deref(),
        Some("upcoming")
    );
    assert_eq!(request.query("maxResults").as_deref(), Some("2"));
    assert_eq!(request.query("pageToken").as_deref(), Some("CAAQAA"));
    assert_eq!(request.query("mine"), None);

    assert_eq!(page.next_page_token.as_deref(), Some("CAEQAA"));
    assert_eq!(
        page.page_info,
        Some(PageInfo {
            total_results: 3,
            results_per_page: 2
        })
    );
    let [first, second] = <[Broadcast; 2]>::try_from(page.items).unwrap();
    assert_eq!(first.id, "dQw4w9WgXcQ");
    assert_eq!(first.snippet.title, "Friday stream");
    assert_eq!(
        first.snippet.scheduled_start_time.as_deref(),
        Some("2026-10-16T19:00:00Z")
    );
    assert!(first.snippet.live_chat_id.is_some());
    assert_eq!(first.status.life_cycle_status, Some(LifeCycleStatus::Ready));
    assert_eq!(first.status.privacy_status, Some(Privacy::Unlisted));
    assert_eq!(
        first.content_details.bound_stream_id.as_deref(),
        Some("UCfixturechannel000000000fixturestream1")
    );
    assert_eq!(
        first.content_details.monitor_stream,
        Some(MonitorStream {
            enable_monitor_stream: Some(true),
            broadcast_stream_delay_ms: Some(0)
        })
    );
    assert_eq!(
        second.status.life_cycle_status,
        Some(LifeCycleStatus::Other)
    );
    assert_eq!(second.content_details.bound_stream_id, None);
}

/// Persistent ("Stream now") broadcasts are listed only when asked for (review N5 on
/// #167): the type goes out as `broadcastType`, and an ID list refuses one.
#[test]
fn a_broadcast_type_filter_is_sent_with_mine() {
    let transport = FakeTransport::new();
    transport.respond(200, BROADCASTS);
    let mut client = signed_in(&transport, vec![Scope::Manage]);
    client
        .list_broadcasts_of_type(
            &BroadcastFilter::Mine,
            Some(BroadcastType::All),
            &PageRequest::default(),
        )
        .unwrap();
    let request = only(&transport);
    assert_eq!(request.query("mine").as_deref(), Some("true"));
    assert_eq!(request.query("broadcastType").as_deref(), Some("all"));

    let err = client
        .list_broadcasts_of_type(
            &BroadcastFilter::Ids(vec!["dQw4w9WgXcQ".into()]),
            Some(BroadcastType::Persistent),
            &PageRequest::default(),
        )
        .unwrap_err();
    assert!(matches!(err, YoutubeError::InvalidArgument { .. }), "{err}");
    assert_eq!(transport.requests().len(), 1, "nothing more was sent");
}

#[test]
fn filters_and_page_sizes_are_checked_before_sending() {
    let transport = FakeTransport::new();
    let mut client = signed_in(&transport, vec![Scope::Readonly]);
    let page = PageRequest::default();
    let too_many: Vec<String> = (0..51).map(|i| format!("id{i}")).collect();
    for filter in [
        BroadcastFilter::Ids(vec![]),
        BroadcastFilter::Ids(too_many),
        BroadcastFilter::Ids(vec!["a,b".into()]),
        BroadcastFilter::Ids(vec![String::new()]),
    ] {
        let err = client.list_broadcasts(&filter, &page).unwrap_err();
        assert!(
            matches!(err, YoutubeError::InvalidArgument { .. }),
            "{filter:?}: {err}"
        );
    }
    let err = client
        .list_streams(
            &StreamFilter::Mine,
            &PageRequest {
                max_results: 51,
                page_token: None,
            },
        )
        .unwrap_err();
    assert!(matches!(err, YoutubeError::InvalidArgument { .. }));
    assert!(client.bind("", None).is_err());
    assert!(transport.requests().is_empty());
}

#[test]
fn insert_broadcast_sends_jstreams_defaults() {
    let transport = FakeTransport::new();
    transport.respond(200, INSERTED);
    let mut client = signed_in(&transport, vec![Scope::Manage]);

    let created = client
        .insert_broadcast(&NewBroadcast::new(
            "JStream go-live test",
            "2026-10-15T02:00:00Z",
            Privacy::Private,
        ))
        .unwrap();

    let request = only(&transport);
    assert_eq!(request.method, Method::Post);
    assert_eq!(request.endpoint, "liveBroadcasts.insert");
    assert_eq!(
        request.query("part").as_deref(),
        Some("id,snippet,contentDetails,status")
    );
    let body: serde_json::Value = serde_json::from_str(&request.body).unwrap();
    assert_eq!(
        body,
        serde_json::json!({
            "snippet": {
                "title": "JStream go-live test",
                "description": "",
                "scheduledStartTime": "2026-10-15T02:00:00Z",
            },
            "status": { "privacyStatus": "private", "selfDeclaredMadeForKids": false },
            "contentDetails": {
                "enableAutoStart": false,
                "enableAutoStop": false,
                "monitorStream": { "enableMonitorStream": true },
            },
        })
    );
    assert_eq!(created.id, "jNQXAC9IVRw");
    assert_eq!(
        created.status.life_cycle_status,
        Some(LifeCycleStatus::Created)
    );
}

#[test]
fn insert_broadcast_refuses_what_the_api_would() {
    let transport = FakeTransport::new();
    let mut client = signed_in(&transport, vec![Scope::Manage]);
    let good = NewBroadcast::new("t", "2026-10-15T02:00:00Z", Privacy::Public);
    for bad in [
        NewBroadcast {
            title: " ".into(),
            ..good.clone()
        },
        NewBroadcast {
            scheduled_start_time: String::new(),
            ..good.clone()
        },
        NewBroadcast {
            privacy: Privacy::Other,
            ..good.clone()
        },
    ] {
        assert!(matches!(
            client.insert_broadcast(&bad),
            Err(YoutubeError::InvalidArgument { .. })
        ));
    }
    let mut with_end = good;
    with_end.scheduled_end_time = Some("2026-10-15T04:00:00Z".into());
    transport.respond(200, INSERTED);
    client.insert_broadcast(&with_end).unwrap();
    let body: serde_json::Value = serde_json::from_str(&only(&transport).body).unwrap();
    assert_eq!(body["snippet"]["scheduledEndTime"], "2026-10-15T04:00:00Z");
}

#[test]
fn broadcast_control_needs_the_manage_scope() {
    let transport = FakeTransport::new();
    let mut client = signed_in(&transport, vec![Scope::Readonly]);
    let err = client
        .transition("jNQXAC9IVRw", Transition::Live)
        .unwrap_err();
    assert!(
        matches!(
            err,
            YoutubeError::MissingScope {
                needed: Scope::Manage
            }
        ),
        "{err}"
    );
    assert!(transport.requests().is_empty());

    // A token restored without its scope list goes through, and the API decides.
    transport.respond(200, LIVE);
    let mut restored = signed_in(&transport, vec![]);
    restored
        .transition("jNQXAC9IVRw", Transition::Live)
        .unwrap();
}

#[test]
fn bind_and_unbind() {
    let transport = FakeTransport::new();
    transport.respond(200, BOUND).respond(200, INSERTED);
    let mut client = signed_in(&transport, vec![Scope::Manage]);

    let bound = client
        .bind(
            "jNQXAC9IVRw",
            Some("UCfixturechannel000000000fixturestream2"),
        )
        .unwrap();
    assert_eq!(
        bound.content_details.bound_stream_id.as_deref(),
        Some("UCfixturechannel000000000fixturestream2")
    );
    client.bind("jNQXAC9IVRw", None).unwrap();

    let [bind, unbind] = <[RecordedRequest; 2]>::try_from(transport.requests()).unwrap();
    for request in [&bind, &unbind] {
        assert_eq!(request.method, Method::Post);
        assert!(
            request
                .url
                .starts_with("https://www.googleapis.com/youtube/v3/liveBroadcasts/bind?")
        );
        assert_eq!(request.query("id").as_deref(), Some("jNQXAC9IVRw"));
        assert!(request.body.is_empty());
    }
    assert_eq!(
        bind.query("streamId").as_deref(),
        Some("UCfixturechannel000000000fixturestream2")
    );
    assert_eq!(unbind.query("streamId"), None);
}

#[test]
fn transition_and_its_documented_errors() {
    let transport = FakeTransport::new();
    transport.respond(200, LIVE).respond(403, REDUNDANT);
    let mut client = signed_in(&transport, vec![Scope::Manage]);

    let live = client.transition("jNQXAC9IVRw", Transition::Live).unwrap();
    assert_eq!(
        live.status.life_cycle_status,
        Some(LifeCycleStatus::LiveStarting)
    );
    assert_eq!(
        live.snippet.actual_start_time.as_deref(),
        Some("2026-10-15T02:00:31Z")
    );

    let err = client
        .transition("jNQXAC9IVRw", Transition::Live)
        .unwrap_err();
    match &err {
        YoutubeError::Api {
            status: 403,
            reason: Some(reason),
            message,
        } => {
            assert_eq!(reason, "redundantTransition");
            assert_eq!(message, "Redundant transition");
        }
        other => panic!("{other}"),
    }
    assert!(!err.is_quota());

    let requests = transport.requests();
    assert_eq!(
        requests[0].query("broadcastStatus").as_deref(),
        Some("live")
    );
    assert!(requests[0].url.contains("/liveBroadcasts/transition?"));
}

#[test]
fn quota_errors_are_recognised() {
    let transport = FakeTransport::new();
    transport
        .respond(403, QUOTA)
        .respond(500, "<html>backend</html>");
    let mut client = signed_in(&transport, vec![Scope::Readonly]);
    let err = client
        .list_broadcasts(&BroadcastFilter::Mine, &PageRequest::default())
        .unwrap_err();
    assert!(err.is_quota(), "{err}");

    let err = client
        .list_broadcasts(&BroadcastFilter::Mine, &PageRequest::default())
        .unwrap_err();
    assert!(
        matches!(
            &err,
            YoutubeError::Api {
                status: 500,
                reason: None,
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn streams_parse_with_the_key_as_a_secret() {
    let transport = FakeTransport::new();
    transport.respond(200, STREAMS);
    let mut client = signed_in(&transport, vec![Scope::Readonly]);

    let stream = client
        .stream("UCfixturechannel000000000fixturestream1")
        .unwrap();
    let request = only(&transport);
    assert_eq!(
        request.query("part").as_deref(),
        Some("id,snippet,cdn,status")
    );
    assert_eq!(
        request.query("id").as_deref(),
        Some("UCfixturechannel000000000fixturestream1")
    );

    let info = &stream.cdn.ingestion_info;
    assert_eq!(
        info.stream_name.as_ref().map(Secret::expose),
        Some("abcd-efgh-ijkl-mnop-qrst")
    );
    assert_eq!(
        info.rtmps_ingestion_address.as_deref(),
        Some("rtmps://a.rtmps.youtube.com/live2")
    );
    assert_eq!(stream.cdn.ingestion_type.as_deref(), Some("rtmp"));
    assert_eq!(stream.status.stream_status, Some(StreamStatus::Inactive));
    assert_eq!(
        stream.status.health_status.as_ref().and_then(|h| h.status),
        Some(HealthStatus::NoData)
    );
    let shown = format!("{stream:?}");
    assert!(!shown.contains("abcd-efgh"), "{shown}");
}

#[test]
fn a_missing_resource_is_not_found() {
    let transport = FakeTransport::new();
    transport
        .respond(200, r#"{"items": []}"#)
        .respond(200, "{}");
    let mut client = signed_in(&transport, vec![Scope::Readonly]);
    assert!(matches!(
        client.stream("nope"),
        Err(YoutubeError::NotFound { kind: "stream", .. })
    ));
    assert!(matches!(
        client.broadcast("nope"),
        Err(YoutubeError::NotFound {
            kind: "broadcast",
            ..
        })
    ));
}

#[test]
fn insert_stream_asks_for_a_reusable_variable_rtmp_stream() {
    let transport = FakeTransport::new();
    transport.respond(200, NEW_STREAM);
    let mut client = signed_in(&transport, vec![Scope::Manage]);

    let stream = client.insert_stream(&NewStream::new("JStream")).unwrap();
    let request = only(&transport);
    assert_eq!(request.endpoint, "liveStreams.insert");
    assert_eq!(
        request.query("part").as_deref(),
        Some("id,snippet,cdn,contentDetails,status")
    );
    let body: serde_json::Value = serde_json::from_str(&request.body).unwrap();
    assert_eq!(
        body,
        serde_json::json!({
            "snippet": { "title": "JStream", "description": "" },
            "cdn": { "ingestionType": "rtmp", "resolution": "variable", "frameRate": "variable" },
            "contentDetails": { "isReusable": true },
        })
    );
    assert_eq!(stream.content_details.is_reusable, Some(true));
    assert_eq!(
        stream
            .cdn
            .ingestion_info
            .stream_name
            .as_ref()
            .map(Secret::expose),
        Some("wxyz-1234-5678-9abc-defg")
    );

    let long = "x".repeat(129);
    for bad in [
        NewStream::new(""),
        NewStream::new(&long),
        NewStream {
            resolution: Resolution::P1080,
            ..NewStream::new("t")
        },
        NewStream {
            frame_rate: FrameRate::Fps60,
            ..NewStream::new("t")
        },
    ] {
        assert!(matches!(
            client.insert_stream(&bad),
            Err(YoutubeError::InvalidArgument { .. })
        ));
    }
    let fixed = NewStream {
        resolution: Resolution::P1080,
        frame_rate: FrameRate::Fps60,
        ..NewStream::new("t")
    };
    assert_eq!(fixed.json_body().map(|b| !b.is_empty()).ok(), Some(true));
}

#[test]
fn stream_health_reads_status_and_configuration_issues() {
    let transport = FakeTransport::new();
    transport.respond(200, HEALTH);
    let mut client = signed_in(&transport, vec![Scope::Readonly]);

    let status = client
        .stream_health("UCfixturechannel000000000fixturestream2")
        .unwrap();
    assert_eq!(only(&transport).query("part").as_deref(), Some("id,status"));
    assert_eq!(status.stream_status, Some(StreamStatus::Active));
    let health = status.health_status.unwrap();
    assert_eq!(health.status, Some(HealthStatus::Bad));
    assert_eq!(health.last_update_time_seconds, Some(1_792_030_000));
    let kinds: Vec<_> = health
        .configuration_issues
        .iter()
        .map(|i| (i.kind.as_str(), i.severity))
        .collect();
    assert_eq!(
        kinds,
        [
            ("gopSizeLong", Some(Severity::Error)),
            ("bitrateLow", Some(Severity::Warning)),
            ("somethingNew", Some(Severity::Other)),
        ]
    );
}

#[test]
fn the_first_call_refreshes_and_a_stale_token_is_refreshed_first() {
    let transport = FakeTransport::new();
    transport.respond(200, REFRESH).respond(200, STREAMS);
    let mut client = LiveClient::new(
        Arc::new(transport.clone()),
        oauth(),
        Secret::new("1//0grefresh"),
    );
    assert_eq!(client.granted_scopes(), None);

    client
        .list_streams(&StreamFilter::Mine, &PageRequest::default())
        .unwrap();
    let [refresh, list] = <[RecordedRequest; 2]>::try_from(transport.requests()).unwrap();
    assert_eq!(refresh.endpoint, "token");
    assert_eq!(
        refresh.form("refresh_token").as_deref(),
        Some("1//0grefresh")
    );
    assert_eq!(
        list.bearer.as_deref(),
        Some("ya29.a0AfH6SMfixture-access-2")
    );
    assert_eq!(list.query("mine").as_deref(), Some("true"));
    assert_eq!(client.granted_scopes(), Some(&[Scope::Manage][..]));
    assert!(client.take_rotated_refresh_token().is_none());

    // A token inside the refresh margin is refreshed before the call.
    let transport = FakeTransport::new();
    transport.respond(200, REFRESH).respond(200, STREAMS);
    let grant = crate::Grant {
        access: AccessToken::new(
            Secret::new("ya29.old"),
            Instant::now() + Duration::from_secs(30),
            vec![Scope::Readonly],
        ),
        refresh: Secret::new("1//0grefresh"),
    };
    let mut client = LiveClient::from_grant(Arc::new(transport.clone()), oauth(), grant);
    client
        .list_streams(&StreamFilter::Mine, &PageRequest::default())
        .unwrap();
    assert_eq!(transport.requests()[0].endpoint, "token");
    assert_eq!(
        transport.requests()[1].bearer.as_deref(),
        Some("ya29.a0AfH6SMfixture-access-2")
    );
}

#[test]
fn a_401_refreshes_once_and_retries_with_the_same_body() {
    let transport = FakeTransport::new();
    transport
        .respond(401, UNAUTHORIZED)
        .respond(200, REFRESH)
        .respond(200, INSERTED);
    let mut client = signed_in(&transport, vec![Scope::Manage]);

    client
        .insert_broadcast(&NewBroadcast::new(
            "t",
            "2026-10-15T02:00:00Z",
            Privacy::Private,
        ))
        .unwrap();
    let [first, refresh, retry] = <[RecordedRequest; 3]>::try_from(transport.requests()).unwrap();
    assert_eq!(first.bearer.as_deref(), Some("ya29.current"));
    assert_eq!(refresh.endpoint, "token");
    assert_eq!(
        retry.bearer.as_deref(),
        Some("ya29.a0AfH6SMfixture-access-2")
    );
    assert_eq!(retry.body, first.body);
    assert_eq!(retry.url, first.url);
}

#[test]
fn a_second_401_or_a_revoked_grant_means_signing_in_again() {
    let transport = FakeTransport::new();
    transport
        .respond(401, UNAUTHORIZED)
        .respond(200, REFRESH)
        .respond(401, UNAUTHORIZED);
    let mut client = signed_in(&transport, vec![Scope::Readonly]);
    assert!(matches!(
        client.list_streams(&StreamFilter::Mine, &PageRequest::default()),
        Err(YoutubeError::SignInRequired)
    ));
    assert_eq!(transport.pending(), 0);

    let transport = FakeTransport::new();
    transport.respond(400, INVALID_GRANT);
    let mut client = LiveClient::new(Arc::new(transport.clone()), oauth(), Secret::new("1//0g"));
    assert!(matches!(
        client.stream_health("s"),
        Err(YoutubeError::SignInRequired)
    ));
}

#[test]
fn a_rotated_refresh_token_is_handed_over_once() {
    let transport = FakeTransport::new();
    transport
        .respond(200, include_str!("../../fixtures/token_exchange.json"))
        .respond(200, HEALTH);
    let mut client = LiveClient::new(
        Arc::new(transport.clone()),
        oauth(),
        Secret::new("1//0gold"),
    );
    client
        .stream_health("UCfixturechannel000000000fixturestream2")
        .unwrap();
    assert_eq!(
        client
            .take_rotated_refresh_token()
            .map(|s| s.expose().to_owned())
            .as_deref(),
        Some("1//0gfixture-refresh-token")
    );
    assert!(client.take_rotated_refresh_token().is_none());
    let shown = format!("{client:?}");
    assert!(
        !shown.contains("1//0g") && !shown.contains("ya29"),
        "{shown}"
    );
}

#[test]
fn a_malformed_response_is_reported_without_quoting_it() {
    let transport = FakeTransport::new();
    transport.respond(
        200,
        r#"{"items": [{"id": 5, "cdn": {"ingestionInfo": {"streamName": "abcd-leak"}}}]}"#,
    );
    let mut client = signed_in(&transport, vec![Scope::Readonly]);
    let err = client
        .list_streams(&StreamFilter::Mine, &PageRequest::default())
        .unwrap_err();
    let shown = format!("{err} {err:?}");
    assert!(matches!(
        err,
        YoutubeError::InvalidResponse {
            endpoint: "liveStreams.list",
            ..
        }
    ));
    assert!(!shown.contains("abcd-leak"), "{shown}");
}

/// What a recording hook was handed, in order.
type Seen = Arc<std::sync::Mutex<Vec<(SecretKind, String)>>>;

/// A hook that keeps what it is handed, for the tests.
fn recording_hook() -> (SecretHook, Seen) {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let keep = Arc::clone(&seen);
    let hook: SecretHook = Arc::new(move |kind: SecretKind, secret: &str| {
        keep.lock().unwrap().push((kind, secret.to_owned()));
    });
    (hook, seen)
}

#[test]
fn the_secret_hook_sees_every_token_and_stream_key_the_client_gets() {
    let transport = FakeTransport::new();
    transport
        .respond(200, include_str!("../../fixtures/token_exchange.json"))
        .respond(200, STREAMS);
    let (hook, seen) = recording_hook();
    let mut client = LiveClient::new(
        Arc::new(transport.clone()),
        oauth(),
        Secret::new("1//0gold"),
    )
    .with_secret_hook(hook);
    assert_eq!(
        *seen.lock().unwrap(),
        [(SecretKind::RefreshToken, "1//0gold".to_owned())],
        "the refresh token at once"
    );

    client
        .stream("UCfixturechannel000000000fixturestream1")
        .unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        [
            (SecretKind::RefreshToken, "1//0gold".to_owned()),
            (
                SecretKind::RefreshToken,
                "1//0gfixture-refresh-token".to_owned()
            ),
            (
                SecretKind::AccessToken,
                "ya29.a0AfH6SMfixture-access-1".to_owned()
            ),
            (SecretKind::StreamKey, "abcd-efgh-ijkl-mnop-qrst".to_owned()),
        ],
        "the rotated refresh token and the access token as the client refreshes, then the key"
    );
}

#[test]
fn the_secret_hook_sees_a_grants_tokens_and_a_new_streams_key() {
    let transport = FakeTransport::new();
    transport.respond(200, NEW_STREAM);
    let (hook, seen) = recording_hook();
    let mut client = signed_in(&transport, vec![Scope::Manage]).with_secret_hook(hook);
    assert_eq!(
        *seen.lock().unwrap(),
        [
            (SecretKind::RefreshToken, "1//0grefresh".to_owned()),
            (SecretKind::AccessToken, "ya29.current".to_owned()),
        ]
    );

    client.insert_stream(&NewStream::new("Main")).unwrap();
    assert_eq!(
        seen.lock().unwrap().last(),
        Some(&(SecretKind::StreamKey, "wxyz-1234-5678-9abc-defg".to_owned()))
    );
}

/// Only a client's access tokens supersede the ones before them: a sign-in's access
/// token, the refresh token and stream keys are kept for the life of the process (#844).
#[test]
fn only_an_access_token_has_a_slot() {
    assert_eq!(SecretKind::AccessToken.slot(), Some("youtube-access-token"));
    assert_eq!(SecretKind::SignInAccessToken.slot(), None);
    assert_eq!(SecretKind::RefreshToken.slot(), None);
    assert_eq!(SecretKind::StreamKey.slot(), None);
}
