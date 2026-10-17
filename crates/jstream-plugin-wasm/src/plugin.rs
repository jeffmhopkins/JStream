//! Loading and invoking plugins.

use crate::abi;
use crate::block_on::block_on;
use crate::capability::Capabilities;
use crate::error::Error;
use crate::limits::Limits;
use jstream_control::protocol::Call;
use jstream_control::{Control, ControlError};
use std::sync::Arc;
use wasmtime::{
    Caller, Config, Engine, Extern, Instance, Linker, Memory, Module, Store, StoreLimits,
    StoreLimitsBuilder,
};

/// The deepest the WASM stack of a plugin may grow, in bytes. Fixed for every plugin,
/// because wasmtime sets it per engine; overflowing it traps.
const MAX_WASM_STACK: usize = 512 * 1024;

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
}

impl PluginHost {
    /// An engine that meters fuel and caps the WASM stack. Proposals that need more of the
    /// host (threads, the component model, GC) are not compiled into this crate.
    pub fn new() -> Result<Self, Error> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.max_wasm_stack(MAX_WASM_STACK);
        let engine = Engine::new(&config).map_err(|e| Error::Engine(e.into()))?;
        Ok(PluginHost { engine })
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
}

impl Plugin {
    /// Compiles and instantiates `wasm`, a binary core module. Every import must be one of the [`abi`] functions, so a module that asks
    /// for WASI or anything else is refused before any of its code runs. The module's
    /// `start` function, if any, runs with one invocation's fuel.
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
        };
        let mut store = Store::new(&host.engine, state);
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(limits.fuel())
            .map_err(|e| Error::Engine(e.into()))?;

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
        })
    }

    /// Runs the exported function `entry` (type `() -> ()`) with a fresh fuel budget.
    /// `input` is what the plugin reads with `input_len` and `input_read` (an event, say).
    pub fn invoke(&mut self, entry: &str, input: &[u8]) -> Result<(), Error> {
        let func = self
            .instance
            .get_typed_func::<(), ()>(&mut self.store, entry)
            .map_err(|_| Error::NoEntryPoint(entry.to_owned()))?;
        self.store
            .set_fuel(self.fuel)
            .map_err(|e| Error::Engine(e.into()))?;
        let state = self.store.data_mut();
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
            let (status, result) = call(caller.data(), &bytes);
            caller.data_mut().result = result;
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

/// Checks one call against the plugin's grants and, if allowed, sends it. Answers the
/// status and the result bytes the plugin reads back.
fn call(state: &HostState, bytes: &[u8]) -> (i32, Vec<u8>) {
    let call: Call = match serde_json::from_slice(bytes) {
        Ok(call) => call,
        Err(e) => return (abi::STATUS_MALFORMED, e.to_string().into_bytes()),
    };
    if let Err(denied) = state.capabilities.check(&call) {
        let body = serde_json::json!({ "denied": denied });
        return (abi::STATUS_DENIED, body.to_string().into_bytes());
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
