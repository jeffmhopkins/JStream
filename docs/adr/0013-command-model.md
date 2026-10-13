+++
id = "ADR-0013"
title = "Command model: edits, actions, revisions, per-client undo"
kind = "adr"
status = "accepted"
+++

# ADR-0013: Command model: edits, actions, revisions, per-client undo

<a id="context"></a>
## Context
Humans and agents edit the same setup at the same time. An "atomic batch" can't contain
side effects like starting a stream, and a single shared undo stack would let an agent undo
the user's work.

<a id="decision"></a>
## Decision
- **Edits** change the collection state. They are undoable, batchable and atomic. Each
  applied edit or batch increments the state `rev`.
- **Actions** have side effects (start/stop outputs, transitions, go-live, OAuth). They are
  never undoable and never batchable. A `script` runs actions in sequence, with one result
  per step.
- Edits accept `expected_rev`, which rejects the edit if the state has moved on
  (optimistic concurrency), and an idempotency key that makes retries safe.
- **Undo is per client** (per connection identity). An undo whose inverse conflicts with
  later edits from another client fails, rather than overwriting their work.

<a id="consequences"></a>
## Consequences
- Agents get safe read-modify-write with no hidden races, and humans keep control of
  their own undo history.
- Every command must be classified ([R-ARCH-006](../RULES.md#R-ARCH-006)), and every edit
  needs an inverse.
