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
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-graph", "docs/ARCHITECTURE.md#data-model", "crates/jstream-protocol/README.md#design"]
verified = { hash = "b3:e49e29924f4ca5a7", by = "agent:claude@session_01Vy4KuKBCruDTJakQ9j2gFA", note = "Merge of main (#198, #173, #190, #191) into B6: re-read; scene role, Groups and moves bullet and item.move cycle check hold" }
-->

The scene graph of a collection, and the only code that changes it. Layer 2: plain data,
no GPU, no I/O, no `unsafe`. It depends only on
[`jstream-protocol`](../jstream-protocol/README.md): its edits are the protocol's `Edit`s,
its IDs are the protocol's ID types, and its errors are the protocol's `ProtocolError`
codes, so the engine passes them to clients unchanged.

**Model** (`model/`, [ARCH › Data model](../../docs/ARCHITECTURE.md#data-model)):
- `Graph`: the `State` plus its `rev`, and the kind descriptors it validates `builtin`
  sources against. The engine supplies them (`Graph::with_kinds`); they are not
  collection state, and `Graph::new()` describes none.
- `State`: the `sources`, keyed by ID, the `scenes` in display order, and the
  `outputs`, keyed by ID.
- `Source`: a single shared instance with an ID, a name, a `SourceKind` (`builtin` by
  kind ID, or `scene`, a nested scene) and its `Settings`: every parameter of its kind,
  or none for a `scene` source. `item.add` places an existing source by reference,
  so one source can sit in many scenes, or several times in one, and is still one
  capture. `source.duplicate` makes a separate copy under a new ID. Removing an item
  never deletes its source.
- `Scene`: a name, a `role` (`scene` or `group`, ADR-0024 §1; it changes no behaviour
  here) and its items in stacking order, bottom first.
- `Item`: the source it places, a `transform`, a `crop` and `visible`.
- `Output`: an ID and an `OutputKind` (so far RTMP to a generic URL, or a recording
  to an MP4 file). Whether it
  runs is the engine's runtime state, and its stream key is in the OS credential store,
  never here ([R-SEC-001](../../docs/RULES.md#R-SEC-001)).

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
  item's restore (with `scene.set_role` for a group). The inverse carries no `expected_rev`: the engine's per-client undo
  history (in `jstream-core`) chooses it.
- **Settings** ([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)):
  `source.create` of a `builtin` source needs a kind the graph describes, checks each
  given value with the protocol's `ParamDescriptor::check`, and stores the complete map
  with defaults filled in. `source.set_settings` checks and applies a patch (`null`
  resets a parameter to its default) and its inverse is the patch of the old values.
  `source.rename` renames. A reference to a missing scene or source is `not_found`, and
  a `source` reference restricted to `media` needs a source whose kind produces it.
- **References are never left dangling.** `source.delete` is rejected while an item
  places the source or a setting refers to it, and `scene.delete` while a `scene`
  source shows the scene or a setting refers to it. The error names the blockers (a
  setting as `<source>.settings.<param>`), and the client removes them first, in the
  same batch if it likes. Rejecting rather than cascading keeps each edit's effect, and
  its inverse, limited to the object it names.
- **Outputs** (`target.rs` checks the URL): `output.create` and `output.set_target`
  reject, with `invalid_argument` and a fixed reason that never repeats the URL, one
  that isn't an `rtmp://` or `rtmps://` server URL with a host and an application
  name, and one that could carry a credential (`user:password@`, a query or a
  fragment). A recording's `path` is rejected (field `kind.path`) when it is empty,
  over 4096 bytes, has control characters or a `..` segment, is not absolute (`/…`,
  `\…`, `C:\…` or `C:/…`, the same on every platform, as for an absolute asset), or
  doesn't end in a file name; whether it is absolute on the engine's own machine, and
  whether the file can be written, is the engine's check at `output.start`. `output.set_target` on a recording fails with
  `invalid_argument` (it has no RTMP target). `output.delete` is undone by `output.create` with its kind, and
  `output.set_target` by the previous target. `Graph::apply_with_running` is `apply`
  for an engine whose outputs in the given set are running: deleting one of them fails
  with `invalid_argument`, in edit order.
- **Groups and moves** ([ADR-0024](../../docs/adr/0024-scene-structure.md) §1):
  `scene.set_role` changes a scene's role and is undone by the previous role.
  `item.move` moves an item to an index in another scene (up to one past its top) or
  within its own (a reorder), keeping the whole item, and is undone by `item.move`
  back. The placement edits (`item.set_transform`, `item.set_crop`, `item.set_visible`)
  take an optional `scene_id`: absent or the item's own scene changes the item, another
  scene is `invalid_argument` until keyframe scenes exist, and the inverse keeps the
  addressing. No keyframe overrides exist yet, so no edit drops any
  ([ADR-0040](../../docs/adr/0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md)
  §1 applies once they do).
- **No cycles** (`deps.rs`). One function lists every dependency edge: a scene depends
  on the sources its items place, a `scene` source on its scene, and a source on the
  scenes and sources its settings refer to. Asset references are not edges. Every edit
  that adds an edge runs the cycle check: `item.add`, `item.move`, `source.create` and
  `source.set_settings`; `source.duplicate` copies outgoing edges onto a source nothing
  depends on, so it can't close one. A cycle is `invalid_argument` naming it
  (for example scene `a` -> source `sa` -> scene `a`).

**Reading** (`read.rs`): `Graph::query` answers the protocol queries that read the
collection (`state.get`, `scene.list`, `source.list`, `item.list` and `output.list`)
with the protocol's result shapes, at the current `rev`, and the kind queries
(`kind.list`, `kind.describe`, which fails with `invalid_argument` on `kind_id` for an
unknown kind). It returns `None` for a query the graph doesn't hold the answer to (`output.status` and `video.snapshot` read runtime state), and fails only with
`not_found` for an `item.list` of a missing scene. Sources come ordered by ID, scenes in
display order, items bottom first, outputs ordered by ID; `Source::to_info`,
`Scene::to_info`, `Item::to_info` and `Output::to_info` convert single objects. Reads change nothing.

**Tests** (`tests.rs`; the reads' own in `read.rs`): validation and error codes per
edit, shared sources, cycles (self, 2- and 3-cycles, legal diamonds, and through
settings references, one case per edge-adding edit), settings checks and defaults,
delete rejection by references, roles, moves and the grouping and ungrouping batches,
`scene_id` addressing, output URL checks and the refused delete of a running
output, exact inverses,
what each query reports, and parity with `jstream-control`'s mock: the reads and
rejected batches of `jstream_protocol::parity` (a dev-dependency with the `fixtures`
feature) must get exactly the fixture's answers, the same ones the mock is held to. And a
property test: a seeded generator applies 6000 random
batches of every edit, including moves, role changes and addressed placement edits. After each successful one, applying its
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
- **Kinds from `jstream-sources`.** The engine's kinds are stubs until that crate
  supplies the real ones. Kind versions on stored settings, migrations, and keeping
  data for a missing kind come with persistence.
- The collection canvas and item `geometry` of
  [ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md), replacing the protocol's
  2D `ItemTransform`, which an item keeps as a legacy placement until 1.0 unless an
  `item.set_geometry` replaces it; and blend modes.
- Keyframe scenes (`variant_of`, per-item `overrides`, `item.clear_override`) and the
  rest of ADR-0024: placement edits addressed to a variant will write its override, and
  the edits ADR-0040 lists will drop orphaned overrides and restore them in their
  inverses. `scene.create`'s optional `role` comes with a change to the daemon, which
  builds that edit; until then a batch of `scene.create` and `scene.set_role` makes a
  group.
- Item lookup is a linear scan over scenes. An ID index can come once collections grow.
