//! The pairing panel (ADR-0044, ADR-0047): start pairing, show the one-time code until
//! it is used or expires, and list and revoke the paired devices.
//!
//! - **Calls.** `pairing.start` opens a code, `device.list` reads the devices and
//!   `device.revoke` revokes one. The server in front of the engine answers them; a
//!   session with no listener behind it, such as the desktop app's, answers
//!   `unsupported`, and the panel then hides itself.
//! - **The code** is shown, large, with the time it has left, and how a device uses it:
//!   a browser opens `/pair` on the host's TLS address and types it; a native client
//!   posts it to `POST /api/v1/pair`. It is shown until the countdown ends or a device
//!   pairs: while it is open the panel re-reads `device.list` every few seconds, and a
//!   device that wasn't there when the code was drawn means the code was used. The code
//!   is never logged or stored by the page (R-SEC-001).
//!
//! Docs: crates/jstream-ui/README.md#design, docs/adr/0047-browser-pairing-page-and-device-cookie.md

use crate::app::SessionContext;
use jstream_control::protocol::{
    Action, ActionResult, Call, CallResult, DeviceId, DeviceInfo, ProtocolError, Query, QueryResult,
};
use jstream_control::{Control, ControlError};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::time::Duration;

/// How often, in countdown seconds, the panel re-reads `device.list` while a code is
/// open, to notice it was used.
const POLL_EVERY_SECS: u32 = 3;

/// An open pairing code, as `pairing.start` answered it.
#[derive(Clone, PartialEq, Eq)]
pub struct OpenCode {
    /// The code, `XXXX-XXXX`, to show on screen.
    pub code: String,
    /// How long it stays open, in seconds.
    pub expires_in_secs: u32,
}

impl std::fmt::Debug for OpenCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenCode")
            .field("code", &"***")
            .field("expires_in_secs", &self.expires_in_secs)
            .finish()
    }
}

/// Whether a call failed because nothing in front of the engine serves it.
fn unsupported(error: &ControlError) -> bool {
    matches!(error.protocol(), Some(ProtocolError::Unsupported { .. }))
}

/// The paired devices (`device.list`), or `None` when the session doesn't serve the
/// call, so the panel hides.
pub async fn devices(control: &dyn Control) -> Result<Option<Vec<DeviceInfo>>, String> {
    let call = Call::Query {
        query: Query::DeviceList,
    };
    match control.call(call).await {
        Ok(response) => match response.result {
            CallResult::Query {
                result: QueryResult::DeviceList { devices },
            } => Ok(Some(devices)),
            _ => Err("the engine answered `device.list` with something else".into()),
        },
        Err(error) if unsupported(&error) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// Opens a pairing code (`pairing.start`), replacing any open one. `None` when the
/// session doesn't serve the call.
pub async fn start(control: &dyn Control) -> Result<Option<OpenCode>, String> {
    let call = Call::Action {
        action: Action::PairingStart,
    };
    match control.call(call).await {
        Ok(response) => match response.result {
            CallResult::Action {
                result:
                    Some(ActionResult::PairingStart {
                        code,
                        expires_in_secs,
                    }),
            } => Ok(Some(OpenCode {
                code: code.expose().to_owned(),
                expires_in_secs,
            })),
            _ => Err("the engine answered `pairing.start` without a code".into()),
        },
        Err(error) if unsupported(&error) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// Revokes a device (`device.revoke`): its token stops working and its connections
/// close.
pub async fn revoke(control: &dyn Control, device_id: DeviceId) -> Result<(), String> {
    let call = Call::Action {
        action: Action::DeviceRevoke { device_id },
    };
    control
        .call(call)
        .await
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Whether `devices` holds one that isn't in `known`: a device paired since.
pub fn newly_paired(known: &[DeviceId], devices: &[DeviceInfo]) -> bool {
    devices
        .iter()
        .any(|device| !known.contains(&device.device_id))
}

/// Seconds left as `m:ss`.
pub fn countdown(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// A time in seconds since the Unix epoch as its UTC date, `YYYY-MM-DD`.
pub fn utc_date(unix: u64) -> String {
    // Howard Hinnant's days-to-civil algorithm, for days since 1970-01-01.
    let days = i64::try_from(unix / 86_400).unwrap_or(i64::MAX / 2) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The panel's state, shared by its parts. Every field is `Copy`.
#[derive(Clone, Copy)]
struct Panel {
    session: SessionContext,
    /// Whether the session serves the pairing calls. The panel shows only then.
    served: RwSignal<bool>,
    devices: RwSignal<Vec<DeviceInfo>>,
    /// The open code and the seconds it has left.
    code: RwSignal<Option<(OpenCode, u32)>>,
    /// The devices paired when the open code was drawn.
    known: StoredValue<Vec<DeviceId>>,
    /// Bumped by each new code, so an older countdown stops.
    codes: StoredValue<u64>,
    problem: RwSignal<Option<String>>,
}

impl Panel {
    /// Re-reads the devices. Returns them, or `None` when the read failed or isn't
    /// served (and hides the panel then).
    async fn reload(self) -> Option<Vec<DeviceInfo>> {
        let session = self.session.get()?;
        match devices(&*session.control).await {
            Ok(Some(devices)) => {
                self.served.try_set(true);
                self.devices.try_set(devices.clone());
                Some(devices)
            }
            Ok(None) => {
                self.served.try_set(false);
                None
            }
            Err(problem) => {
                self.problem.try_set(Some(problem));
                None
            }
        }
    }

    /// Opens a new code and starts its countdown.
    fn start(self) {
        let Some(session) = self.session.get() else {
            return;
        };
        spawn_local(async move {
            match start(&*session.control).await {
                Ok(Some(code)) => {
                    let known = self
                        .devices
                        .try_with_untracked(|devices| {
                            devices.iter().map(|d| d.device_id.clone()).collect()
                        })
                        .unwrap_or_default();
                    self.known.try_set_value(known);
                    let Some(mine) = self.codes.try_update_value(|n| {
                        *n += 1;
                        *n
                    }) else {
                        return;
                    };
                    let left = code.expires_in_secs;
                    self.code.try_set(Some((code, left)));
                    self.problem.try_set(None);
                    self.tick(mine);
                }
                Ok(None) => {
                    self.served.try_set(false);
                }
                Err(problem) => {
                    self.problem.try_set(Some(problem));
                }
            }
        });
    }

    /// One second of code `mine`'s countdown: stops when a newer code replaced it,
    /// clears it when it runs out, and every few seconds checks whether a device paired.
    fn tick(self, mine: u64) {
        set_timeout(
            move || {
                if self.codes.try_get_value() != Some(mine) {
                    return;
                }
                let left = self
                    .code
                    .try_update(|code| match code {
                        Some((_, left)) => {
                            *left = left.saturating_sub(1);
                            *left
                        }
                        None => 0,
                    })
                    .unwrap_or(0);
                if left == 0 {
                    self.code.try_set(None);
                    return;
                }
                if left % POLL_EVERY_SECS != 0 {
                    self.tick(mine);
                    return;
                }
                spawn_local(async move {
                    let listed = self.reload().await;
                    if self.codes.try_get_value() != Some(mine) {
                        return;
                    }
                    let known = self.known.try_get_value().unwrap_or_default();
                    if listed.is_some_and(|devices| newly_paired(&known, &devices)) {
                        self.code.try_set(None);
                    } else {
                        self.tick(mine);
                    }
                });
            },
            Duration::from_secs(1),
        );
    }

    /// Revokes a device, then re-reads the list.
    fn revoke(self, device_id: DeviceId) {
        let Some(session) = self.session.get() else {
            return;
        };
        spawn_local(async move {
            match revoke(&*session.control, device_id).await {
                Ok(()) => {
                    self.problem.try_set(None);
                }
                Err(problem) => {
                    self.problem.try_set(Some(problem));
                }
            }
            self.reload().await;
        });
    }
}

/// The pairing panel. Shows nothing until `device.list` answers, and nothing at all on
/// a session that doesn't serve the pairing calls.
#[component]
pub fn Pairing() -> impl IntoView {
    let Some(session) = use_context::<SessionContext>() else {
        return ().into_any();
    };
    let panel = Panel {
        session,
        served: RwSignal::new(false),
        devices: RwSignal::new(Vec::new()),
        code: RwSignal::new(None),
        known: StoredValue::new(Vec::new()),
        codes: StoredValue::new(0),
        problem: RwSignal::new(None),
    };
    spawn_local(async move {
        panel.reload().await;
    });

    let code_view = move || {
        panel.code.get().map(|(code, left)| {
            view! {
                <div class="pairing-open" role="status">
                    <p class="pairing-code">{code.code}</p>
                    <p class="pairing-help">
                        "Open for " {countdown(left)} ". In a browser, open "
                        <code>"/pair"</code> " on this host's HTTPS address and type the code."
                    </p>
                </div>
            }
        })
    };
    let ids = move || {
        panel.devices.with(|devices| {
            devices
                .iter()
                .map(|d| d.device_id.clone())
                .collect::<Vec<_>>()
        })
    };
    let device_view = move |device_id: DeviceId| {
        let info = move || {
            panel
                .devices
                .with(|devices| devices.iter().find(|d| d.device_id == device_id).cloned())
        };
        move || {
            info().map(|device| {
                let id = device.device_id.clone();
                view! {
                    <li class="device">
                        <span class="device-name">{device.name.clone()}</span>
                        <span class="device-dates">
                            "paired " {utc_date(device.paired_at)} ", last used "
                            {utc_date(device.last_used_at)}
                        </span>
                        <button on:click=move |_| panel.revoke(id.clone())>"Revoke"</button>
                    </li>
                }
            })
        }
    };

    view! {
        <Show when=move || panel.served.get()>
            <section class="pairing">
                <h2>"Devices"</h2>
                <button class="primary" on:click=move |_| panel.start()>
                    {move || if panel.code.with(Option::is_some) { "New code" } else { "Pair a device" }}
                </button>
                {code_view}
                {move || {
                    panel
                        .problem
                        .get()
                        .map(|problem| view! { <p class="problem" role="alert">{problem}</p> })
                }}
                <Show
                    when=move || panel.devices.with(|devices| !devices.is_empty())
                    fallback=|| view! { <p class="placeholder">"No paired devices."</p> }
                >
                    <ul class="device-list">
                        <For each=ids key=|id| id.clone() let:device_id>
                            {device_view(device_id)}
                        </For>
                    </ul>
                </Show>
            </section>
        </Show>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::{OpenCode, countdown, devices, newly_paired, revoke, start, utc_date};
    use jstream_control::protocol::{
        Action, ActionResult, Call, CallResult, DeviceId, DeviceInfo, EntityKind, Hello,
        PROTOCOL_VERSION, ProtocolError, Query, QueryResult, RequestId, Response, SecretString,
        Welcome,
    };
    use jstream_control::{BoxFuture, Control, ControlError, EventStream, MockEngine};
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

    fn hello() -> Hello {
        Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "test".into(),
        }
    }

    /// A session with a server in front: the mock engine, with the pairing calls
    /// answered as the server answers them.
    struct Served {
        inner: jstream_control::MockControl,
        devices: Mutex<Vec<DeviceInfo>>,
    }

    impl Control for Served {
        fn hello(&self, hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
            self.inner.hello(hello)
        }

        fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
            let answer = |result| {
                Ok(Response {
                    id: RequestId::new("server_1"),
                    result,
                })
            };
            let mut devices = self.devices.lock().expect("not poisoned");
            let result = match call {
                Call::Action {
                    action: Action::PairingStart,
                } => answer(CallResult::Action {
                    result: Some(ActionResult::PairingStart {
                        code: SecretString::new("7K3M-9QXA"),
                        expires_in_secs: 120,
                    }),
                }),
                Call::Query {
                    query: Query::DeviceList,
                } => answer(CallResult::Query {
                    result: QueryResult::DeviceList {
                        devices: devices.clone(),
                    },
                }),
                Call::Action {
                    action: Action::DeviceRevoke { device_id },
                } => {
                    let before = devices.len();
                    devices.retain(|d| d.device_id != device_id);
                    if devices.len() == before {
                        Err(ControlError::from(ProtocolError::NotFound {
                            entity: EntityKind::Device,
                            id: device_id.to_string(),
                        }))
                    } else {
                        answer(CallResult::action())
                    }
                }
                other => return self.inner.call(other),
            };
            Box::pin(std::future::ready(result))
        }

        fn events(&self) -> EventStream {
            self.inner.events()
        }
    }

    fn device(id: &str, name: &str) -> DeviceInfo {
        DeviceInfo {
            device_id: DeviceId::new(id),
            name: name.into(),
            paired_at: 1_760_486_400,
            last_used_at: 1_760_486_400,
        }
    }

    /// A session without a server in front answers `unsupported`, and the panel hides.
    #[test]
    fn the_panel_hides_without_a_server() {
        let control = MockEngine::new().connect();
        now(control.hello(hello())).expect("hello");
        assert_eq!(now(devices(&control)), Ok(None));
        assert_eq!(now(start(&control)), Ok(None));
    }

    /// With a server in front, the panel's calls start pairing, list and revoke.
    #[test]
    fn the_panel_s_calls_pair_list_and_revoke() {
        let served = Served {
            inner: MockEngine::new().connect(),
            devices: Mutex::new(vec![device("dev_1", "Tablet")]),
        };
        now(served.hello(hello())).expect("hello");
        let code = now(start(&served)).expect("served");
        assert_eq!(
            code,
            Some(OpenCode {
                code: "7K3M-9QXA".into(),
                expires_in_secs: 120
            })
        );
        assert!(!format!("{code:?}").contains("7K3M"), "never in Debug");
        let listed = now(devices(&served)).expect("served").expect("listed");
        assert_eq!(listed, vec![device("dev_1", "Tablet")]);
        assert_eq!(now(revoke(&served, DeviceId::new("dev_1"))), Ok(()));
        assert_eq!(now(devices(&served)), Ok(Some(vec![])));
        let gone = now(revoke(&served, DeviceId::new("dev_1")));
        assert!(gone.is_err());
    }

    #[test]
    fn a_new_device_means_the_code_was_used() {
        let known = vec![DeviceId::new("dev_1")];
        assert!(!newly_paired(&known, &[device("dev_1", "Tablet")]));
        assert!(!newly_paired(&known, &[]), "a revoke is not a pairing");
        assert!(newly_paired(
            &known,
            &[device("dev_1", "Tablet"), device("dev_2", "Browser")]
        ));
    }

    #[test]
    fn times_read_as_a_countdown_and_utc_dates() {
        assert_eq!(countdown(120), "2:00");
        assert_eq!(countdown(65), "1:05");
        assert_eq!(countdown(0), "0:00");
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(951_782_400), "2000-02-29");
        assert_eq!(utc_date(1_760_486_399), "2025-10-14");
        assert_eq!(utc_date(1_760_486_400), "2025-10-15");
    }
}
