//! Plugins written in the WASM text format, run against a fake `Control` that records
//! what reaches it: fuel exhaustion, the wall-time deadline, the memory limit, a denied
//! capability and an allowed call.

use jstream_control::protocol::{Call, CallResult, Event, Hello, RequestId, Response, Welcome};
use jstream_control::{BoxFuture, Control, ControlError, EventStream};
use jstream_plugin_wasm::{Capabilities, Error, Limits, Plugin, PluginConfig, PluginHost, abi};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A `Control` that answers every call with an empty action result, after `delay`, and
/// records it.
#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<Call>>,
    delay: std::time::Duration,
}

impl Fake {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("not poisoned").clone()
    }
}

impl Control for Fake {
    fn hello(&self, _: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
        Box::pin(async {
            Err(ControlError::Disconnected {
                reason: "plugins never send hello".into(),
            })
        })
    }

    fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
        self.calls.lock().expect("not poisoned").push(call);
        std::thread::sleep(self.delay);
        Box::pin(async {
            Ok(Response {
                id: RequestId::from("r1"),
                result: CallResult::action(),
            })
        })
    }

    fn events(&self) -> EventStream {
        Box::pin(futures_none())
    }
}

/// An event stream that ends at once.
fn futures_none() -> impl futures_core::Stream<Item = Event> + Send {
    struct Empty;
    impl futures_core::Stream for Empty {
        type Item = Event;
        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Event>> {
            std::task::Poll::Ready(None)
        }
    }
    Empty
}

fn load(wat: &str, config: PluginConfig) -> (Result<Plugin, Error>, Arc<Fake>) {
    load_with(wat, config, Fake::default())
}

fn load_with(wat: &str, config: PluginConfig, fake: Fake) -> (Result<Plugin, Error>, Arc<Fake>) {
    let fake = Arc::new(fake);
    let host = PluginHost::new().expect("the engine configures");
    let control: Arc<dyn Control> = fake.clone();
    let wasm = wat::parse_str(wat).expect("valid WAT");
    (Plugin::load(&host, wasm, config, control), fake)
}

/// A plugin whose `run` sends `call` and stores the status at address 0 and the result
/// (its length at 4, its bytes from 64) for the test to check through `report`.
fn calling_plugin(call: &str) -> String {
    format!(
        r#"(module
            (import "jstream" "call" (func $call (param i32 i32) (result i32)))
            (import "jstream" "result_len" (func $result_len (result i32)))
            (import "jstream" "result_read" (func $result_read (param i32)))
            (import "jstream" "input_len" (func $input_len (result i32)))
            (import "jstream" "input_read" (func $input_read (param i32)))
            (memory (export "memory") 1)
            (data (i32.const 1024) "{data}")
            ;; Fails (traps) unless the status is the one passed as input.
            (func (export "run")
                (local $status i32)
                (local.set $status (call $call (i32.const 1024) (i32.const {len})))
                (call $input_read (i32.const 0))
                (if (i32.ne (local.get $status) (i32.load8_u (i32.const 0)))
                    (then unreachable))
                (call $result_read (i32.const 64))))"#,
        data = call.replace('"', "\\\""),
        len = call.len(),
    )
}

#[test]
fn a_granted_call_reaches_control() {
    let call = r#"{"kind":"action","action":{"type":"output.stop","output_id":"main"}}"#;
    let config = PluginConfig {
        capabilities: Capabilities::from_names(["output.stop"]),
        ..PluginConfig::default()
    };
    let (plugin, fake) = load(&calling_plugin(call), config);
    let mut plugin = plugin.expect("the plugin loads");
    let status = u8::try_from(abi::STATUS_OK).expect("fits");
    plugin.invoke("run", &[status]).expect("the call succeeds");
    let sent = fake.calls();
    assert_eq!(sent.len(), 1);
    let expected: Call = serde_json::from_str(call).expect("a valid call");
    assert_eq!(sent[0], expected);
}

#[test]
fn a_denied_call_never_reaches_control() {
    // Granted `output.stop`, but the plugin tries to start the stream.
    let call = r#"{"kind":"action","action":{"type":"output.start","output_id":"main"}}"#;
    let config = PluginConfig {
        capabilities: Capabilities::from_names(["output.stop"]),
        ..PluginConfig::default()
    };
    let (plugin, fake) = load(&calling_plugin(call), config);
    let mut plugin = plugin.expect("the plugin loads");
    let status = u8::try_from(abi::STATUS_DENIED).expect("fits");
    plugin
        .invoke("run", &[status])
        .expect("the plugin sees the denial and returns");
    assert!(fake.calls().is_empty());
}

#[test]
fn a_malformed_call_is_reported_to_the_plugin() {
    let (plugin, fake) = load(&calling_plugin("not json"), PluginConfig::default());
    let mut plugin = plugin.expect("the plugin loads");
    let status = u8::try_from(abi::STATUS_MALFORMED).expect("fits");
    plugin.invoke("run", &[status]).expect("the plugin returns");
    assert!(fake.calls().is_empty());
}

#[test]
fn a_status_the_plugin_did_not_expect_traps_it() {
    // The harness itself: a wrong expectation is a trap, not a pass.
    let call = r#"{"kind":"query","query":{"type":"scene.list"}}"#;
    let (plugin, _) = load(&calling_plugin(call), PluginConfig::default());
    let mut plugin = plugin.expect("the plugin loads");
    let status = u8::try_from(abi::STATUS_OK).expect("fits");
    assert!(matches!(
        plugin.invoke("run", &[status]),
        Err(Error::Trap(_))
    ));
}

#[test]
fn a_plugin_that_runs_out_of_fuel_is_stopped() {
    let wat = r#"(module
        (memory (export "memory") 1)
        (func (export "spin") (loop $forever (br $forever))))"#;
    let config = PluginConfig {
        limits: Limits::default().with_fuel(100_000),
        ..PluginConfig::default()
    };
    let (plugin, _) = load(wat, config);
    let mut plugin = plugin.expect("the plugin loads");
    assert!(matches!(plugin.invoke("spin", &[]), Err(Error::OutOfFuel)));
    assert_eq!(plugin.fuel_left(), 0);
    // The next invocation gets a fresh budget, and runs out again.
    assert!(matches!(plugin.invoke("spin", &[]), Err(Error::OutOfFuel)));
}

#[test]
fn host_calls_are_charged_by_size() {
    // A plugin with no grants loops on a 1 MiB call that is all whitespace: malformed,
    // so no grant is consulted, but each one makes the host copy and parse 1 MiB. The
    // host charges those bytes to the plugin's fuel, so a small budget ends it after a
    // few calls instead of letting it run on host time.
    let wat = format!(
        r#"(module
            (import "jstream" "call" (func $call (param i32 i32) (result i32)))
            (memory (export "memory") 17)
            (data (i32.const 0) "{spaces}")
            (func (export "flood")
                (loop $again
                    (drop (call $call (i32.const 0) (i32.const {len})))
                    (br $again))))"#,
        spaces = " ".repeat(abi::MAX_CALL_BYTES as usize),
        len = abi::MAX_CALL_BYTES,
    );
    let config = PluginConfig {
        limits: Limits::default().with_fuel(5 * u64::from(abi::MAX_CALL_BYTES)),
        ..PluginConfig::default()
    };
    let (plugin, fake) = load(&wat, config);
    let mut plugin = plugin.expect("the plugin loads");
    let started = std::time::Instant::now();
    assert!(matches!(plugin.invoke("flood", &[]), Err(Error::OutOfFuel)));
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(fake.calls().is_empty());
}

#[test]
fn the_default_fuel_stops_an_endless_loop() {
    let wat = r#"(module
        (memory (export "memory") 1)
        (func (export "spin") (loop $forever (br $forever))))"#;
    let (plugin, _) = load(wat, PluginConfig::default());
    let mut plugin = plugin.expect("the plugin loads");
    assert!(matches!(plugin.invoke("spin", &[]), Err(Error::OutOfFuel)));
}

#[test]
fn a_start_function_that_spins_fails_the_load() {
    let wat = r#"(module
        (memory (export "memory") 1)
        (func $start (loop $forever (br $forever)))
        (start $start))"#;
    let (plugin, _) = load(wat, PluginConfig::default());
    assert!(matches!(plugin, Err(Error::OutOfFuel)));
}

#[test]
fn memory_cannot_grow_past_the_limit() {
    // Starts with one page, may grow to four; `grow` reports what `memory.grow` answered.
    let wat = r#"(module
        (memory (export "memory") 1)
        (func (export "grow_ok")
            (if (i32.eq (memory.grow (i32.const 3)) (i32.const -1)) (then unreachable)))
        (func (export "grow_too_far")
            (if (i32.ne (memory.grow (i32.const 1)) (i32.const -1)) (then unreachable))))"#;
    let config = PluginConfig {
        limits: Limits::default().with_memory_bytes(4 * 65_536),
        ..PluginConfig::default()
    };
    let (plugin, _) = load(wat, config);
    let mut plugin = plugin.expect("the plugin loads");
    plugin
        .invoke("grow_ok", &[])
        .expect("growing to the limit works");
    plugin
        .invoke("grow_too_far", &[])
        .expect("growing past the limit fails with -1");
}

#[test]
fn the_default_memory_limit_holds() {
    let pages = Limits::DEFAULT_MEMORY_BYTES / 65_536;
    let wat = format!(
        r#"(module
            (memory (export "memory") 1)
            (func (export "grow_too_far")
                (if (i32.ne (memory.grow (i32.const {pages})) (i32.const -1))
                    (then unreachable))))"#
    );
    let (plugin, _) = load(&wat, PluginConfig::default());
    let mut plugin = plugin.expect("the plugin loads");
    plugin
        .invoke("grow_too_far", &[])
        .expect("one page more than the limit is refused");
}

#[test]
fn a_memory_that_starts_above_the_limit_is_refused() {
    let wat = r#"(module (memory (export "memory") 8))"#;
    let config = PluginConfig {
        limits: Limits::default().with_memory_bytes(4 * 65_536),
        ..PluginConfig::default()
    };
    let (plugin, _) = load(wat, config);
    assert!(matches!(plugin, Err(Error::Instantiate(_))));
}

#[test]
fn a_plugin_must_export_its_memory() {
    let (plugin, _) = load("(module)", PluginConfig::default());
    assert!(matches!(plugin, Err(Error::NoMemory)));
}

#[test]
fn reading_a_result_outside_memory_traps() {
    let wat = r#"(module
        (import "jstream" "call" (func $call (param i32 i32) (result i32)))
        (import "jstream" "result_read" (func $result_read (param i32)))
        (memory (export "memory") 1)
        (func (export "run")
            (drop (call $call (i32.const 0) (i32.const 4)))
            (call $result_read (i32.const 65535))))"#;
    let (plugin, _) = load(wat, PluginConfig::default());
    let mut plugin = plugin.expect("the plugin loads");
    assert!(matches!(plugin.invoke("run", &[]), Err(Error::Trap(_))));
}

#[test]
fn a_missing_entry_point_is_named() {
    let (plugin, _) = load(
        r#"(module (memory (export "memory") 1))"#,
        PluginConfig::default(),
    );
    let mut plugin = plugin.expect("the plugin loads");
    match plugin.invoke("on_event", &[]) {
        Err(Error::NoEntryPoint(name)) => assert_eq!(name, "on_event"),
        other => panic!("expected NoEntryPoint, got {other:?}"),
    }
}

#[test]
fn time_the_engine_spends_answering_is_not_the_plugins() {
    // Each call takes 60 ms to answer against a 30 ms deadline. The deadline is re-armed
    // when a call returns, so the loop after it, whose back edges check the deadline,
    // still runs to the end.
    let call = r#"{"kind":"action","action":{"type":"output.stop","output_id":"main"}}"#;
    let wat = format!(
        r#"(module
            (import "jstream" "call" (func $call (param i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 1024) "{data}")
            (func (export "run")
                (local $n i32)
                (drop (call $call (i32.const 1024) (i32.const {len})))
                (drop (call $call (i32.const 1024) (i32.const {len})))
                (loop $again
                    (local.set $n (i32.add (local.get $n) (i32.const 1)))
                    (br_if $again (i32.lt_u (local.get $n) (i32.const 1000))))))"#,
        data = call.replace('"', "\\\""),
        len = call.len(),
    );
    let config = PluginConfig {
        capabilities: Capabilities::from_names(["output.stop"]),
        limits: Limits::default().with_call_deadline(Duration::from_millis(30)),
    };
    let slow = Fake {
        delay: Duration::from_millis(60),
        ..Fake::default()
    };
    let (plugin, fake) = load_with(&wat, config, slow);
    let mut plugin = plugin.expect("the plugin loads");
    plugin
        .invoke("run", &[])
        .expect("the engine's time isn't charged");
    assert_eq!(fake.calls().len(), 2);
}

/// A plugin whose `run` sends `call` as many times as the input's first byte, and traps
/// unless the last status is the input's second byte.
fn repeating_plugin(call: &str) -> String {
    format!(
        r#"(module
            (import "jstream" "call" (func $call (param i32 i32) (result i32)))
            (import "jstream" "input_read" (func $input_read (param i32)))
            (memory (export "memory") 1)
            (data (i32.const 1024) "{data}")
            (func (export "run")
                (local $n i32)
                (local $status i32)
                (call $input_read (i32.const 0))
                (block $done
                    (loop $again
                        (br_if $done (i32.ge_u (local.get $n) (i32.load8_u (i32.const 0))))
                        (local.set $status (call $call (i32.const 1024) (i32.const {len})))
                        (local.set $n (i32.add (local.get $n) (i32.const 1)))
                        (br $again)))
                (if (i32.ne (local.get $status) (i32.load8_u (i32.const 1)))
                    (then unreachable))))"#,
        data = call.replace('"', "\\\""),
        len = call.len(),
    )
}

const STOP: &str = r#"{"kind":"action","action":{"type":"output.stop","output_id":"main"}}"#;

fn stop_granted(limits: Limits) -> PluginConfig {
    PluginConfig {
        capabilities: Capabilities::from_names(["output.stop"]),
        limits,
    }
}

#[test]
fn calls_past_the_cap_a_second_never_reach_control() {
    // ADR-0069: 50 calls a second by default. Sixty in one invocation, well inside a
    // second: the first fifty reach `Control`, the last ten are limited, and the plugin
    // isn't stopped.
    assert_eq!(Limits::DEFAULT_CALLS_PER_SECOND, 50);
    let (plugin, fake) = load(&repeating_plugin(STOP), stop_granted(Limits::default()));
    let mut plugin = plugin.expect("the plugin loads");
    let limited = u8::try_from(abi::STATUS_LIMITED).expect("fits");
    plugin
        .invoke("run", &[60, limited])
        .expect("a limited call doesn't stop the plugin");
    assert_eq!(fake.calls().len(), 50);
}

#[test]
fn a_lowered_cap_holds_across_invocations_and_frees_after_a_second() {
    let limits = Limits::default().with_calls_per_second(3);
    let (plugin, fake) = load(&repeating_plugin(STOP), stop_granted(limits));
    let mut plugin = plugin.expect("the plugin loads");
    let ok = u8::try_from(abi::STATUS_OK).expect("fits");
    let limited = u8::try_from(abi::STATUS_LIMITED).expect("fits");
    plugin.invoke("run", &[3, ok]).expect("three calls fit");
    plugin
        .invoke("run", &[1, limited])
        .expect("a fourth within the second is limited");
    assert_eq!(fake.calls().len(), 3);
    std::thread::sleep(Duration::from_millis(1_050));
    plugin
        .invoke("run", &[1, ok])
        .expect("a second later the cap has room again");
    assert_eq!(fake.calls().len(), 4);
}

#[test]
fn a_denied_call_does_not_use_the_cap() {
    // Denied calls never reach `Control`, so they don't count: with a cap of three, three
    // denied calls leave room for three granted ones.
    let start = r#"{"kind":"action","action":{"type":"output.start","output_id":"main"}}"#;
    let limits = Limits::default().with_calls_per_second(3);
    let both = format!(
        r#"(module
            (import "jstream" "call" (func $call (param i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 1024) "{start}")
            (data (i32.const 2048) "{stop}")
            (func $expect (param $status i32) (param $want i32)
                (if (i32.ne (local.get $status) (local.get $want)) (then unreachable)))
            (func (export "run")
                (call $expect (call $call (i32.const 1024) (i32.const {start_len})) (i32.const {denied}))
                (call $expect (call $call (i32.const 1024) (i32.const {start_len})) (i32.const {denied}))
                (call $expect (call $call (i32.const 1024) (i32.const {start_len})) (i32.const {denied}))
                (call $expect (call $call (i32.const 2048) (i32.const {stop_len})) (i32.const {ok}))
                (call $expect (call $call (i32.const 2048) (i32.const {stop_len})) (i32.const {ok}))
                (call $expect (call $call (i32.const 2048) (i32.const {stop_len})) (i32.const {ok}))
                (call $expect (call $call (i32.const 2048) (i32.const {stop_len})) (i32.const {limited}))))"#,
        start = start.replace('"', "\\\""),
        start_len = start.len(),
        stop = STOP.replace('"', "\\\""),
        stop_len = STOP.len(),
        denied = abi::STATUS_DENIED,
        ok = abi::STATUS_OK,
        limited = abi::STATUS_LIMITED,
    );
    let (plugin, fake) = load(&both, stop_granted(limits));
    let mut plugin = plugin.expect("the plugin loads");
    plugin
        .invoke("run", &[])
        .expect("denials leave the cap for granted calls");
    assert_eq!(fake.calls().len(), 3);
}
