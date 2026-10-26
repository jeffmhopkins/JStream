//! WASM automation plugins through the engine (WA1, ADR-0069, ADR-0075): installing a
//! package, a plugin's calls held to its grant, events handed to `on_event`, a plugin
//! that traps or runs past its limit disabled with `plugin.state_changed`, and re-enabled
//! only by `plugin.enable`; grants held to what the manifest asks for, and the installed
//! plugins saved in the plugin folder and loaded again (ADR-0076).

use super::{block_on, connected, create, edits};
use crate::Engine;
use crate::plugins::automation::{EVENT_ENTRY, MANIFEST_FILE, MODULE_FILE, PLUGINS_FILE};
use jstream_control::{Control, ControlError, EventStream};
use jstream_protocol::{
    Action, Call, CallResult, EntityKind, Event, PluginClass, PluginDisabledReason, PluginGrant,
    PluginId, PluginState, ProtocolError, Query, QueryResult, SceneId,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// A plugin whose `on_event` grows its memory past the default cap of 64 MiB.
const GROWS: &str = r#"(module
    (memory (export "memory") 1)
    (func (export "on_event") (drop (memory.grow (i32.const 1024)))))"#;

/// A manifest asking for `edit`.
const ASKS_EDIT: &str = r#"{"classes": ["edit"]}"#;

/// A fresh, empty plugin folder.
fn plugin_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "jstream-core-plugin-dir-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    dir
}

/// An engine with the plugin folder `dir` open.
fn opened(dir: &Path) -> Engine {
    let engine = Engine::new();
    engine.open_plugin_dir(dir);
    engine
}

/// A module whose `on_event` makes each of `calls` in order, ignoring their results.
fn calling_each_on_event(calls: &[Call]) -> String {
    let mut data = String::new();
    let mut body = String::new();
    for (i, call) in calls.iter().enumerate() {
        let call = serde_json::to_string(call).expect("a call encodes");
        let at = 1024 + i * 4096;
        data.push_str(&format!(
            r#"(data (i32.const {at}) "{}")"#,
            call.replace('"', "\\\"")
        ));
        body.push_str(&format!(
            "(drop (call $call (i32.const {at}) (i32.const {})))",
            call.len()
        ));
    }
    format!(
        r#"(module
            (import "jstream" "call" (func $call (param i32 i32) (result i32)))
            (memory (export "memory") 2)
            {data}
            (func (export "{EVENT_ENTRY}") {body}))"#
    )
}

/// The plugin folder's saved plugins, as JSON.
fn saved(dir: &Path) -> serde_json::Value {
    let bytes = fs::read(dir.join(PLUGINS_FILE)).expect("the plugins file");
    serde_json::from_slice(&bytes).expect("JSON")
}

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
        Some(r#"{"name": "Scene maker", "classes": ["edit"], "later": true}"#),
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
        Some(ASKS_EDIT),
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

/// #839 review B1: events that arrive while the plugin loads are held until its entry
/// is `Running`, so one that traps on the first of them is disabled after it is enabled,
/// and `plugin.enable` can load it again, instead of being left `Running` with no thread.
#[test]
fn a_plugin_that_traps_on_an_event_during_its_install_is_disabled() {
    let engine = Engine::new();
    let person = connected(&engine);
    let mut events = person.events();
    let dir = package("traps_early", TRAPS, None);
    // Another session edits throughout the install, so events are queued for the plugin
    // from the moment its subscription opens.
    let busy = Arc::new(AtomicBool::new(true));
    // Stops the editor however the test ends, so a timed-out wait doesn't leave it running.
    struct Stop(Arc<AtomicBool>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Relaxed);
        }
    }
    let stop = Stop(busy.clone());
    let editor = {
        let engine = engine.clone();
        std::thread::spawn(move || {
            let other = connected(&engine);
            let mut n = 0u32;
            while busy.load(Ordering::Relaxed) {
                let _ = call(&other, create_scene(&format!("s{n}")));
                n += 1;
            }
        })
    };
    std::thread::sleep(Duration::from_millis(20));
    // Events reach the plugin's queue while `start` waits, so it faults on one at once
    // if it is handed one before its entry is `Running`.
    engine
        .automation()
        .running_delay_ms
        .store(200, Ordering::Relaxed);
    let installed = call(&person, install("plg_early", &dir, PluginGrant::default()));
    engine
        .automation()
        .running_delay_ms
        .store(0, Ordering::Relaxed);
    assert_eq!(installed, Ok(CallResult::action()));
    let enabled = next_state(&mut events);
    let disabled = next_state(&mut events);
    drop(stop);
    editor.join().expect("the editor ends");
    assert_eq!(enabled.1, PluginState::Enabled, "enabled first");
    assert_eq!(
        (disabled.1, disabled.2),
        (PluginState::Disabled, Some(PluginDisabledReason::Trap)),
        "then disabled by its first event"
    );
    assert_eq!(call(&person, enable("plg_early")), Ok(CallResult::action()));
    assert_eq!(
        next_state(&mut events).1,
        PluginState::Enabled,
        "a person can load it again"
    );
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
    let edits = package(
        "alive",
        &calling_on_event(&create_scene("x")),
        Some(ASKS_EDIT),
    );
    call(&person, install("p", &edits, grant(&[PluginClass::Edit]))).expect("installs");
    let idle = package("alive_idle", IDLE, None);
    call(&person, install("q", &idle, PluginGrant::default())).expect("installs");
    let weak = engine.downgrade();
    drop(person);
    drop(engine);
    eventually("the engine to end", || weak.upgrade().is_none());
}

#[test]
fn a_plugin_that_grows_past_its_memory_cap_is_disabled_as_memory_limit() {
    let engine = Engine::new();
    let person = connected(&engine);
    let mut events = person.events();
    let dir = package("grows", GROWS, None);
    call(&person, install("plg_grow", &dir, PluginGrant::default())).expect("installs");
    assert_eq!(next_state(&mut events).1, PluginState::Enabled);
    call(&person, create_scene("trigger")).expect("the person's edit applies");
    let (_, state, reason, _) = next_state(&mut events);
    assert_eq!(
        (state, reason),
        (
            PluginState::Disabled,
            Some(PluginDisabledReason::MemoryLimit)
        )
    );
}

#[test]
fn a_grant_never_gives_more_than_the_manifest_asks_for() {
    let engine = Engine::new();
    let person = connected(&engine);
    let field = |result: Result<CallResult, ProtocolError>| match result {
        Err(ProtocolError::InvalidArgument { field, .. }) => field,
        other => panic!("not invalid_argument: {other:?}"),
    };
    let asks = package(
        "asks",
        IDLE,
        Some(r#"{"classes": ["edit"], "hosts": ["https://api.example.com:443"]}"#),
    );
    let host = |host: &str| PluginGrant {
        hosts: vec![host.into()],
        ..PluginGrant::default()
    };
    assert_eq!(
        field(call(
            &person,
            install("p", &asks, grant(&[PluginClass::Live]))
        )),
        "grant.classes.0"
    );
    assert_eq!(
        field(call(
            &person,
            install("p", &asks, host("https://other.example.com:443"))
        )),
        "grant.hosts.0"
    );
    let folder = PluginGrant {
        folders: vec![std::env::temp_dir().to_string_lossy().into_owned()],
        ..PluginGrant::default()
    };
    assert_eq!(
        field(call(&person, install("p", &asks, folder.clone()))),
        "grant.folders.0"
    );
    // Less than it asks for is fine, and so is `read`, which every plugin has.
    call(&person, install("p", &asks, grant(&[PluginClass::Read]))).expect("installs");
    assert_eq!(
        field(call(&person, regrant("p", grant(&[PluginClass::Outputs])))),
        "grant.classes.0"
    );
    let all = PluginGrant {
        hosts: vec!["https://api.example.com:443".into()],
        ..grant(&[PluginClass::Edit])
    };
    assert_eq!(call(&person, regrant("p", all)), Ok(CallResult::action()));
    let folders = package("folders", IDLE, Some(r#"{"folders": true}"#));
    call(&person, install("f", &folders, folder)).expect("a manifest may ask for folders");

    // A manifest that asks for something it can't have is the package's fault.
    for (name, manifest) in [
        ("unknown_class", r#"{"classes": ["everything"]}"#),
        ("bad_host", r#"{"hosts": ["example.com"]}"#),
        ("bad_folders", r#"{"folders": "yes"}"#),
    ] {
        let dir = package(name, IDLE, Some(manifest));
        assert_eq!(
            field(call(&person, install("q", &dir, PluginGrant::default()))),
            "path",
            "{manifest}"
        );
    }
}

#[test]
fn installed_plugins_are_saved_and_loaded_again() {
    let dir = plugin_dir("saved");
    let package_dir = package(
        "saved_edits",
        &calling_on_event(&create_scene("from_plugin")),
        Some(r#"{"name": "Saver", "classes": ["edit"]}"#),
    );
    let traps = package("saved_traps", TRAPS, None);
    {
        let engine = opened(&dir);
        let person = connected(&engine);
        let mut events = person.events();
        call(
            &person,
            install("plg_a", &package_dir, PluginGrant::default()),
        )
        .expect("installs");
        call(&person, regrant("plg_a", grant(&[PluginClass::Edit]))).expect("regrants");
        call(&person, install("plg_t", &traps, PluginGrant::default())).expect("installs");
        let _ = next_state(&mut events);
        let _ = next_state(&mut events);
        call(&person, create_scene("trigger")).expect("the person's edit applies");
        eventually("the trapping plugin to be disabled", || {
            saved(&dir)["plugins"]
                .as_array()
                .is_some_and(|plugins| plugins.iter().any(|p| p["disabled"] == "trap"))
        });
    }
    let file = saved(&dir);
    assert_eq!(file["version"], 0);
    let plugins = file["plugins"].as_array().expect("plugins");
    assert_eq!(plugins.len(), 2);
    assert_eq!(plugins[0]["plugin_id"], "plg_a");
    assert_eq!(plugins[0]["grant"]["classes"], serde_json::json!(["edit"]));
    assert_eq!(plugins[0]["name"], "Saver");
    assert!(plugins[0].get("disabled").is_none(), "{file}");
    assert_eq!(plugins[1]["disabled"], "trap");

    // A fixed build replaces the trapping module, so once a person enables it nothing
    // disables it again: `IDLE` has no `on_event` and gets no events. With `TRAPS` still
    // in place, the first event after the enable traps it, and that later save of
    // `disabled = "trap"` races the read below (#880).
    fs::write(
        traps.join(MODULE_FILE),
        wat::parse_str(IDLE).expect("valid WAT"),
    )
    .expect("module written");

    // The next run loads the enabled one with its grant, and keeps the other disabled.
    let engine = opened(&dir);
    let person = connected(&engine);
    assert_eq!(
        saved(&dir)["plugins"][1]["disabled"],
        "trap",
        "a disabled plugin stays disabled across a reopen"
    );
    call(&person, create_scene("again")).expect("the person's edit applies");
    eventually("the restored plugin's scene", || {
        scenes(&person).contains(&SceneId::new("from_plugin"))
    });
    let mut events = person.events();
    assert_eq!(call(&person, enable("plg_t")), Ok(CallResult::action()));
    assert_eq!(next_state(&mut events).1, PluginState::Enabled);
    assert!(
        saved(&dir)["plugins"][1].get("disabled").is_none(),
        "a person's enable is saved"
    );
    assert_eq!(
        call(
            &person,
            install("plg_a", &package_dir, PluginGrant::default())
        ),
        Err(ProtocolError::AlreadyExists {
            entity: EntityKind::Plugin,
            id: "plg_a".into(),
        })
    );
}

#[test]
fn an_update_that_asks_for_more_needs_a_grant() {
    let dir = plugin_dir("needs_grant");
    let package_dir = package(
        "needs_grant",
        &calling_on_event(&create_scene("from_plugin")),
        None,
    );
    {
        let engine = opened(&dir);
        let person = connected(&engine);
        call(
            &person,
            install("plg_u", &package_dir, PluginGrant::default()),
        )
        .expect("installs");
    }
    // The update asks for `edit`, which nobody granted.
    fs::write(package_dir.join(MANIFEST_FILE), ASKS_EDIT).expect("manifest written");
    let engine = opened(&dir);
    let person = connected(&engine);
    assert_eq!(saved(&dir)["plugins"][0]["disabled"], "needs_grant");
    match call(&person, enable("plg_u")) {
        Err(ProtocolError::ActionFailed { reason }) => {
            assert!(reason.contains("`edit`"), "{reason}");
        }
        other => panic!("not action_failed: {other:?}"),
    }
    // A person's grant, even of nothing more, acknowledges what it asks for now.
    call(&person, regrant("plg_u", grant(&[PluginClass::Edit]))).expect("regrants");
    let mut events = person.events();
    assert_eq!(call(&person, enable("plg_u")), Ok(CallResult::action()));
    assert_eq!(next_state(&mut events).1, PluginState::Enabled);
    call(&person, create_scene("trigger")).expect("the person's edit applies");
    eventually("the updated plugin's scene", || {
        scenes(&person).contains(&SceneId::new("from_plugin"))
    });
}

#[test]
fn an_update_that_asks_for_less_narrows_its_grant() {
    let dir = plugin_dir("narrows");
    let package_dir = package("narrows", IDLE, Some(ASKS_EDIT));
    {
        let engine = opened(&dir);
        let person = connected(&engine);
        call(
            &person,
            install("plg_n", &package_dir, grant(&[PluginClass::Edit])),
        )
        .expect("installs");
    }
    fs::write(package_dir.join(MANIFEST_FILE), "{}").expect("manifest written");
    let _engine = opened(&dir);
    let plugin = &saved(&dir)["plugins"][0];
    assert!(plugin.get("disabled").is_none(), "{plugin}");
    assert!(plugin["grant"].get("classes").is_none(), "{plugin}");
}

#[test]
fn a_plugins_file_it_cant_read_is_left_alone() {
    let dir = plugin_dir("unreadable");
    fs::create_dir_all(&dir).expect("plugin dir");
    let newer = r#"{"version": 1, "plugins": []}"#;
    fs::write(dir.join(PLUGINS_FILE), newer).expect("file written");
    let engine = opened(&dir);
    let person = connected(&engine);
    let idle = package("unreadable_idle", IDLE, None);
    call(&person, install("p", &idle, PluginGrant::default())).expect("installs");
    assert_eq!(
        fs::read_to_string(dir.join(PLUGINS_FILE)).expect("file"),
        newer,
        "nothing is saved over a file this version doesn't read"
    );
}

#[test]
fn a_saved_plugin_that_no_longer_loads_stays_disabled_as_trap() {
    let dir = plugin_dir("no_longer_loads");
    let package_dir = package("no_longer_loads", IDLE, None);
    {
        let engine = opened(&dir);
        let person = connected(&engine);
        call(
            &person,
            install("plg_b", &package_dir, PluginGrant::default()),
        )
        .expect("installs");
    }
    fs::write(package_dir.join(MODULE_FILE), b"not a module").expect("module written");
    let engine = opened(&dir);
    assert_eq!(saved(&dir)["plugins"][0]["disabled"], "trap");
    let person = connected(&engine);
    assert!(matches!(
        call(&person, enable("plg_b")),
        Err(ProtocolError::ActionFailed { .. })
    ));
}

#[test]
fn a_saved_grant_a_person_couldnt_give_needs_a_new_grant() {
    let dir = plugin_dir("bad_saved_grant");
    let package_dir = package(
        "bad_saved_grant",
        IDLE,
        Some(r#"{"classes": ["edit"], "folders": true}"#),
    );
    {
        let engine = opened(&dir);
        let person = connected(&engine);
        call(
            &person,
            install("plg_g", &package_dir, grant(&[PluginClass::Edit])),
        )
        .expect("installs");
    }
    let mut file = saved(&dir);
    file["plugins"][0]["grant"]["folders"] = serde_json::json!(["relative/folder"]);
    fs::write(dir.join(PLUGINS_FILE), file.to_string()).expect("file written");
    let _engine = opened(&dir);
    let plugin = &saved(&dir)["plugins"][0];
    assert_eq!(plugin["disabled"], "needs_grant", "{plugin}");
    assert!(plugin["grant"].get("folders").is_none(), "{plugin}");
}

/// A saved plugin acts on the engine as built, with its backends: the daemon opens the
/// plugin folder last (#843 review B1). Without a publish target its `output.start`
/// fails as a person's does, and the output stays stopped.
#[test]
fn a_saved_plugin_acts_through_the_engines_backends() {
    use jstream_media::Timebase;
    use jstream_platform::fake::{self, FakeClock};
    use jstream_protocol::{OutputId, OutputState};

    let dir = plugin_dir("backends");
    let package_dir = package(
        "backends",
        &calling_each_on_event(&[
            super::output_call("out_1", true),
            create_scene("after_start"),
        ]),
        Some(r#"{"classes": ["edit", "outputs"]}"#),
    );
    {
        let engine = opened(&dir);
        let person = connected(&engine);
        call(
            &person,
            install(
                "plg_o",
                &package_dir,
                grant(&[PluginClass::Edit, PluginClass::Outputs]),
            ),
        )
        .expect("installs");
    }
    let clock = FakeClock::new(Timebase::MICROS);
    let engine = Engine::new()
        .with_output(OutputId::new("out_1"))
        .with_backends(fake::backends(&clock).expect("fake backends"));
    engine.open_plugin_dir(&dir);
    let person = connected(&engine);
    call(&person, create_scene("trigger")).expect("the person's edit applies");
    eventually("the plugin's scene after its start", || {
        scenes(&person).contains(&SceneId::new("after_start"))
    });
    match call(
        &person,
        Call::Query {
            query: Query::OutputStatus {
                output_id: OutputId::new("out_1"),
            },
        },
    ) {
        Ok(CallResult::Query {
            result: QueryResult::OutputStatus { state, .. },
        }) => assert_eq!(state, OutputState::Stopped),
        other => panic!("output.status: {other:?}"),
    }
}

#[test]
fn a_second_plugin_folder_is_ignored() {
    let first = plugin_dir("first_folder");
    let second = plugin_dir("second_folder");
    let engine = opened(&first);
    engine.open_plugin_dir(&second);
    assert!(first.join(PLUGINS_FILE).exists());
    assert!(!second.join(PLUGINS_FILE).exists());
}
