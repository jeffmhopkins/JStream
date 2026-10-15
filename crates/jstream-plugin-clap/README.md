<!-- @doc
id = "CRATE-jstream-plugin-clap"
title = "jstream-plugin-clap"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-plugin-clap

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-plugin-clap/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-plugin-clap", "docs/adr/0035-clap-plugins-out-of-process.md#decision", "docs/RULES.md#unsafe"]
-->

A safe wrapper over the CLAP audio plugin ABI
([ADR-0035](../../docs/adr/0035-clap-plugins-out-of-process.md) §5). Layer 2, with
`unsafe` allowed: a CLAP plugin is a C ABI in a native library. The raw types come from
`clap-sys` ([R-UNSAFE-004](../../docs/RULES.md#R-UNSAFE-004)), and the library is loaded
with `libloading`. **Only `jstream-plugin-host` links this crate**, and lint-deps checks
that no other member reaches it ([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001)), so
third-party native code never loads into the engine's process.

**API.**
- `Library::load(path)`: loads a plugin file, reads its `clap_entry`, checks its CLAP
  version is compatible (major 1) and runs `init` with the path. Dropping the last
  `Arc<Library>` runs `deinit`, then unloads the file. `Library::plugins` lists the
  factory's descriptors as `PluginInfo` (id, name, vendor, version, description,
  features); `is_audio_effect` checks the `audio-effect` feature.
- `Plugin::new(library, id)`: creates and initialises one instance, on the host's main
  thread. It refuses a plugin that isn't an audio effect (instruments and note effects
  are out of scope). It reads `params` (id, name, module, range, default and the
  stepped, enum, hidden, read-only and bypass flags) and `param_value`, and saves and
  loads its state as bytes (`save_state`, `load_state`, at most `MAX_STATE_BYTES`,
  2 MiB, the size ROADMAP M3 gives a filter's `state`).
- `Plugin::activate(rate, max_frames)`: checks that the main input and main output
  ports (port 0 each way) are stereo, reads the plugin's other ports (at most
  `MAX_PORTS`, 16, each way, of at most `MAX_PORT_CHANNELS`, 32, channels), activates
  the plugin, and returns its `Processor`. `latency` is read while active.
  `deactivate` refuses while the `Processor` exists.
- `Processor::process(input, output, params)`: for the audio thread. It deinterleaves
  a stereo block into the main input's buffers, reserved at activation, and passes
  every port the plugin reported, as CLAP requires: sidechain inputs get silence
  (re-zeroed each block and marked constant), and auxiliary outputs are dropped. It
  passes `params` (at most `MAX_BLOCK_EVENTS`, 256) as CLAP `PARAM_VALUE` events at
  the block's start. It runs
  the plugin, interleaves the output, and returns the parameter changes the plugin
  reported itself (its own editor), in a list reserved at activation that refuses
  rather than grows. It never allocates. The first call starts processing; dropping it
  stops processing. `reset` clears the plugin's audio state. `Processor` is `Send`,
  so it can move to the host's real-time thread. `Plugin` is neither `Send` nor `Sync`,
  so main-thread calls stay on the thread that created it, and only it deactivates and
  destroys the instance: dropped while its `Processor` still exists, it leaks the
  instance rather than let the audio thread destroy it.
- Requests the plugin makes through the host: `run_main_thread_callback` runs its
  `on_main_thread` when it asked, `take_restart_request` reports a restart or latency
  change (applied by deactivating and activating again), and `take_state_dirty` a
  changed state.

**The host side.** Each instance gets its own `clap_host` on the heap, named JStream,
freed after the instance is destroyed, with the latency, params and state host
extensions. Every callback a plugin can call
(`request_restart`, `request_process`, `request_callback`, those extensions'
functions, the event lists and the state streams) catches panics
([R-UNSAFE-005](../../docs/RULES.md#R-UNSAFE-005)) and only sets a flag or copies
bytes, so it is safe from any thread. Every `unsafe` block has a `// SAFETY:` comment
([R-UNSAFE-002](../../docs/RULES.md#R-UNSAFE-002)).

**Untrusted input.** The plugin is native code running with the user's rights. The
host process contains its crashes, not its behaviour. What it returns is still checked
before use: null pointers, plugin, feature, parameter and port counts (capped), port
layouts, event headers (type, space and size), and stream sizes (the state limit).
Strings are taken as NUL-terminated, as CLAP requires; only how much of one is kept is
capped.

**Tests** run against a CLAP plugin file written in the test binary against the raw
ABI (`src/testplugin.rs`): a stereo gain effect, an instrument, a mono effect, the
gain effect with a sidechain input and an auxiliary output, and an effect with too
many ports. They cover: listing a file's plugins; refusing instruments, mono effects
and too many ports; sidechain inputs silenced every block and auxiliary outputs
dropped; the instance destroyed only by its `Plugin`, on its thread, and leaked when
the `Processor` outlives it (and a compile-fail doc test that `Plugin` isn't `Send`);
parameters, latency (only while active) and the main-thread callback the plugin asks for at
`init`; processing with a parameter change applied and the plugin's own change
reported; block sizes refused; `reset`; deactivation refused while a `Processor`
exists; activating again; a `Processor` on another thread; and state saved, restored,
refused when truncated, and refused over 2 MiB either way. Processing runs under
`assert_no_alloc` with a flood of events. Loading files: a missing file is a load
error, and on Linux the C library (a native library with no `clap_entry`) is refused.

**Not yet:** loading and running a real plugin file (that comes with the host app and
is checked on Windows), the plugin's editor window (the GUI extension), the `flush`
path for parameter changes while inactive, and the thread-check and log host
extensions.
