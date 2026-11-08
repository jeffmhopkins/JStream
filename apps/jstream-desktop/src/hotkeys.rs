//! Hotkeys heard while another application has the focus
//! ([ADR-0112](../../../docs/adr/0112-assignable-camera-zoom-bindings.md) §5 and §6, #1188).
//!
//! The host opens its own session on the engine, reads the collection's bindings, and
//! has the OS listen for exactly those chords ([`Registrar`]; on Windows
//! `jstream_platform_windows::GlobalHotkeys`, which uses `RegisterHotKey`). After each
//! `state.changed` it reads them again, so a chord bound, rebound or cleared from any
//! client is heard or let go at once. A press runs the binding's action as an `action`
//! call from that session, as a press in the page would: the host keeps nothing it hears
//! and sends it nowhere else.
//!
//! The off switch is the UI setting [`GLOBAL_KEY`] (ADR-0084), on unless it holds
//! `false`. Off, every chord is let go, and the page hears them only while it has the
//! focus, as before. A chord the OS refuses (another application holds it) is logged and
//! heard only while JStream has the focus. Wheel chords (ADR-0112 §3) aren't keys, so
//! they aren't registered: the page hears them while it has the focus.
//!
//! Docs: apps/jstream-desktop/README.md#design

use futures_util::{FutureExt, StreamExt};
use jstream_control::{Control, ControlError};
use jstream_protocol::{
    Call, CallResult, Event, Hello, HotkeyBinding, PROTOCOL_VERSION, Query, QueryResult,
};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

/// The UI setting that turns global hotkeys off: `false` is off, anything else on.
pub const GLOBAL_KEY: &str = "hotkeys.global";

/// The name the host's session gives the engine, which the audit log shows.
pub const CLIENT_NAME: &str = "jstream-desktop hotkeys";

/// What has the OS listen for chords.
pub trait Registrar: Send + Sync + 'static {
    /// Listens for `chords` (as the protocol spells them) in place of the ones before,
    /// and returns each one it couldn't, with the reason.
    fn set(&self, chords: Vec<String>) -> Vec<(String, String)>;
}

/// Whether global hotkeys are on, from the [`GLOBAL_KEY`] setting's value.
pub fn global_on(value: Option<&Value>) -> bool {
    value.and_then(Value::as_bool).unwrap_or(true)
}

/// The off switch: whether it is on now, and a signal when it may have changed.
pub struct Switch {
    /// Whether global hotkeys are on now.
    pub on: Box<dyn Fn() -> bool + Send>,
    /// Changes when the setting may have.
    pub changes: watch::Receiver<u64>,
}

/// Runs the listener until the engine ends the session or `presses` closes: says hello
/// on `control`, keeps the registered chords in step with the bindings and the switch,
/// and runs the bound action of each chord that arrives on `presses`. Lets every chord
/// go before it returns.
///
/// # Errors
/// When `hello` or reading the bindings fails.
pub async fn run<C: Control>(
    control: C,
    registrar: Arc<dyn Registrar>,
    mut switch: Switch,
    mut presses: mpsc::UnboundedReceiver<String>,
) -> Result<(), ControlError> {
    control
        .hello(Hello {
            protocol: PROTOCOL_VERSION,
            client_name: CLIENT_NAME.into(),
        })
        .await?;
    let mut events = control.events();
    let mut bindings = read_bindings(&control).await?;
    let mut registered: Option<Vec<String>> = None;
    let mut switch_open = true;
    let result = loop {
        // Keyboard chords only: the wheel needs a hook of its own (ADR-0112 §5), so a
        // wheel chord is heard only while the page has the focus for now.
        let wanted: Vec<String> = if (switch.on)() {
            bindings
                .iter()
                .filter(|b| !b.chord.is_wheel())
                .map(|b| b.chord.to_string())
                .collect()
        } else {
            Vec::new()
        };
        if registered.as_ref() != Some(&wanted) {
            register(&registrar, wanted.clone()).await;
            registered = Some(wanted);
        }
        tokio::select! {
            event = events.next() => match event {
                None => break Ok(()),
                Some(Event::StateChanged { .. }) => {
                    // Read once for every change already queued.
                    let mut ended = false;
                    loop {
                        match events.next().now_or_never() {
                            Some(Some(_)) => {}
                            Some(None) => {
                                ended = true;
                                break;
                            }
                            None => break,
                        }
                    }
                    if ended {
                        break Ok(());
                    }
                    match read_bindings(&control).await {
                        Ok(read) => bindings = read,
                        Err(error) => break Err(error),
                    }
                }
                Some(_) => {}
            },
            changed = switch.changes.changed(), if switch_open => {
                // The settings are gone (the server stopped): keep what is registered.
                switch_open = changed.is_ok();
            }
            press = presses.recv() => match press {
                None => break Ok(()),
                Some(chord) => run_binding(&control, &bindings, &chord).await,
            },
        }
    };
    register(&registrar, Vec::new()).await;
    result
}

/// Registers `chords` off the async threads, as the OS call waits on the listener's
/// thread, and logs each one the OS refused.
async fn register(registrar: &Arc<dyn Registrar>, chords: Vec<String>) {
    let count = chords.len();
    let registrar = Arc::clone(registrar);
    let failed = tokio::task::spawn_blocking(move || registrar.set(chords))
        .await
        .unwrap_or_default();
    for (chord, reason) in &failed {
        tracing::warn!(
            %chord,
            %reason,
            "a hotkey is heard only while JStream has the focus"
        );
    }
    tracing::debug!(heard = count - failed.len().min(count), "global hotkeys");
}

/// The collection's bindings, from `state.get`.
async fn read_bindings<C: Control>(control: &C) -> Result<Vec<HotkeyBinding>, ControlError> {
    let response = control
        .call(Call::Query {
            query: Query::StateGet,
        })
        .await?;
    match response.result {
        CallResult::Query {
            result: QueryResult::StateGet { hotkeys, .. },
        } => Ok(hotkeys),
        _ => Err(ControlError::Disconnected {
            reason: "the engine answered `state.get` with another result".into(),
        }),
    }
}

/// Runs the action bound to `chord`, logging why when it can't.
async fn run_binding<C: Control>(control: &C, bindings: &[HotkeyBinding], chord: &str) {
    let Some(binding) = bindings.iter().find(|b| b.chord.as_str() == chord) else {
        return;
    };
    let Some(action) = binding.action() else {
        tracing::warn!(%chord, "the hotkey's action is one this version doesn't know");
        return;
    };
    if let Err(error) = control.call(Call::Action { action }).await {
        tracing::warn!(%chord, %error, "the hotkey's action failed");
    }
}

#[cfg(test)]
mod tests {
    use super::{Registrar, Switch, global_on, run};
    use jstream_control::Control;
    use jstream_core::Engine;
    use jstream_protocol::{
        Action, Batch, Call, CallResult, Chord, Edit, Hello, PROTOCOL_VERSION, Query, QueryResult,
        SceneId,
    };
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::{mpsc, watch};

    /// Records each set of chords it is given, and refuses `Ctrl+KeyC`.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<Vec<String>>>);

    impl Registrar for Recorder {
        fn set(&self, chords: Vec<String>) -> Vec<(String, String)> {
            let failed = chords
                .iter()
                .filter(|chord| *chord == "Ctrl+KeyC")
                .map(|chord| (chord.clone(), "held".to_owned()))
                .collect();
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(chords);
            failed
        }
    }

    impl Recorder {
        fn last(&self) -> Option<Vec<String>> {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .last()
                .cloned()
        }
    }

    fn batch(edits: Vec<Edit>) -> Call {
        Call::Batch(Batch {
            edits,
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        })
    }

    fn switch_to(scene: &str) -> Action {
        Action::SceneSwitch {
            scene_id: SceneId::new(scene),
            preset_id: None,
        }
    }

    async fn program(control: &impl Control) -> Option<SceneId> {
        let response = control
            .call(Call::Query {
                query: Query::StudioGet,
            })
            .await
            .unwrap_or_else(|e| panic!("studio.get: {e}"));
        match response.result {
            CallResult::Query {
                result: QueryResult::StudioGet { program_scene, .. },
            } => program_scene,
            other => panic!("not studio.get: {other:?}"),
        }
    }

    /// Waits until `check` holds, for up to five seconds.
    async fn eventually(mut check: impl AsyncFnMut() -> bool) {
        for _ in 0..500 {
            if check().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("not in time");
    }

    #[test]
    fn the_switch_is_on_unless_it_holds_false() {
        assert!(global_on(None));
        assert!(global_on(Some(&json!(true))));
        assert!(global_on(Some(&json!("off"))), "only `false` turns it off");
        assert!(!global_on(Some(&json!(false))));
    }

    /// The listener registers the bound chords, follows a new binding, runs a pressed
    /// chord's action, lets every chord go while the switch is off, and lets them go
    /// when it stops.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn it_hears_the_bound_chords_and_runs_their_actions() {
        let engine = Engine::new();
        let client = engine.connect();
        client
            .hello(Hello {
                protocol: PROTOCOL_VERSION,
                client_name: "test".into(),
            })
            .await
            .unwrap_or_else(|e| panic!("hello: {e}"));
        client
            .call(batch(vec![
                Edit::SceneCreate {
                    scene_id: SceneId::new("a"),
                    name: "A".into(),
                    role: None,
                },
                Edit::SceneCreate {
                    scene_id: SceneId::new("b"),
                    name: "B".into(),
                    role: None,
                },
                Edit::HotkeySet {
                    chord: Chord::new("F9"),
                    action: switch_to("b"),
                },
                // A wheel chord isn't a key: it isn't registered.
                Edit::HotkeySet {
                    chord: Chord::new("Ctrl+WheelUp"),
                    action: Action::SceneZoomCamera {
                        scene_id: SceneId::new("a"),
                        steps: 1,
                    },
                },
            ]))
            .await
            .unwrap_or_else(|e| panic!("setup: {e}"));
        assert_eq!(program(&client).await, Some(SceneId::new("a")));

        let recorder = Arc::new(Recorder::default());
        let on = Arc::new(AtomicBool::new(true));
        let (changed, changes) = watch::channel(0_u64);
        let (press, presses) = mpsc::unbounded_channel();
        let switch = Switch {
            on: Box::new({
                let on = Arc::clone(&on);
                move || on.load(Ordering::SeqCst)
            }),
            changes,
        };
        let listener = tokio::spawn(run(
            engine.connect(),
            Arc::clone(&recorder) as Arc<dyn Registrar>,
            switch,
            presses,
        ));
        eventually(async || recorder.last() == Some(vec!["F9".to_owned()])).await;

        // A binding made by another client is heard too, even one the OS refuses.
        client
            .call(batch(vec![Edit::HotkeySet {
                chord: Chord::new("Ctrl+KeyC"),
                action: switch_to("a"),
            }]))
            .await
            .unwrap_or_else(|e| panic!("bind: {e}"));
        eventually(async || recorder.last() == Some(vec!["Ctrl+KeyC".to_owned(), "F9".to_owned()]))
            .await;

        // A press runs the bound action; a chord with no binding does nothing.
        press.send("F8".into()).unwrap_or_else(|e| panic!("{e}"));
        press.send("F9".into()).unwrap_or_else(|e| panic!("{e}"));
        eventually(async || program(&client).await == Some(SceneId::new("b"))).await;

        // Off lets every chord go; on takes them back.
        on.store(false, Ordering::SeqCst);
        changed.send_modify(|n| *n += 1);
        eventually(async || recorder.last() == Some(Vec::new())).await;
        on.store(true, Ordering::SeqCst);
        changed.send_modify(|n| *n += 1);
        eventually(async || recorder.last().is_some_and(|last| last.len() == 2)).await;

        // Closing the presses ends it, and it lets every chord go.
        drop(press);
        listener
            .await
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap_or_else(|e| panic!("run: {e}"));
        assert_eq!(recorder.last(), Some(Vec::new()));
    }
}
