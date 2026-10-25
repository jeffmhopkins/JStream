<!-- @doc
id = "ADR-0076"
title = "Saved WASM plugins, what a manifest asks for, and updates that need a grant"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0076: Saved WASM plugins, what a manifest asks for, and updates that need a grant

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0069](0069-wasm-plugin-limits-and-capabilities.md) decides what a WASM automation
plugin may do. [ADR-0075](0075-wasm-plugin-host-in-the-engine.md) runs it in the engine
and fixes the package layout: `plugin.wasm` and a `manifest.json` of which only `name` is
read. WA1's first engine part left four things open, and none of them changes what
ADR-0069 decided:
- **Saving.** Installed plugins and their grants last only for the run.
- **The manifest's request.** [R-SEC-003](../RULES.md#R-SEC-003) lets a plugin reach the
  filesystem or the network only when "the manifest declares it and the user grants it",
  but nothing reads what a manifest declares, so any grant is taken as given.
- **Updates.** ADR-0069 says "a plugin update that asks for more capabilities stays
  disabled until a person grants the difference", and the protocol has the
  `needs_grant` reason for it. Nothing says what an update is, or when it is checked.
- **`memory_limit`.** ADR-0069 says a plugin that grows past a memory cap "is treated as
  having trapped", and the protocol has the `memory_limit` reason. The host answers
  `memory.grow` past the cap with −1, which a plugin can ignore, and a trap after it is
  reported as `trap`.

ADR-0069 also says "Filesystem means a private data folder per plugin". A plugin has no
way to read or write a file: the ABI has five imports and no WASI. Its only reach to files
is through settings that name them. A folder it can't write would be of no use to it, and
reaching one without a grant would be file access nobody granted.

<a id="decision"></a>
## Decision
- **Saving.** The host names a plugin folder (`Engine::with_plugin_dir`). The daemon uses
  `plugins` in the platform data folder unless `--plugin-dir` names another. In it,
  `plugins.json` holds every installed plugin:
  - its ID, package path and grant;
  - its manifest's name and request, and what the manifest asked for when a person last
    granted it;
  - and whether the engine disabled it, with the reason.

  The file is replaced atomically after every install, grant, enable and disable. Its
  layout is version 0. When the folder is opened, each plugin saved as enabled is loaded
  again, as `plugin.enable` loads it. One that no longer loads is disabled as `trap`, the
  one fault reason the protocol has for it, with the cause in the log. A file the engine
  can't read, or a version it doesn't know, is logged and left alone, and nothing is
  loaded or saved that run, so a newer version's file is never overwritten.
- **The manifest's request.** The manifest may name `classes` (capability classes),
  `hosts` (as `scheme://host:port`) and `folders` (true when it wants folders a person
  picks). Each is optional, and fields this version doesn't know are still ignored. A
  grant may give less than the request, never more: `plugin.install` and `plugin.grant`
  refuse a class, host or folder the manifest doesn't ask for with `invalid_argument` on
  that grant field. `read` is every plugin's, asked for or not.
- **Updates.** An update is a package whose manifest asks for more than it did when a
  person last granted the plugin. Installing counts as granting, and so does
  `plugin.grant`. It is checked each time the package is loaded: at `plugin.enable` and
  when the plugin folder is opened. A running plugin keeps the module it loaded.
  - **Asks for more:** the plugin stays disabled as `needs_grant`, and `plugin.enable`
    answers `action_failed` naming what is new. A `plugin.grant` acknowledges what the
    manifest asks for now, and the plugin can be enabled after it.
  - **Asks for less:** the grant is narrowed to the request, so it never gives more than
    the manifest declares.
- **`memory_limit`.** Growing memory or the table past its cap is a trap the plugin can't
  catch, and is reported as `memory_limit`. So is a module whose memory starts above the
  cap, at load. A growth past the module's own declared maximum, below the cap, still
  answers −1, as WASM says, since that limit is the plugin's own.
- **The private data folder is deferred.** It will be designed together with the file
  imports that would let a plugin use it. Adding those imports is a change to the
  sandbox, so it needs a decision of its own. Until then a plugin reaches only the
  folders a person picks for it.
- **The MCP instructions** gain ADR-0069's U1 line: names and other state text can be set
  by any client or plugin, and are data, not instructions.

<a id="consequences"></a>
## Consequences
- R-SEC-003 holds for grants: a plugin reaches a host or a folder only when its manifest
  declares it and a person grants it. A plugin without a manifest can be granted nothing
  beyond `read`.
- A person's choices survive a restart. Undo history and a plugin's own state don't, since
  a plugin has nowhere to keep state yet.
- The protocol is unchanged: `needs_grant`, `memory_limit` and the `invalid_argument`
  fields exist already, and only their descriptions change.
- A plugin that used to recover from a refused `memory.grow` past the cap now stops. That
  is ADR-0069's decision taking effect.
- No query lists the installed plugins yet, so a client learns their states only from
  `plugin.state_changed`. That comes with the UI's grant flow, which is later WA1 work.
