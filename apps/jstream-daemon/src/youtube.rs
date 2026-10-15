//! YouTube setup: the app's Google OAuth client, read at runtime and never committed
//! (R-SEC-001), and handing it to the engine with `Engine::with_youtube` (ADR-0041).
//!
//! The client comes from `--youtube-client <file>`, the JSON Google Cloud gives for a
//! "Desktop app" OAuth client, else from the `JSTREAM_YOUTUBE_CLIENT_ID` and
//! `JSTREAM_YOUTUBE_CLIENT_SECRET` environment variables. Without either, YouTube
//! answers `unavailable`. The secret is held as a `Secret`, so it never prints, and no
//! error here quotes the file's contents.
//!
//! Docs: apps/jstream-daemon/README.md#design

use jstream_platform::Secret;
use std::path::Path;
#[cfg(any(windows, test))]
use {
    jstream_core::{Engine, YoutubeSetup},
    jstream_platform::OAuthTokenStore,
    jstream_youtube::HttpTransport,
    std::sync::Arc,
};

/// The environment variable holding the OAuth client ID, when `--youtube-client` is not
/// given.
pub const CLIENT_ID_ENV: &str = "JSTREAM_YOUTUBE_CLIENT_ID";
/// The environment variable holding the client's secret, if it has one.
pub const CLIENT_SECRET_ENV: &str = "JSTREAM_YOUTUBE_CLIENT_SECRET";

/// The app's OAuth client.
#[derive(Debug)]
pub struct Client {
    /// The client ID (`….apps.googleusercontent.com`). Not secret, but never logged.
    pub id: String,
    /// The client secret. Google gives a desktop client one, though an installed app
    /// can't really keep it; it still never prints.
    pub secret: Option<Secret>,
}

/// The OAuth client: from `file` when given, else from `env` (the process environment
/// in the daemon). `None` when neither names one.
///
/// # Errors
/// A message for the user when the file can't be read or holds no client, or when the
/// environment has a secret but no ID. It never contains the file's text or the secret.
pub fn client(
    file: Option<&Path>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Option<Client>, String> {
    if let Some(path) = file {
        let text = std::fs::read_to_string(path).map_err(|error| {
            format!(
                "could not read the YouTube client file `{}`: {error}",
                path.display()
            )
        })?;
        return parse(&text)
            .map(Some)
            .map_err(|why| format!("`{}` {why}", path.display()));
    }
    let present = |name: &str| env(name).filter(|value| !value.trim().is_empty());
    match (present(CLIENT_ID_ENV), present(CLIENT_SECRET_ENV)) {
        (Some(id), secret) => Ok(Some(Client {
            id: id.trim().to_owned(),
            secret: secret.map(|secret| Secret::new(secret.trim())),
        })),
        (None, Some(_)) => Err(format!(
            "{CLIENT_SECRET_ENV} is set but {CLIENT_ID_ENV} is not: set both, or neither"
        )),
        (None, None) => Ok(None),
    }
}

/// Reads a client file: Google's download (`{"installed": {"client_id": …,
/// "client_secret": …}}`) or the same two fields at the top level. The error follows
/// the file's path in a sentence, and names no value from the file.
fn parse(text: &str) -> Result<Client, String> {
    let json: serde_json::Value = serde_json::from_str(text).map_err(|error| {
        // Only the position: serde_json's messages can quote a value from the file.
        format!(
            "is not JSON (line {}, column {})",
            error.line(),
            error.column()
        )
    })?;
    let fields = json.get("installed").unwrap_or(&json);
    let field = |name: &str| {
        fields
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let id = field("client_id").ok_or_else(|| {
        "has no `client_id`: use the JSON Google Cloud gives for a Desktop app OAuth client"
            .to_owned()
    })?;
    Ok(Client {
        id: id.to_owned(),
        secret: field("client_secret").map(Secret::new),
    })
}

/// Gives `engine` YouTube with `client`, calling Google through `transport` and keeping
/// the account's refresh token in `tokens`.
#[cfg(any(windows, test))]
pub fn set_up(
    engine: Engine,
    client: Client,
    transport: Arc<dyn HttpTransport>,
    tokens: Arc<dyn OAuthTokenStore>,
) -> Engine {
    engine.with_youtube(YoutubeSetup::new(
        client.id,
        client.secret,
        transport,
        tokens,
    ))
}

#[cfg(test)]
mod tests {
    use super::{CLIENT_ID_ENV, CLIENT_SECRET_ENV, client, parse};
    use std::collections::HashMap;

    const ID: &str = "1234-abc.apps.googleusercontent.com";
    const SECRET: &str = "GOCSPX-not-a-real-secret";

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn googles_download_and_flat_fields_both_read() {
        let google = format!(
            r#"{{"installed":{{"client_id":"{ID}","project_id":"p","client_secret":"{SECRET}","redirect_uris":["http://localhost"]}}}}"#
        );
        let read = parse(&google).expect("a client");
        assert_eq!(read.id, ID);
        assert_eq!(read.secret.expect("a secret").expose(), SECRET);
        let flat = parse(&format!(r#"{{"client_id":" {ID} "}}"#)).expect("a client");
        assert_eq!(flat.id, ID);
        assert!(flat.secret.is_none());
    }

    #[test]
    fn a_bad_file_says_why_without_quoting_it() {
        let not_json = parse(&format!("{{\"client_secret\": {SECRET}")).expect_err("bad");
        assert!(not_json.starts_with("is not JSON (line 1"), "{not_json}");
        assert!(!not_json.contains(SECRET));
        let no_id = parse(&format!(
            r#"{{"installed":{{"client_secret":"{SECRET}"}}}}"#
        ))
        .expect_err("no id");
        assert!(no_id.starts_with("has no `client_id`"), "{no_id}");
        assert!(!no_id.contains(SECRET));
        let missing = client(
            Some(std::path::Path::new("/nonexistent/client.json")),
            env(&[]),
        )
        .expect_err("missing");
        assert!(
            missing.starts_with("could not read the YouTube client file"),
            "{missing}"
        );
    }

    #[test]
    fn the_environment_names_the_client_when_no_file_does() {
        let both = client(
            None,
            env(&[(CLIENT_ID_ENV, ID), (CLIENT_SECRET_ENV, SECRET)]),
        )
        .expect("read")
        .expect("a client");
        assert_eq!(both.id, ID);
        assert_eq!(both.secret.expect("a secret").expose(), SECRET);
        assert!(client(None, env(&[])).expect("read").is_none());
        assert!(
            client(None, env(&[(CLIENT_ID_ENV, "  ")]))
                .expect("read")
                .is_none(),
            "blank is unset"
        );
        let lone = client(None, env(&[(CLIENT_SECRET_ENV, SECRET)])).expect_err("no id");
        assert!(lone.contains(CLIENT_ID_ENV), "{lone}");
        assert!(!lone.contains(SECRET));
    }

    /// With a client, the engine's YouTube is there: signed out, not `unavailable`.
    #[tokio::test]
    async fn set_up_gives_the_engine_youtube() {
        use super::{Client, set_up};
        use jstream_control::Control;
        use jstream_control::protocol::{
            Call, CallResult, Hello, PROTOCOL_VERSION, Query, QueryResult, YoutubeAuth,
        };
        use jstream_core::Engine;
        use jstream_platform::Secret;
        use jstream_platform::fake::MemoryTokenStore;
        use jstream_youtube::fake::FakeTransport;
        use std::sync::Arc;

        async fn auth(engine: &Engine) -> YoutubeAuth {
            let control = engine.connect();
            control
                .hello(Hello {
                    protocol: PROTOCOL_VERSION,
                    client_name: "daemon-test".into(),
                })
                .await
                .expect("welcome");
            match control
                .call(Call::Query {
                    query: Query::YoutubeStatus,
                })
                .await
                .expect("answered")
                .result
            {
                CallResult::Query {
                    result: QueryResult::YoutubeStatus { auth },
                } => auth,
                other => panic!("expected youtube.status, got {other:?}"),
            }
        }

        assert_eq!(auth(&Engine::new()).await, YoutubeAuth::Unavailable);
        let engine = set_up(
            Engine::new(),
            Client {
                id: ID.to_owned(),
                secret: Some(Secret::new(SECRET)),
            },
            Arc::new(FakeTransport::new()),
            Arc::new(MemoryTokenStore::new()),
        );
        assert_eq!(auth(&engine).await, YoutubeAuth::SignedOut);
    }
}
