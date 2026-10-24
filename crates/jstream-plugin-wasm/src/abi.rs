//! The plugin ABI, version 0: the only imports a plugin may have, and what they answer.
//!
//! It is unstable until a later decision fixes it (README › ABI). Every import lives in
//! the [`MODULE`] namespace. Pointers and lengths are `i32`s read as unsigned offsets into
//! the plugin's exported memory, [`MEMORY`]. A pointer or length outside that memory
//! traps the plugin.
//!
//! - `call(ptr, len) -> status`: `len` bytes at `ptr` are one protocol `Call` as JSON.
//!   The host checks it against the plugin's capabilities and its cap of commands a second,
//!   then sends it through `Control`. Answers one of the `STATUS_*` codes and leaves a result to read.
//! - `result_len() -> len` and `result_read(ptr)`: the length of the last call's result,
//!   and a copy of it into the plugin's memory at `ptr`.
//! - `input_len() -> len` and `input_read(ptr)`: the same for the bytes the host passed
//!   to the current invocation.
//!
//! An entry point is any exported function of type `() -> ()`.

/// The import namespace of every host function.
pub const MODULE: &str = "jstream";

/// The name the plugin's linear memory must be exported under.
pub const MEMORY: &str = "memory";

/// The host functions, by name. An import from [`MODULE`] not in this list, or from any
/// other module, refuses the plugin at load.
pub const IMPORTS: &[&str] = &[
    "call",
    "result_len",
    "result_read",
    "input_len",
    "input_read",
];

/// The call reached `Control` and succeeded: the result is the protocol `Response` as
/// JSON.
pub const STATUS_OK: i32 = 0;

/// The call names a command the plugin was not granted, or names none. It never reached
/// `Control`. The result is a JSON object `{"denied": [<names>]}` listing the commands
/// that were not granted (empty when the call names none or is of an unknown kind).
pub const STATUS_DENIED: i32 = 1;

/// The bytes were not UTF-8 JSON for a protocol `Call`, or were longer than
/// [`MAX_CALL_BYTES`]. The result is the parser's message as UTF-8 text.
pub const STATUS_MALFORMED: i32 = 2;

/// The call reached `Control` and failed. The result is the protocol's wire error as
/// JSON when the engine answered one, else `{"message": <text>}`.
pub const STATUS_FAILED: i32 = 3;

/// The call was well formed and granted, but its commands (every edit of a batch, or its
/// one action, query or history operation) don't fit in what is left of the plugin's cap
/// of commands to `Control` in the last second
/// ([`Limits::commands_per_second`](crate::Limits::commands_per_second), 50 by default).
/// It never reached `Control`, and doesn't count against the cap. The result is a JSON
/// object `{"limited": {"per_second": <cap>, "commands": <the call's>}}`. The plugin keeps
/// running; a later call gets through once its earlier commands are a second old, and a
/// batch of more commands than the cap never does.
pub const STATUS_LIMITED: i32 = 4;

/// The longest call the host reads, in bytes.
pub const MAX_CALL_BYTES: u32 = 1024 * 1024;
