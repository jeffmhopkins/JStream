<!-- @doc
id = "ADR-0035"
title = "CLAP audio plugins, out of process"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0035: CLAP audio plugins, out of process

Supersedes [ADR-0005](0005-plugins-wgsl-and-wasm.md). It restates all of ADR-0005's
decision, which stands, and makes one exception to "no native dynamic plugins": CLAP audio
effect plugins, hosted in a separate process.

<a id="context"></a>
## Context
Native dynamic plugins (OBS-style) need a stable ABI and can crash the process. That is
why [ADR-0005](0005-plugins-wgsl-and-wasm.md) allows only WGSL effect packs and sandboxed
WASM plugins, with no native dynamic plugins in v1.

The chosen UI prototype (`design/ui-prototype/`, direction D) offers a **CLAP plugin**
audio filter. Its README says this is not settled, because hosting CLAP means loading
native dynamic libraries. The UI decision register
([C. Audio](../../design/ui-prototype/decision-register.md#c-audio), entry D23) marks it
as a conflict with ADR-0005, and weighs three options:
1. Keep ADR-0005: no CLAP.
2. Host CLAP **out of process**, superseding ADR-0005 for audio effects only.
3. Host CLAP in process: fastest, but it puts third-party native code on the mixer
   thread. A crash then takes down the engine, which is exactly what ADR-0005 avoids, and
   [R-RT-001](../RULES.md#R-RT-001) can't be enforced on code JStream didn't write.

CLAP is an open plugin ABI (MIT licensed) with a C interface. Plugins are native DLLs the
user installs. Each plugin reports its own latency, exposes its parameters, saves and
restores its state, and may open its own editor window.

Superseding an ADR is a maintainer decision ([ADR-0017](0017-adr-decision-tiers.md)). The
maintainer decided D23 in the builder session. The maintainer's choice was **"Later, out
of process"**.

<a id="decision"></a>
## Decision

**Unchanged from ADR-0005:**
- Built-in sources and effects are Rust code in the workspace.
- Third-party extensions may be **WGSL effect packs** with an `effect.toml` manifest,
  hot-reloadable.
- They may also be **sandboxed WASM plugins** ([wasmtime](https://wasmtime.dev/)) for
  automation and logic, which call the API through `Control`.
- Both stay exactly as ADR-0005 decides, including [R-SEC-003](../RULES.md#R-SEC-003)
  for WASM.

**Changed: one kind of native plugin is allowed.**
- **CLAP audio effect plugins** may be used as audio filters, on sources, buses and
  outputs. Instruments and note effects are out of scope.
- They run **only out of process**. JStream never loads a CLAP plugin into the engine's
  process.
- No other native plugin is allowed. Third parties still can't add capture devices,
  encoders, video effects or other plugin formats without a later ADR.
- **Not before M3.** Until it is built, the protocol has no CLAP filter kind, and the UI
  hides the prototype's CLAP filter.

**1. The plugin host process.**
- A new binary, `jstream-plugin-host.exe`, ships in the portable zip next to the other exes
  ([ADR-0020](0020-portable-windows-distribution.md)).
- The engine starts **one host process per CLAP filter instance**, so one crashing plugin
  takes down only its own filter. Grouping several instances into one process is a later
  optimisation, and needs a measurement first.
- The host loads the plugin, runs its audio processing on its own real-time thread
  (registered with MMCSS "Pro Audio", as in [ADR-0014](0014-media-clock.md)), and opens
  the plugin's editor window on request.

**2. Audio IPC and latency.**
- Audio crosses processes through **shared-memory ring buffers**: one in, one out, plus a
  parameter-event ring and a status block. The layout is a versioned `repr(C)` contract in
  a new L0 crate, `jstream-plugin-proto`, following the pattern of `jstream-vcam-proto`.
- **The mixer never waits on the host.** It writes block *n* into the input ring and reads
  the processed block *n − d* from the output ring, where *d* is a fixed pipeline depth
  (default two engine blocks).
- The host's thread is woken by its own timer on the shared media clock (the OS
  high-resolution counter is the same in both processes). The mixer makes no syscall and
  takes no lock for the plugin ([R-RT-001](../RULES.md#R-RT-001) holds unchanged).
- **Latency** is the pipeline depth plus the plugin's own reported latency. It is reported
  as the filter's latency, so the mixer's timestamp-preserving latency compensation keeps
  audio and video in sync (register entry D15).
- **A late block is bypassed, never waited for.** The mixer passes the dry signal for that
  block, with a short crossfade, and counts the miss in `stats.tick`, as overload drops are
  counted ([R-RT-004](../RULES.md#R-RT-004)). Repeated misses raise an event that marks the
  filter degraded.

**3. Crashes.**
- The host writes a heartbeat into the status block. The control thread watches it, and
  the process handle.
- When a host dies, the mixer is already bypassing it, so the engine, the audio thread and
  every output keep running. The control thread raises an error event, and restarts the
  host with backoff, restoring the plugin's last saved state. After repeated crashes the
  filter is disabled and reported, and is not restarted again until the user re-enables it.
- When the engine exits, its host processes exit with it (a Windows job object with
  kill-on-close).

**4. Isolation and trust.**
- Out-of-process hosting is **crash isolation, not a security sandbox**. A CLAP plugin is
  native code the user chose to install, and it runs with the user's rights, as in any
  audio workstation. Adding one shows that warning.
- The host gets nothing it doesn't need:
  - no control token and no protocol connection;
  - no inherited handles except its shared memory and synchronisation objects;
  - a scrubbed environment.
- Plugins are found in the standard CLAP search paths, or in a file the user picks.
- Tighter restriction (a low-integrity token or an AppContainer) is evaluated when this is
  built. It is adopted only if common plugins still work, and that is recorded in ARCH.

**5. Unsafe code.**
- Calling a C plugin ABI needs `unsafe`. It lives in a new crate, `jstream-plugin-clap`
  (L2, `unsafe_allowed = true`), which wraps the CLAP ABI in safe types
  ([R-UNSAFE-003](../RULES.md#R-UNSAFE-003)). Raw bindings come from maintained CLAP
  bindings ([R-UNSAFE-004](../RULES.md#R-UNSAFE-004)), picked and justified when built.
- Every host callback that a plugin can call catches panics
  ([R-UNSAFE-005](../RULES.md#R-UNSAFE-005)).
- **Only `jstream-plugin-host` links `jstream-plugin-clap`.** The engine and every other
  crate never do, and lint-deps checks it, the way it checks that clients never reach
  `jstream-core`.
- Shared-memory mapping and process creation on the engine side live in
  `jstream-platform-windows`, which already allows `unsafe`.

**6. Parameters, state and editors.**
- A plugin's parameters become the filter's settings, so the protocol, the web UI and
  agents can read and change them like any filter's. Parameter changes reach the host
  through the parameter ring.
- The plugin's saved state is stored in the collection as an opaque blob in the filter's
  settings.
- The plugin's own editor is a native window that the host opens on the machine running
  the engine. It is an action, not available to remote browser clients, which use the
  parameters instead.

**7. Platforms.** Windows first. macOS and Linux follow the same design when their
platform crates land.

<a id="consequences"></a>
## Consequences
- **Unchanged from ADR-0005:**
  - WGSL and WASM plugins can't crash the engine, and there is no ABI for JStream to keep
    stable.
  - Third parties can't add capture devices or encoders until a later ADR allows it.
- **Users can use their CLAP effects** (noise reduction, EQ, compressors), and a plugin
  that crashes or stalls costs only its own filter, never the engine, the audio thread or
  an output.
- **CLAP filters add latency**: at least the pipeline depth, plus the plugin's own. Sync
  is preserved by latency compensation, but monitoring through a CLAP filter is delayed.
- **One process per instance** costs memory and process start-up time per filter.
- **Plugins are trusted code.** JStream isolates their crashes, not their behaviour.
- **A third `unsafe` area** joins the platform and virtual-camera crates, and a new exe
  joins the portable zip.
- **Prerequisite:** per-filter latency reporting and timestamp-preserving compensation in
  the mixer, which M1b builds.
- **Follow-on edits when this ADR is accepted** (not made while it is proposed):
  - ADR-0005: `status = "superseded"`, `superseded_by` pointing here.
  - [R-UNSAFE-001](../RULES.md#R-UNSAFE-001) gains `jstream-plugin-clap` in its list, and
    the lint-deps `unsafe` allowlist gains it. The `applies_to` of R-UNSAFE-002 to
    R-UNSAFE-005 gain the crate. These are declared governance changes.
  - [R-RT-001](../RULES.md#R-RT-001)'s `applies_to` gains the host's real-time module
    (also a governance change). [R-SEC-003](../RULES.md#R-SEC-003) is reworded to say
    that it covers WASM plugins, and that CLAP plugins are crash-isolated, not sandboxed.
  - [ARCH › Crate map](../ARCHITECTURE.md#crate-map): `jstream-plugin-proto` (L0),
    `jstream-plugin-clap` (L2, unsafe) and the `jstream-plugin-host` app.
  - [ARCH › Packaging](../ARCHITECTURE.md#packaging): the host exe in the zip.
  - [ARCH › Threads](../ARCHITECTURE.md#threading): the host's real-time thread and the
    non-waiting rings.
  - [ROADMAP › M3](../../ROADMAP.md#m3): CLAP audio filters, out of process.
  - `design/ui-prototype/README.md`: the CLAP filter is decided, and stays hidden until M3.
