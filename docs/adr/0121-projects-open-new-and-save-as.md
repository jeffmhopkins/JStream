<!-- @doc
id = "ADR-0121"
title = "Projects: open, new and save as, over the protocol"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0121: Projects: open, new and save as, over the protocol

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
An engine holds one collection, loaded from the file its host names and replaced
atomically after every change ([ARCHITECTURE › Persistence](../ARCHITECTURE.md#persistence)).
The desktop app always names `collection.json` in its data folder, and the daemon takes
`--collection <path>`. No UI or client can save the setup to another file, open another
one, or start an empty one (#1218). Agents should be able to switch projects as people
do ([ADR-0006](0006-mcp-full-access.md)).

Three things make a switch more than a load at startup:

- Clients are connected. They hold copies of the state, undo stacks
  ([ADR-0013](0013-command-model.md)) and open gestures
  ([ADR-0032](0032-live-edits-and-undo.md)) that name the old collection's objects.
- Outputs may be running, drawing the old collection.
- Stream keys sit in the OS credential store under the output's ID alone
  ([R-SEC-001](../RULES.md#R-SEC-001)). The UI's IDs are predictable
  (`output-<client>-<n>`), so two projects easily share an output ID. Opening another
  project would then pair one project's key with another project's server, which #343
  closed for edits.

<a id="decision"></a>
## Decision
1. **A project is one collection file.** It is the collection JSON as today, at any
   `schema_version` the engine reads, and its name is any name. The UI suggests `.json`.
   Assets with the `collection` root are relative to the file's folder, so a folder
   holding the file and its assets moves or is shared as one.
2. **Protocol 0.93 adds three actions, a query and an event.** All answer `unsupported`
   when the host named no collection file.
   - `collection.open { path }` loads the file at `path` and saves there from then on.
   - `collection.new { path }` creates an empty collection at `path` (the four sample
     labels, the default canvas and rate) and saves there. An existing file is
     `already_exists`.
   - `collection.save_as { path, overwrite }` writes the current collection to `path` and
     saves there from then on. An existing file is `already_exists` unless `overwrite`
     is true. Saving to the open file is `invalid_argument`.
   - `collection.info` answers the open file's `path` and up to ten `recent` paths, most
     recent first, the open one among them.
   - `collection.file_changed { path, cause, client_id, request_id }` tells every client
     that the engine saves to another file, with `cause` `open`, `new` or `save_as`.

   A path is absolute and follows the rules of an `absolute` asset path
   (`check_asset_path`): no `..`, UNC share, device or NT object path. It is a path on the
   engine's machine. A file the engine can't read or load is refused with
   `action_failed` and its reason, and the open project stays open with nothing stopped.
3. **Opening or creating replaces the collection.** The new collection is loaded and
   checked first, with no lock held. Then every output that isn't stopped stops, as an
   `output.stop` would, with its events. Then the collection is swapped:
   - `rev` becomes the file's or one past the current one, whichever is greater, so no
     `expected_rev` from before matches.
   - Every client's undo and redo stacks and idempotency keys are cleared, open gestures
     are cancelled, timelines stop, and the first scene goes on program and preview.
     Shared values ([ADR-0102](0102-page-api-for-browser-sources.md)) are not collection
     state and stay.
   - Sources restart from the new collection, with its folder behind `collection` assets.
   - Every client gets `state.changed` (so a client older than 0.93 reads the state
     again), `studio.changed` and `collection.file_changed`.
4. **Save as keeps `collection` assets pointing at the same files.** When the new
   folder differs, each `collection` asset reference is rewritten: relative to the new
   folder when the file is under it, otherwise `absolute`. A rewrite is a new `rev` and
   clears undo history, as in §3, because an inverse edit would bring back the old
   relative path. Assets are not copied. Nothing stops.
5. **Stream keys belong to one project file.** The file holds a random `key_scope`, and
   an output's key is stored under `<key_scope>/<output_id>`. A new file and a save as
   get a fresh scope; save as copies each output's key to it. A file opened without a
   scope gets a fresh one, so its outputs start with no key. The file the host loads at
   startup without a scope keeps its keys: they move from the bare output ID to the new
   scope once the engine has its key store, and the bare entries are removed. Hosts read
   a key through the engine's name for it (`Engine::stream_key_name`).
6. **The engine keeps the recent list** in a file the host names
   (`recent-collections.json` in the data folder), updated on every open, new and save
   as. The desktop app opens the most recent project at startup if it still loads, and
   `collection.json` otherwise. The daemon's `--collection` stays as it is.
7. **People and agents, not plugins.** MCP gets a `collection` tool, marked
   `destructiveHint` ([ADR-0098](0098-mcp-destructive-hint-for-what-undo-cant-take-back.md)):
   an open stops outputs and clears undo. WASM plugins never get these commands, so a
   plugin's grant of folders never reaches a collection file. A hotkey can't bind them.

<a id="consequences"></a>
## Consequences
- This stays inside ADR-0006: a client with the control token can already read any file
  an `absolute` asset names and write where a recording output points. A project file
  adds one more file it can name, with the same path rules.
- A project shared with someone carries no secret, and its outputs ask for keys again.
  Two copies of one file made outside JStream share a scope, and so share keys.
- The UI's File menu (New project…, Open project…, Save project as…, Open recent, and
  the open project's name) builds on this in its own change.
- An engine before 0.93 refuses a file that holds a `key_scope`, as it refuses any field
  it doesn't know, and would not find the keys under their scoped names.
