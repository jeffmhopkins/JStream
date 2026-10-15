//! One admitted client's `Control` session, as the server hands it out: the connector's
//! session, wrapped so the server answers the calls about its own credentials
//! (ADR-0044) and so a paired device's sessions are named after it.
//!
//! - **Calls the server answers.** `pairing.start`, `device.list` and `device.revoke`
//!   concern who the listener admits, not the engine, so they never reach the session
//!   behind: [`Admitted`] answers them from [`Devices`]. Every other call goes through
//!   unchanged. Because the WebSocket connections and the MCP sessions both get their
//!   sessions through it, the same calls work from the UI, the CLI and MCP agents (an
//!   MCP agent sees them as the `pairing` and `device` tools). Like every call, they need
//!   the handshake first (`handshake_required`).
//! - **Names.** A device's `hello` is sent on with `client_name` set to
//!   `device:<name>`, so the engine's audit log names its calls that way whatever the
//!   client calls itself.
//! - **Pairing only on loopback or TLS.** A device token travels only over loopback or
//!   TLS, so `pairing.start` fails with `action_failed` while the server listens anywhere
//!   but a loopback address, unless the listener has TLS (`ServerConfig::tls`), which
//!   it has no way to serve yet.
//!
//! The pairing code is in `pairing.start`'s answer and nowhere else: it is not logged,
//! and the answer goes only to the client that asked (R-SEC-001).

use crate::devices::{CODE_LIFETIME, Devices, Now};
use crate::gate::Credential;
use jstream_control::{BoxFuture, Control, ControlError, EventStream};
use jstream_protocol::{
    Action, ActionResult, Call, CallResult, EntityKind, Hello, ProtocolError, Query, QueryResult,
    RequestId, Response, SecretString, Welcome,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A `Control` session as admitted by one credential.
pub(crate) struct Admitted {
    inner: Box<dyn Control>,
    credential: Credential,
    devices: Arc<Devices>,
    /// Whether pairing is offered: the server listens on a loopback address or on a TLS
    /// listener.
    pairing_offered: bool,
    /// Whether the server answers the pairing and device calls (`ServerConfig::pairing`).
    /// Off, they reach `inner` like any other call.
    serves_devices: bool,
    /// Whether `hello` succeeded, which the calls answered here need too.
    greeted: AtomicBool,
    /// Numbers the requests answered here (`server_1`, ...).
    answered: AtomicU64,
}

impl Admitted {
    pub(crate) fn new(
        inner: Box<dyn Control>,
        credential: Credential,
        devices: Arc<Devices>,
        pairing_offered: bool,
        serves_devices: bool,
    ) -> Admitted {
        Admitted {
            inner,
            credential,
            devices,
            pairing_offered,
            serves_devices,
            greeted: AtomicBool::new(false),
            answered: AtomicU64::new(0),
        }
    }

    /// The answer to a call the server serves ([`served_here`]).
    fn answer(&self, call: &Call) -> Result<CallResult, ProtocolError> {
        match call {
            Call::Action {
                action: Action::PairingStart,
            } => self.start_pairing(),
            Call::Action {
                action: Action::DeviceRevoke { device_id },
            } => match self.devices.revoke(device_id) {
                Ok(true) => Ok(CallResult::action()),
                Ok(false) => Err(ProtocolError::NotFound {
                    entity: EntityKind::Device,
                    id: device_id.to_string(),
                }),
                // Revoked in memory all the same: its token no longer gets in.
                Err(error) => Err(ProtocolError::ActionFailed {
                    reason: format!(
                        "the device is revoked until the server restarts, but the device \
                         file could not be written: {error}"
                    ),
                }),
            },
            Call::Query {
                query: Query::DeviceList,
            } => Ok(CallResult::Query {
                result: QueryResult::DeviceList {
                    devices: self.devices.list(Now::current().unix),
                },
            }),
            _ => Err(ProtocolError::Internal {
                reason: "a call the server doesn't serve reached it".into(),
            }),
        }
    }

    fn start_pairing(&self) -> Result<CallResult, ProtocolError> {
        if !self.pairing_offered {
            return Err(ProtocolError::ActionFailed {
                reason: "pairing is offered only on a loopback address or a TLS listener \
                         (ADR-0044)"
                    .into(),
            });
        }
        let code = self
            .devices
            .start_pairing(Now::current())
            .map_err(|error| ProtocolError::ActionFailed {
                reason: format!("the OS random source failed: {error}"),
            })?;
        tracing::info!("pairing started");
        Ok(CallResult::Action {
            result: Some(ActionResult::PairingStart {
                code: SecretString::new(code),
                expires_in_secs: u32::try_from(CODE_LIFETIME.as_secs()).unwrap_or(u32::MAX),
            }),
        })
    }
}

/// Whether the server answers `call` itself: `pairing.start`, `device.revoke` and
/// `device.list`.
fn served_here(call: &Call) -> bool {
    matches!(
        call,
        Call::Action {
            action: Action::PairingStart | Action::DeviceRevoke { .. }
        } | Call::Query {
            query: Query::DeviceList
        }
    )
}

impl Control for Admitted {
    fn hello(&self, mut hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
        if let Credential::Device { name, .. } = &self.credential {
            hello.client_name = format!("device:{name}");
        }
        let welcome = self.inner.hello(hello);
        Box::pin(async move {
            let welcome = welcome.await?;
            self.greeted.store(true, Ordering::Release);
            Ok(welcome)
        })
    }

    fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
        if !self.serves_devices || !served_here(&call) {
            return self.inner.call(call);
        }
        Box::pin(async move {
            if !self.greeted.load(Ordering::Acquire) {
                return Err(ProtocolError::HandshakeRequired.into());
            }
            let n = self.answered.fetch_add(1, Ordering::Relaxed) + 1;
            let result = self.answer(&call)?;
            Ok(Response {
                id: RequestId::new(format!("server_{n}")),
                result,
            })
        })
    }

    fn events(&self) -> EventStream {
        self.inner.events()
    }
}

#[cfg(test)]
mod tests {
    use super::Admitted;
    use crate::devices::Devices;
    use crate::gate::Credential;
    use jstream_control::{BoxFuture, Control, ControlError, EventStream, MockEngine};
    use jstream_protocol::{Action, Call, DeviceId, Hello, PROTOCOL_VERSION, Response, Welcome};
    use std::future::Future;
    use std::sync::{Arc, Mutex};

    /// A session that remembers the `client_name` of its `hello`.
    struct Named {
        inner: jstream_control::MockControl,
        name: Arc<Mutex<String>>,
    }

    impl Control for Named {
        fn hello(&self, hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
            if let Ok(mut name) = self.name.lock() {
                name.clone_from(&hello.client_name);
            }
            self.inner.hello(hello)
        }

        fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
            self.inner.call(call)
        }

        fn events(&self) -> EventStream {
            self.inner.events()
        }
    }

    fn block<T>(future: impl Future<Output = T>) -> T {
        match tokio::runtime::Builder::new_current_thread().build() {
            Ok(runtime) => runtime.block_on(future),
            Err(error) => panic!("runtime: {error}"),
        }
    }

    fn admitted(credential: Credential, pairing_offered: bool) -> (Admitted, Arc<Mutex<String>>) {
        admitted_with(credential, pairing_offered, true)
    }

    fn admitted_with(
        credential: Credential,
        pairing_offered: bool,
        serves_devices: bool,
    ) -> (Admitted, Arc<Mutex<String>>) {
        let name = Arc::new(Mutex::new(String::new()));
        let inner = Named {
            inner: MockEngine::new().connect(),
            name: Arc::clone(&name),
        };
        let devices = match Devices::load(None) {
            Ok(devices) => Arc::new(devices),
            Err(error) => panic!("devices: {error}"),
        };
        (
            Admitted::new(
                Box::new(inner),
                credential,
                devices,
                pairing_offered,
                serves_devices,
            ),
            name,
        )
    }

    fn hello() -> Hello {
        Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "claude-desktop".into(),
        }
    }

    fn said(name: &Mutex<String>) -> String {
        name.lock().map(|n| n.clone()).unwrap_or_default()
    }

    /// The audit log names a device's calls `device:<name>`; the control token's keep the
    /// client's own name.
    #[test]
    fn a_device_says_hello_by_its_name() {
        let device = Credential::Device {
            id: DeviceId::new("dev_1"),
            name: "Tablet".into(),
            renew_cookie: false,
        };
        let (session, name) = admitted(device, true);
        assert!(block(session.hello(hello())).is_ok());
        assert_eq!(said(&name), "device:Tablet");

        let (session, name) = admitted(Credential::Control, true);
        assert!(block(session.hello(hello())).is_ok());
        assert_eq!(said(&name), "claude-desktop");
    }

    /// Beyond loopback, without TLS, pairing is refused.
    #[test]
    fn pairing_is_refused_beyond_loopback() {
        let (session, _) = admitted(Credential::Control, false);
        assert!(block(session.hello(hello())).is_ok());
        let refused = block(session.call(Call::Action {
            action: Action::PairingStart,
        }));
        let code = refused.err().and_then(|e| e.protocol().map(|p| p.code()));
        assert_eq!(code, Some("action_failed"));
    }

    /// With pairing off, the pairing and device calls reach the session, which answers
    /// `unsupported`, so a host's UI hides its pairing panel.
    #[test]
    fn with_pairing_off_the_device_calls_are_unsupported() {
        let (session, _) = admitted_with(Credential::Control, true, false);
        assert!(block(session.hello(hello())).is_ok());
        for call in [
            Call::Action {
                action: Action::PairingStart,
            },
            Call::Query {
                query: jstream_protocol::Query::DeviceList,
            },
        ] {
            let refused = block(session.call(call));
            let code = refused.err().and_then(|e| e.protocol().map(|p| p.code()));
            assert_eq!(code, Some("unsupported"));
        }
    }
}
