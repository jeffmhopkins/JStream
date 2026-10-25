<!-- @doc
id = "CRATE-jstream-plugin-wasm"
title = "jstream-plugin-wasm"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-plugin-wasm

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-plugin-wasm/Cargo.toml", "crates/jstream-plugin-wasm/src/**", "crates/jstream-plugin-wasm/tests/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-plugin-wasm", "docs/adr/0035-clap-plugins-out-of-process.md#decision", "docs/RULES.md#R-SEC-003"]
-->

Layer-3 host for sandboxed WASM automation plugins
([ADR-0035](../../docs/adr/0035-clap-plugins-out-of-process.md), which keeps ADR-0005's
decision on them). A plugin is a core WASM module, compiled with
[wasmtime](https://wasmtime.dev/), that reaches the engine only through `Control`.

**Sandbox ([R-SEC-003](../../docs/RULES.md#R-SEC-003)).** A plugin gets nothing it
wasn't granted:
- **No ambient imports.** The only imports a plugin may have are the five `jstream`
  functions of the [ABI](#abi). Any other import, WASI's included, refuses the plugin at
  load, before any of its code runs. There is no WASI in the build at all, so no
  filesystem, network, clock or randomness, and no host memory, table or global.
- **Capabilities.** `Control` is full access, so a plugin never holds it. Each plugin has
  a `Capabilities` list of protocol command names (`scene.list`, `output.stop`); a call
  reaches `Control` only when it names at least one command and every command in it is
  on the list (each edit of a batch, or the one action, query or history operation).
  Names match exactly, with no wildcards. A denied call never reaches `Control`: the
  plugin gets `STATUS_DENIED` and the names that were refused. **The list is empty by
  default.** `Capabilities::every()` grants every command, for a caller whose `Control`
  checks each call itself: `jstream-core` holds its plugins to ADR-0069's capability
  classes and grants that way, in the scoped session it lends each plugin (ADR-0075). A
  call that names no command is denied either way.
- **Refusals** (`PluginConfig::refusals`). `Refusals::Statuses`, the default, answers a
  denied call with `STATUS_DENIED` and one past the cap of commands a second with
  `STATUS_LIMITED`. `Refusals::Forbidden` answers both with `STATUS_FAILED` and the
  protocol's `forbidden` wire error (its `call` is the refused command, or the call's
  first), which is what ADR-0069 gives a plugin and what `jstream-core` picks.
- **Limits** (`Limits`). Each invocation, and the module's `start` function at load, runs
  on a fuel budget, by default 10,000,000 units (one per WASM instruction, roughly);
  running out stops the plugin with `Error::OutOfFuel`, and the next invocation gets a
  fresh budget. The host calls that copy or parse bytes (`call`, `result_read` and
  `input_read`) are charged to the same budget before the host does their work: 1,000
  units each plus one per byte, so a plugin can't spend host time its fuel doesn't
  cover. `result_len` and `input_len` do constant work and aren't charged. Each entry into the plugin also has a
  wall-time deadline, by default 100 ms for an invocation and 1 s for the load (#348's
  figures). A ticker thread advances wasmtime's epoch every 10 ms (`EPOCH_TICK`), so a
  plugin gets at least its deadline and at most its deadline rounded up to a whole tick,
  plus one tick, as the ticker keeps time. Each `call` re-arms the deadline of the entry
  it was made from (the load's or the invocation's) when it returns, so the time the
  engine spends answering doesn't count.
  Running past the deadline stops the plugin with `Error::Deadline`. This catches what
  fuel misses, such as a plugin whose few instructions are slow. The ticker runs while
  the host or any plugin loaded on it lives. Linear memory is capped at 64 MiB and tables
  at 10,000 elements by default (`limits::Caps`, the store's resource limiter). Growing
  either past its cap is a trap the plugin can't catch, `Error::MemoryLimit` (ADR-0069
  treats reaching a cap as a trap; the engine reports it as `memory_limit`, ADR-0076), and
  so is a module whose memory starts above it, at load. A growth past the module's own
  declared maximum, below the cap, answers −1 as WASM says. One instance, one memory and
  one table per plugin. The WASM stack is
  capped at 512 KiB for every plugin. A plugin may send at most 50 commands to
  `Control` in any second (ADR-0069), counted over a sliding window. Every edit of a
  batch is a command (ADR-0013), as is a call's one action, query or history operation.
  A granted, well-formed call whose commands don't fit in what is left gets
  `STATUS_LIMITED` and never reaches `Control`, and the plugin keeps running; a batch of
  more than 50 edits never fits. A refused call doesn't count, nor does a denied or
  malformed one. A plugin's configuration can lower each limit, never raise it.

**Running a plugin.** `PluginHost` owns the wasmtime engine. `Plugin::load` compiles and
instantiates one module with its `PluginConfig` (capabilities and limits) and the
`Arc<dyn Control>` session it calls through; `Plugin::invoke(entry, input)` runs an
exported `() -> ()` function, with `input` readable by the plugin. A plugin's calls are
answered synchronously: the thread running it parks until the `Control` future
completes, so plugins run on threads that may block, never on an async executor's
worker. `Plugin::has_entry(entry)` says whether the plugin exports `entry` as a
`() -> ()` function, without running any of its code. **Events.** `Plugin::events` gives the `EventSender` the engine queues events
for the plugin with, from any thread; `Plugin::deliver_event(entry)` hands the oldest to
the plugin, invoking `entry` with the protocol `Event` as JSON for its input. The queue
holds at most 1,000 events (`MAX_QUEUED_EVENTS`, ADR-0069): an event that finds it full
drops the queued ones, and the plugin is next handed one `events.dropped` notice with
their count (overflows before it is handed out add to it), then the events that came
after, as a WebSocket client that reads too slowly gets. After a trap, fuel exhaustion or a missed deadline the caller decides what to do
with the plugin. Under ADR-0069 `jstream-core` disables it, with
`plugin.state_changed`, until a person re-enables it.

**Dependencies.** wasmtime with only `cranelift`, `runtime` and `std`: no async, WASI,
component model, GC, threads, cache or profiling. Its `runtime` build script compiles a
small C helper file, which is on the lint-deps native-code allowlist.

**Tests:** imports from WASI, from other modules and of host memory or globals are
refused (`no_ambient_capabilities`, which R-SEC-003 names); a plugin with no grants is
denied even a read; capability checks of queries, actions, batches and history calls;
limits only lower; through a fake `Control`, a granted call arrives intact, and denied
and malformed calls never arrive; time a slow `Control` takes to answer doesn't count
against the deadline; with fuel unmetered, an endless loop and a spinning `start`
function miss the default and a lowered deadline, even after the host is dropped, and a
host call in `start` re-arms the load's deadline, not an invocation's; an endless loop
runs out of fuel under the default and a lowered budget, in an invocation and in
`start`; growing memory past the default or a lowered cap, or the table past a lowered
one, is `Error::MemoryLimit`, as is a memory that starts above the cap, while growing
past the module's own maximum answers −1; a
result read outside memory traps; of sixty calls in one invocation the first fifty reach
`Control` and the rest are limited without stopping the plugin, a lowered cap holds
across invocations and frees a second later, denied calls don't use it, each edit of a
batch counts against it, a batch of 51 edits never reaches `Control` while one of 50
does, and the window admits again as commands age and a cap of zero admits nothing
(`rate.rs`); queued events reach the plugin as JSON in order, from another thread, a
1,001st event reaches it as one `events.dropped` notice of 1,000 and then itself,
overflows before a notice is handed out add up, and a failed delivery still takes its
event (`events.rs`); with `Refusals::Forbidden`, a denied call and calls past the cap
fail with the `forbidden` wire error instead, still without reaching `Control` or
stopping the plugin; `Capabilities::every` grants every command but not a call that
names none.

**Not here:** installing plugins, the package and its manifest, the person's grant and
the scoped session that holds a plugin to it, pumping a plugin's events into its queue
and the entry that handles them (`on_event`), and disabling a failed plugin with
`plugin.state_changed`, saving the installed plugins and holding a grant to what the
manifest asks for. `jstream-core` does all of these
([ADR-0075](../../docs/adr/0075-wasm-plugin-host-in-the-engine.md),
[ADR-0076](../../docs/adr/0076-saved-wasm-plugins-and-manifest-requests.md)). Not
anywhere yet: a private data folder per plugin and the file imports it would need, and
any limits on what a plugin may write into names and other strings the UI and agents
show.

<a id="abi"></a>
## Plugin ABI
<!-- @section
covers = ["crates/jstream-plugin-wasm/src/abi.rs", "crates/jstream-plugin-wasm/src/plugin.rs"]
-->

Version 0, and **unstable**: no compatibility is promised for it until a later decision
fixes the ABI plugins are written against. `src/abi.rs` holds its constants.

A plugin exports its linear memory as `memory`. Pointers and lengths are `i32`s read as
unsigned offsets into it; a pointer or length outside it traps the plugin. `call`,
`result_read` and `input_read` take fuel for the bytes they handle before handling them
(see Limits above); with too little left, the plugin stops out of fuel. When `call`
returns it re-arms the plugin's deadline. It may import
only these functions, from the module `jstream`:

| Import | Type | Does |
|---|---|---|
| `call` | `(ptr, len: i32) -> i32` | Reads `len` bytes at `ptr` as one protocol `Call` in JSON, checks it against the plugin's capabilities and its cap of commands a second (each edit of a batch counts), and sends it through `Control`. Answers a status, and keeps a result to read. |
| `result_len` | `() -> i32` | The length of the last call's result. |
| `result_read` | `(ptr: i32)` | Copies the last call's result to `ptr`. |
| `input_len` | `() -> i32` | The length of the current invocation's input. |
| `input_read` | `(ptr: i32)` | Copies the input to `ptr`. |

The statuses, and the result each leaves:

| Status | Value | Result |
|---|---|---|
| `STATUS_OK` | 0 | The protocol `Response`, as JSON. |
| `STATUS_DENIED` | 1 | `{"denied": [<names>]}`: the commands not granted. Empty when the call names no command or is of a kind this version doesn't know. `Control` was not called. |
| `STATUS_MALFORMED` | 2 | The parse error, as text: not UTF-8 JSON for a `Call`, or longer than 1 MiB. `Control` was not called. |
| `STATUS_FAILED` | 3 | The engine's wire error, as JSON, or `{"message": …}` when the transport failed. |
| `STATUS_LIMITED` | 4 | `{"limited": {"per_second": <cap>, "commands": <the call's>}}`: the call was granted, but its commands don't fit in what is left of the plugin's cap of commands in the last second. `Control` was not called, and the call doesn't count against the cap. A batch of more edits than the cap always gets this. |

An entry point is any exported function of type `() -> ()`, run by `Plugin::invoke`, or
by `Plugin::deliver_event` with one protocol `Event` as JSON for its input (an
`events.dropped` notice among them after an overflow). The engine hands its plugins
their events through an export named `on_event` (ADR-0075).

Under `Refusals::Forbidden`, the setting the engine uses, a plugin never sees
`STATUS_DENIED` or `STATUS_LIMITED`: a denied call and one past the cap get
`STATUS_FAILED`, with the `forbidden` wire error as the result.

The host never calls `hello` for a plugin: it lends the plugin a session the host opened,
and a plugin can't subscribe to events or open the binary channel.
