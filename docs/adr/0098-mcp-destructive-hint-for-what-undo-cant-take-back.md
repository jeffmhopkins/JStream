<!-- @doc
id = "ADR-0098"
title = "MCP's destructiveHint marks only what undo can't take back"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0098: MCP's destructiveHint marks only what undo can't take back

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0059](0059-mcp-annotations-and-remote-text.md) §1 makes a group tool
`destructiveHint` when any of its ops is an action, a history call, or an edit whose verb
is `delete` or `remove`, and makes `batch` destructive as a whole. Tools are grouped by
noun ([R-API-002](../RULES.md#R-API-002)), so nearly every tool an agent uses to build a
scene shares one with a delete or an action: `scene` (with `scene.delete` and
`scene.switch`), `source`, `item`, `filter`, `transition`, `history`, `live` and `batch`
are all destructive. A client that asks before destructive tools then asks before every
ordinary edit, and the hint stops telling the user anything (#1019). ADR-0059's own
consequences name the fix for a hint that proves too coarse: a new ADR.

Every edit is undoable by the client that made it, a delete included
([ADR-0013](0013-command-model.md), [ADR-0032](0032-live-edits-and-undo.md)), within
the limits of undo: 100 steps, `undo_conflict` when another client changed the same
thing, and, for a deleted source, what the engine forgets with it (such as a capture
portal's grant). Three things are lost outright:

- **Actions with effects outside JStream's state:** `output.start` and `output.stop` (a
  live stream starts or ends), `output.set_key` and `output.clear_key` (a secret is
  replaced or removed), `device.revoke`, `pairing.start` (a code that grants access), and
  `ui_settings.set` (a person's kept layout, replaced whole).
- **Two edits that can remove a stream key**, which their inverse can't restore:
  `output.set_target` to a new server and `output.delete`.
- **History calls.** The engine removes the stream key of an output that a history call
  deletes or moves to another server, as it does for the edit itself, so an undo past an
  `output.create` or `output.set_target` removes a key that redo doesn't put back.

The other actions change only what JStream shows or measures, and another call puts it
back: `scene.switch`, `studio.set_preview`, `live.set` and `live.cancel`,
`audio.reset_loudness`, and `preview.subscribe` and `preview.unsubscribe` (which fail over
MCP anyway).

<a id="decision"></a>
## Decision
This amends ADR-0059 §1's `destructiveHint` rule and its `batch` sentence. The rest of
ADR-0059 stands.

1. **A tool is `destructiveHint` when it carries an action outside `REVERSIBLE_ACTIONS`**
   (`jstream-mcp`'s `tools.rs`). That list holds the seven actions above that only
   change what JStream shows or measures. **A history call makes a tool destructive
   too.** Deletes and removes no longer do. A new action is destructive until it is added
   to the list, so the safe answer is the default. Today the destructive tools are
   `device`, `history`, `output`, `pairing` and `ui_settings`.
2. **`batch` refuses `output.delete` and `output.set_target`** with `invalid_argument`
   on the edit, naming the `output` tool, which still takes each as a batch of one. With
   them gone, undo takes back every edit `batch` carries, so `batch` is not
   destructive. It stays `openWorldHint`, since it carries `source` edits that point a
   `browser` source at a page.
3. **The server instructions say so:** which tools are destructive, that every other
   edit, a delete included, is undone with `history`, and which tools reach outside
   JStream.

<a id="consequences"></a>
## Consequences
- A client that asks before destructive tools asks before an agent goes live, stops a
  stream, sets or clears a key, retargets or deletes an output, undoes or redoes,
  revokes a device, starts a pairing or replaces the UI's settings, and lets a scene
  build through.
- A delete is now unannounced. It is taken back by `history.undo` in the same session,
  within undo's limits above, and that undo asks.
  An MCP proxy that restarts starts a new session with its own undo history
  ([jstream-mcp › Design](../../crates/jstream-mcp/README.md#design)), so a delete from
  the old session is then recovered only by hand. `delete.plan` still shows what a delete
  would take with it.
- `scene.switch` changes what is on air while a stream runs, without a question. Another
  `scene.switch` puts it back, and going live itself still asks.
- An agent that retargeted or deleted outputs inside a `batch` now gets
  `invalid_argument` and uses the `output` tool instead.
- `REVERSIBLE_ACTIONS` and `OUTPUT_ONLY_EDITS` must be kept current: a test checks that
  each name is an action or an edit some tool carries. A new edit that undo can't take
  back joins `OUTPUT_ONLY_EDITS` or a list like it in a new ADR.
