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
verified = { hash = "b3:364f863aaa6eeca7", by = "agent:claude@session_01MjzPr23VZGbNvi9iGqnMxs", note = "B7×D5 merge (main 94fb25d, D5 #240) on B7 eed5622, resolved from the D8 thread; re-read; Source has Chains and SourceAudio; touched, reads, file format (filters, buses, sends, audio), rebuild order (filters before outputs, audio last), tests list both; parity collection names desk's filters and the audio; holds" }
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
- `State`: the `canvas` (default 1920×1080), the `sources`, keyed by ID, the `scenes`
  in display order, the `outputs`, keyed by ID, the `encoders` (encoder profiles),
  keyed by ID, the `buses`, keyed by ID and always holding the master, and the `sends`,
  keyed by bus and source.
- `Source`: a single shared instance with an ID, a name, a `SourceKind` (`builtin` by
  kind ID, or `scene`, a nested scene) and its `Settings`: every parameter of its kind,
  or none for a `scene` source. `item.add` places an existing source by reference,
  so one source can sit in many scenes, or several times in one, and is still one
  capture. `source.duplicate` makes a separate copy under a new ID and records the
  original in the copy's `duplicated_from`, a plain link that is no dependency edge:
  the original may be deleted, and reads then omit the link. Removing an item never
  deletes its source. A source also has its filter `Chains`, a video and an audio
  chain of `Filter`s (an ID unique within the source, a kind, `enabled` and complete
  settings), at most `MAX_CHAIN_FILTERS` (8) each
  ([ADR-0026](../../docs/adr/0026-source-filter-chains.md)). An audio source (its kind
  has audio, or a `scene` source) also has its `SourceAudio`
  ([ADR-0028](../../docs/adr/0028-audio-model-v2.md)).
- `Scene`: a name, a `role` (`scene` or `group`, ADR-0024 §1; it changes no behaviour
  here) and its items in stacking order, bottom first.
- `Item`: the source it places, a `Placement`, a `crop`, `visible`, an `opacity` from
  0 to 1 and a `BlendMode`, and the reserved per-placement `effects`, an uninhabited
  `Effect` list that is always empty. `Placement` is
  one of [ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md)'s two forms: a
  `Geometry` (a box on the canvas) or a `Legacy` 0.3 `ItemTransform`, kept exactly as
  written. Only an edit changes the form.
  `item.set_source` changes the source and keeps the rest, the item's ID and its
  stacking position, so it converts the item between a reference and a clone
  ([ADR-0023](../../docs/adr/0023-placements-references-and-clones.md)).
- `Bus`: an ID, a name and a fader (`gain_db`, `muted`). The master bus (`master`,
  `MASTER_NAME`) is in every state; at most `MAX_BUSES` (16) buses in all. A send is a
  gain in dB from a source into a bus.
- `Encoder`: an encoder profile, an ID, a name and the protocol's `EncoderProfile`.
  `State::encoding_of` gives the encoding an output starts with.
- `Output`: an ID, an `OutputKind` (so far RTMP to a generic URL, a recording
  to an MP4 file, or the virtual camera, which has nothing to check) and the encoder
  profile it uses, if not the default (a virtual camera encodes nothing, so it ignores
  its profile). Whether it
  runs is the engine's runtime state, and its stream key is in the OS credential store,
  never here ([R-SEC-001](../../docs/RULES.md#R-SEC-001)).

**Applying edits** (`apply.rs`): `Graph::apply(edits, expected_rev)` (or `apply_batch`)
checks `expected_rev` (`rev_conflict`) and rejects an empty batch (`empty_batch`). Then it
applies the edits in order to a working copy of the state. The first edit that fails
aborts the batch with `not_found`, `already_exists` or `invalid_argument` (field
`edits.<n>.<field>`), and nothing changes. On success the copy replaces the state, `rev`
goes up by exactly one, and the result (`Applied`) carries the **inverse batch** and what
the batch **touched**.

- **Inverses** are computed from the state before each edit, and the batch's inverse is
  its edits' inverses in reverse order. They are plain protocol edits, as the protocol's
  Undo design requires: removing an item is undone by `item.add`, then `item.reorder`,
  `item.set_crop` and `item.set_visible` only where the item differed from what
  `item.add` produces, and deleting a scene by `scene.create` (carrying its `role` for a
  group), `scene.reorder` and each item's restore. The inverse carries no `expected_rev`: the engine's per-client undo
  history (in `jstream-core`) chooses it.
- **Touched sets** (`touched.rs`, [ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)):
  `Touched::of(edits, inverse)` names, from a batch's edits and its inverse's (so a
  delete also names what it removed), the objects created or deleted, the objects used
  (an item's scene and source, both sources of an `item.set_source`, a `scene`
  source's scene, a duplicate's original, the scene an `item.move` names, the profile
  an `output.set_encoder` names, a send's source and bus), the properties set (names,
  a scene's role, an item's placement, crop, visibility, opacity, blend, index and
  source, each settings parameter, a filter's switch, settings parameters and index, an
  output's target and encoder choice, a profile's encoding, each audio field a
  `source.set_audio` or `bus.set_fader` sets, a send's gain), and whether the scenes'
  order or the canvas size was set. A send is an object of its own, so two sources'
  sends into one bus don't conflict, and deleting a bus conflicts with every change to
  its sends. An edit it doesn't know marks the set as conflicting with
  everything. `conflicts_with` is the undo rule the engine's history applies to other
  clients' later batches: one set creates or deletes an object the other touches, both
  set the same property, both set the scenes' order, or both set the canvas size.
- **Settings** ([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)):
  `source.create` of a `builtin` source needs a kind the graph describes, checks each
  given value with the protocol's `ParamDescriptor::check`, and stores the complete map
  with defaults filled in. `source.set_settings` checks and applies a patch (`null`
  resets a parameter to its default) and its inverse is the patch of the old values.
  `source.rename` renames. `source.create` rejects a `duplicated_from` naming the new
  source itself, and accepts one naming a missing source, as `source.delete`'s inverse
  may need after the original went first. A reference to a missing scene or source is `not_found`, and
  a `source` reference restricted to `media` needs a source whose kind produces it.
- **References are never left dangling.** `source.delete` is rejected while an item
  places the source or a setting refers to it, and `scene.delete` while a `scene`
  source shows the scene or a setting refers to it. The `invalid_argument` lists every
  blocker in `blockers` (`deps::blockers`, the reverse of the cycle check's edges): the
  item (`source_id`), the `scene` source (`kind.scene_id`), the setting
  (`settings.<param>`) or a filter's setting (`filters.<filter_id>.<param>`). The client removes them first, in the same batch if it likes,
  or sends the batch `delete.plan` returns. Rejecting rather than cascading keeps each
  edit's effect, and its inverse, limited to the object it names.
- **Canvas and geometry** ([ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md)):
  `canvas.set_size` accepts each side from 1 to `MAX_CANVAS_EDGE` (8192) and moves no
  item. `item.add` stores its `geometry`, or else its `transform` (the identity when
  omitted) as a legacy placement; sending both is `invalid_argument`.
  `item.set_geometry` and `item.set_transform` replace the placement, and each one's
  inverse restores the previous placement in its own form, so a conversion undoes to
  `item.set_transform`. A geometry is checked before it is stored: a positive size and
  perspective, finite corners, and neutral tilt, turn, depth and perspective until the
  compositor draws 3D; each failure names its field (`edits.N.geometry.tilt_deg`).
  `item.set_opacity` (optionally addressed by `scene_id`, like the placement edits) and
  `item.add`'s `opacity` accept 0 to 1; `item.set_blend` and `item.add`'s `blend`
  accept only `normal` until the compositor draws the other modes. Each is undone by
  the previous value, and removing an item restores both.
- **Filters** (`filter.rs`, [ADR-0026](../../docs/adr/0026-source-filter-chains.md)):
  `filter.add` checks, in order, the owner source, the filter ID (non-empty, unused in
  the source), that the source has the chain (video if its kind produces video, audio
  if it produces audio and it is not a `scene` source), that the chain has room, that
  the kind is a filter kind of that chain (`kind`), the settings exactly as a source's,
  and the index; it is undone by `filter.remove`. `filter.remove` is undone by the
  `filter.add` that puts the filter back where it was, with its switch and settings;
  `filter.set_enabled`, `filter.set_settings` (a patch, `null` resets) and
  `filter.reorder` by the previous values. `source.duplicate` copies both chains with
  their IDs, `source.delete`'s inverse re-adds every filter after `source.create`, and
  `source.create` refuses a filter kind.
- **Outputs** (`target.rs` checks the URL): `output.create` and `output.set_target`
  reject, with `invalid_argument` and a fixed reason that never repeats the URL, one
  that isn't an `rtmp://` or `rtmps://` server URL with a host and an application
  name, and one that could carry a credential (`user:password@`, a query or a
  fragment). A recording's `path` is rejected (field `kind.path`) when it is empty,
  over 4096 bytes, has control characters or a `..` segment, is not absolute (`/…`,
  `\…`, `C:\…` or `C:/…`, the same on every platform, as for an absolute asset), or
  doesn't end in a file name; whether it is absolute on the engine's own machine, and
  whether the file can be written, is the engine's check at `output.start`. `output.set_target` on a recording or a virtual camera fails with
  `invalid_argument` (neither has an RTMP target). `output.delete` is undone by `output.create` with its kind, and
  `output.set_target` by the previous target. `Graph::apply_with_running` is `apply`
  for an engine whose outputs in the given set are running: deleting one of them fails
  with `invalid_argument`, in edit order. The `OutputDelete` inverse also restores the
  output's encoder profile.
- **Encoder profiles** (`encoder.rs`): `encoder.create` and `encoder.update` check the
  profile with the protocol's `EncoderProfile::problem` (`invalid_argument`, field
  `profile.<field>`) and the name; `encoder.delete` is refused with `invalid_argument`
  while an output uses the profile, naming it. `output.set_encoder` checks the output,
  then the profile (`not_found`), and needs no running check: it applies from the
  output's next start. Each is undone by its opposite (`encoder.delete` by
  `encoder.create`, `encoder.update` and `output.set_encoder` by the previous value).
- **Audio** (`audio.rs`, ADR-0028): a new source takes its kind's audio defaults
  (`follow` `always` for a kind with audio and no video, `when_shown` for every other
  audio source, `scene` sources included), and a new source with a channel of its own
  (an audio source that is not a `scene` source) sends to the master at 0 dB.
  `source.duplicate` copies the audio and the sends. `source.delete` removes the
  source's sends, and its inverse adds `source.set_audio`, `send.set` and a
  `send.remove` of the default master send for whatever differs from the defaults.
  `source.set_audio` needs an audio source and at least one field, and refuses a
  `scene` source's `gain_db` or `muted`. `bus.create` checks the ID, the name and the
  bus limit; `bus.delete` refuses the master and removes the bus's sends, and its
  inverse is `bus.create`, `bus.set_fader` if the fader differed, and the sends.
  `send.set` and `send.remove` check the source, then the bus (`not_found`), then that
  the source has a channel (a `scene` source sends nothing of its own). Every gain is
  checked with the protocol's `gain_db_problem` (−100 to +24 dB). A send always runs
  from a source to a bus, so buses can't form a cycle (buses never feed buses); a
  routing check comes with filters.
- **Groups and moves** ([ADR-0024](../../docs/adr/0024-scene-structure.md) §1):
  `scene.set_role` changes a scene's role and is undone by the previous role.
  `scene.create` sets the `role` it names, or `scene` when it names none.
  `item.move` moves an item to an index in another scene (up to one past its top) or
  within its own (a reorder), keeping the whole item, and is undone by `item.move`
  back. The placement edits (`item.set_geometry`, `item.set_transform`, `item.set_crop`,
  `item.set_visible`, `item.set_opacity`)
  take an optional `scene_id`: absent or the item's own scene changes the item, another
  scene is `invalid_argument` until keyframe scenes exist, and the inverse keeps the
  addressing. No keyframe overrides exist yet, so no edit drops any
  ([ADR-0040](../../docs/adr/0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md)
  §1 applies once they do).
- **No cycles** (`deps.rs`). One function lists every dependency edge: a scene depends
  on the sources its items place, a `scene` source on its scene, and a source on the
  scenes and sources its settings, or its filters' settings, refer to. Asset references
  are not edges. Every edit that adds an edge runs the cycle check: `item.add`,
  `item.move`, `item.set_source` (from the item's scene to its new source),
  `source.create`, `source.set_settings`, `filter.add` and `filter.set_settings`; `source.duplicate` copies outgoing edges onto a source nothing
  depends on, so it can't close one. A cycle is `invalid_argument` naming it
  (for example scene `a` -> source `sa` -> scene `a`).

**Reading** (`read.rs`): `Graph::query` answers the protocol queries that read the
collection (`state.get`, `scene.list`, `source.list`, `item.list`, `output.list`,
`encoder.list` and `bus.list`)
with the protocol's result shapes, at the current `rev`, and the kind queries
(`kind.list`, `kind.describe`, which fails with `invalid_argument` on `kind_id` for an
unknown kind), and `delete.plan` (`plan.rs`,
[ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)). It returns `None` for a query the graph doesn't hold the answer to (`output.status` and `video.snapshot` read runtime state), and fails only with
`not_found` for an `item.list` of a missing scene or a `delete.plan` of a missing
target. `Graph::delete_plan` builds the plan from the same `deps::blockers` the delete
check uses: a scene's plan also deletes the `scene` sources that show it, the items
placing any deleted source are removed, settings references to anything deleted are set
to `null` (one `source.set_settings` per source, and one `filter.set_settings` per
filter), and the target's own delete comes last. Sources come ordered by ID, scenes in
display order, items bottom first, outputs, encoder profiles and buses ordered by
ID, and sends by bus and then source. `state.get` carries the canvas, the buses and
the sends, and an audio source's info its `audio`. A geometry item reads its `geometry`, and the deprecated `transform` view the
protocol computes from it (`ItemGeometry::transform_view`); a legacy item reads its
stored transform and no geometry. `Source::to_info` (given the `State`, to omit a
`duplicated_from` whose source is gone),
`Scene::to_info`, `Item::to_info`, `Filter::to_info` and `Output::to_info` convert single
objects; a source reports its chains in `filters`, omitted while both are empty, and an
item its `opacity` and `blend`. Reads change nothing.

**The collection file** (`persist/`, [ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence)):
`Graph::to_collection_json` writes the collection as pretty JSON: `schema_version`
(`SCHEMA_VERSION`, now 1), `rev`, the `canvas`, scenes in display order (a group with
`role: "group"`, ADR-0024) with their items bottom first (each with its placement in
its own form, a legacy `transform` or a `geometry`, exactly one of them, then its crop
and `visible`, and its `opacity` and `blend` only when they differ from 1 and
`normal`), sources ordered by ID (with `kind_version`, the descriptor version a
`builtin` source's settings follow, ADR-0025, `duplicated_from` as stored, even
when it names a deleted source, and `filters`, omitted while both chains are empty: each
filter with its `kind_version` and settings, and `enabled` only when false), outputs ordered by ID (each with its
`encoder_profile`, omitted for the default), encoder profiles (`encoders`) ordered by
ID, omitted when there are none, `buses` (omitted while the master is the only one and
at its defaults) and `sends`, written whenever there is a send or a source with a
channel of its own (so an empty list means every master send was removed) and omitted
otherwise. A source's `audio` is saved
only where it differs from its kind's defaults. Kinds, undo history and runtime state are not in it, and
neither is any secret: stream keys are in the OS credential store and output URLs can't
carry one ([R-SEC-001](../../docs/RULES.md#R-SEC-001)). Reading and writing the file is
the engine's; this crate does no I/O.
- **Loading** (`Graph::with_collection_json`) parses the JSON, refuses a
  `schema_version` it has no path from (missing, 0, or newer), runs the forward
  migrations from the file's version (`MIGRATIONS[n]` upgrades `n + 1` to `n + 2`;
  there are none while the schema is at 1), and refuses unknown fields in the file's
  own objects, so a newer field is never silently dropped by the next save. An item's
  reserved `effects` is accepted only empty.
- **A loaded collection passes every edit check.** Rather than trust the file, loading
  rebuilds the state by applying the edits that create it, one at a time, to an empty
  graph with the caller's kinds: the canvas size, scenes with their roles, then sources (each after the
  sources its settings refer to), then items in their placement form with their crop
  and visibility, opacity and blend, then every source's filters, then encoder
  profiles, then outputs with their profiles, then the buses (the master renamed or
  faded as saved), each source's audio and the sends (with a `send.remove` of a
  default master send the file doesn't list; a file without `sends`, such as one saved
  before sends existed, keeps every default master send). A
  dangling reference, a cycle, settings that don't fit the kind, an unknown kind or a
  URL with a credential fails as `LoadError::Invalid`, naming the object
  (`` item `i1` ``) and the protocol error (boxed, so `Result`s carrying a
  `LoadError` stay small); nothing is partly loaded. Then `rev` is set
  to the file's, so revisions keep increasing across restarts. A source or filter whose
  `kind_version` differs from its kind's descriptor is refused
  (`LoadError::KindVersion`) until kinds ship settings migrations.
- **Errors** never quote a value beyond IDs, so they are safe to log. `serde_json`'s
  own messages quote the offending value, so a parse error keeps only a missing field's
  name, a fixed reason, and the line and column of a syntax error.

**Tests** (`tests.rs`; the reads' own in `read.rs`, the touched sets' conflict rule in
`touched.rs`): validation and error codes per
edit, shared sources, references and clones (`item.set_source` keeping the item, the
`duplicated_from` link through deletes and undo, and both conversions as one batch),
cycles (self, 2- and 3-cycles, legal diamonds, and through settings references, one
case per edge-adding edit), settings checks and defaults,
placement conversions and their inverses, geometry and canvas validation,
delete rejection by references, roles, moves and the grouping and ungrouping batches,
`scene_id` addressing, output URL checks and the refused delete of a running
output, encoder edits and their inverses (a profile freed and deleted in one batch),
opacity and blend checks and inverses, the filter edits (`tests/filters.rs`: each
check's field, exact inverses, duplicate and delete, references in filter settings as
cycles, blockers and plans, and the collection file), audio defaults by kind, a
duplicate's audio and a delete's restore, a bus deleted with its sends and undone
exactly, exact inverses,
what each query reports, and parity with `jstream-control`'s mock: the reads and
rejected batches of `jstream_protocol::parity` (a dev-dependency with the `fixtures`
feature) must get exactly the fixture's answers, the same ones the mock is held to. And a
property test: a seeded generator applies 6000 random
batches of every edit, including moves, role changes, source switches, addressed
placement edits, filter edits and the audio edits. After each successful one, the state must hold no cycle, applying its
inverse must restore the state exactly, and applying the inverse's inverse (redo) must
restore the batch's result. After each failed one, the graph must be unchanged. And
after every batch, for each scene and source: a plain delete is refused exactly when
it has blockers, the error lists them, and the `delete.plan` batch applies at the
planned `rev`, removes the target, and undoes exactly.
The proptest suites (`tests/properties.rs`, public API only, shrinking failures to a
minimal batch sequence) start from a graph holding every sort of reference and apply
random batches with free-form names, numbers and settings values: a batch that applies
must undo and redo exactly and a failed one change nothing; and after every batch no
reference may dangle, and each scene, source and encoder profile delete must be refused
exactly when something refers to it, a scene's or source's with the blockers the test
works out itself. The
seed is fixed, so CI is repeatable; `PROPTEST_RNG_SEED` explores others.
The collection file's tests (`persist/tests.rs`): saving the parity collection (a
1280×720 canvas, legacy and geometry items, two encoder profiles that two outputs use,
scene `b` made a group, a clone, a `duplicated_from` naming a missing source,
opacities, a desktop audio source with two gain filters, one disabled, and the
fixture's buses, sends and source audio, less the virtual camera the parity collection
gained later) plus a source whose
settings refer to a later one must produce exactly the checked-in
`persist/fixtures/v1.json`, and that fixture must load back to the same state and
`rev` ([R-API-003](../../docs/RULES.md#R-API-003): the fixture stays when the schema
moves on, and a migration has to load it); a file without `encoders` or any
`encoder_profile` (as saved before profiles existed) loading with none; a file
without `sends`, `buses` or source `audio` (as saved before audio existed) keeping
its audio sources' master sends at 0 dB; a removed master send's round trip; a profile's
round trip; a virtual camera's round trip; a clone's link surviving a round trip after its original is deleted; the migration runner over stand-in
migrations; and refused files (versions, malformed JSON, unknown fields, dangling
references (an output naming a missing profile among them), cycles, unknown kinds, a credential in a URL, a kind version mismatch, an
item with both or neither placement form, an invalid geometry or canvas), and that
no error quotes a credential-like value placed where a number, key or tag belongs.

<a id="follow-ups"></a>
## Not yet
<!-- @section
-->

Planned, and kept possible by the private fields of the model types:

- **Filters and effects:** `filters[]` on sources (applied wherever the source is
  placed) and `effects[]` on items (per placement), with their edits.
- **Kind versions.** The engine's kinds come from `jstream-sources`. The collection file
  records each source's kind version but refuses a mismatch: settings migrations per
  kind, and keeping the data of a source whose kind is missing, come later.
- Blend modes and opacity on placements (B7). Legacy placements go in 1.0, whose
  collection migration converts them
  ([ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md) decision 5).
- Keyframe scenes (`variant_of`, per-item `overrides`, `item.clear_override`) and the
  rest of ADR-0024: placement edits addressed to a variant will write its override, and
  the edits ADR-0040 lists will drop orphaned overrides and restore them in their
  inverses.
- Item lookup is a linear scan over scenes. An ID index can come once collections grow.
