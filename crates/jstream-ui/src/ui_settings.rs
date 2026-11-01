//! The UI settings the engine's server keeps for every client (`ui_settings.get` and
//! `ui_settings.set`, ADR-0083), so the workspace follows the person from the desktop app
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
    use super::{DOCK_KEY, Kept, get, set};
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
}
