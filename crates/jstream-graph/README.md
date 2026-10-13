<!-- @doc
id = "CRATE-jstream-graph"
title = "jstream-graph"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-graph

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-graph/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map", "docs/ARCHITECTURE.md#data-model", "crates/jstream-protocol/README.md#design"]
verified = { hash = "b3:b036665d1d8f73f0", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "New section: checked model bullets against model/mod.rs (BTreeMap sources, scenes Vec, items bottom-first), apply bullets against apply.rs (rev_conflict then empty_batch, working copy, edits.<n> fields, reverse-order inverse, reject-not-cascade deletes, item.add-only cycle check), tests against tests.rs (40 seeds x 150 batches = 6000, undo and redo); depends_on crate-map row, data-model and protocol design match" }
-->

The scene graph of a collection, and the only code that changes it. Layer 2: plain data,
no GPU, no I/O, no `unsafe`. It depends only on
[`jstream-protocol`](../jstream-protocol/README.md): its edits are the protocol's `Edit`s,
its IDs are the protocol's ID types, and its errors are the protocol's `ProtocolError`
codes, so the engine passes them to clients unchanged.

**Model** (`model/`, [ARCH › Data model](../../docs/ARCHITECTURE.md#data-model)):
- `Graph`: the `State` plus its `rev`.
- `State`: the `sources`, keyed by ID, and the `scenes` in display order.
- `Source`: a single shared instance with an ID, a name and a `SourceKind` (`builtin` by
  name, or `scene`, a nested scene). `item.add` places an existing source by reference,
  so one source can sit in many scenes, or several times in one, and is still one
  capture. `source.duplicate` makes a separate copy under a new ID. Removing an item
  never deletes its source.
- `Scene`: a name and its items in stacking order, bottom first.
- `Item`: the source it places, a `transform`, a `crop` and `visible`.

**Applying edits** (`apply.rs`): `Graph::apply(edits, expected_rev)` (or `apply_batch`)
checks `expected_rev` (`rev_conflict`) and rejects an empty batch (`empty_batch`). Then it
applies the edits in order to a working copy of the state. The first edit that fails
aborts the batch with `not_found`, `already_exists` or `invalid_argument` (field
`edits.<n>.<field>`), and nothing changes. On success the copy replaces the state, `rev`
goes up by exactly one, and the result carries the **inverse batch**.

- **Inverses** are computed from the state before each edit, and the batch's inverse is
  its edits' inverses in reverse order. They are plain protocol edits, as the protocol's
  Undo design requires: removing an item is undone by `item.add`, then `item.reorder`,
  `item.set_crop` and `item.set_visible` only where the item differed from what
  `item.add` produces, and deleting a scene by `scene.create`, `scene.reorder` and each
  item's restore. The inverse carries no `expected_rev`: the engine's per-client undo
  history (in `jstream-core`) chooses it.
- **References are never left dangling.** `source.delete` is rejected while an item
  places the source, and `scene.delete` while a `scene` source shows the scene. The
  error names the blockers, and the client removes them first, in the same batch if it
  likes. Rejecting rather than cascading keeps each edit's effect, and its inverse,
  limited to the object it names.
- **No cycles.** `item.add` of a `scene` source is rejected with `invalid_argument` if
  the placed scene is the target scene or contains it at any depth. The reason names
  the cycle (`a -> b -> a`). Only `item.add` adds nesting, so it is the only edit
  checked.

**Tests** (`tests.rs`): validation and error codes per edit, shared sources, cycles
(self, 2- and 3-cycles, legal diamonds), exact inverses, and a property test: a seeded
generator applies 6000 random batches. After each successful one, applying its
inverse must restore the state exactly, and applying the inverse's inverse (redo) must
restore the batch's result. After each failed one, the graph must be unchanged.

<a id="follow-ups"></a>
## Not yet
<!-- @section
-->

Planned, and kept possible by the private fields of the model types:

- **Persistence and migrations.** A versioned JSON collection file with
  `schema_version` and forward migrations ([R-API-003](../../docs/RULES.md#R-API-003),
  [ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence)) belongs here, in
  `src/persist/`. Deferred to keep this first cut reviewable; nothing is saved yet.
- **Filters and effects:** `filters[]` on sources (applied wherever the source is
  placed) and `effects[]` on items (per placement), with their edits.
- **Source settings** for builtin kinds, and `source.rename`, come with `jstream-sources`.
- The full 3D transform, and blend modes, replacing the protocol's 2D `ItemTransform`.
- Item lookup is a linear scan over scenes. An ID index can come once collections grow.
