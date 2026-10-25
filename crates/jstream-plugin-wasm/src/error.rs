//! What loading or invoking a plugin can fail with.

/// wasmtime's error, kept as the source of ours. wasmtime's own type doesn't implement
/// `std::error::Error`, so it is boxed through the conversion wasmtime provides.
type Source = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Why a plugin could not be loaded or invoked.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// wasmtime refused the host's engine configuration.
    #[error("the WASM engine could not be configured")]
    Engine(#[source] Source),
    /// The bytes are not a valid WASM module, or use a feature the host leaves off.
    #[error("the plugin is not a valid WASM module")]
    Compile(#[source] Source),
    /// The plugin imports something the host doesn't provide: anything outside the
    /// `jstream` functions, WASI included (R-SEC-003).
    #[error("the plugin imports `{module}::{name}`, which the host does not provide")]
    ForbiddenImport {
        /// The import's module.
        module: String,
        /// The import's name.
        name: String,
    },
    /// The plugin doesn't export its linear memory as `memory`.
    #[error("the plugin exports no memory named `memory`")]
    NoMemory,
    /// Instantiating the plugin failed: its memory or table starts above the limits, or
    /// its `start` function trapped.
    #[error("the plugin could not be instantiated")]
    Instantiate(#[source] Source),
    /// The plugin has no exported function of that name with type `() -> ()`.
    #[error("the plugin exports no function `{0}` of type () -> ()")]
    NoEntryPoint(String),
    /// The invocation (or the `start` function) used up its fuel and was stopped.
    #[error("the plugin ran out of fuel")]
    OutOfFuel,
    /// The invocation (or the load) ran past its wall-time deadline and was interrupted
    /// (ADR-0069's `cpu_limit`).
    #[error("the plugin ran past its time limit")]
    Deadline,
    /// The plugin asked to grow its memory or table past its cap, or its module starts
    /// above it, and was stopped (ADR-0069's `memory_limit`).
    #[error("the plugin reached its memory or table cap")]
    MemoryLimit(#[source] Source),
    /// The plugin trapped: a WASM trap, a host function it misused (a pointer outside its
    /// memory), or a stack overflow. Its state may be inconsistent.
    #[error("the plugin trapped")]
    Trap(#[source] Source),
}

impl Error {
    /// Classifies a wasmtime error from running plugin code: fuel exhaustion is
    /// [`Error::OutOfFuel`], an epoch interrupt [`Error::Deadline`], a growth past a cap
    /// [`Error::MemoryLimit`], anything else is `other`.
    pub(crate) fn from_run(error: wasmtime::Error, other: fn(Source) -> Error) -> Error {
        if error.downcast_ref::<crate::limits::CapReached>().is_some() {
            return Error::MemoryLimit(error.into());
        }
        match error.downcast_ref::<wasmtime::Trap>() {
            Some(wasmtime::Trap::OutOfFuel) => Error::OutOfFuel,
            Some(wasmtime::Trap::Interrupt) => Error::Deadline,
            _ => other(error.into()),
        }
    }
}
