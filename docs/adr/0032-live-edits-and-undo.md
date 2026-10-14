<!-- @doc
id = "ADR-0032"
title = "Live parameter edits and undo on the wire"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0032: Live parameter edits and undo on the wire

<a id="context"></a>
## Context
A UI edits by continuous gestures: dragging an item on the canvas, scrubbing a numeric
field by drag, wheel or arrow keys, moving a fader, and changing a filter in a dialog that
previews live and restores the original on Cancel. Each writes a new value on every
pointer move, about 60 times a second.

[ADR-0013](0013-command-model.md) makes every applied batch one `rev` bump and one undo
step for the client that sent it, and [ADR-0006](0006-mcp-full-access.md) sends every
agent action to the audit log. Sent as batches, one drag would cost 60 `rev` bumps, 60
undo steps, 60 audit entries and 60 `state.changed` broadcasts a second, and every other
client would re-read the state 60 times a second. `rev` would stop meaning "the state
changed in a way someone decided", which is what agents rely on for `expected_rev`.

The engine side is already designed: continuous parameters bypass the `arc-swap`
snapshots and reach the real-time threads through lock-free parameter channels
([ARCH › Threads](../ARCHITECTURE.md#threading),
[R-RT-003](../RULES.md#R-RT-003)). What the protocol sends during a gesture, how the
gesture ends up in the collection and the undo history, and how undo and redo are
requested at all, are not. The protocol leaves undo and redo on the wire open
([jstream-protocol › Open questions](../../crates/jstream-protocol/README.md#open-questions),
question 4), and `jstream-core` keeps no history yet.

Options for continuous gestures:

1. **Every step is a batch.** Simple and already built. It costs everything listed above,
   and needs a second mechanism anyway to fold 60 undo steps into one.
2. **Client-side coalescing only:** the client sends a batch every 100 ms or so. It cuts
   the rate but keeps every cost per batch: about ten `rev` bumps, undo steps and
   broadcasts a second, a choppy engine preview, and no clean Cancel (it has to send a
   batch back to the original value, which is one more undo step).
3. **Server-side coalescing by time window:** batches carry a `coalesce_key`, and the
   engine merges consecutive batches from one client with the same key, within a window,
   into one undo step. Undo gets one step per gesture, but each batch still bumps `rev`,
   is audited and is broadcast, and where the window closes is a guess: a fader held
   still for longer than the window splits into two steps.
4. **Live values plus one commit:** during the gesture the client sends live values that
   change only what the engine renders and plays, never the collection. On release it
   sends one ordinary batch from the start value to the end value.

For undo and redo, the protocol README proposes a fourth kind of call, `history`. An undo
changes the collection and bumps `rev`, but it is not itself an undo step, so it doesn't
fit a batch, and it has no effect outside the collection, so it isn't an action.

<a id="decision"></a>
## Decision
**Continuous gestures use live values and one committing batch (option 4).** Undo and
redo are `history` calls.

### Live values

- **A live value overrides one parameter of the collection at runtime.** It is not
  collection state: it never changes `rev`, never becomes an undo step, is never saved,
  and collection reads (`state.get`, `item.list`) never show it. The engine applies it
  through the parameter channels, so the render and audio threads use it from their
  next tick without a snapshot rebuild.
- **A gesture groups live values.** It has a `gesture_id`, chosen by the client like other
  IDs and unique per client, and belongs to the client that opened it.
- **`live.set {gesture_id, values}`** opens the gesture if it is new and sets its live
  values. `values` is a list of setter edits, the same `Edit` values a batch carries
  (`item.set_transform`, `item.set_crop`, …), so a multi-item drag moves every item in one
  message, and the committing batch is built from the same values. Each value replaces
  that gesture's previous value for the same parameter. Values are validated exactly as
  in a batch (`not_found`, `invalid_argument` with `values.<n>.<field>`). A value for a
  parameter that is not live-adjustable is rejected with `invalid_argument`, and nothing
  in the message is applied.
- **Commit:** on release (or OK in a dialog) the client sends one ordinary batch with the
  final values and a new optional field, `ends_gesture`. The batch is one `rev` bump and
  one undo step, and since the collection was never touched during the gesture, its
  inverse restores the values from before it. When the batch applies, the engine ends the
  gesture in the same step. The parameter channels keep the live values until the render
  and audio threads hold the snapshot with the committed values, so no frame shows the
  old value in between. A batch that fails (`rev_conflict`, `invalid_argument`) leaves the
  gesture open, and the client retries or cancels. A drag commits without
  `expected_rev` by default (last writer wins per field); structural batches keep using
  it.
- **Cancel:** `live.cancel {gesture_id}` drops the gesture's live values, so each
  parameter returns to its collection value. No edit is made.
- **Classification ([R-ARCH-006](../RULES.md#R-ARCH-006)):** `live.set` and
  `live.cancel` are **actions**: their whole effect is outside the collection state,
  they are never undoable and never batchable, and they answer `{"kind":"action"}`.
  `live.list` is a query that returns the open gestures with their owners and current
  values, for a client that connects during one.
- **Live-adjustable parameters** are those the real-time threads can take from a parameter
  channel with no snapshot rebuild and no new resources:
  - item transform and crop (`item.set_transform`, `item.set_crop`), today;
  - audio gains and sends (faders), when their edits exist;
  - fields of source, filter and effect settings that their kind descriptor marks live,
    once kind descriptors exist (ADR-0025, proposed separately, defines that flag; this
    ADR doesn't decide its content).

  Everything else is not: creating, deleting, adding, removing and reordering, names,
  visibility, and settings that pick a device, file, URL or other resource. A dialog
  that edits a draft and applies it on OK needs no live values at all: OK sends one
  batch.
- **Two clients, one parameter:** a parameter is held by at most one open gesture. A
  `live.set` naming a parameter that another gesture holds takes it over: last writer
  wins, as for batches. The gesture that lost it keeps its other parameters, and its
  commit is an ordinary batch. A batch (from anyone) that changes a parameter held by a
  gesture changes the collection, but the live value still wins on output until the
  gesture ends. A batch that deletes an object drops every live value on it.
- **Ending without a commit:** a gesture also ends when its client's session ends: at once
  on a clean close, and after the server's connection liveness timeout when the
  connection dies. Its parameters revert to their collection values. There is no idle
  timeout while the session is alive, because a live dialog can stay open, unchanged,
  for minutes.
- **What other clients see:**
  - `live.changed {gesture_id, client_id, values}` carries the newest value of each
    parameter that changed. The engine coalesces it per gesture to at most one event
    per render tick, and at most 30 a second by default, so slow clients get fewer,
    newer values, never a backlog. The last values before a gesture ends are always
    sent.
  - `live.ended {gesture_id, client_id, outcome, rev?}` closes it. `outcome` is
    `committed` (with the `rev` of the committing batch, whose `state.changed` comes
    first), `cancelled` or `expired`.
  - These are high-rate events in the sense of protocol open question 7. Until event
    subscriptions exist every client gets them, bounded by the coalescing above.
- **Limits:** a client may hold 16 open gestures, and a `live.set` may name 256 values.
  Beyond that it gets `invalid_argument`. The engine never rejects a `live.set` for
  arriving too fast: it keeps the newest value per parameter. A client keeps at most one
  `live.set` per gesture in flight and replaces the pending one with newer values while
  it waits, which paces it to the connection and the engine.

### Undo and redo

- **`history.undo` and `history.redo`** are a fourth kind of call, `history`, beside
  `batch`, `action` and `query`. They carry no command: each asks the engine to apply a
  batch of edits it already holds, the inverse of one of the client's own steps (undo) or
  that step again (redo). Every state change they make is an edit, so R-ARCH-006 still
  covers every command and needs no change, as queries are neither class today.
- **Per client ([ADR-0013](0013-command-model.md)):** each `client_id` has an undo stack
  and a redo stack. Every batch it applies pushes one step and clears its redo stack.
  Another client's batches never touch them. An undo moves the step to the redo stack, a
  redo moves it back, and neither creates a new step.
- **Each applied undo or redo is one `rev` bump**, emits `state.changed` with the
  client's `client_id` and `request_id`, and goes to the audit log, like a batch.
  `state.changed` gains an optional `cause` (`undo` or `redo`).
- **They accept `expected_rev` and `idempotency_key`** like a batch
  ([R-API-004](../RULES.md#R-API-004)). `expected_rev` is checked first and fails with
  `rev_conflict`. Keys share the client's batch keys: a retried undo whose response was
  lost gets the original result and does not undo a second step.
- **Result:** `{"kind":"history","rev":N,"step_rev":S,"undo_depth":U,"redo_depth":R}`.
  `rev` is the revision produced, `step_rev` is the revision the step's batch originally
  produced (it names the step), and the depths say what the UI can enable next.
- **Conflicts:** an undo or redo fails with `undo_conflict` (details: `step_rev`, and the
  `client_id` and `rev` of the conflicting batch) when another client's batch, after the
  step, changed a property the step sets, or created or deleted an object it creates or
  deletes, or when its edits no longer apply. Nothing is applied, and the step is dropped
  from the client's history so that the next undo reaches the step before it. It never
  overwrites the other client's work.
- **Nothing to undo or redo:** `history_empty` (detail: `direction`).
- **Limits:** 100 steps per client by default (an app setting), the oldest dropped first.
  History lives in memory for the session: it is not saved, and a reconnecting client
  that gets a new `client_id` starts with none.
- **Not undoable:** actions (scene switching and transitions, output start and stop,
  going live, OAuth), live values (a gesture's undo step is its commit), queries, and the
  history calls themselves.

### Agents

Every new call is a protocol capability, so MCP exposes it ([ADR-0006](0006-mcp-full-access.md)):
a `history` tool (ops `undo`, `redo`) and a `live` tool (ops `set`, `cancel`, `list`).
An agent's undo is its own. Live values are audited once per gesture, never per
`live.set`: a committed gesture's entry is its committing batch's, and a gesture that
ends cancelled or expired gets one entry with its parameters and outcome, so an agent's
live change that never commits is still logged.

<a id="consequences"></a>
## Consequences
- **`rev` stays meaningful.** A drag, a fader move or a live dialog is one `rev` bump, one
  undo step and one audit entry, so `expected_rev` keeps protecting agents'
  read-modify-write, and undo matches what the user did.
- **Cancel and preview are free:** nothing to undo, and no batch to send back.
- **Protocol additions, all minor ([R-API-001](../RULES.md#R-API-001)):** the actions
  `live.set` and `live.cancel`; the query `live.list`; the events `live.changed` and
  `live.ended`; the `history` call kind and result with `history.undo` and
  `history.redo`; the optional `ends_gesture` on batches and `cause` on `state.changed`;
  the error codes `undo_conflict` and `history_empty`; and schema snapshot updates
  ([R-TEST-002](../RULES.md#R-TEST-002)). The classification test's list of call kinds
  grows by `history`, and it must check that a `history` call carries no command.
- **Older clients:** a 0.x client that doesn't know `live.changed` ignores it and sees
  only committed state, which is correct, just not live.
- **`jstream-core` must build** the per-client undo and redo stacks from the inverse
  batches `jstream-graph` already returns, the conflict check (the properties and objects
  each step touched, and which later batches touched them), the gesture table with its
  takeover and expiry rules, and the handover from live values to the committed snapshot.
  `jstream-graph` needs a "touched set" for each applied batch.
- **The real-time threads need parameter channels** that can be overridden per parameter
  and cleared, for transforms and crops first. A live value is applied on the next tick
  at the latest, so the preview follows the pointer.
- **`jstream-server` needs a liveness check** (WebSocket pings with a timeout), which it
  doesn't have yet, so a dead connection ends its session and expires its gestures
  instead of leaving live values in place until the socket errors.
- **UIs:** canvas drags, scrubbable fields, faders and live dialogs send `live.set`, commit
  with `ends_gesture`, and cancel with `live.cancel`. They show other clients' gestures
  from `live.changed`.
- **Undo can fail** for a user whose step another client has since changed. The UI shows
  `undo_conflict` and moves on to the step before it.
- **Still open:** event subscriptions (protocol open question 7), which will let a client
  opt out of `live.changed`; and whether undo history should survive a reconnect, which
  needs a resumable client identity first.
