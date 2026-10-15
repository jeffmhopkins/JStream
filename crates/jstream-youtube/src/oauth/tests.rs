use std::time::{Duration, Instant};

use jstream_platform::Secret;

use super::*;
use crate::fake::FakeTransport;
use crate::http::Method;

const EXCHANGE: &str = include_str!("../../fixtures/token_exchange.json");
const REFRESH: &str = include_str!("../../fixtures/token_refresh.json");
const INVALID_GRANT: &str = include_str!("../../fixtures/token_invalid_grant.json");

const REDIRECT: &str = "http://127.0.0.1:49152";

fn client() -> OAuthClient {
    OAuthClient::new(
        "1234-fixture.apps.googleusercontent.com",
        Some(Secret::new("GOCSPX-fixture-secret")),
    )
}

fn query(url: &str, name: &str) -> Option<String> {
    url.split_once('?')?
        .1
        .split('&')
        .filter_map(|p| p.split_once('='))
        .find(|(n, _)| *n == name)
        .and_then(|(_, v)| crate::http::decode_component(v))
}

#[test]
fn the_s256_challenge_matches_rfc_7636_appendix_b() {
    let verifier = Secret::new("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
    assert_eq!(
        pkce_challenge(&verifier),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn authorize_builds_a_pkce_consent_url_with_fresh_state() {
    let pending = client()
        .authorize(REDIRECT, &[Scope::Readonly, Scope::Manage, Scope::Readonly])
        .unwrap();
    let url = pending.url();
    assert!(
        url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"),
        "{url}"
    );
    assert_eq!(
        query(url, "client_id").as_deref(),
        Some("1234-fixture.apps.googleusercontent.com")
    );
    assert_eq!(query(url, "redirect_uri").as_deref(), Some(REDIRECT));
    assert_eq!(query(url, "response_type").as_deref(), Some("code"));
    assert_eq!(query(url, "code_challenge_method").as_deref(), Some("S256"));
    assert_eq!(
        query(url, "scope").as_deref(),
        Some(
            "https://www.googleapis.com/auth/youtube.readonly \
             https://www.googleapis.com/auth/youtube"
        )
    );
    assert_eq!(pending.scopes(), [Scope::Readonly, Scope::Manage]);
    assert_eq!(query(url, "state").as_deref(), Some(pending.state()));

    // 32 random bytes make a 43-character verifier, the shortest RFC 7636 allows; the
    // URL carries its challenge, never the verifier itself.
    assert_eq!(pending.verifier.expose().len(), 43);
    assert_eq!(
        query(url, "code_challenge"),
        Some(pkce_challenge(&pending.verifier))
    );
    assert!(!url.contains(pending.verifier.expose()));
    assert_eq!(pending.state().len(), 22);

    let again = client().authorize(REDIRECT, &[Scope::Readonly]).unwrap();
    assert_ne!(again.state(), pending.state());
    assert_ne!(again.verifier.expose(), pending.verifier.expose());

    let shown = format!("{pending:?}");
    assert!(!shown.contains(pending.verifier.expose()), "{shown}");
}

#[test]
fn authorize_needs_a_scope() {
    assert!(matches!(
        client().authorize(REDIRECT, &[]),
        Err(YoutubeError::InvalidArgument { .. })
    ));
}

#[test]
fn exchange_trades_the_code_and_verifier_for_a_grant() {
    let transport = FakeTransport::new();
    transport.respond(200, EXCHANGE);
    let pending = client().authorize(REDIRECT, &[Scope::Readonly]).unwrap();
    let verifier = pending.verifier.expose().to_owned();

    let before = Instant::now();
    let grant = client()
        .exchange(&transport, pending, Secret::new("4/0Afixture-code"))
        .unwrap();

    let [request] = transport.requests().try_into().unwrap();
    assert_eq!(request.method, Method::Post);
    assert_eq!(request.url, TOKEN_ENDPOINT);
    assert_eq!(request.bearer, None);
    assert_eq!(
        request.form("grant_type").as_deref(),
        Some("authorization_code")
    );
    assert_eq!(request.form("code").as_deref(), Some("4/0Afixture-code"));
    assert_eq!(request.form("code_verifier"), Some(verifier));
    assert_eq!(request.form("redirect_uri").as_deref(), Some(REDIRECT));
    assert_eq!(
        request.form("client_secret").as_deref(),
        Some("GOCSPX-fixture-secret")
    );

    assert_eq!(
        grant.access.secret().expose(),
        "ya29.a0AfH6SMfixture-access-1"
    );
    assert_eq!(grant.refresh.expose(), "1//0gfixture-refresh-token");
    assert_eq!(grant.access.scopes(), [Scope::Readonly]);
    assert!(grant.access.allows(Scope::Readonly));
    assert!(!grant.access.allows(Scope::Manage));
    let lifetime = grant.access.expires_at() - before;
    // Timed from the request, which is after `before`: no longer than Google said.
    assert!(lifetime >= Duration::from_secs(3599), "{lifetime:?}");
    assert!(lifetime < Duration::from_secs(3609), "{lifetime:?}");
    assert!(!grant.access.is_stale(Instant::now()));
    assert!(
        grant
            .access
            .is_stale(Instant::now() + Duration::from_secs(3550))
    );

    let shown = format!("{grant:?}");
    assert!(
        !shown.contains("ya29") && !shown.contains("1//0g"),
        "{shown}"
    );
}

#[test]
fn an_exchange_without_a_refresh_token_is_refused() {
    let transport = FakeTransport::new();
    transport.respond(200, REFRESH);
    let pending = client().authorize(REDIRECT, &[Scope::Readonly]).unwrap();
    let err = client()
        .exchange(&transport, pending, Secret::new("code"))
        .unwrap_err();
    assert!(
        matches!(
            err,
            YoutubeError::InvalidResponse {
                endpoint: "token",
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn a_client_without_a_secret_sends_none() {
    let transport = FakeTransport::new();
    transport.respond(200, REFRESH);
    OAuthClient::new("id", None)
        .refresh(&transport, &Secret::new("1//0g"))
        .unwrap();
    assert_eq!(transport.requests()[0].form("client_secret"), None);
}

#[test]
fn refresh_returns_a_new_token_and_any_rotated_refresh_token() {
    let transport = FakeTransport::new();
    transport.respond(200, REFRESH).respond(200, EXCHANGE);

    let refreshed = client()
        .refresh(&transport, &Secret::new("1//0gfixture-refresh-token"))
        .unwrap();
    assert_eq!(
        refreshed.access.secret().expose(),
        "ya29.a0AfH6SMfixture-access-2"
    );
    assert_eq!(refreshed.access.scopes(), [Scope::Manage]);
    assert!(refreshed.access.allows(Scope::Readonly));
    assert!(refreshed.rotated_refresh_token.is_none());
    let request = &transport.requests()[0];
    assert_eq!(request.form("grant_type").as_deref(), Some("refresh_token"));
    assert_eq!(
        request.form("refresh_token").as_deref(),
        Some("1//0gfixture-refresh-token")
    );

    let rotated = client()
        .refresh(&transport, &Secret::new("1//0gold"))
        .unwrap()
        .rotated_refresh_token
        .unwrap();
    assert_eq!(rotated.expose(), "1//0gfixture-refresh-token");
}

#[test]
fn a_revoked_refresh_token_means_signing_in_again() {
    let transport = FakeTransport::new();
    transport
        .respond(400, INVALID_GRANT)
        .respond(400, INVALID_GRANT);
    let err = client()
        .refresh(&transport, &Secret::new("1//0grevoked"))
        .unwrap_err();
    assert!(matches!(err, YoutubeError::SignInRequired), "{err}");

    // In a code exchange, the same error means a bad code, which is an OAuth error.
    let pending = client().authorize(REDIRECT, &[Scope::Readonly]).unwrap();
    let err = client()
        .exchange(&transport, pending, Secret::new("used"))
        .unwrap_err();
    match err {
        YoutubeError::OAuth { error, description } => {
            assert_eq!(error, "invalid_grant");
            assert_eq!(
                description.as_deref(),
                Some("Token has been expired or revoked.")
            );
        }
        other => panic!("{other}"),
    }
}

#[test]
fn token_endpoint_failures_never_quote_the_body() {
    let transport = FakeTransport::new();
    transport
        .respond(
            200,
            r#"{"access_token": "ya29.leak", "expires_in": "soon", "token_type": "Bearer"}"#,
        )
        .respond(
            200,
            r#"{"access_token": "ya29.leak", "expires_in": 60, "token_type": "MAC"}"#,
        )
        .respond(502, "<html>ya29.leak</html>");
    for _ in 0..3 {
        let err = client()
            .refresh(&transport, &Secret::new("1//0g"))
            .unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("ya29"), "{shown}");
    }
}

#[test]
fn revoke_sends_the_token_in_the_body_not_the_url() {
    let transport = FakeTransport::new();
    transport
        .respond(200, "")
        .respond(400, r#"{"error":"invalid_token"}"#);
    client()
        .revoke(&transport, &Secret::new("1//0gbye"))
        .unwrap();
    let request = &transport.requests()[0];
    assert_eq!(request.url, REVOCATION_ENDPOINT);
    assert_eq!(request.form("token").as_deref(), Some("1//0gbye"));

    let err = client()
        .revoke(&transport, &Secret::new("1//0gbye"))
        .unwrap_err();
    assert!(matches!(err, YoutubeError::OAuth { ref error, .. } if error == "invalid_token"));
}

#[test]
fn granted_scopes_parse_force_ssl_as_manage_and_ignore_others() {
    assert_eq!(
        Scope::parse_granted(
            "openid https://www.googleapis.com/auth/youtube.force-ssl \
             https://www.googleapis.com/auth/youtube.readonly https://www.googleapis.com/auth/youtube"
        ),
        [Scope::Readonly, Scope::Manage]
    );
    assert!(Scope::parse_granted("").is_empty());
}
