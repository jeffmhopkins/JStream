<!-- @doc
id = "ADR-0069"
title = "WASM plugin resource limits and the Control calls a plugin may make"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0069: WASM plugin resource limits and the Control calls a plugin may make

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
WASM automation plugins (roadmap WA1, M3) run in [wasmtime](https://wasmtime.dev/) and call
the API through `Control` ([ADR-0035](0035-clap-plugins-out-of-process.md), which keeps
ADR-0005's WASM decision). [R-SEC-003](../RULES.md#R-SEC-003) and `SECURITY.md` promise
that a WASM plugin gets no filesystem or network access unless its manifest declares it and
the user grants it. The threat model treats a plugin as **untrusted** code the user installed
([THREAT-MODEL › Plugins](../security/threat-model.md#plugins)). Issue #348 lists what
nothing decides yet:

- **CPU.** A plugin that loops forever holds its thread. It also holds whatever engine work
  waits on it.
- **Memory.** Nothing caps its linear memory, tables or instances.
- **API scope.** `Control` is full access with no permission tiers
  ([ADR-0006](0006-mcp-full-access.md)). A plugin with the whole of `Control` needs no
  network grant to reach the network:
  - `output.set_target` and `output.start` publish the program to any server;
  - a `browser` source loads any URL;
  - a media or image source reads any file the user can, and `video.snapshot` returns it.

  The sandbox promise is moot until `Control` is narrowed. #343 already stops a retarget
  from carrying the old stream key along, but the program itself still goes out.
- **Untrusted output.** Names and text a plugin sets reach the UI and MCP agents. ADR-0059
  already lists "plugin-supplied names" as third-party text.

These choices set the app's security posture toward installed code. That makes this
maintainer tier under [R-GOV-005](../RULES.md#R-GOV-005). No accepted ADR or open PR decides
them. Wasmtime offers two ways to interrupt guest code:
- **Epoch interruption** is a counter the host advances on a timer. A store traps when it
  passes its deadline, and the cost is close to nothing.
- **Fuel** is a per-instruction budget. It is deterministic, but it slows guest code
  noticeably.

A per-store resource limiter (`StoreLimits`) caps memory, table and instance growth. A
growth past the cap fails, and that normally ends in a trap.

<a id="options"></a>
## Options

**CPU**
- **C1, epoch deadline per call (recommended).** The host ticks the epoch every 10 ms. Each
  entry into the plugin (init, an event handler, a timer) gets a wall-clock budget: 1 s for
  init, 100 ms for the others. The budget is re-armed after each host call returns, so time
  the engine spends answering `Control` doesn't count against the plugin. Plugins run on
  their own threads, never on the render, audio or output threads.
- **C2, fuel per call.** The same budgets, counted in instructions. Runs are reproducible,
  but plugin code runs slower, and budgets don't map to wall time.
- **C3, no limit; a watchdog kills the thread.** Rust threads can't be killed safely, so in
  practice this means a process per plugin. That is more isolation than WA1 needs.

**Memory**
- **M1, a fixed cap (recommended).** One instance, one linear memory of at most 64 MiB, and
  one table of at most 10,000 elements per plugin. Growing past a cap fails, and the plugin
  is treated as having trapped.
- **M2, manifest-declared, up to a ceiling.** The manifest asks for up to 512 MiB, and the
  grant dialog shows the figure. This is more flexible, and it adds one more thing to
  grant.

**API scope**
- **S1, full `Control`.** Plugins are token holders, as ADR-0006 makes agents. This is
  simplest, but it breaks R-SEC-003's promise in practice, as above.
- **S2, capability classes in the manifest, granted at install (recommended).** The host
  hands each plugin a scoped `Control`. That is a wrapper with its own session, so the audit
  log and undo name the plugin (ADR-0013). The wrapper checks every command against a fixed
  table that maps each protocol command to one class:
  - `read` (always granted): every query, `kind.*`, `delete.plan` and event subscription.
  - `edit`: scene, item, source, filter, transition, bus, send, hotkey, canvas and
    studio edits, live parameter edits (`live.*`, ADR-0032), plus `history.*` on the
    plugin's own session. Excluded are the edits
    that reach the network or the filesystem: a `browser` source's URL, and file and URL
    settings of media and image sources. Those need `network` or `filesystem` as well,
    checked against the grant's hosts or folders.
  - `live`: actions that change what is on air: `scene.switch` and studio
    transitions.
  - `outputs`: `output.start` and `output.stop` on outputs that already exist. It never
    covers `output.create`, `set_target`, `set_key`, `clear_key` or `delete`, and never
    `encoder.*`.
  - **Never granted to a plugin:**
    - `output.set_target`, `set_key`, `clear_key`, `create` and `delete`;
    - `youtube.*`;
    - `pairing.*` and `device.*`;
    - `video.snapshot` and `preview.subscribe` without `filesystem`. The pixels of a
      user's capture are data a plugin could leak through any granted host.

  A denied call fails with a new error code, `forbidden`. The code is additive: clients
  already treat an unknown code as an error. Commands added later default to "never" until
  a PR places them in a class, and a test checks that every command is placed (alongside
  `every_command_is_classified`).
- **S3, a per-method allowlist in the manifest.** This is the finest-grained option, but
  the grant dialog becomes a list of protocol methods. Every protocol change could also
  break a manifest.

**Who installs and grants**
- **G1, people only (recommended).** Installing a plugin, granting or widening its
  capabilities, and re-enabling a disabled plugin are WebSocket API calls, not MCP tools.
  The UI shows the manifest's classes, hosts and folders and asks the person to confirm.
  This follows ADR-0058, where `youtube.*` is for people only. A prompt-injected agent
  could otherwise install a plugin and grant it everything, which would launder its own
  full access into code that keeps running after the conversation ends.
- **G2, any token holder.** This keeps ADR-0006's "no tiers" for agents. Grants then protect
  only against the plugin, not against an agent misled into installing one.

**Untrusted output**
- **U1 (recommended).** Text a plugin supplies about itself (its name, description,
  messages) appears only in `remote_*` fields, per ADR-0059. Names a plugin sets on scenes
  and sources are ordinary state: they keep the existing length caps, and the plugin's
  session id in the audit log and history attributes them. The MCP instructions already
  say that state text can come from anyone with access.

<a id="decision"></a>
## Decision
**Proposed, awaiting the maintainer:** C1, M1, S2, G1 and U1.

- **CPU.** Epoch interruption gives each entry into a plugin 100 ms of wall time (init
  gets 1 s), re-armed after each host call. A plugin's trap, whether from the CPU deadline,
  a memory cap or its own fault, drops its instance and **disables** the plugin. It is
  never restarted on its own. The engine logs the reason (`cpu_limit`, `memory_limit` or
  `trap`) and emits it as a plugin state event. Only a person re-enables the plugin.
- **Memory.** One instance, at most 64 MiB of linear memory and 10,000 table elements per
  plugin. The host also caps each plugin's event queue at 1,000 events, dropping the oldest
  as other clients do, and caps its commands at 50 a second. Past the command cap, calls
  fail with `forbidden` and no trap.
- **API scope.** Each plugin gets a scoped `Control` with its own session. The manifest's
  capability classes are checked command by command against one table in the protocol
  crate, and every command must be placed in a class or under "never".
- **Grants.**
  - **Filesystem and network.** R-SEC-003's grants are concrete. Network means the hosts
    the manifest names (scheme, host and port). Filesystem means a private data folder per
    plugin, plus folders the person picks.
  - **The command classes need them too.** A capability class never widens network or
    filesystem reach: a command that would reach a host or path needs the matching grant.
  - **Changes need a new grant.** A plugin update that asks for more capabilities stays
    disabled until a person grants the difference.
- **People only.** Install, grant, widen and re-enable are WebSocket API calls only, with
  no MCP tool.
- **Untrusted output.** Plugin-supplied text about the plugin is `remote_*`. Changes a
  plugin makes are attributed to its session.

<a id="consequences"></a>
## Consequences
- **What it fixes.** R-SEC-003's promise holds in practice: without a grant, a plugin can't
  publish, load a page or read a file through `Control`, and it can't hang or exhaust the
  engine.
- **What WA1 must build.**
  - The scoped `Control` wrapper and its command-to-class table, with a test that every
    command is placed.
  - The `forbidden` error code.
  - The plugin state event.
  - The epoch ticker.
  - The `StoreLimits` setup.
  - The grant flow in the UI.
  - `jstream_plugin_wasm::no_ambient_capabilities`, the test R-SEC-003 already names.
- **Protocol churn.** Every new command needs a class. The test makes forgetting one fail
  CI instead of granting it silently.
- **Plugins stay limited.** A plugin can't make outputs, change targets or keys, or drive
  YouTube. Automation that needs those stays with the user or an agent. Wider classes can
  come later, in a new ADR.
- **Agents.** Under G1 an agent can't install or grant plugins. That is the first
  people-only area besides `youtube.*`, and ADR-0006's "no tiers" stays true of the API an
  agent drives. Under G2, this consequence and its protection both go away.
- **Budgets.** 100 ms, 64 MiB and 50 commands a second are starting figures. Changing them
  later is an agent-tier tuning change, unless it removes a limit.
