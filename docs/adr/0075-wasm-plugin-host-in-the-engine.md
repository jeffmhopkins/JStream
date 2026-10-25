<!-- @doc
id = "ADR-0075"
title = "The WASM plugin host runs in the engine, and a plugin's package"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0075: The WASM plugin host runs in the engine, and a plugin's package

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0069](0069-wasm-plugin-limits-and-capabilities.md) settles what a WASM automation
plugin may do and what happens when it misbehaves, and says "the engine" disables it and
gives it a scoped `Control`. It doesn't say where the wasmtime host
(`jstream-plugin-wasm`) runs, nor what `plugin.install`'s `path` names: the protocol calls
it "the plugin's package: its manifest and module" without a layout. WA1's engine side
needs both.

The host could run in three places:
1. **In `jstream-core`**, on threads of the engine's process. The engine already owns the
   sessions, events and threads a plugin needs, and `jstream-plugin-wasm` is layer 3, so
   core (layer 5) may depend on it. It brings wasmtime into every build of core.
2. **In the daemon**, behind a trait core calls. Core stays free of wasmtime, but the
   trait has to carry sessions, events and disabling back and forth, and the in-process
   app (`jstream-desktop`) would need the same wiring a second time.
3. **In a process of its own**, as CLAP plugins are
   ([ADR-0035](0035-clap-plugins-out-of-process.md)). A native plugin can crash its host,
   so CLAP needs that. A WASM plugin can't: it has no native code, and every fault is a
   trap the host catches. [ADR-0005](0005-plugins-wgsl-and-wasm.md) and ADR-0035 treat
   WASM plugins as sandboxed code that can't crash the engine, though neither says where
   the host runs.

<a id="decision"></a>
## Decision
- **The host runs in `jstream-core`**, behind the cargo feature `wasm-plugins`, which is
  on by default. A build without it answers `plugin.install` with `unsupported` and
  `plugin.grant` and `plugin.enable` with `not_found`, and leaves wasmtime out.
- **Threads.** Each running plugin has a thread of its own, which loads its module and
  runs it, and, when it handles events, a second thread that moves its events into the
  host's capped queue. They never run on the render, audio, output or async threads,
  and they hold the engine weakly, so installed plugins don't keep it alive. Neither
  ever holds the collection lock while the plugin runs.
- **The package** is a directory on the engine's machine, named by an absolute path. It
  holds `plugin.wasm`, the module, and may hold `manifest.json`, a JSON object. This
  version reads only the manifest's `name`, the plugin's own text (`remote_name`), and
  ignores fields it doesn't know, so a later version can add the classes a plugin asks
  for. Like the ABI, the layout is version 0 and unstable.
- **Events.** A plugin that exports `on_event` gets each engine event, in order, as JSON
  for that invocation's input. One that doesn't export it gets none. That is every event
  a client gets, the people-only nouns' (`youtube.*`, other plugins' `plugin.*`) among
  them: ADR-0069 puts the event subscription under `read`, which every plugin has, and
  none of these events carries a secret. Unlike MCP's feed for agents, the plugin feed
  filters nothing.
- **Refusals.** The engine's plugins get the protocol's `forbidden` error, not the ABI's
  `STATUS_DENIED` and `STATUS_LIMITED`, for a call outside their grant and past the cap
  of commands a second, as ADR-0069 says. The host offers both, and the engine picks
  `forbidden`.

<a id="consequences"></a>
## Consequences
- Every build of the engine with the default features compiles wasmtime. It was already in
  the workspace for `jstream-plugin-wasm`, so the dependency tree doesn't change.
- A bug in wasmtime itself could take down the engine. That is the residual risk of
  treating WASM plugins as sandboxed in-process code, and moving the host out of process
  later would need no protocol change.
- An installed plugin lasts for the engine's run. Saving installed plugins and their
  grants, the manifest's classes and the `needs_grant` check on an update, and each
  plugin's private data folder are later WA1 work.
- The engine checks a plugin's call against its grant after the host's cap of commands a
  second, so a call outside the grant counts against the cap.
