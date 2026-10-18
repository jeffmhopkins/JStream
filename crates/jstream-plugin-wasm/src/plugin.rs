//! Loading and invoking plugins.

use crate::abi;
use crate::block_on::block_on;
use crate::capability::Capabilities;
use crate::error::Error;
use crate::limits::{self, Limits};
use crate::rate::CallRate;
use jstream_control::protocol::Call;
use jstream_control::{Control, ControlError};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use wasmtime::{
    AsContextMut, Caller, Config, Engine, Extern, Instance, Linker, Memory, Module, Store,
    StoreLimits, StoreLimitsBuilder,
};

/// The deepest the WASM stack of a plugin may grow, in bytes. Fixed for every plugin,
/// because wasmtime sets it per engine; overflowing it traps.
const MAX_WASM_STACK: usize = 512 * 1024;

/// How often the host's ticker advances the engine's epoch. A plugin's deadlines
/// ([`Limits::call_deadline`], [`Limits::load_deadline`]) are counted in these ticks.
pub const EPOCH_TICK: Duration = Duration::from_millis(10);

/// What one plugin is allowed: the commands it may call and the resources it may use.
/// The default grants nothing and uses the default [`Limits`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginConfig {
    /// The commands the plugin may call through `Control`.
    pub capabilities: Capabilities,
    /// Its fuel, memory and table limits.
    pub limits: Limits,
}

/// The shared wasmtime engine plugins are compiled and run on. Create one and load every
/// plugin with it.
#[derive(Clone)]
pub struct PluginHost {
    engine: Engine,
    ticker: Arc<Ticker>,
}

impl PluginHost {
    /// An engine that meters fuel, interrupts plugins at their deadlines and caps the WASM
    /// stack, with the thread that advances its epoch every [`EPOCH_TICK`]. Proposals that
    /// need more of the host (threads, the component model, GC) are not compiled into
    /// this crate.
    pub fn new() -> Result<Self, Error> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.epoch_interruption(true);
        config.max_wasm_stack(MAX_WASM_STACK);
        let engine = Engine::new(&config).map_err(|e| Error::Engine(e.into()))?;
        let ticker = Ticker::start(engine.clone())?;
        Ok(PluginHost {
            engine,
            ticker: Arc::new(ticker),
        })
    }
}

/// The thread that advances the engine's epoch every [`EPOCH_TICK`], so a plugin running
/// past its deadline traps. It runs while the host or any plugin loaded on it lives, and
/// stops when the last of them is dropped.
struct Ticker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Ticker {
    fn start(engine: Engine) -> Result<Ticker, Error> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::Builder::new()
            .name("jstream-plugin-epoch".into())
            .spawn(move || {
                while !stopped.load(Ordering::Relaxed) {
                    std::thread::sleep(EPOCH_TICK);
                    engine.increment_epoch();
                }
            })
            .map_err(|e| Error::Engine(e.into()))?;
        Ok(Ticker {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // The thread only sleeps and ticks, so it ends within a tick.
            let _ = thread.join();
        }
    }
}

/// What a plugin's store holds for the host functions.
struct HostState {
    control: Arc<dyn Control>,
    capabilities: Capabilities,
    limits: StoreLimits,
    /// What the last `call` answered, for `result_read`.
    result: Vec<u8>,
    /// What the host passed to the current invocation, for `input_read`.
    input: Vec<u8>,
    /// The calls sent to `Control` in the last second, against the plugin's cap.
    rate: CallRate,
    /// The deadline of one invocation, in epoch ticks.
    call_ticks: u64,
    /// The deadline of the current entry into the plugin, the load's or an
    /// invocation's, in epoch ticks: what each `call` re-arms when it returns.
    entry_ticks: u64,
}

/// One loaded plugin: its own store, memory and instance, and the session it calls
/// `Control` through.
///
/// Invoking it blocks the calling thread while the plugin runs and while each of its
/// calls is answered, so invoke it from a thread that may block (`spawn_blocking`), never
/// on an async executor's worker. After an error other than [`Error::NoEntryPoint`] the
/// plugin's state may be inconsistent; what the caller then does (disable it, report it)
/// is its policy.
pub struct Plugin {
    store: Store<HostState>,
    instance: Instance,
    fuel: u64,
    /// Keeps the epoch advancing while the plugin lives, even after its host is dropped.
    _ticker: Arc<Ticker>,
}

impl Plugin {
    /// Compiles and instantiates `wasm`, a binary core module. Every import must be one of the [`abi`] functions, so a module that asks
    /// for WASI or anything else is refused before any of its code runs. The module's
    /// `start` function, if any, runs with one invocation's fuel, within
    /// [`Limits::load_deadline`] of wall time between host calls.
    pub fn load(
        host: &PluginHost,
        wasm: impl AsRef<[u8]>,
        config: PluginConfig,
        control: Arc<dyn Control>,
    ) -> Result<Plugin, Error> {
        let module = Module::new(&host.engine, wasm).map_err(|e| Error::Compile(e.into()))?;
        for import in module.imports() {
            if import.module() != abi::MODULE || !abi::IMPORTS.contains(&import.name()) {
                return Err(Error::ForbiddenImport {
                    module: import.module().to_owned(),
                    name: import.name().to_owned(),
                });
            }
        }

        let limits = config.limits;
        let state = HostState {
            control,
            capabilities: config.capabilities,
            limits: StoreLimitsBuilder::new()
                .memory_size(limits.memory_bytes())
                .table_elements(limits.table_elements())
                .instances(1)
                .memories(1)
                .tables(1)
                .build(),
            result: Vec::new(),
            input: Vec::new(),
            rate: CallRate::new(limits.calls_per_second()),
            call_ticks: limits::ticks(limits.call_deadline(), EPOCH_TICK),
            entry_ticks: limits::ticks(limits.load_deadline(), EPOCH_TICK),
        };
        let mut store = Store::new(&host.engine, state);
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(limits.fuel())
            .map_err(|e| Error::Engine(e.into()))?;
        store.epoch_deadline_trap();
        let ticks = store.data().entry_ticks;
        store.set_epoch_deadline(ticks);

        let linker = linker(&host.engine).map_err(|e| Error::Engine(e.into()))?;
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| Error::from_run(e, Error::Instantiate))?;
        if instance.get_memory(&mut store, abi::MEMORY).is_none() {
            return Err(Error::NoMemory);
        }
        Ok(Plugin {
            store,
            instance,
            fuel: limits.fuel(),
            _ticker: host.ticker.clone(),
        })
    }

    /// Runs the exported function `entry` (type `() -> ()`) with a fresh fuel budget,
    /// within [`Limits::call_deadline`] of wall time between host calls.
    /// `input` is what the plugin reads with `input_len` and `input_read` (an event, say).
    pub fn invoke(&mut self, entry: &str, input: &[u8]) -> Result<(), Error> {
        let func = self
            .instance
            .get_typed_func::<(), ()>(&mut self.store, entry)
            .map_err(|_| Error::NoEntryPoint(entry.to_owned()))?;
        self.store
            .set_fuel(self.fuel)
            .map_err(|e| Error::Engine(e.into()))?;
        let ticks = self.store.data().call_ticks;
        self.store.set_epoch_deadline(ticks);
        let state = self.store.data_mut();
        state.entry_ticks = ticks;
        state.input.clear();
        state.input.extend_from_slice(input);
        state.result.clear();
        func.call(&mut self.store, ())
            .map_err(|e| Error::from_run(e, Error::Trap))
    }

    /// The fuel the last invocation left unused.
    pub fn fuel_left(&self) -> u64 {
        self.store.get_fuel().unwrap_or(0)
    }
}

/// The host functions, and nothing else.
fn linker(engine: &Engine) -> wasmtime::Result<Linker<HostState>> {
    let mut linker = Linker::new(engine);
    linker.func_wrap(
        abi::MODULE,
        "call",
        |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> wasmtime::Result<i32> {
            let memory = memory(&mut caller)?;
            let len = offset(len);
            // Charged before the size check, so an oversized call costs its length too.
            charge(&mut caller, len)?;
            if len > abi::MAX_CALL_BYTES as usize {
                let message = format!("a call is at most {} bytes", abi::MAX_CALL_BYTES);
                caller.data_mut().result = message.into_bytes();
                return Ok(abi::STATUS_MALFORMED);
            }
            let mut bytes = vec![0; len];
            memory.read(&caller, offset(ptr), &mut bytes)?;
            let (status, result) = call(caller.data_mut(), &bytes);
            caller.data_mut().result = result;
            // The time the engine took to answer doesn't count against the plugin.
            let ticks = caller.data().entry_ticks;
            caller.as_context_mut().set_epoch_deadline(ticks);
            Ok(status)
        },
    )?;
    linker.func_wrap(
        abi::MODULE,
        "result_len",
        |caller: Caller<'_, HostState>| -> wasmtime::Result<i32> {
            Ok(i32::try_from(caller.data().result.len())?)
        },
    )?;
    linker.func_wrap(
        abi::MODULE,
        "result_read",
        |mut caller: Caller<'_, HostState>, ptr: i32| -> wasmtime::Result<()> {
            let memory = memory(&mut caller)?;
            let bytes = caller.data().result.len();
            charge(&mut caller, bytes)?;
            let (memory_data, state) = memory.data_and_store_mut(&mut caller);
            write(memory_data, offset(ptr), &state.result)
        },
    )?;
    linker.func_wrap(
        abi::MODULE,
        "input_len",
        |caller: Caller<'_, HostState>| -> wasmtime::Result<i32> {
            Ok(i32::try_from(caller.data().input.len())?)
        },
    )?;
    linker.func_wrap(
        abi::MODULE,
        "input_read",
        |mut caller: Caller<'_, HostState>, ptr: i32| -> wasmtime::Result<()> {
            let memory = memory(&mut caller)?;
            let bytes = caller.data().input.len();
            charge(&mut caller, bytes)?;
            let (memory_data, state) = memory.data_and_store_mut(&mut caller);
            write(memory_data, offset(ptr), &state.input)
        },
    )?;
    Ok(linker)
}

/// Checks one call against the plugin's grants and its cap of calls a second and, if both
/// allow it, sends it. Answers the status and the result bytes the plugin reads back.
fn call(state: &mut HostState, bytes: &[u8]) -> (i32, Vec<u8>) {
    let call: Call = match serde_json::from_slice(bytes) {
        Ok(call) => call,
        Err(e) => return (abi::STATUS_MALFORMED, e.to_string().into_bytes()),
    };
    if let Err(denied) = state.capabilities.check(&call) {
        let body = serde_json::json!({ "denied": denied });
        return (abi::STATUS_DENIED, body.to_string().into_bytes());
    }
    if !state.rate.admit(Instant::now()) {
        let body = serde_json::json!({ "limited": { "per_second": state.rate.cap() } });
        return (abi::STATUS_LIMITED, body.to_string().into_bytes());
    }
    let answer = block_on(state.control.call(call));
    let encoded = match answer {
        Ok(response) => serde_json::to_vec(&response).map(|r| (abi::STATUS_OK, r)),
        Err(ControlError::Protocol(wire)) => {
            serde_json::to_vec(&wire).map(|r| (abi::STATUS_FAILED, r))
        }
        Err(other) => {
            let body = serde_json::json!({ "message": other.to_string() });
            Ok((abi::STATUS_FAILED, body.to_string().into_bytes()))
        }
    };
    encoded.unwrap_or_else(|e| (abi::STATUS_FAILED, e.to_string().into_bytes()))
}

/// The fuel a host call costs before its bytes, so a loop of empty calls still drains fuel.
pub(crate) const HOST_CALL_FUEL: u64 = 1_000;

/// Takes the fuel for host work on `bytes` bytes (copying them, and parsing a call) from
/// the plugin's budget before doing it: [`HOST_CALL_FUEL`] plus one unit per byte. With
/// too little left, the plugin stops with an out-of-fuel trap, so host work stays inside
/// the plugin's budget like its own instructions.
fn charge(caller: &mut Caller<'_, HostState>, bytes: usize) -> wasmtime::Result<()> {
    let cost = HOST_CALL_FUEL.saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
    let left = caller.get_fuel()?;
    if left < cost {
        caller.set_fuel(0)?;
        return Err(wasmtime::Trap::OutOfFuel.into());
    }
    caller.set_fuel(left - cost)
}

/// The plugin's exported memory.
fn memory(caller: &mut Caller<'_, HostState>) -> wasmtime::Result<Memory> {
    match caller.get_export(abi::MEMORY) {
        Some(Extern::Memory(memory)) => Ok(memory),
        _ => Err(wasmtime::format_err!("the plugin exports no memory")),
    }
}

/// A WASM `i32` pointer or length, read as the unsigned 32-bit offset it is.
fn offset(value: i32) -> usize {
    value.cast_unsigned() as usize
}

/// Copies `bytes` into the plugin's memory at `at`, trapping if they don't fit.
fn write(memory: &mut [u8], at: usize, bytes: &[u8]) -> wasmtime::Result<()> {
    let end = at
        .checked_add(bytes.len())
        .filter(|end| *end <= memory.len())
        .ok_or_else(|| wasmtime::format_err!("result_read: out of bounds"))?;
    memory[at..end].copy_from_slice(bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Plugin, PluginConfig, PluginHost};
    use crate::{Error, Limits};
    use jstream_control::{Control, MockEngine};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// An endless loop, and the same loop as a module's `start` function.
    const SPIN: &str = r#"(module
        (memory (export "memory") 1)
        (func (export "spin") (loop $forever (br $forever))))"#;
    const SPIN_AT_START: &str = r#"(module
        (memory (export "memory") 1)
        (func $start (loop $forever (br $forever)))
        (start $start))"#;
    /// A `start` function that makes one (malformed) host call, then loops forever.
    const CALL_THEN_SPIN_AT_START: &str = r#"(module
        (import "jstream" "call" (func $call (param i32 i32) (result i32)))
        (memory (export "memory") 1)
        (func $spin (loop $forever (br $forever)))
        (func $start (drop (call $call (i32.const 0) (i32.const 0))) (call $spin))
        (start $start))"#;

    fn load_on(host: &PluginHost, wat: &str, limits: Limits) -> Result<Plugin, Error> {
        let control: Arc<dyn Control> = Arc::new(MockEngine::new().connect());
        let wasm = wat::parse_str(wat).expect("valid WAT");
        let config = PluginConfig {
            limits,
            ..PluginConfig::default()
        };
        Plugin::load(host, wasm, config, control)
    }

    fn load(wat: &str, limits: Limits) -> Result<Plugin, Error> {
        let host = PluginHost::new().expect("the engine configures");
        load_on(&host, wat, limits)
    }

    #[test]
    fn a_plugin_past_its_deadline_is_interrupted() {
        let limits = Limits::unmetered().with_call_deadline(Duration::from_millis(30));
        let mut plugin = load(SPIN, limits).expect("the plugin loads");
        let started = Instant::now();
        assert!(matches!(plugin.invoke("spin", &[]), Err(Error::Deadline)));
        assert!(started.elapsed() < Duration::from_secs(5));
        // The next invocation gets a fresh deadline, and is interrupted again.
        assert!(matches!(plugin.invoke("spin", &[]), Err(Error::Deadline)));
    }

    #[test]
    fn the_default_deadline_interrupts_a_plugin() {
        let mut plugin = load(SPIN, Limits::unmetered()).expect("the plugin loads");
        let started = Instant::now();
        assert!(matches!(plugin.invoke("spin", &[]), Err(Error::Deadline)));
        let took = started.elapsed();
        assert!(took >= Limits::DEFAULT_CALL_DEADLINE, "{took:?}");
        assert!(took < Duration::from_secs(5), "{took:?}");
    }

    #[test]
    fn a_start_function_past_the_load_deadline_fails_the_load() {
        let limits = Limits::unmetered().with_load_deadline(Duration::from_millis(30));
        let refused = load(SPIN_AT_START, limits);
        assert!(
            matches!(refused, Err(Error::Deadline)),
            "{:?}",
            refused.err()
        );
    }

    #[test]
    fn a_host_call_during_the_load_re_arms_the_load_deadline() {
        let load_deadline = Duration::from_millis(400);
        let limits = Limits::unmetered()
            .with_call_deadline(Duration::from_millis(30))
            .with_load_deadline(load_deadline);
        let started = Instant::now();
        let refused = load(CALL_THEN_SPIN_AT_START, limits);
        let took = started.elapsed();
        assert!(
            matches!(refused, Err(Error::Deadline)),
            "{:?}",
            refused.err()
        );
        // The invocation deadline would have stopped it after about 40 ms.
        assert!(took >= load_deadline, "{took:?}");
        assert!(took < Duration::from_secs(5), "{took:?}");
    }

    #[test]
    fn a_plugin_outliving_its_host_keeps_its_deadline() {
        let host = PluginHost::new().expect("the engine configures");
        let limits = Limits::unmetered().with_call_deadline(Duration::from_millis(30));
        let mut plugin = load_on(&host, SPIN, limits).expect("the plugin loads");
        drop(host);
        assert!(matches!(plugin.invoke("spin", &[]), Err(Error::Deadline)));
    }
}
