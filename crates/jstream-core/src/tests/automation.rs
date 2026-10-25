//! WASM automation plugins through the engine (WA1, ADR-0069, ADR-0075): installing a
//! package, a plugin's calls held to its grant, events handed to `on_event`, a plugin
//! that traps or runs past its limit disabled with `plugin.state_changed`, and re-enabled
//! only by `plugin.enable`.

use super::{block_on, connected, create, edits};
use crate::Engine;
use crate::plugins::automation::{EVENT_ENTRY, MANIFEST_FILE, MODULE_FILE};
use jstream_control::{Control, ControlError, EventStream};
use jstream_protocol::{
    Action, Call, CallResult, EntityKind, Event, PluginClass, PluginDisabledReason, PluginGrant,
    PluginId, PluginState, ProtocolError, Query, QueryResult, SceneId,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

/// How long a test waits for a plugin's thread to act on an event.
const PATIENCE: Duration = Duration::from_secs(10);

/// A fresh package directory holding `wat` as its module, and `manifest` if given.
fn package(name: &str, wat: &str, manifest: Option<&str>) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("jstream-core-plugin-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir");
    let wasm = wat::parse_str(wat).expect("valid WAT");
    fs::write(dir.join(MODULE_FILE), wasm).expect("module written");
    if let Some(manifest) = manifest {
        fs::write(dir.join(MANIFEST_FILE), manifest).expect("manifest written");
    }
    dir
}

/// A plugin whose `on_event` sends `call` once per event, and ignores the answer.
fn calling_on_event(call: &Call) -> String {
    let call = serde_json::to_string(call).expect("a call encodes");
    format!(
        r#"(module
            (import "jstream" "call" (func $call (param i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 1024) "{data}")
            (func (export "{EVENT_ENTRY}")
                (drop (call $call (i32.const 1024) (i32.const {len})))))"#,
        data = call.replace('"', "\\\""),
        len = call.len(),
    )
}

/// A plugin whose `on_event` traps.
const TRAPS: &str = r#"(module
    (memory (export "memory") 1)
    (func (export "on_event") unreachable))"#;

/// A plugin whose `on_event` never returns.
const SPINS: &str = r#"(module
    (memory (export "memory") 1)
    (func (export "on_event") (loop $forever (br $forever))))"#;

/// A plugin that runs nothing.
const IDLE: &str = r#"(module (memory (export "memory") 1))"#;

fn create_scene(id: &str) -> Call {
    Call::Batch(edits(vec![create(id)]))
}

fn grant(classes: &[PluginClass]) -> PluginGrant {
    PluginGrant {
        classes: classes.iter().copied().collect(),
        ..PluginGrant::default()
    }
}

fn install(plugin: &str, path: &Path, grant: PluginGrant) -> Call {
    Call::Action {
        action: Action::PluginInstall {
            plugin_id: PluginId::new(plugin),
            path: path.to_string_lossy().into_owned(),
            grant,
        },
    }
}

fn enable(plugin: &str) -> Call {
    Call::Action {
        action: Action::PluginEnable {
            plugin_id: PluginId::new(plugin),
        },
    }
}

fn regrant(plugin: &str, grant: PluginGrant) -> Call {
    Call::Action {
        action: Action::PluginGrant {
            plugin_id: PluginId::new(plugin),
            grant,
        },
    }
}

fn call(control: &impl Control, call: Call) -> Result<CallResult, ProtocolError> {
    block_on(control.call(call))
        .map(|response| response.result)
        .map_err(|error| match error {
            ControlError::Protocol(wire) => wire.error,
            other => panic!("the in-process engine never disconnects: {other}"),
        })
}

fn scenes(control: &impl Control) -> Vec<SceneId> {
    match call(
        control,
        Call::Query {
            query: Query::SceneList,
        },
    ) {
        Ok(CallResult::Query {
            result: QueryResult::SceneList { scenes, .. },
        }) => scenes.into_iter().map(|scene| scene.scene_id).collect(),
        other => panic!("scene.list: {other:?}"),
    }
}

/// Waits until `done` holds, or fails the test after [`PATIENCE`].
fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The next `plugin.state_changed` on `events`, waiting up to [`PATIENCE`].
fn next_state(
    events: &mut EventStream,
) -> (
    PluginId,
    PluginState,
    Option<PluginDisabledReason>,
    Option<String>,
) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        match events
            .as_mut()
            .poll_next(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(Some(Event::PluginStateChanged {
                plugin_id,
                state,
                reason,
                remote_name,
            })) => return (plugin_id, state, reason, remote_name),
            Poll::Ready(Some(_)) => {}
            Poll::Ready(None) => panic!("the event stream ended"),
            Poll::Pending => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for plugin.state_changed"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[test]
fn an_installed_plugin_gets_events_and_edits_within_its_grant() {
    let engine = Engine::new();
    let person = connected(&engine);
    let mut events = person.events();
    let dir = package(
        "edits",
        &calling_on_event(&create_scene("from_plugin")),
        Some(r#"{"name": "Scene maker", "later": true}"#),
    );
    assert_eq!(
        call(
            &person,
            install("plg_edit", &dir, grant(&[PluginClass::Edit]))
        ),
        Ok(CallResult::action())
    );
    assert_eq!(
        next_state(&mut events),
        (
            PluginId::new("plg_edit"),
            PluginState::Enabled,
            None,
            Some("Scene maker".into())
        ),
        "the manifest's name is the plugin's own text"
    );
    // Any event reaches the plugin; this one is the person's edit.
    call(&person, create_scene("trigger")).expect("the person's edit applies");
    eventually("the plugin's scene", || {
        scenes(&person).contains(&SceneId::new("from_plugin"))
    });
}

#[test]
fn a_call_outside_the_grant_is_forbidden_until_a_grant_widens_it() {
    let engine = Engine::new();
    let person = connected(&engine);
    let mut events = person.events();
    let dir = package(
        "read_only",
        &calling_on_event(&create_scene("from_plugin")),
        None,
    );
    call(&person, install("plg_read", &dir, PluginGrant::default())).expect("installs");
    assert_eq!(next_state(&mut events).1, PluginState::Enabled);
    call(&person, create_scene("trigger")).expect("the person's edit applies");
    // The plugin handled the event: its call was refused, not a fault.
    std::thread::sleep(Duration::from_millis(200));
    assert!(!scenes(&person).contains(&SceneId::new("from_plugin")));

    // The new grant applies to the running plugin's next call.
    call(&person, regrant("plg_read", grant(&[PluginClass::Edit]))).expect("regrants");
    call(&person, create_scene("again")).expect("the person's edit applies");
    eventually("the plugin's scene", || {
        scenes(&person).contains(&SceneId::new("from_plugin"))
    });
    let disabled = std::iter::from_fn(|| {
        match events
            .as_mut()
            .poll_next(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(Some(event)) => Some(event),
            _ => None,
        }
    })
    .any(|event| {
        matches!(
            event,
            Event::PluginStateChanged {
                state: PluginState::Disabled,
                ..
            }
        )
    });
    assert!(!disabled, "a forbidden call doesn't disable the plugin");
}

#[test]
fn the_plugin_session_answers_forbidden_outside_the_grant() {
    let engine = Engine::new();
    let control =
        crate::plugins::automation::tests_control(&engine, "plg_s", PluginGrant::default());
    assert_eq!(
        call(&control, create_scene("s")),
        Err(ProtocolError::Forbidden {
            call: "scene.create".into(),
            reason: "the plugin is not granted `edit`".into(),
        })
    );
    // `read` is always granted, and `plugin.*` never is.
    assert!(
        call(
            &control,
            Call::Query {
                query: Query::SceneList
            }
        )
        .is_ok()
    );
    let error = call(&control, enable("plg_s")).expect_err("never granted");
    assert_eq!(error.code(), "forbidden");
}

#[test]
fn a_plugin_that_traps_is_disabled_until_a_person_enables_it() {
    let engine = Engine::new();
    let person = connected(&engine);
    let mut events = person.events();
    let dir = package("traps", TRAPS, None);
    call(&person, install("plg_trap", &dir, PluginGrant::default())).expect("installs");
    assert_eq!(next_state(&mut events).1, PluginState::Enabled);
    call(&person, create_scene("a")).expect("applies");
    assert_eq!(
        next_state(&mut events),
        (
            PluginId::new("plg_trap"),
            PluginState::Disabled,
            Some(PluginDisabledReason::Trap),
            None
        )
    );
    // Disabled, it gets no more events, and isn't restarted by itself.
    call(&person, create_scene("b")).expect("applies");
    std::thread::sleep(Duration::from_millis(200));
    // Re-enabled, it runs again, traps on the next event, and is disabled again.
    assert_eq!(call(&person, enable("plg_trap")), Ok(CallResult::action()));
    assert_eq!(next_state(&mut events).1, PluginState::Enabled);
    call(&person, create_scene("c")).expect("applies");
    assert_eq!(next_state(&mut events).1, PluginState::Disabled);
}

#[test]
fn a_plugin_that_runs_past_its_deadline_is_disabled_as_cpu_limit() {
    let engine = Engine::new();
    let person = connected(&engine);
    let mut events = person.events();
    let dir = package("spins", SPINS, None);
    call(&person, install("plg_spin", &dir, PluginGrant::default())).expect("installs");
    assert_eq!(next_state(&mut events).1, PluginState::Enabled);
    call(&person, create_scene("a")).expect("applies");
    assert_eq!(
        next_state(&mut events).2,
        Some(PluginDisabledReason::CpuLimit)
    );
}

#[test]
fn installing_checks_the_package_the_grant_and_the_id() {
    let engine = Engine::new();
    let person = connected(&engine);
    let field = |result: Result<CallResult, ProtocolError>| match result {
        Err(ProtocolError::InvalidArgument { field, .. }) => field,
        other => panic!("not invalid_argument: {other:?}"),
    };
    let missing = std::env::temp_dir().join("jstream-core-plugin-none-here");
    assert_eq!(
        field(call(
            &person,
            install("p", &missing, PluginGrant::default())
        )),
        "path"
    );
    assert_eq!(
        field(call(
            &person,
            install("p", Path::new("relative/dir"), PluginGrant::default())
        )),
        "path"
    );
    let wasi = package(
        "wasi",
        r#"(module
            (import "wasi_snapshot_preview1" "fd_write"
                (func (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1))"#,
        None,
    );
    assert_eq!(
        field(call(&person, install("p", &wasi, PluginGrant::default()))),
        "path"
    );
    let bad_manifest = package("manifest", IDLE, Some(r#"{"name": 3}"#));
    assert_eq!(
        field(call(
            &person,
            install("p", &bad_manifest, PluginGrant::default())
        )),
        "path"
    );
    let idle = package("idle", IDLE, None);
    let wide = PluginGrant {
        hosts: vec!["https://example.com".into()],
        ..PluginGrant::default()
    };
    assert_eq!(
        field(call(&person, install("p", &idle, wide))),
        "grant.hosts.0"
    );

    call(&person, install("p", &idle, PluginGrant::default())).expect("installs");
    assert_eq!(
        call(&person, install("p", &idle, PluginGrant::default())),
        Err(ProtocolError::AlreadyExists {
            entity: EntityKind::Plugin,
            id: "p".into(),
        })
    );
    // Enabling a running plugin is fine and changes nothing.
    assert_eq!(call(&person, enable("p")), Ok(CallResult::action()));
    let unknown = ProtocolError::NotFound {
        entity: EntityKind::Plugin,
        id: "q".into(),
    };
    assert_eq!(call(&person, enable("q")), Err(unknown.clone()));
    assert_eq!(
        call(&person, regrant("q", PluginGrant::default())),
        Err(unknown)
    );
}

#[test]
fn a_failed_install_leaves_no_plugin() {
    let engine = Engine::new();
    let person = connected(&engine);
    // Its `start` function traps, so it never loads.
    let dir = package(
        "start_traps",
        r#"(module
            (memory (export "memory") 1)
            (func $start unreachable)
            (start $start))"#,
        None,
    );
    match call(&person, install("p", &dir, PluginGrant::default())) {
        Err(ProtocolError::ActionFailed { .. }) => {}
        other => panic!("not action_failed: {other:?}"),
    }
    assert_eq!(
        call(&person, enable("p")),
        Err(ProtocolError::NotFound {
            entity: EntityKind::Plugin,
            id: "p".into(),
        })
    );
}

#[test]
fn installed_plugins_dont_keep_the_engine_alive() {
    let engine = Engine::new();
    let person = connected(&engine);
    let edits = package("alive", &calling_on_event(&create_scene("x")), None);
    call(&person, install("p", &edits, grant(&[PluginClass::Edit]))).expect("installs");
    let idle = package("alive_idle", IDLE, None);
    call(&person, install("q", &idle, PluginGrant::default())).expect("installs");
    let weak = engine.downgrade();
    drop(person);
    drop(engine);
    eventually("the engine to end", || weak.upgrade().is_none());
}
