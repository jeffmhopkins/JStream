//! The UI settings the engine's server keeps for every client (`ui_settings.get` and
//! `ui_settings.set`, ADR-0084), so the workspace follows the person from the desktop app
//! to a browser. The UI owns each value's shape; the server only stores it. A session
//! with no server in front answers `unsupported`, and the UI then keeps its settings in
//! the browser alone.
//!
//! Docs: docs/UI-FEATURES.md#docking, crates/jstream-ui/README.md#design

use jstream_control::{Control, ControlError};
use jstream_protocol::{Action, Call, CallResult, ProtocolError, Query, QueryResult};
use serde_json::Value;

/// The key the dock is kept under.
pub const DOCK_KEY: &str = "workspace.dock";

/// What the server holds under a key.
#[derive(Clone, Debug, PartialEq)]
pub enum Kept {
    /// The value kept.
    Value(Value),
    /// Nothing is kept under the key yet.
    Nothing,
    /// The session doesn't serve UI settings: nothing in front of the engine keeps them.
    Unserved,
}

/// What the server answered for the dock, as the first step needs it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Theirs<'a> {
    /// The server keeps none.
    Nothing,
    /// The server keeps a dock this UI reads.
    Readable(&'a Value),
    /// The server keeps a value this UI can't read as a dock, perhaps a newer UI's.
    Unreadable,
}

/// What the workspace does once the server first answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstStep {
    /// Show the server's dock.
    Adopt,
    /// Send this browser's dock.
    Send,
    /// Change nothing now; the person's next change is sent.
    Hold,
}

/// The first step, from the server's answer, this browser's dock (`ours`) and the dock
/// this browser last read from or sent to the server (`last_shared`). The browser's copy
/// holds a change the server never received when it differs from `last_shared`, for
/// example one made in the last moments before the page closed, or while the server
/// couldn't be reached: then ours is sent rather than replaced. A browser that never
/// shared a dock takes the server's, so a layout made elsewhere follows the person. An
/// unreadable dock is left alone unless ours holds an unsent change, so an older UI never
/// overwrites a newer one's dock just by opening.
pub fn first_step(theirs: Theirs<'_>, ours: &Value, last_shared: Option<&Value>) -> FirstStep {
    let unsent = last_shared.is_some_and(|shared| shared != ours);
    match theirs {
        Theirs::Nothing => FirstStep::Send,
        Theirs::Unreadable if unsent => FirstStep::Send,
        Theirs::Unreadable => FirstStep::Hold,
        Theirs::Readable(theirs) if theirs == ours => FirstStep::Hold,
        Theirs::Readable(_) if unsent => FirstStep::Send,
        Theirs::Readable(_) => FirstStep::Adopt,
    }
}

/// Reads the value kept under `key` (`ui_settings.get`).
pub async fn get(control: &dyn Control, key: &str) -> Result<Kept, String> {
    let call = Call::Query {
        query: Query::UiSettingsGet { key: key.into() },
    };
    match control.call(call).await {
        Ok(response) => match response.result {
            CallResult::Query {
                result: QueryResult::UiSettingsGet { value, .. },
            } => Ok(value.map_or(Kept::Nothing, Kept::Value)),
            _ => Err("the engine answered `ui_settings.get` with something else".into()),
        },
        Err(error) if unsupported(&error) => Ok(Kept::Unserved),
        Err(error) => Err(error.to_string()),
    }
}

/// Keeps `value` under `key` (`ui_settings.set`).
pub async fn set(control: &dyn Control, key: &str, value: Value) -> Result<(), String> {
    let call = Call::Action {
        action: Action::UiSettingsSet {
            key: key.into(),
            value: Some(value),
        },
    };
    control
        .call(call)
        .await
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Whether a call failed because nothing in front of the engine serves it.
fn unsupported(error: &ControlError) -> bool {
    matches!(error.protocol(), Some(ProtocolError::Unsupported { .. }))
}

#[cfg(test)]
mod tests {
    use super::{DOCK_KEY, FirstStep, Kept, Theirs, first_step, get, set};
    use jstream_control::{BoxFuture, Control, ControlError, EventStream, MockEngine};
    use jstream_protocol::{
        Action, Call, CallResult, Hello, PROTOCOL_VERSION, Query, QueryResult, RequestId, Response,
        Welcome,
    };
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use std::pin::pin;
    use std::sync::Mutex;
    use std::task::{Context, Poll, Waker};

    /// Polls a future that is ready at once, as the mock's and the stub's are.
    fn now<F: Future>(future: F) -> F::Output {
        match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the futures here are ready at once"),
        }
    }

    /// A session with a server in front: the mock engine, with the UI settings calls
    /// answered as the server answers them.
    struct Served {
        inner: jstream_control::MockControl,
        kept: Mutex<BTreeMap<String, Value>>,
    }

    impl Control for Served {
        fn hello(&self, hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
            self.inner.hello(hello)
        }

        fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
            let mut kept = self.kept.lock().expect("not poisoned");
            let result = match call {
                Call::Action {
                    action: Action::UiSettingsSet { key, value },
                } => {
                    match value {
                        Some(value) => kept.insert(key, value),
                        None => kept.remove(&key),
                    };
                    CallResult::action()
                }
                Call::Query {
                    query: Query::UiSettingsGet { key },
                } => CallResult::Query {
                    result: QueryResult::UiSettingsGet {
                        value: kept.get(&key).cloned(),
                        key,
                    },
                },
                other => return self.inner.call(other),
            };
            Box::pin(async move {
                Ok(Response {
                    id: RequestId::new("server_1"),
                    result,
                })
            })
        }

        fn events(&self) -> EventStream {
            self.inner.events()
        }
    }

    fn hello() -> Hello {
        Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "test".into(),
        }
    }

    #[test]
    fn the_dock_is_kept_and_read_back() {
        let served = Served {
            inner: MockEngine::new().connect(),
            kept: Mutex::new(BTreeMap::new()),
        };
        now(served.hello(hello())).expect("welcome");
        assert_eq!(now(get(&served, DOCK_KEY)), Ok(Kept::Nothing));
        let dock = json!({"version": 1, "current": "Live"});
        now(set(&served, DOCK_KEY, dock.clone())).expect("kept");
        assert_eq!(now(get(&served, DOCK_KEY)), Ok(Kept::Value(dock)));
    }

    /// With no server in front, the engine answers `unsupported`, and the UI keeps the
    /// dock in the browser alone.
    #[test]
    fn without_a_server_nothing_is_served() {
        let bare = MockEngine::new().connect();
        now(bare.hello(hello())).expect("welcome");
        assert_eq!(now(get(&bare, DOCK_KEY)), Ok(Kept::Unserved));
        let refused = now(set(&bare, DOCK_KEY, json!(1)));
        assert!(refused.is_err(), "{refused:?}");
    }

    /// A change the server never received (the page closed within the send delay, or
    /// the server couldn't be reached) is sent on the next load, not replaced by the
    /// server's older dock.
    #[test]
    fn an_unsent_change_wins_over_the_server_s_older_dock() {
        let old = json!({"version": 1, "current": "Editing"});
        let ours = json!({"version": 1, "current": "My layout 1"});
        assert_eq!(
            first_step(Theirs::Readable(&old), &ours, Some(&old)),
            FirstStep::Send
        );
    }

    #[test]
    fn the_first_step_follows_what_each_side_holds() {
        let a = json!({"version": 1, "current": "Editing"});
        let b = json!({"version": 1, "current": "Live"});
        // Nothing on the server: ours goes up.
        assert_eq!(first_step(Theirs::Nothing, &a, None), FirstStep::Send);
        // Another device changed it, and ours is what we last shared: take theirs.
        assert_eq!(
            first_step(Theirs::Readable(&b), &a, Some(&a)),
            FirstStep::Adopt
        );
        // An unsent change wins even over another device's newer dock.
        let c = json!({"version": 1, "current": "My layout 1"});
        assert_eq!(
            first_step(Theirs::Readable(&b), &c, Some(&a)),
            FirstStep::Send
        );
        // A browser that never shared takes the server's.
        assert_eq!(first_step(Theirs::Readable(&b), &a, None), FirstStep::Adopt);
        // The same on both sides: nothing to do.
        assert_eq!(
            first_step(Theirs::Readable(&a), &a, Some(&b)),
            FirstStep::Hold
        );
        // A newer UI's dock is left alone unless ours holds an unsent change.
        assert_eq!(
            first_step(Theirs::Unreadable, &a, Some(&a)),
            FirstStep::Hold
        );
        assert_eq!(first_step(Theirs::Unreadable, &a, None), FirstStep::Hold);
        assert_eq!(
            first_step(Theirs::Unreadable, &a, Some(&b)),
            FirstStep::Send
        );
    }
}
