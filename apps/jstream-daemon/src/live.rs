//! The live engine's wiring: an engine on a set of backends, with a stream key store and
//! a publish opener that connects RTMP(S) on each `output.start`. The daemon builds it on
//! the Windows backends (`serve.rs`); the tests build the same wiring on the platform
//! fakes, so it is compiled and tested on every OS.
//!
//! Docs: apps/jstream-daemon/README.md#design

use jstream_core::{Engine, LivePublish, OpenPublish, PublishError};
use jstream_platform::{Backends, Secret, StreamKeyStore};
use jstream_protocol::{OutputId, OutputKind, RtmpTarget};
use std::sync::{Arc, OnceLock, Weak};

/// How the publish opener connects to an RTMP(S) server URL with a stream key:
/// [`rtmp`] in the daemon, a recording target in tests.
pub type Connect =
    Arc<dyn Fn(&str, &Secret) -> Result<Box<dyn LivePublish>, PublishError> + Send + Sync>;

/// Connects with `connect_rtmp`, whose errors never carry the key (R-SEC-001).
pub fn rtmp() -> Connect {
    Arc::new(|url, key| {
        let publish: Box<dyn LivePublish> =
            Box::new(jstream_core::connect_rtmp(url, key.expose())?);
        Ok(publish)
    })
}

/// Makes `engine` live: it runs on `backends`, keeps stream keys in `keys`, and opens
/// its publish target with the opener on every `output.start`: it reads the output's
/// server URL from the collection (`Engine::output_kind`) and its key from `keys`, then
/// calls `connect`. A start fails with a reason that says what to fix when the output
/// is not in the collection (a `--output` placeholder has no target), has no stream
/// key, or the store can't be read. No reason carries the key (R-SEC-001).
///
/// The engine stores the opener, so the opener must not hold the engine strongly: that
/// cycle would keep the engine, and its running pipeline, alive after the server and
/// every session let go of it. So the engine is returned in an `Arc` and the opener
/// gets only a `Weak` to it, set once the engine is built.
pub fn live(
    engine: Engine,
    backends: Backends,
    keys: Arc<dyn StreamKeyStore>,
    connect: Connect,
) -> Arc<Engine> {
    let handle = Arc::new(OnceLock::new());
    let engine = Arc::new(
        engine
            .with_key_store(Arc::clone(&keys))
            .with_backends(backends)
            .with_publish_opener(opener(Arc::clone(&handle), keys, connect)),
    );
    // The cell is new, so this is its only value.
    let _ = handle.set(Arc::downgrade(&engine));
    engine
}

/// The publish opener `live` gives the engine (see there). It upgrades `engine` only to
/// read the target, so it holds no strong handle while it connects.
fn opener(
    engine: Arc<OnceLock<Weak<Engine>>>,
    keys: Arc<dyn StreamKeyStore>,
    connect: Connect,
) -> OpenPublish {
    Arc::new(move |output_id: &OutputId| {
        let kind = engine
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| PublishError::new("the engine is shutting down"))?
            .output_kind(output_id);
        let url = match kind {
            Some(OutputKind::Rtmp {
                target: RtmpTarget::Generic { url },
            }) => url,
            Some(_) => {
                return Err(PublishError::new(format!(
                    "output `{output_id}` is of a kind this daemon can't publish yet"
                )));
            }
            None => {
                return Err(PublishError::new(format!(
                    "output `{output_id}` has no target: create it with `output.create`"
                )));
            }
        };
        let key = keys
            .get(output_id.as_str())
            .map_err(|error| PublishError::new(format!("could not read the stream key: {error}")))?
            .ok_or_else(|| {
                PublishError::new(format!(
                    "output `{output_id}` has no stream key: set one with `output.set_key`"
                ))
            })?;
        connect(&url, &key)
    })
}

#[cfg(test)]
mod tests {
    use super::{Connect, live, opener};
    use futures_util::StreamExt;
    use jstream_control::protocol::{
        Action, Batch, Call, CallResult, Edit, Hello, OutputId, OutputKind, OutputState,
        PROTOCOL_VERSION, ProtocolError, Query, QueryResult, RtmpTarget, SecretString,
    };
    use jstream_control::{Control, ControlError};
    use jstream_core::{Engine, LivePublish, RecordingPublish};
    use jstream_media::Timebase;
    use jstream_platform::fake::{FakeClock, MemoryKeyStore, backends};
    use jstream_platform::{Secret, StreamKeyStore};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;

    const URL: &str = "rtmp://127.0.0.1:1935/live";
    const KEY: &str = "secret-stream-key-1234";

    /// Fails the test after 10 s instead of hanging.
    async fn timely<T>(future: impl Future<Output = T>) -> T {
        match tokio::time::timeout(Duration::from_secs(10), future).await {
            Ok(value) => value,
            Err(_) => panic!("timed out"),
        }
    }

    /// The URL and key of each connect, in order.
    type Seen = Arc<Mutex<Vec<(String, String)>>>;

    /// A connect that records the URL and key it was given, and publishes into memory.
    fn recording() -> (Connect, Seen) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let connect: Connect = {
            let seen = Arc::clone(&seen);
            Arc::new(move |url, key: &Secret| {
                seen.lock()
                    .expect("seen")
                    .push((url.to_owned(), key.expose().to_owned()));
                let publish: Box<dyn LivePublish> = Box::new(RecordingPublish::new());
                Ok(publish)
            })
        };
        (connect, seen)
    }

    fn rtmp(url: &str) -> OutputKind {
        OutputKind::Rtmp {
            target: RtmpTarget::Generic { url: url.into() },
        }
    }

    /// A session on `engine`, past `hello`.
    async fn session(engine: &Engine) -> jstream_core::InProcessControl {
        let control = engine.connect();
        timely(control.hello(Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "daemon-test".into(),
        }))
        .await
        .expect("welcome");
        control
    }

    async fn create(control: &jstream_core::InProcessControl, id: &str, url: &str) {
        timely(control.call(Call::Batch(Batch::single(Edit::OutputCreate {
            output_id: OutputId::new(id),
            kind: rtmp(url),
        }))))
        .await
        .expect("output.create");
    }

    fn start(id: &str) -> Call {
        Call::Action {
            action: Action::OutputStart {
                output_id: OutputId::new(id),
            },
        }
    }

    fn reason(error: &ControlError) -> String {
        match error.protocol() {
            Some(ProtocolError::ActionFailed { reason }) => reason.clone(),
            other => panic!("expected action_failed, got {other:?}"),
        }
    }

    /// The opener over `engine` alone, as `live` builds it.
    fn open_on(
        engine: &Arc<Engine>,
        keys: &MemoryKeyStore,
        connect: Connect,
    ) -> jstream_core::OpenPublish {
        let handle = Arc::new(OnceLock::new());
        let _ = handle.set(Arc::downgrade(engine));
        opener(handle, Arc::new(keys.clone()), connect)
    }

    /// The opener connects with the output's URL from the collection and its key from
    /// the store, and reads both again on every start.
    #[tokio::test]
    async fn the_opener_connects_with_the_collection_url_and_the_stored_key() {
        let engine = Arc::new(Engine::new());
        let control = session(&engine).await;
        create(&control, "out_yt", URL).await;
        let keys = MemoryKeyStore::new();
        keys.set("out_yt", &Secret::new(KEY)).expect("stored");
        let (connect, seen) = recording();
        let open = open_on(&engine, &keys, connect);
        assert!(open(&OutputId::new("out_yt")).is_ok());

        let other = "rtmps://a.rtmps.youtube.com:443/live2";
        timely(
            control.call(Call::Batch(Batch::single(Edit::OutputSetTarget {
                output_id: OutputId::new("out_yt"),
                target: RtmpTarget::Generic { url: other.into() },
            }))),
        )
        .await
        .expect("output.set_target");
        keys.set("out_yt", &Secret::new("another-key"))
            .expect("stored");
        assert!(open(&OutputId::new("out_yt")).is_ok());
        assert_eq!(
            *seen.lock().expect("seen"),
            [
                (URL.to_owned(), KEY.to_owned()),
                (other.to_owned(), "another-key".to_owned())
            ]
        );
    }

    /// Every way a start can't publish fails with a reason that says what to fix, and
    /// none of them connects or names the key (R-SEC-001).
    #[tokio::test]
    async fn the_opener_says_what_is_missing() {
        let engine = Arc::new(Engine::new().with_output(OutputId::new("placeholder")));
        let control = session(&engine).await;
        create(&control, "no_key", URL).await;
        create(&control, "out_yt", URL).await;
        let keys = MemoryKeyStore::new();
        keys.set("out_yt", &Secret::new(KEY)).expect("stored");
        let (connect, seen) = recording();
        let open = open_on(&engine, &keys, connect);
        let fails = |id: &str| match open(&OutputId::new(id)) {
            Ok(_) => panic!("`{id}` opened"),
            Err(error) => error.to_string(),
        };

        let placeholder = fails("placeholder");
        assert!(placeholder.contains("has no target"), "{placeholder}");
        assert!(fails("missing").contains("has no target"));
        let no_key = fails("no_key");
        assert!(no_key.contains("set one with `output.set_key`"), "{no_key}");
        keys.set_failing(true);
        let unreadable = fails("out_yt");
        assert!(
            unreadable.contains("could not read the stream key"),
            "{unreadable}"
        );
        assert!(!unreadable.contains(KEY), "{unreadable}");
        keys.set_failing(false);
        drop(control);
        drop(engine);
        let gone = fails("out_yt");
        assert!(gone.contains("shutting down"), "{gone}");
        assert!(seen.lock().expect("seen").is_empty(), "nothing connected");
    }

    /// The live engine runs `output.start` through the opener: a configured output with
    /// a key goes `active` on the fake backends, and one without a key fails with the
    /// opener's reason. Dropping the last handle ends the event stream, so the opener
    /// does not keep the engine alive.
    #[tokio::test]
    async fn the_live_engine_publishes_through_the_opener() {
        let clock = FakeClock::new(Timebase::MICROS);
        let keys = MemoryKeyStore::new();
        let (connect, seen) = recording();
        let engine = live(
            Engine::new(),
            backends(&clock).expect("fake backends"),
            Arc::new(keys.clone()),
            connect,
        );
        // Clones share the media host, so this sets the CPU compositor on `engine` too,
        // and the opener's weak handle stays the one `live` made.
        let _ = Engine::clone(&engine).with_cpu_compositor();
        let control = session(&engine).await;
        let mut events = control.events();
        create(&control, "out_yt", URL).await;

        let refused = timely(control.call(start("out_yt")))
            .await
            .expect_err("no key yet");
        assert!(reason(&refused).contains("has no stream key"), "{refused}");
        timely(control.call(Call::Action {
            action: Action::OutputSetKey {
                output_id: OutputId::new("out_yt"),
                key: SecretString::new(KEY),
            },
        }))
        .await
        .expect("output.set_key");
        timely(control.call(start("out_yt")))
            .await
            .expect("started");
        let status = timely(control.call(Call::Query {
            query: Query::OutputStatus {
                output_id: OutputId::new("out_yt"),
            },
        }))
        .await
        .expect("answered");
        match status.result {
            CallResult::Query {
                result: QueryResult::OutputStatus { state, .. },
            } => assert_eq!(state, OutputState::Active),
            other => panic!("expected output.status, got {other:?}"),
        }
        assert_eq!(
            *seen.lock().expect("seen"),
            [(URL.to_owned(), KEY.to_owned())]
        );
        timely(control.call(Call::Action {
            action: Action::OutputStop {
                output_id: OutputId::new("out_yt"),
            },
        }))
        .await
        .expect("stopped");

        drop(control);
        drop(engine);
        timely(async { while events.next().await.is_some() {} }).await;
    }
}
