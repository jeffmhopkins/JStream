//! YouTube in the engine (ADR-0058): the sign-in, the broadcast actions and queries,
//! a `youtube` output publishing to its broadcast's ingest, its audience stats and the
//! day's quota estimate (ADR-0041 §3 and §5), all on
//! `jstream-youtube`'s `FakeTransport` and its JSON fixtures (R-TEST-004).

use super::{batch, block_on, code, connected, drain, edits, output_call, query};
use crate::runtime::cpu_draw;
use crate::{Engine, LivePublish, RecordingPublish, TOKEN_ACCOUNT, YoutubeSetup};
use jstream_control::{Control, ControlError, EventStream};
use jstream_media::{ChannelLayout, FrameSize, Timebase};
use jstream_platform::fake::{
    FakeAudioIo, FakeCapture, FakeClock, FakeCodecs, FakeGpuInterop, FakeVideoDevices,
    MemoryTokenStore,
};
use jstream_platform::{Backends, OAuthTokenStore, Secret};
use jstream_protocol::{
    Action, ActionResult, BroadcastLifeCycle, BroadcastListStatus, BroadcastPrivacy,
    BroadcastTransition, BroadcastType, Call, CallResult, Edit, Event, IssueSeverity, OutputId,
    OutputKind, OutputState, Query, QueryResult, QuotaInfo, RtmpTarget, StatsPolling, StreamHealth,
    StreamStatus, YoutubeAuth,
};
use jstream_youtube::fake::FakeTransport;
use jstream_youtube::http::{HttpRequest, HttpResponse};
use jstream_youtube::{HttpTransport, YoutubeError};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};

const REFRESH: &str = include_str!("../../../jstream-youtube/fixtures/token_refresh.json");
const INVALID_GRANT: &str =
    include_str!("../../../jstream-youtube/fixtures/token_invalid_grant.json");
const BROADCASTS: &str = include_str!("../../../jstream-youtube/fixtures/broadcasts_list.json");
const INSERTED: &str = include_str!("../../../jstream-youtube/fixtures/broadcast_insert.json");
const BOUND: &str = include_str!("../../../jstream-youtube/fixtures/broadcast_bind.json");
const LIVE: &str = include_str!("../../../jstream-youtube/fixtures/broadcast_transition_live.json");
const STREAMS: &str = include_str!("../../../jstream-youtube/fixtures/streams_list.json");
const NEW_STREAM: &str = include_str!("../../../jstream-youtube/fixtures/stream_insert.json");
const HEALTH: &str = include_str!("../../../jstream-youtube/fixtures/stream_health.json");
const QUOTA: &str = include_str!("../../../jstream-youtube/fixtures/error_quota.json");
const AUDIENCE: &str = include_str!("../../../jstream-youtube/fixtures/video_stats_live.json");

/// The token endpoint's answer to a sign-in with the full scope (ADR-0041 §4, kept by ADR-0058).
const EXCHANGE: &str = r#"{
  "access_token": "ya29.a0AfH6SMfixture-access-1",
  "expires_in": 3599,
  "refresh_token": "1//0gfixture-signed-in-refresh",
  "scope": "https://www.googleapis.com/auth/youtube",
  "token_type": "Bearer"
}"#;

/// A refresh that rotates the refresh token, as Google may.
const ROTATED: &str = r#"{
  "access_token": "ya29.a0AfH6SMfixture-access-2",
  "expires_in": 3599,
  "refresh_token": "1//0gfixture-rotated-refresh",
  "scope": "https://www.googleapis.com/auth/youtube",
  "token_type": "Bearer"
}"#;

/// The refresh token an earlier sign-in left in the store.
const STORED: &str = "1//0gfixture-stored-refresh";

/// The key of the stream `STREAMS` lists.
const STREAM_KEY: &str = "abcd-efgh-ijkl-mnop-qrst";

fn setup(transport: &FakeTransport, tokens: &MemoryTokenStore) -> YoutubeSetup {
    YoutubeSetup::new(
        "1234-fixture.apps.googleusercontent.com",
        None,
        Arc::new(transport.clone()),
        Arc::new(tokens.clone()),
    )
}

/// A store holding a refresh token from an earlier sign-in.
fn signed_in_store() -> MemoryTokenStore {
    let tokens = MemoryTokenStore::new();
    tokens
        .set(TOKEN_ACCOUNT, &Secret::new(STORED))
        .expect("stores");
    tokens
}

fn action(control: &dyn Control, action: Action) -> Result<Option<ActionResult>, ControlError> {
    match block_on(control.call(Call::Action { action }))?.result {
        CallResult::Action { result } => Ok(result),
        other => panic!("not an action result: {other:?}"),
    }
}

fn status(control: &dyn Control) -> YoutubeAuth {
    match query(control, Query::YoutubeStatus).expect("youtube.status") {
        QueryResult::YoutubeStatus { auth } => auth,
        other => panic!("not a youtube status: {other:?}"),
    }
}

fn statuses(events: &mut EventStream) -> Vec<YoutubeAuth> {
    drain(events)
        .into_iter()
        .filter_map(|event| match event {
            Event::YoutubeStatusChanged { auth } => Some(auth),
            _ => None,
        })
        .collect()
}

fn list_all() -> Query {
    Query::YoutubeListBroadcasts {
        status: BroadcastListStatus::All,
        broadcast_type: BroadcastType::All,
        page_token: None,
    }
}

/// The value of query parameter `name` in `url`, decoded.
fn param(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(n, _)| *n == name)
        .map(|(_, v)| {
            let mut out = Vec::new();
            let bytes = v.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                match bytes[i] {
                    b'%' => {
                        let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).expect("utf-8");
                        out.push(u8::from_str_radix(hex, 16).expect("hex"));
                        i += 3;
                    }
                    b'+' => {
                        out.push(b' ');
                        i += 1;
                    }
                    b => {
                        out.push(b);
                        i += 1;
                    }
                }
            }
            String::from_utf8(out).expect("utf-8")
        })
}

/// What the user's browser does once they agree: a GET to the redirect URI.
fn redirect(redirect_uri: &str, query: &str) {
    let authority = redirect_uri.strip_prefix("http://").expect("http redirect");
    let mut stream = TcpStream::connect(authority).expect("connects");
    write!(
        stream,
        "GET /?{query} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
    )
    .expect("writes");
    let mut answer = String::new();
    let _ = stream.read_to_string(&mut answer);
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
}

/// Waits for a `youtube.status_changed` event, collecting every event seen.
fn wait_for_status(events: &mut EventStream, seen: &mut Vec<Event>) -> YoutubeAuth {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        for event in drain(events) {
            seen.push(event.clone());
            if let Event::YoutubeStatusChanged { auth } = event {
                return auth;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("no youtube.status_changed");
}

#[test]
fn without_a_setup_youtube_is_unavailable() {
    let engine = Engine::new();
    let control = connected(&engine);
    assert_eq!(status(&control), YoutubeAuth::Unavailable);
    let err = action(&control, Action::YoutubeSignIn).expect_err("no client");
    assert_eq!(code(&err), Some("action_failed"));
    let err = query(&control, list_all()).expect_err("no client");
    assert_eq!(code(&err), Some("action_failed"));
    let err = query(&control, Query::YoutubeStats).expect_err("no client");
    assert_eq!(code(&err), Some("action_failed"));
}

/// The built-in client is empty until the maintainer fills it in (ADR-0058 §1), so a
/// build has no setup and YouTube stays unavailable. Filled-in constants make one,
/// with no secret when the secret constant is empty.
#[test]
fn the_built_in_client_is_empty_so_youtube_is_unavailable() {
    let transport = FakeTransport::new();
    let tokens = MemoryTokenStore::new();
    let built_in = YoutubeSetup::built_in(Arc::new(transport.clone()), Arc::new(tokens.clone()));
    assert!(built_in.is_none(), "the built-in client is filled in");

    let filled = |secret: &str| {
        YoutubeSetup::from_constants(
            "1234-fixture.apps.googleusercontent.com",
            secret,
            Arc::new(transport.clone()),
            Arc::new(tokens.clone()),
        )
        .map(|setup| format!("{setup:?}"))
    };
    let with_secret = filled("fixture-secret").expect("a client ID makes a setup");
    assert!(with_secret.contains("1234-fixture.apps.googleusercontent.com"));
    assert!(!with_secret.contains("fixture-secret"), "{with_secret}");
    assert!(
        with_secret.contains("client_secret: Some("),
        "{with_secret}"
    );
    let without = filled("").expect("a client ID makes a setup");
    assert!(without.contains("client_secret: None"), "{without}");
    let none = YoutubeSetup::from_constants(
        "",
        "fixture-secret",
        Arc::new(transport.clone()),
        Arc::new(tokens.clone()),
    );
    assert!(none.is_none());
}

/// A refresh token kept by an earlier sign-in signs the engine in. The first call
/// refreshes it, and the list asks for every state and type (persistent "Stream now"
/// broadcasts included, review N5 on #167).
#[test]
fn a_stored_token_signs_the_engine_in_and_lists_broadcasts() {
    let transport = FakeTransport::new();
    transport.respond(200, REFRESH).respond(200, BROADCASTS);
    let engine = Engine::new().with_youtube(setup(&transport, &signed_in_store()));
    let control = connected(&engine);
    assert_eq!(status(&control), YoutubeAuth::SignedIn);

    let QueryResult::YoutubeListBroadcasts {
        broadcasts,
        next_page_token,
    } = query(&control, list_all()).expect("lists")
    else {
        panic!("not a broadcast list");
    };
    assert_eq!(next_page_token.as_deref(), Some("CAEQAA"));
    let [first, second] = <[_; 2]>::try_from(broadcasts).expect("two broadcasts");
    assert_eq!(first.broadcast_id, "dQw4w9WgXcQ");
    assert_eq!(first.title, "Friday stream");
    assert_eq!(first.description, "Weekly build session");
    assert_eq!(first.life_cycle, BroadcastLifeCycle::Ready);
    assert_eq!(first.privacy, BroadcastPrivacy::Unlisted);
    assert_eq!(
        first.bound_stream_id.as_deref(),
        Some("UCfixturechannel000000000fixturestream1")
    );
    assert_eq!(second.life_cycle, BroadcastLifeCycle::Other);
    assert_eq!(second.bound_stream_id, None);

    let [refresh, list] = <[_; 2]>::try_from(transport.requests()).expect("two requests");
    assert_eq!(refresh.endpoint, "token");
    assert_eq!(refresh.form("refresh_token").as_deref(), Some(STORED));
    assert_eq!(list.endpoint, "liveBroadcasts.list");
    assert_eq!(list.query("broadcastStatus").as_deref(), Some("all"));
    assert_eq!(list.query("broadcastType").as_deref(), Some("all"));
    assert_eq!(list.query("maxResults").as_deref(), Some("50"));
}

/// `youtube.sign_in` returns the consent URL for the full scope alone (ADR-0041 §4, kept by ADR-0058), the
/// browser's redirect finishes it, and the refresh token reaches only the store under
/// the opaque account ID (review N8 on #167), never an event or a response.
#[test]
fn sign_in_returns_the_consent_url_and_the_redirect_finishes_it() {
    let transport = FakeTransport::new();
    transport.respond(200, EXCHANGE);
    let tokens = MemoryTokenStore::new();
    let engine = Engine::new().with_youtube(setup(&transport, &tokens));
    let control = connected(&engine);
    let mut events = control.events();
    assert_eq!(status(&control), YoutubeAuth::SignedOut);

    let Some(ActionResult::YoutubeSignIn { url }) =
        action(&control, Action::YoutubeSignIn).expect("signs in")
    else {
        panic!("no consent URL");
    };
    assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
    assert_eq!(
        param(&url, "scope").as_deref(),
        Some("https://www.googleapis.com/auth/youtube")
    );
    assert_eq!(
        param(&url, "code_challenge_method").as_deref(),
        Some("S256")
    );
    let redirect_uri = param(&url, "redirect_uri").expect("redirect_uri");
    assert!(
        redirect_uri.starts_with("http://127.0.0.1:"),
        "{redirect_uri}"
    );
    let state = param(&url, "state").expect("state");
    assert_eq!(statuses(&mut events), vec![YoutubeAuth::SigningIn]);
    assert_eq!(status(&control), YoutubeAuth::SigningIn);

    // Sent again while it waits, it hands back the same sign-in.
    assert_eq!(
        action(&control, Action::YoutubeSignIn).expect("signs in"),
        Some(ActionResult::YoutubeSignIn { url: url.clone() })
    );

    redirect(
        &redirect_uri,
        &format!("state={state}&code=4%2Ffixture-code"),
    );
    let mut seen = Vec::new();
    assert_eq!(
        wait_for_status(&mut events, &mut seen),
        YoutubeAuth::SignedIn
    );
    assert_eq!(status(&control), YoutubeAuth::SignedIn);

    let [exchange] = <[_; 1]>::try_from(transport.requests()).expect("one request");
    assert_eq!(exchange.form("code").as_deref(), Some("4/fixture-code"));
    assert!(exchange.form("code_verifier").is_some());
    assert_eq!(
        tokens
            .get(TOKEN_ACCOUNT)
            .expect("reads")
            .as_ref()
            .map(Secret::expose),
        Some("1//0gfixture-signed-in-refresh")
    );
    assert_eq!(tokens.len(), 1);
    let wire = serde_json::to_string(&seen).expect("serializes") + &url;
    assert!(!wire.contains("fixture-signed-in-refresh"), "{wire}");
    assert!(!wire.contains("fixture-access-1"), "{wire}");
}

/// A sign-in the user declines goes back to the state before it.
#[test]
fn a_declined_sign_in_goes_back_to_signed_out() {
    let transport = FakeTransport::new();
    let tokens = MemoryTokenStore::new();
    let engine = Engine::new().with_youtube(setup(&transport, &tokens));
    let control = connected(&engine);
    let mut events = control.events();
    let Some(ActionResult::YoutubeSignIn { url }) =
        action(&control, Action::YoutubeSignIn).expect("signs in")
    else {
        panic!("no consent URL");
    };
    assert_eq!(statuses(&mut events), vec![YoutubeAuth::SigningIn]);
    let redirect_uri = param(&url, "redirect_uri").expect("redirect_uri");
    let state = param(&url, "state").expect("state");
    redirect(&redirect_uri, &format!("state={state}&error=access_denied"));
    let mut seen = Vec::new();
    assert_eq!(
        wait_for_status(&mut events, &mut seen),
        YoutubeAuth::SignedOut
    );
    assert!(transport.requests().is_empty());
    assert!(tokens.is_empty());
}

#[test]
fn sign_out_forgets_and_revokes_the_token() {
    let transport = FakeTransport::new();
    transport.respond(200, "");
    let tokens = signed_in_store();
    let engine = Engine::new().with_youtube(setup(&transport, &tokens));
    let control = connected(&engine);
    let mut events = control.events();

    assert_eq!(action(&control, Action::YoutubeSignOut), Ok(None));
    assert_eq!(statuses(&mut events), vec![YoutubeAuth::SignedOut]);
    assert_eq!(status(&control), YoutubeAuth::SignedOut);
    assert!(tokens.is_empty());
    let [revoke] = <[_; 1]>::try_from(transport.requests()).expect("one request");
    assert_eq!(revoke.endpoint, "revoke");
    assert_eq!(revoke.form("token").as_deref(), Some(STORED));

    // Signed out, the Live API calls are refused before anything is sent.
    let err = query(&control, list_all()).expect_err("signed out");
    assert_eq!(code(&err), Some("action_failed"));
    assert_eq!(transport.requests().len(), 1);
    // Signing out again succeeds.
    assert_eq!(action(&control, Action::YoutubeSignOut), Ok(None));
}

/// Holds the first request until the test lets it through, so a call can be caught in
/// flight.
struct Gated {
    inner: FakeTransport,
    entered: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl HttpTransport for Gated {
    fn send(&self, request: HttpRequest<'_>) -> Result<HttpResponse, YoutubeError> {
        let entered = self.entered.lock().expect("lock").take();
        if let Some(entered) = entered {
            let _ = entered.send(());
            let release = self.release.lock().expect("lock").take();
            if let Some(release) = release {
                let _ = release.recv();
            }
        }
        self.inner.send(request)
    }
}

/// A sign-out sent while a call refreshes and rotates the refresh token waits for the
/// call, then clears and revokes the rotated token, so none is left in the store
/// (review B5 on #223).
#[test]
fn a_sign_out_during_a_call_clears_the_token_the_call_rotates() {
    let fake = FakeTransport::new();
    fake.respond(200, ROTATED)
        .respond(200, BROADCASTS)
        .respond(200, "");
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let gated = Gated {
        inner: fake.clone(),
        entered: Mutex::new(Some(entered_tx)),
        release: Mutex::new(Some(release_rx)),
    };
    let tokens = signed_in_store();
    let engine = Engine::new().with_youtube(YoutubeSetup::new(
        "1234-fixture.apps.googleusercontent.com",
        None,
        Arc::new(gated),
        Arc::new(tokens.clone()),
    ));
    let lister = {
        let engine = engine.clone();
        std::thread::spawn(move || query(&connected(&engine), list_all()).is_ok())
    };
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("the refresh is in flight");
    let signer = {
        let engine = engine.clone();
        std::thread::spawn(move || action(&connected(&engine), Action::YoutubeSignOut))
    };
    // Long enough for the sign-out to reach the client's mutex.
    std::thread::sleep(Duration::from_millis(200));
    release.send(()).expect("releases");
    assert!(lister.join().expect("list thread"));
    assert_eq!(signer.join().expect("sign-out thread"), Ok(None));

    assert!(tokens.is_empty(), "a token outlived the sign-out");
    let revoke = fake.requests().pop().expect("revoke");
    assert_eq!(revoke.endpoint, "revoke");
    assert_eq!(
        revoke.form("token").as_deref(),
        Some("1//0gfixture-rotated-refresh")
    );
}

/// Signing in again replaces the account and revokes the replaced account's token
/// (review N8 on #223).
#[test]
fn a_new_sign_in_revokes_the_replaced_accounts_token() {
    let transport = FakeTransport::new();
    transport.respond(200, EXCHANGE).respond(200, "");
    let tokens = signed_in_store();
    let engine = Engine::new().with_youtube(setup(&transport, &tokens));
    let control = connected(&engine);
    let mut events = control.events();
    let Some(ActionResult::YoutubeSignIn { url }) =
        action(&control, Action::YoutubeSignIn).expect("signs in")
    else {
        panic!("no consent URL");
    };
    let redirect_uri = param(&url, "redirect_uri").expect("redirect_uri");
    let state = param(&url, "state").expect("state");
    assert_eq!(statuses(&mut events), vec![YoutubeAuth::SigningIn]);
    redirect(
        &redirect_uri,
        &format!("state={state}&code=4%2Ffixture-code"),
    );
    let mut seen = Vec::new();
    assert_eq!(
        wait_for_status(&mut events, &mut seen),
        YoutubeAuth::SignedIn
    );
    assert_eq!(
        tokens
            .get(TOKEN_ACCOUNT)
            .expect("reads")
            .as_ref()
            .map(Secret::expose),
        Some("1//0gfixture-signed-in-refresh")
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while transport.requests().len() < 2 {
        assert!(
            Instant::now() < deadline,
            "the replaced token was not revoked"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let revoke = transport.requests().pop().expect("revoke");
    assert_eq!(revoke.endpoint, "revoke");
    assert_eq!(revoke.form("token").as_deref(), Some(STORED));
}

#[test]
fn broadcast_actions_return_the_broadcast() {
    let transport = FakeTransport::new();
    transport.respond(200, REFRESH).respond(200, INSERTED);
    let engine = Engine::new().with_youtube(setup(&transport, &signed_in_store()));
    let control = connected(&engine);

    // Refused before anything is sent.
    for (title, start, privacy, field) in [
        (
            " ",
            "2026-10-15T02:00:00Z",
            BroadcastPrivacy::Private,
            "title",
        ),
        (
            "Show",
            "",
            BroadcastPrivacy::Private,
            "scheduled_start_time",
        ),
        (
            "Show",
            "2026-10-15T02:00:00Z",
            BroadcastPrivacy::Other,
            "privacy",
        ),
    ] {
        let err = action(
            &control,
            Action::YoutubeCreateBroadcast {
                title: title.into(),
                description: String::new(),
                scheduled_start_time: start.into(),
                privacy,
                made_for_kids: false,
            },
        )
        .expect_err("invalid");
        assert!(
            matches!(err.protocol(), Some(jstream_protocol::ProtocolError::InvalidArgument { field: f, .. }) if f == field),
            "{err:?}"
        );
    }
    assert!(transport.requests().is_empty());

    let Some(ActionResult::YoutubeCreateBroadcast { broadcast }) = action(
        &control,
        Action::YoutubeCreateBroadcast {
            title: "JStream go-live test".into(),
            description: "Testing".into(),
            scheduled_start_time: "2026-10-15T02:00:00Z".into(),
            privacy: BroadcastPrivacy::Private,
            made_for_kids: false,
        },
    )
    .expect("creates") else {
        panic!("no broadcast");
    };
    assert_eq!(broadcast.broadcast_id, "jNQXAC9IVRw");
    assert_eq!(broadcast.life_cycle, BroadcastLifeCycle::Created);
    let insert = transport.requests().pop().expect("insert");
    assert_eq!(insert.endpoint, "liveBroadcasts.insert");
    let body: serde_json::Value = serde_json::from_str(&insert.body).expect("json");
    assert_eq!(body["snippet"]["description"], "Testing");
    assert_eq!(body["contentDetails"]["enableAutoStart"], false);

    // With no stream named, the engine creates one after the broadcast's title.
    transport
        .respond(200, BROADCASTS)
        .respond(200, NEW_STREAM)
        .respond(200, BOUND);
    let Some(ActionResult::YoutubeBindBroadcast { broadcast }) = action(
        &control,
        Action::YoutubeBindBroadcast {
            broadcast_id: "dQw4w9WgXcQ".into(),
            stream_id: None,
        },
    )
    .expect("binds") else {
        panic!("no broadcast");
    };
    assert_eq!(
        broadcast.bound_stream_id.as_deref(),
        Some("UCfixturechannel000000000fixturestream2")
    );
    let requests = transport.requests();
    let [lookup, create, bind] = &requests[requests.len() - 3..] else {
        panic!("three requests");
    };
    assert_eq!(lookup.query("id").as_deref(), Some("dQw4w9WgXcQ"));
    assert_eq!(create.endpoint, "liveStreams.insert");
    let body: serde_json::Value = serde_json::from_str(&create.body).expect("json");
    assert_eq!(body["snippet"]["title"], "Friday stream");
    assert_eq!(bind.endpoint, "liveBroadcasts.bind");
    assert_eq!(
        bind.query("streamId").as_deref(),
        Some("UCfixturechannel000000000fixturestream2")
    );

    transport.respond(200, LIVE);
    let Some(ActionResult::YoutubeTransitionBroadcast { broadcast }) = action(
        &control,
        Action::YoutubeTransitionBroadcast {
            broadcast_id: "jNQXAC9IVRw".into(),
            to: BroadcastTransition::Live,
        },
    )
    .expect("transitions") else {
        panic!("no broadcast");
    };
    assert_eq!(broadcast.life_cycle, BroadcastLifeCycle::LiveStarting);
    let transition = transport.requests().pop().expect("transition");
    assert_eq!(transition.query("broadcastStatus").as_deref(), Some("live"));
}

/// A used-up quota is a protocol error that says so (ADR-0041 §5, kept by ADR-0058). A refresh token
/// Google no longer accepts signs the engine out and leaves the store.
#[test]
fn quota_and_revoked_tokens_are_reported() {
    let transport = FakeTransport::new();
    transport.respond(200, REFRESH).respond(403, QUOTA);
    let engine = Engine::new().with_youtube(setup(&transport, &signed_in_store()));
    let control = connected(&engine);
    let err = query(&control, list_all()).expect_err("quota");
    assert_eq!(code(&err), Some("action_failed"));
    assert!(err.to_string().contains("quota"), "{err}");
    assert_eq!(status(&control), YoutubeAuth::SignedIn);
    // The refusal marks the day used up, whatever the estimate said.
    let quota = quota(&control);
    assert!(quota.exhausted);
    assert_eq!(quota.used, 1);
    assert_eq!(quota.stats, StatsPolling::Stopped);

    let transport = FakeTransport::new();
    transport.respond(400, INVALID_GRANT);
    let tokens = signed_in_store();
    let engine = Engine::new().with_youtube(setup(&transport, &tokens));
    let control = connected(&engine);
    let mut events = control.events();
    let err = query(&control, Query::YoutubeListStreams { page_token: None }).expect_err("revoked");
    assert_eq!(code(&err), Some("action_failed"));
    assert!(!err.to_string().contains(STORED), "{err}");
    assert_eq!(statuses(&mut events), vec![YoutubeAuth::SignedOut]);
    assert_eq!(status(&control), YoutubeAuth::SignedOut);
    assert!(tokens.is_empty());
}

#[test]
fn streams_are_listed_without_their_keys() {
    let transport = FakeTransport::new();
    transport.respond(200, REFRESH).respond(200, STREAMS);
    let engine = Engine::new().with_youtube(setup(&transport, &signed_in_store()));
    let control = connected(&engine);
    let result = query(
        &control,
        Query::YoutubeListStreams {
            page_token: Some("CAEQAA".into()),
        },
    )
    .expect("lists");
    let QueryResult::YoutubeListStreams { streams, .. } = &result else {
        panic!("not a stream list");
    };
    assert_eq!(streams.len(), 1);
    assert_eq!(
        streams[0].stream_id,
        "UCfixturechannel000000000fixturestream1"
    );
    assert_eq!(streams[0].status, StreamStatus::Inactive);
    assert_eq!(streams[0].health, StreamHealth::NoData);
    let wire = serde_json::to_string(&result).expect("serializes");
    assert!(!wire.contains(STREAM_KEY), "{wire}");
    let list = transport.requests().pop().expect("list");
    assert_eq!(list.query("mine").as_deref(), Some("true"));
    assert_eq!(list.query("pageToken").as_deref(), Some("CAEQAA"));
}

fn backends() -> Backends {
    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let size = FrameSize::new(64, 36).expect("size");
    Backends {
        clock: Arc::new(clock.clone()),
        capture: Arc::new(FakeCapture::new(clock.clone(), size)),
        video_devices: Arc::new(FakeVideoDevices::new(clock)),
        audio: Arc::new(audio),
        codecs: Arc::new(FakeCodecs),
        gpu: Arc::new(FakeGpuInterop),
        vcam: None,
        media: Arc::new(jstream_platform::fake::FakeMediaDecoders::new()),
    }
}

fn youtube_output(broadcast_id: &str) -> Edit {
    Edit::OutputCreate {
        output_id: OutputId::new("live"),
        kind: OutputKind::Rtmp {
            target: RtmpTarget::YouTube {
                broadcast_id: broadcast_id.into(),
            },
        },
    }
}

/// Starting a `youtube` output reads its broadcast's bound stream and publishes to that
/// stream's `rtmps://` ingest with its key, which reaches only the connection. While it
/// runs, the stream's health comes in `youtube.health_changed`.
#[test]
fn a_youtube_output_publishes_to_its_broadcasts_ingest() {
    let transport = FakeTransport::new();
    transport
        .respond(200, REFRESH)
        .respond(200, BROADCASTS)
        .respond(200, STREAMS)
        .respond(200, HEALTH)
        .respond(200, HEALTH);
    let engine = Engine::new()
        .with_backends(backends())
        .with_draw(cpu_draw())
        .with_youtube(
            setup(&transport, &signed_in_store()).with_health_interval(Duration::from_millis(20)),
        );
    let connected_to: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let publish = RecordingPublish::new();
    {
        let connected_to = Arc::clone(&connected_to);
        let publish = publish.clone();
        engine
            .youtube()
            .expect("youtube")
            .set_connect(Arc::new(move |url: &str, key: &Secret| {
                connected_to
                    .lock()
                    .expect("lock")
                    .push((url.to_owned(), key.expose().to_owned()));
                Ok(Box::new(publish.clone()) as Box<dyn LivePublish>)
            }));
    }
    let control = connected(&engine);
    batch(&control, edits(vec![youtube_output("dQw4w9WgXcQ")])).expect("creates");
    let mut events = control.events();

    block_on(control.call(output_call("live", true))).expect("output.start");
    assert_eq!(
        *connected_to.lock().expect("lock"),
        vec![(
            "rtmps://a.rtmps.youtube.com/live2".to_owned(),
            STREAM_KEY.to_owned()
        )]
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    let health = loop {
        assert!(Instant::now() < deadline, "no health event: {seen:?}");
        let found = drain(&mut events).into_iter().find_map(|event| {
            seen.push(event.clone());
            match event {
                Event::YoutubeHealthChanged {
                    output_id,
                    broadcast_id,
                    stream_id,
                    status,
                    health,
                    issues,
                } => Some((output_id, broadcast_id, stream_id, status, health, issues)),
                _ => None,
            }
        });
        if let Some(found) = found {
            break found;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let (output_id, broadcast_id, stream_id, status, health, issues) = health;
    assert_eq!(output_id, OutputId::new("live"));
    assert_eq!(broadcast_id, "dQw4w9WgXcQ");
    assert_eq!(stream_id, "UCfixturechannel000000000fixturestream1");
    assert_eq!(status, StreamStatus::Active);
    assert_eq!(health, StreamHealth::Bad);
    let severities: Vec<_> = issues.iter().map(|issue| issue.severity).collect();
    assert_eq!(
        severities,
        vec![
            IssueSeverity::Error,
            IssueSeverity::Warning,
            IssueSeverity::Other
        ]
    );
    assert_eq!(issues[0].issue, "gopSizeLong");

    block_on(control.call(output_call("live", false))).expect("output.stop");
    seen.extend(drain(&mut events));
    assert!(seen.contains(&Event::OutputStateChanged {
        output_id: OutputId::new("live"),
        state: OutputState::Active,
    }));
    let wire = serde_json::to_string(&seen).expect("serializes");
    assert!(!wire.contains(STREAM_KEY), "{wire}");
    assert!(!wire.contains(STORED), "{wire}");

    let requests = transport.requests();
    assert_eq!(requests[1].query("id").as_deref(), Some("dQw4w9WgXcQ"));
    assert_eq!(
        requests[2].query("id").as_deref(),
        Some("UCfixturechannel000000000fixturestream1")
    );
}

/// Answers by endpoint, so polls from several outputs can interleave in any order, and
/// records which threads read health.
#[derive(Default)]
struct Routed {
    health_readers: Mutex<BTreeSet<String>>,
}

impl HttpTransport for Routed {
    fn send(&self, request: HttpRequest<'_>) -> Result<HttpResponse, YoutubeError> {
        let health =
            request.url.contains("part=id%2Cstatus") || request.url.contains("part=id,status");
        let body = match request.endpoint {
            "token" => REFRESH,
            "liveBroadcasts.list" => BROADCASTS,
            "liveStreams.list" if health => {
                let reader = format!("{:?}", std::thread::current().id());
                self.health_readers.lock().expect("lock").insert(reader);
                HEALTH
            }
            "liveStreams.list" => STREAMS,
            other => panic!("unexpected request to {other}"),
        };
        Ok(HttpResponse::new(200, body.into()))
    }
}

fn named_youtube_output(output_id: &str) -> Edit {
    Edit::OutputCreate {
        output_id: OutputId::new(output_id),
        kind: OutputKind::Rtmp {
            target: RtmpTarget::YouTube {
                broadcast_id: "dQw4w9WgXcQ".into(),
            },
        },
    }
}

/// Each running `youtube` output keeps its own health poll: starting a second one
/// doesn't end the first one's (review B6 on #223).
#[test]
fn every_running_youtube_output_keeps_its_health_poll() {
    let routed = Arc::new(Routed::default());
    let engine = Engine::new()
        .with_backends(backends())
        .with_draw(cpu_draw())
        .with_youtube(
            YoutubeSetup::new(
                "1234-fixture.apps.googleusercontent.com",
                None,
                Arc::clone(&routed) as Arc<dyn HttpTransport>,
                Arc::new(signed_in_store()),
            )
            .with_health_interval(Duration::from_millis(20)),
        );
    let publish = RecordingPublish::new();
    engine
        .youtube()
        .expect("youtube")
        .set_connect(Arc::new(move |_: &str, _: &Secret| {
            Ok(Box::new(publish.clone()) as Box<dyn LivePublish>)
        }));
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            named_youtube_output("first"),
            named_youtube_output("second"),
        ]),
    )
    .expect("creates");
    block_on(control.call(output_call("first", true))).expect("starts first");
    block_on(control.call(output_call("second", true))).expect("starts second");

    // From here on, two different poll threads read health.
    routed.health_readers.lock().expect("lock").clear();
    let deadline = Instant::now() + Duration::from_secs(10);
    while routed.health_readers.lock().expect("lock").len() < 2 {
        assert!(
            Instant::now() < deadline,
            "only one output's health is polled"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // Stopping one leaves the other's poll running.
    block_on(control.call(output_call("first", false))).expect("stops first");
    std::thread::sleep(Duration::from_millis(100));
    routed.health_readers.lock().expect("lock").clear();
    let deadline = Instant::now() + Duration::from_secs(10);
    while routed.health_readers.lock().expect("lock").is_empty() {
        assert!(Instant::now() < deadline, "the second output's poll ended");
        std::thread::sleep(Duration::from_millis(10));
    }
    block_on(control.call(output_call("second", false))).expect("stops second");
}

/// `output.start` on a `youtube` output waits for a YouTube call in flight without
/// holding the collection lock, so the engine's other calls still answer meanwhile.
#[test]
fn a_youtube_start_waits_for_youtube_without_the_collection_lock() {
    let transport = FakeTransport::new();
    let engine = Engine::new()
        .with_backends(backends())
        .with_draw(cpu_draw())
        .with_youtube(setup(&transport, &signed_in_store()));
    let control = connected(&engine);
    batch(&control, edits(vec![youtube_output("jNQXAC9IVRw")])).expect("creates");
    let youtube = engine.youtube().expect("youtube");
    let in_flight = youtube.hold_client();
    let starter = {
        let engine = engine.clone();
        std::thread::spawn(move || {
            let control = connected(&engine);
            block_on(control.call(output_call("live", true))).is_ok()
        })
    };
    // Long enough for the start to reach the client's mutex.
    std::thread::sleep(Duration::from_millis(200));
    let (tx, rx) = mpsc::channel();
    {
        let engine = engine.clone();
        std::thread::spawn(move || {
            let control = connected(&engine);
            let _ = tx.send(query(&control, Query::OutputList).is_ok());
        });
    }
    let answered = rx.recv_timeout(Duration::from_secs(5));
    drop(in_flight);
    assert_eq!(
        answered,
        Ok(true),
        "output.list waited for the YouTube client"
    );
    // Nothing is scripted for the ingest lookup, so the start fails once let through.
    let _ = starter.join().expect("start thread");
}

/// A `youtube` output can't start signed out (refused before `starting`), and fails
/// when its broadcast has no bound stream.
#[test]
fn a_youtube_output_needs_a_sign_in_and_a_bound_stream() {
    let transport = FakeTransport::new();
    let tokens = MemoryTokenStore::new();
    let engine = Engine::new()
        .with_backends(backends())
        .with_draw(cpu_draw())
        .with_youtube(setup(&transport, &tokens));
    let control = connected(&engine);
    batch(&control, edits(vec![youtube_output("jNQXAC9IVRw")])).expect("creates");
    let mut events = control.events();
    let err = block_on(control.call(output_call("live", true))).expect_err("signed out");
    assert_eq!(code(&err), Some("action_failed"));
    assert!(drain(&mut events).is_empty(), "nothing started");

    let transport = FakeTransport::new();
    let unbound = format!(r#"{{"items": [{INSERTED}]}}"#);
    transport.respond(200, REFRESH).respond(200, unbound);
    let engine = Engine::new()
        .with_backends(backends())
        .with_draw(cpu_draw())
        .with_youtube(setup(&transport, &signed_in_store()));
    let control = connected(&engine);
    batch(&control, edits(vec![youtube_output("jNQXAC9IVRw")])).expect("creates");
    let mut events = control.events();
    let err = block_on(control.call(output_call("live", true))).expect_err("unbound");
    assert_eq!(code(&err), Some("action_failed"));
    assert!(err.to_string().contains("youtube.bind_broadcast"), "{err}");
    let steps: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            Event::OutputStateChanged { state, .. } => Some(state),
            _ => None,
        })
        .collect();
    assert_eq!(steps, vec![OutputState::Starting, OutputState::Failed]);
}

/// `youtube.stats`: the running outputs' audience and the day's quota estimate.
fn audience(control: &dyn Control) -> (Vec<jstream_protocol::AudienceStatsInfo>, QuotaInfo) {
    match query(control, Query::YoutubeStats).expect("youtube.stats") {
        QueryResult::YoutubeStats { outputs, quota } => (outputs, quota),
        other => panic!("not youtube stats: {other:?}"),
    }
}

fn quota(control: &dyn Control) -> QuotaInfo {
    audience(control).1
}

/// A wall clock a test sets.
fn settable_clock(at: SystemTime) -> (Arc<Mutex<SystemTime>>, crate::youtube::WallClock) {
    let now = Arc::new(Mutex::new(at));
    let read = Arc::clone(&now);
    (now, Arc::new(move || *read.lock().expect("lock")))
}

/// 2026-10-17T20:00:00Z, noon Pacific standard time.
fn noon_pacific() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_792_267_200)
}

/// A running `youtube` output's audience comes in `youtube.stats` once per reading
/// that differs from the last, and `youtube.stats` lists it until the output stops.
/// Each read costs a unit of the day's estimate (ADR-0041 §3 and §5).
#[test]
fn a_youtube_output_reports_its_audience() {
    let transport = FakeTransport::new();
    transport
        .respond(200, REFRESH)
        .respond(200, BROADCASTS)
        .respond(200, STREAMS)
        .respond(200, AUDIENCE)
        .respond(200, AUDIENCE);
    let engine = Engine::new()
        .with_backends(backends())
        .with_draw(cpu_draw())
        .with_youtube(
            setup(&transport, &signed_in_store())
                .with_health_interval(Duration::from_secs(3600))
                .with_stats_interval(Duration::from_millis(20)),
        );
    let publish = RecordingPublish::new();
    engine
        .youtube()
        .expect("youtube")
        .set_connect(Arc::new(move |_: &str, _: &Secret| {
            Ok(Box::new(publish.clone()) as Box<dyn LivePublish>)
        }));
    let control = connected(&engine);
    batch(&control, edits(vec![youtube_output("dQw4w9WgXcQ")])).expect("creates");
    let mut events = control.events();
    block_on(control.call(output_call("live", true))).expect("output.start");

    // Both scripted readings are taken, then reads fail on the empty script.
    let deadline = Instant::now() + Duration::from_secs(10);
    while transport.pending() > 0 {
        assert!(Instant::now() < deadline, "the audience is not read");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(60));
    let reported: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            Event::YoutubeStats { stats } => Some(stats),
            _ => None,
        })
        .collect();
    assert_eq!(reported.len(), 1, "the same reading twice is one event");
    let stats = &reported[0];
    assert_eq!(stats.output_id, OutputId::new("live"));
    assert_eq!(stats.broadcast_id, "dQw4w9WgXcQ");
    assert_eq!(stats.concurrent_viewers, Some(1287));
    assert_eq!(stats.likes, Some(412));
    assert_eq!(stats.views, Some(5321));

    // The ingest's two reads, then every audience read, the failed ones included,
    // each charged before it is sent.
    let reads = transport
        .requests()
        .iter()
        .filter(|request| request.endpoint == "videos.list")
        .count() as u64;
    let (outputs, quota) = audience(&control);
    assert_eq!(outputs, reported);
    assert!(reads >= 2, "{reads} reads");
    assert!(quota.used >= 2 + reads, "{quota:?}");
    assert_eq!(quota.daily, crate::DAILY_QUOTA);
    assert!(!quota.exhausted);
    assert_eq!(quota.stats, StatsPolling::Normal);
    let videos = transport
        .requests()
        .into_iter()
        .find(|request| request.endpoint == "videos.list")
        .expect("an audience read");
    assert_eq!(videos.query("id").as_deref(), Some("dQw4w9WgXcQ"));

    block_on(control.call(output_call("live", false))).expect("output.stop");
    assert!(
        audience(&control).0.is_empty(),
        "a stopped output has no audience"
    );
}

/// Stats slow when less than a fifth of the day's quota is left and stop below a
/// tenth; a new day starts the estimate again (ADR-0041 §5).
#[test]
fn stats_polling_slows_then_stops_as_the_quota_runs_low() {
    let transport = FakeTransport::new();
    transport.respond(200, REFRESH);
    for _ in 0..10 {
        transport.respond(200, STREAMS);
    }
    let (now, clock) = settable_clock(noon_pacific());
    let engine = Engine::new().with_youtube(
        setup(&transport, &signed_in_store())
            .with_daily_quota(10)
            .with_clock(clock),
    );
    let control = connected(&engine);
    let list = || Query::YoutubeListStreams { page_token: None };
    assert_eq!(
        quota(&control),
        QuotaInfo {
            used: 0,
            daily: 10,
            exhausted: false,
            stats: StatsPolling::Normal,
        }
    );
    for _ in 0..8 {
        query(&control, list()).expect("lists");
    }
    // 2 of 10 left: a fifth, not less.
    assert_eq!(quota(&control).stats, StatsPolling::Normal);
    query(&control, list()).expect("lists");
    assert_eq!(quota(&control).used, 9);
    assert_eq!(quota(&control).stats, StatsPolling::Slowed);
    query(&control, list()).expect("lists");
    assert_eq!(quota(&control).used, 10);
    assert_eq!(quota(&control).stats, StatsPolling::Stopped);

    // 07:59 UTC the next day is still the same day at UTC−8.
    *now.lock().expect("lock") = noon_pacific() + Duration::from_secs(11 * 3600 + 59 * 60);
    assert_eq!(quota(&control).used, 10);
    *now.lock().expect("lock") = noon_pacific() + Duration::from_secs(12 * 3600);
    assert_eq!(
        quota(&control),
        QuotaInfo {
            used: 0,
            daily: 10,
            exhausted: false,
            stats: StatsPolling::Normal,
        }
    );
}

/// A day of no quota stops audience polling before anything is spent (review N2).
#[test]
fn no_daily_quota_stops_stats_polling() {
    let transport = FakeTransport::new();
    let (_now, clock) = settable_clock(noon_pacific());
    let engine = Engine::new().with_youtube(
        setup(&transport, &signed_in_store())
            .with_daily_quota(0)
            .with_clock(clock),
    );
    let control = connected(&engine);
    assert_eq!(
        quota(&control),
        QuotaInfo {
            used: 0,
            daily: 0,
            exhausted: false,
            stats: StatsPolling::Stopped,
        }
    );
}

/// With the quota nearly used up, a running output's audience is not read at all, and
/// the actions are still charged at a write's cost.
#[test]
fn a_low_quota_stops_audience_reads() {
    let transport = FakeTransport::new();
    transport
        .respond(200, REFRESH)
        .respond(200, BROADCASTS)
        .respond(200, STREAMS);
    let engine = Engine::new()
        .with_backends(backends())
        .with_draw(cpu_draw())
        .with_youtube(
            setup(&transport, &signed_in_store())
                .with_daily_quota(2)
                .with_health_interval(Duration::from_secs(3600))
                .with_stats_interval(Duration::from_millis(10)),
        );
    let publish = RecordingPublish::new();
    engine
        .youtube()
        .expect("youtube")
        .set_connect(Arc::new(move |_: &str, _: &Secret| {
            Ok(Box::new(publish.clone()) as Box<dyn LivePublish>)
        }));
    let control = connected(&engine);
    batch(&control, edits(vec![youtube_output("dQw4w9WgXcQ")])).expect("creates");
    block_on(control.call(output_call("live", true))).expect("output.start");
    // The ingest's two reads use the whole day.
    assert_eq!(quota(&control).used, 2);
    assert_eq!(quota(&control).stats, StatsPolling::Stopped);
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !transport
            .requests()
            .iter()
            .any(|request| request.endpoint == "videos.list"),
        "the audience was read with no quota left"
    );
    assert!(audience(&control).0.is_empty());
    assert_eq!(quota(&control).used, 2);

    // Actions keep going to YouTube, charged as writes.
    transport.respond(200, INSERTED);
    action(
        &control,
        Action::YoutubeCreateBroadcast {
            title: "Late show".into(),
            description: String::new(),
            scheduled_start_time: "2026-10-18T03:00:00Z".into(),
            privacy: BroadcastPrivacy::Private,
            made_for_kids: false,
        },
    )
    .expect("creates");
    assert_eq!(quota(&control).used, 52);
    block_on(control.call(output_call("live", false))).expect("output.stop");
}

/// The setup's secret hook gets the stored refresh token, the access token and the key
/// a `youtube` output publishes with, so a host's crash reports can redact them (#353).
#[test]
fn the_secret_hook_sees_the_tokens_and_a_youtube_outputs_key() {
    let transport = FakeTransport::new();
    transport
        .respond(200, REFRESH)
        .respond(200, BROADCASTS)
        .respond(200, STREAMS)
        .respond(200, HEALTH);
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let hook = {
        let seen = Arc::clone(&seen);
        Arc::new(move |secret: &str| seen.lock().expect("lock").push(secret.to_owned()))
    };
    let engine = Engine::new()
        .with_backends(backends())
        .with_draw(cpu_draw())
        .with_youtube(setup(&transport, &signed_in_store()).with_secret_hook(hook));
    let publish = RecordingPublish::new();
    engine
        .youtube()
        .expect("youtube")
        .set_connect(Arc::new(move |_: &str, _: &Secret| {
            Ok(Box::new(publish.clone()) as Box<dyn LivePublish>)
        }));
    assert_eq!(
        *seen.lock().expect("lock"),
        [STORED],
        "the stored token at once"
    );

    let control = connected(&engine);
    batch(&control, edits(vec![youtube_output("dQw4w9WgXcQ")])).expect("creates");
    block_on(control.call(output_call("live", true))).expect("output.start");
    let seen = seen.lock().expect("lock").clone();
    assert!(
        seen.iter().any(|s| s == STREAM_KEY),
        "{} secrets",
        seen.len()
    );
    assert!(
        seen.iter().any(|s| s.starts_with("ya29.")),
        "an access token"
    );
    drop(block_on(control.call(output_call("live", false))));
}
