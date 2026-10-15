<!-- @doc
id = "CRATE-jstream-protocol"
title = "jstream-protocol"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-protocol

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-protocol/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-protocol"]
verified = { hash = "b3:95c34f9076b560e1", by = "agent:claude@session_01Vy4KuKBCruDTJakQ9j2gFA", note = "Merge of main (F2b #170 record output 0.9, G2 #174) into B6, now protocol 0.10: version sentence now 0.10 for groups, 0.9 record, 0.8 kinds (version.rs minor 10)" }
-->

**The API.** Every capability of JStream is a command, query, event or stream defined
here first ([R-ARCH-001](../../docs/RULES.md#R-ARCH-001)), and the engine, the web UI, the
CLI and MCP agents all speak it ([ADR-0001](../../docs/adr/0001-library-first-engine.md)).
Layer 1: plain data types with serde and JSON Schema derives, no I/O, no `unsafe`, and
WASM-safe, because `jstream-ui` and `jstream-control` compile it to `wasm32`. It depends
on no other JStream crate yet: `jstream-types` has no IDs or transforms so far, so the
first cut defines its own wire types for them.

Contents (a deliberately small first cut; [Design](#design) describes the whole model):
- **Envelope** (`envelope.rs`): `ClientMessage` (`hello`, `request`) and `ServerMessage`
  (`welcome`, `response`, `error`, `event`), `Request`/`Call` and `Response`/`CallResult`.
  `ClientMessage::parse` maps every parse failure to `malformed`.
- **Commands** (`command.rs`): `Edit` (`scene.create`, `scene.rename`, `scene.delete`,
  `scene.reorder`, `scene.set_role`, `item.add`, `item.remove`, `item.reorder`,
  `item.move`, `item.set_transform`,
  `item.set_crop`, `item.set_visible`, `source.create`, `source.rename`,
  `source.set_settings`, `source.duplicate`, `source.delete`, `output.create`,
  `output.set_target`, `output.delete`), with `ItemTransform`, `Crop`, `SourceKind` and
  `SceneRole`;
  `Action` (`output.start`, `output.stop`, `output.set_key`, `output.clear_key`);
  `Batch`; and the `Command`/`CommandClass`
  classification view.
- **Outputs** (`output.rs`): `OutputKind` (`rtmp` and `record` so far), `RtmpTarget`
  (only a generic `url`), `RecordContainer` (only `mp4`), `OutputInfo`, and `SecretString`, the write-only stream key, which
  never prints and never comes back from the engine
  ([R-SEC-001](../../docs/RULES.md#R-SEC-001)).
- **Queries** (`query.rs`): `state.get`, `scene.list`, `source.list`, `item.list`,
  `output.list`, `output.status`, `video.snapshot`, `kind.list` and `kind.describe`,
  with `QueryResult` and the `SceneSummary`, `SceneInfo`, `SourceInfo` and `ItemInfo`
  shapes the collection reads report. `video.snapshot` returns the program mix as a PNG.
- **Kinds and settings** (`kind.rs`, `settings.rs`,
  [ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)): `KindDescriptor`,
  `ParamDescriptor` and `ParamType`, the shapes that describe a source or filter kind's
  parameters, and `Settings`, a map from parameter ID to a JSON value, whose `scene`,
  `source` and `asset` references are tagged objects (`Reference`). The per-value checks
  (`ParamDescriptor::check`, `check_asset_path`) and the settings schema
  (`KindDescriptor::settings_schema`) are pure functions here, so every implementation
  shares them. No kind is defined here.
- **Events** (`event.rs`): `state.changed`, `output.state_changed`, `events.dropped` and
  `stats.tick`.
- **Errors** (`error.rs`): `ProtocolError`, a `thiserror` enum whose variants have stable
  codes, and `WireError`, its wire form with a message.
- **IDs and versions** (`id.rs`, `version.rs`): opaque string IDs, `Rev`, and
  `PROTOCOL_VERSION`.
- **Numbers** (`finite.rs`): `Finite32`, the only float type on the wire, which rejects
  NaN, infinity and out-of-range numbers at construction and deserialization. Computed
  results (`CanvasPoint`, `ContentRect`, `LegacyQuad`) use plain `f32`.
- **Geometry** (`geometry.rs`): [`ItemGeometry`](src/geometry.rs) and the shared
  placement formula ([ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md)).
  `project` places the box, rotation about its centre, with the 3D fields in the
  formula and defaulting to neutral. `content` maps a cropped source with `stretch`,
  `fit` or `fill`. `legacy_quad` and `ItemGeometry::from_legacy` evaluate a 0.3
  transform. A negative scale is not refused: `from_legacy` returns the drawn outline
  and flags the reflection, which geometry cannot store. These types are not
  messages. `PROTOCOL_VERSION` is 0.10 because of groups (`scene.set_role`,
  `item.move`, a scene's `role` and the placement edits' `scene_id`); 0.9 added the
  `record` output kind, 0.8 the kind queries and settings, 0.7 the output edits,
  `output.list`, the key actions and `key_set`, `stats.tick` is the 0.6 addition and
  `video.snapshot` the 0.5 one, and the geometry wire surface ships as a later minor. `ItemTransform`'s schema description names
  `ItemGeometry` as the replacement (`protocol.snap` and the MCP `tools.snap`).
- **Schema** (`schema.rs`): `json_schema()`, snapshot-tested in
  [`schema/protocol.snap`](schema/protocol.snap), with every fixture message's wire form in
  [`schema/examples.snap`](schema/examples.snap).
- **Parity fixture** (`parity.rs`, only with the non-default `fixtures` feature): test
  support, not wire types. One batch that builds a small collection with two RTMP
  outputs and a recording output, the exact answer to every collection read of it
  (`output.list` included), and the exact error for batches rejected against it (RTMP
  URL and recording path checks included).
  `jstream-graph` and `jstream-control`'s mock engine are both layer 2, so neither can
  test against the other ([R-ARCH-002](../../docs/RULES.md#R-ARCH-002)); both enable
  the feature in dev-dependencies only and are held to these answers. `kinds()` names
  the kinds an implementation must describe for the batch to apply (`webcam`, with no
  parameters, so every implementation reports the same empty settings). It changes no
  message, so it never changes `PROTOCOL_VERSION`.

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-protocol/src/**", "crates/jstream-protocol/schema/**"]
depends_on = ["docs/adr/0013-command-model.md", "docs/RULES.md#R-API-001", "docs/RULES.md#R-API-002", "docs/RULES.md#R-API-004", "docs/RULES.md#R-ARCH-006"]
verified = { hash = "b3:1d74a328a8312764", by = "agent:claude@session_01Vy4KuKBCruDTJakQ9j2gFA", note = "Merge of main (F2b #170 record output 0.9, G2 #174) into B6, now protocol 0.10: groups subsection says 0.10 and that a 0.9 reader sees no change; geometry paragraph lists 0.10/0.9/0.8/0.7; F2b's record and Unknown-kind text kept" }
-->

This is the protocol's design. It is a proposal, so the maintainer can steer it: the open
points are listed under [Open questions](#open-questions).

### Envelope and versioning

A connection carries JSON text messages, each tagged by `type`. It starts with a
handshake: the client sends `hello` with its `ProtocolVersion` and a `client_name`, and
the server answers `welcome` with its own version, the `client_id` this connection acts
as, and the current `rev`. If the server can't serve that version it answers an
`unsupported_version` error instead. A request sent before the handshake gets
`handshake_required`.

After the handshake, the client sends `request`s. Each carries an `id` the client
chooses, and the server answers each request with exactly one `response` or `error`
carrying the same `id`. Answers may come out of order. `event`s arrive at any time.

```json
{"type":"hello","protocol":{"major":0,"minor":9},"client_name":"jstream-cli"}
{"type":"welcome","protocol":{"major":0,"minor":9},"client_id":"cli_7","rev":12}
{"type":"request","id":"r1","call":{"kind":"batch","edits":[{"type":"scene.create","scene_id":"scn_1","name":"Main"}],"expected_rev":12}}
{"type":"response","id":"r1","result":{"kind":"batch","rev":13}}
{"type":"event","event":{"type":"state.changed","rev":13,"client_id":"cli_7","request_id":"r1"}}
```

The version is `major.minor` and follows semver independently of the app
([R-API-001](../../docs/RULES.md#R-API-001)). A server serves a client with the same
major and a minor no newer than its own (`can_serve`): a 0.2 server serves a 0.1
client, and a 0.1 server refuses a 0.2 client. The rule is the same at 0.x as after 1.0,
because additions only ever bump the minor (see *Schema and compatibility* below). The
in-process `Control` implementation has
no handshake; it is compiled against the same crate.

### Tagging

Every enum uses serde's internally tagged form, so a message is one flat JSON object
whose tag says what it is:

- `type` names the message (`request`), the command, query or result (`scene.create`), or
  the event (`state.changed`). Command, query and event names are `noun.verb` in
  snake_case ([R-API-002](../../docs/RULES.md#R-API-002)), so the tag *is* the name that
  MCP tools, the CLI and logs use.
- `kind` names the class of a call and its result: `batch`, `action` or `query`. It is a
  different key from `type`, so a call can nest its command without the two tags
  colliding, and the class of every request is explicit on the wire.
- `code` names an error.

Why internally tagged: a flat object is easiest to write by hand and for agents, it
gives the schema one `oneOf` with a `const` per alternative, and an unknown tag fails
with an error that names it. Externally tagged enums nest every value one level deeper
under its name, adjacently tagged ones need a second `content` key, and untagged ones
give useless parse errors and ambiguous schemas.

### Edits, actions and queries

Following [ADR-0013](../../docs/adr/0013-command-model.md), every command is exactly one
of two classes ([R-ARCH-006](../../docs/RULES.md#R-ARCH-006)):

- **Edit** (`Edit`): changes the collection state, the data that is saved in the
  collection file and that `rev` counts. Undoable, batchable, atomic.
- **Action** (`Action`): has effects outside the collection state, such as network,
  devices, files, secrets or time (starting an output, going live, storing a stream
  key, OAuth). Never undoable or
  batchable. The response only says the action was accepted; progress arrives as
  events (`output.state_changed`).

The classification rule: **if the command's whole effect is a change to the collection
state, and its inverse can be computed from the state before it, it is an edit.
Otherwise it is an action.** A command that does both is split into an edit and an
action. A **query** (`Query`) changes nothing and is neither.

The types enforce the split: a `Batch` holds only `Edit`s, an action travels alone, and
`Call` (tagged by `kind`) has exactly three forms: `batch`, `action` and `query`.
`Command` is a classification view over both enums, with `class()`, `is_undoable()` and
`is_batchable()`. The tests `names_are_valid` and `every_command_is_classified` check
every name in the generated schema.

Creating edits carry a client-chosen ID (`scene_id`, `item_id`, `output_id`). A batch can then refer
to an object it creates earlier in the same batch, a retry can't create a duplicate,
and an undo can re-create a deleted object under its old ID. IDs are opaque strings.

Sources are shared instances, independent of scenes. `source.create` adds one, and
`item.add` places an existing source by reference, so one source can appear in many
scenes, or several times in one, as the same capture. `source.duplicate` makes a
separate copy under a new ID, with the same kind and settings, and `source.rename`
renames one. A `SourceKind` is `builtin` (a kind the engine describes) or `scene`, which
nests another scene. Positions are indexes: scenes in display order, items in stacking
order with 0 at the bottom.

### Groups and placement addressing

[ADR-0024](../../docs/adr/0024-scene-structure.md) §1 makes a group a scene with
`role: group` (protocol 0.10). `SceneRole` is `scene` or `group`, and
`scene.set_role { scene_id, role }` changes it; the role changes no engine behaviour.
`scene.list` and `state.get` report `role`, omitted for `scene`, so a 0.9 reader sees
no change for ordinary scenes. `item.move { item_id, scene_id, index }` moves an item
to a position in another scene, keeping its ID and every placement property, under the
same cycle check as `item.add` (field `scene_id`). `index` is the item's position after
the move: up to the number of items already there for another scene, and below it for
the item's own scene, where the move is a reorder. Grouping is a client-built batch:
`scene.create`, `scene.set_role` to `group`, `source.create` of a `scene` source,
`item.add` of it with `item.move` to the lowest selected index, then an `item.move` of
each selected item into the group, bottom first. `scene.create` doesn't take a `role`
yet.

The placement edits (`item.set_transform`, `item.set_crop`, `item.set_visible`) take an
optional `scene_id`, fixed now so that keyframe scenes can arrive additively (ADR-0024
§2). Absent, or the item's own scene, changes the item. Until keyframe scenes exist,
any other existing scene is `invalid_argument` on `edits.N.scene_id`, and a missing one
`not_found`. An inverse keeps the addressing of the edit it undoes.

### Kinds and settings

[ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md) decides the shape. A
`builtin` source's `kind` is a kind ID, and its settings travel beside it, as a
`Settings` map on the source. The protocol defines the descriptor and value shapes once,
never a type per kind.

- `kind.list` returns a `KindSummary` per kind (ID, category, name, version), ordered by
  ID. `kind.describe { kind_id }` returns the `KindDescriptor` and a JSON Schema of its
  settings generated from it (`settings_schema`), or `invalid_argument` on `kind_id` for
  a kind the engine doesn't have. Kinds are not collection state, so neither result
  carries `rev`.
- A `ParamDescriptor` has an `id`, a `type` with its constraints flattened into the same
  object (`bool`; `int` and `float` with `min`, `max`, `step`, `unit`; `enum` with
  `options`; `color` with `alpha`; `string` with `multiline` and `max_len`; `asset` with
  `accept`; `scene`; `source` with an optional `media`), a `default`, a `label`, an
  optional `description`, the UI hints `group`, `advanced` and `visible_if`, and `live`.
  An unknown `type` parses as `ParamType::Unknown`, which a client shows read-only.
  `step`, `multiline` and the hints never affect validation.
- Values are plain JSON. Colours are `#rrggbb`, or `#rrggbbaa` when `alpha` is set.
  References are tagged objects: `{"type":"scene","scene_id":…}`,
  `{"type":"source","source_id":…}` and `{"type":"asset","root":…,"path":…}`, with
  `root` one of `collection`, `app` and `absolute`. A reference parameter also accepts
  `null`. `Settings::references` finds every reference in a map without the descriptor.
- `source.create` takes optional `settings`. The engine checks each entry against the
  descriptor, fills every parameter left out (or sent as `null`, outside reference
  parameters) with its default, and stores the complete map. `source.set_settings
  { source_id, patch }` replaces the patched parameters, and `null` resets one to its
  default. Its inverse is the patch of the old values. A `scene` source has no settings.
- Rejections: an unknown kind (`edits.N.kind.kind`), an unknown parameter, a wrong
  type, a number out of range, an unknown enum option, a bad colour, an over-long
  string, or a bad asset path are `invalid_argument` with `field =
  edits.N.settings.<id>` (or `edits.N.patch.<id>`). Values are never clamped. A
  reference to a missing scene or source is `not_found`. A `source` reference that
  requires `media` rejects a source whose kind doesn't produce it. Asset paths under
  `collection` and `app` are relative, with `/` separators and no `..` segment, drive
  letter or leading `/`; an asset that doesn't exist yet is accepted.
- **References are dependency edges.** Items placing sources, `scene` sources showing
  scenes, and scene and source references in settings form one dependency graph. An
  edit that would close a cycle (`item.add`, `item.move`, `source.create`,
  `source.set_settings`) is
  rejected with `invalid_argument` naming it (for example scene `a` -> source `sa` ->
  scene `a`). `scene.delete` and `source.delete` are rejected while a setting refers to the
  target, naming each `<source>.settings.<param>`. Asset references are not edges.

`SourceInfo` reports `settings`, omitted when empty (a `scene` source, or a kind with no
parameters), so a reader of 0.7 messages sees no change for those.

Every float on the wire (`Vec2`, `ItemTransform`, `ItemGeometry`) is a `Finite32`:
an `f32` that is never NaN or infinite. Computed geometry results use plain `f32`. serde
would otherwise narrow an out-of-range JSON number such as `1e39` to infinity, which
then serializes as `null` and no longer parses. So a
non-finite or out-of-`f32`-range number fails deserialization, and
`ClientMessage::parse` rejects the whole message as `malformed`. In-process clients
can't build a non-finite value either: `Finite32::new` and `Vec2::new` return `None`
for one.

Outputs are collection state too: `output.create` adds one, stopped, with its
`OutputKind`; `output.set_target` changes where an RTMP output sends, from its next
`output.start`; `output.delete` removes one, and is refused with `invalid_argument`
while it runs. A target URL must be an `rtmp://` or `rtmps://` server URL with a host and an
application name, and is refused if it could carry a credential (`user:password@`, a
query or a fragment). A key pasted into the path can't be told from a nested
application name, so it is stored as given. A `record` output (0.9) records to a file
on the engine's machine: `path` must be absolute (`/…`, `\…`, `C:\…` or `C:/…`), have
no `..` segment and end in a file name, and
`container` is `mp4` (fragmented). `output.start` writes `<path>.part` and fails with
`action_failed` when `path` or `<path>.part` already exists, so a recording never
overwrites; `output.stop` renames it to `path`. A recording output has no RTMP target,
so `output.set_target` refuses it with `invalid_argument`; delete and re-create it to
record elsewhere. The
stream key is not collection state: it is a secret held in the OS credential store,
so it is set with the `output.set_key` action and removed with `output.clear_key`.
It is write-only ([R-SEC-001](../../docs/RULES.md#R-SEC-001)): no response, event,
error, query or log carries it, and `output.status` reports only `key_set`. A key
outlives `output.delete`, so undoing the delete gives back a working output; a client
that deletes for good also sends `output.clear_key`, which succeeds for an output no
longer in the collection. Its errors carry fixed reasons that never quote the key.

### Batches, revisions and `expected_rev`

Edits are only sent inside a `Batch`; a single edit is a batch of one.

- `rev` is the collection state revision. Every applied batch increments it by exactly
  one, however many edits it holds. Actions and queries never change it.
- A batch is atomic: the engine applies its edits in order, each against the state the
  previous one left. If any edit fails, nothing is applied, `rev` is unchanged, and the
  error names the failing edit's field (`edits.2.name`).
- `expected_rev` is optional ([R-API-004](../../docs/RULES.md#R-API-004)). If it is set
  and the state is no longer at that revision, the batch fails with `rev_conflict`
  (carrying `expected` and `current`) and nothing is applied. The client re-reads the
  state and retries: optimistic concurrency for read-modify-write. Without it, the batch
  applies to whatever is current.
- `idempotency_key` is optional. The engine remembers, per client, the result of each
  batch applied with a key. A retry with the same key is not applied again and gets the
  original result, even if `rev` has moved on since.
- Results: a successful batch answers `{"kind":"batch","rev":N}`, the revision it
  produced. Queries that read collection state report the `rev` they read.

### Queries

Queries read and never change `rev`. Those that read the collection state carry the
`rev` they read, so a client can send its next batch with that `expected_rev`:

- `state.get`: the whole collection at one revision: every source, every scene in
  display order with its items, and every output (`outputs`, ordered by ID; missing
  from a pre-0.7 server, it parses as empty). A client reads it on connecting, and again after a
  `state.changed` it didn't cause.
- `scene.list`: the scenes in display order, each with its name, `role` (omitted for a
  `scene`) and item count.
- `source.list`: the sources, ordered by ID (sources have no display order), each with
  its name, `SourceKind` and settings.
- `item.list {scene_id}`: one scene's items in stacking order, bottom first, or
  `not_found` for a missing scene.
- `output.list`: the outputs, ordered by ID (outputs have no display order), each with
  its `OutputKind`, at a `rev`.
- `output.status {output_id}`: an output's runtime state and `key_set`, whether the
  credential store holds its key (missing from a pre-0.7 server, it parses as
  `false`). Neither is collection state, so it carries no `rev`.
- `kind.list` and `kind.describe {kind_id}`: the kinds the engine describes, above.
  Not collection state, so no `rev`.
- `video.snapshot`: the current program mix as a PNG. `png` is the file in standard
  base64 with padding and no line breaks. `width` and `height` are that image's size;
  the longest side is at most 1280. `rev` is the collection revision of the layout.
  Source pixels are runtime state and can change without a new `rev`. The query takes
  no arguments and changes nothing.
  **Size.** The PNG is RGBA8 and nothing caps its byte size, so a noise-like frame,
  which PNG can't compress, can be as large as its raw pixels: today's canvas is
  always 1920×1080, so the image is 1280×720, 3.7 MB raw, and `png` can reach about
  4.9 MB of base64 (a square canvas, once its size can be set, about 8.7 MB). A typical
  frame is far smaller. The 4 MiB limit `jstream-server` sets applies only to messages
  it receives, not to its answers
  ([jstream-server › Design](../jstream-server/README.md#design)), so a client must
  accept an answer of that size: `jstream-control`'s WebSocket client keeps
  tungstenite's default limits, which are well above it.

An item is reported with its `source_id`, `transform`, `crop` and `visible`, and a
source with its name, kind and settings: every property an edit sets, so a client can
compute any edit, or its inverse, from what it read. The shapes (`SourceInfo`, `SceneInfo`, `ItemInfo`) mirror `jstream-graph`'s model,
which produces them (`Graph::query`). As the model grows (settings, filters, effects)
they gain fields, which older clients ignore.

### Undo

Undo is per client ([ADR-0013](../../docs/adr/0013-command-model.md)): each applied batch
becomes one undo step on the history of the `client_id` that sent it. The protocol
doesn't carry inverses. `jstream-graph` computes each edit's inverse from the state
before it, and the per-client undo stacks live in `jstream-core`
([ARCH › Data model](../../docs/ARCHITECTURE.md#data-model)). What the protocol must
guarantee is that the edit set is **closed under inversion**: every edit's inverse is
itself a sequence of edits. Each `Edit` variant documents its inverse.

**The edit set is closed under inversion.** Every property of the state has an edit
that sets it: `scene.reorder` and `item.reorder` restore positions, and
`item.set_visible` and `item.set_crop` restore what `item.add` doesn't take. So undoing
`item.remove` is `item.add` plus whichever of `item.reorder`, `item.set_crop` and
`item.set_visible` the removed item needs, and undoing `scene.delete` is `scene.create`,
`scene.reorder` unless it was last, `scene.set_role` for a group, and each item's
restore, bottom to top. `item.move` and `scene.set_role` are each undone by the same
edit with the old scene and index, or the old role. Undoing
`source.delete` is `source.create` with the complete stored settings, and undoing
`source.set_settings` is a patch of the old values. Deleting a source that items still
place or a setting refers to, or a scene that a `scene` source shows or a setting refers
to, is rejected, so no edit removes more than the object it names. `jstream-graph`'s property
test checks that applying a random batch and then its inverse restores the state
exactly. A new edit that can't state its inverse in edits needs new edits first. Undo
and redo requests (`history.undo`, `history.redo`), and the `undo_conflict` error for an
undo whose inverse conflicts with later edits by another client, are decided in
[ADR-0032](../../docs/adr/0032-live-edits-and-undo.md), together with the live values
that keep a drag to one batch and one undo step. They come with the engine's history.

### Events

After `welcome`, the server pushes every event to every client.

- `state.changed {rev, client_id, request_id?}` follows every applied batch. A client
  matches it to its own response by `request_id`; there is no ordering promise between
  the two. Every other client re-reads what it shows. Since each batch adds exactly one
  to `rev`, a gap shows that an event was missed.
- `output.state_changed {output_id, state}` reports runtime state, which is not part of
  the collection, so it carries no `rev`.
- `events.dropped {count}` says that `count` of this connection's events were dropped
  because the client read too slowly. A WebSocket server keeps a bounded event buffer
  per connection; when it overflows, it drops the queued events and sends this one
  notice instead, adding later drops to it until it is sent
  ([jstream-server › Design](../jstream-server/README.md#design)). Answers are never
  dropped. The client re-reads what it shows with `state.get`, which carries `rev`.
  Added in 0.4.
- `stats.tick {seq, drops}` is one sample of the pipeline's drop counters
  ([R-RT-004](../../docs/RULES.md#R-RT-004)). `drops` is a running total since the
  engine started, not a delta, so a client that misses a tick subtracts the last one
  it did see. The fields are `capture_frames` (frames a capture mailbox replaced
  before they were read), `audio_frames` (input-ring overruns, audio that arrived
  after the mixer had passed it, and frames the mixer skipped because it fell behind
  the clock), `render_ticks` (ticks the render loop skipped to catch up with the media
  clock), `encode_frames` and `encode_blocks` (video frames and audio blocks
  dropped before an encoder accepted them) and `output_packets` (encoded packets
  dropped handing a stream to an output). Silence inserted to cover a gap is not a
  drop. A counter missing from `drops` parses as 0 (`DropCounts` is
  `#[serde(default)]`), so a counter added later is an optional field (minor). `seq`
  starts at 1 and increases by one each sample. While an output is
  running, the pipeline's `jstream-stats` thread emits a tick about once a second;
  an idle engine emits none. A render tick that falls a whole period or more behind
  the clock is skipped rather than drawn late, and counted in `render_ticks`. Until
  [open question 7](#open-questions) every client receives it. Added in 0.6. A late plugin block is an audio drop in
  `audio_frames` once the mixer counts it
  ([ADR-0035](../../docs/adr/0035-clap-plugins-out-of-process.md)); that host is not
  built. Per-output bitrate, render and encode times, and CPU and GPU load wait until
  those meters exist, as later optional fields. Audience figures are `youtube.stats`
  ([ADR-0034](../../docs/adr/0034-youtube-interaction-scope-v2.md)).

### Errors

`ProtocolError` is a `thiserror` enum ([R-RS-003](../../docs/RULES.md#R-RS-003)). On the
wire (`WireError`) it is its stable `code`, its detail fields, and a `message` for
people and agents. Clients match on `code`, never on `message`.

| Code | Meaning | Details |
|---|---|---|
| `unsupported_version` | The server can't speak the client's version | `client`, `server` |
| `handshake_required` | A request came before `hello` | — |
| `malformed` | Bad JSON, unknown `type`, missing or mistyped field | `reason` |
| `empty_batch` | A batch had no edits | — |
| `rev_conflict` | `expected_rev` is stale; nothing applied | `expected`, `current` |
| `not_found` | A command named a missing object | `entity`, `id` |
| `already_exists` | A creating edit reused an ID | `entity`, `id` |
| `invalid_argument` | A field's value was rejected | `field`, `reason` |
| `action_failed` | An accepted action could not be carried out | `reason` |
| `internal` | An engine bug or unexpected condition; may be retried | `reason` |

An error that isn't tied to a request (a failed handshake, or a message too broken to
read an `id` from) has no `id`.

### Schema and compatibility

`json_schema()` generates the JSON Schema (draft 2020-12) of every message from the
Rust types with `schemars`. Each type is a named definition, so the MCP server can hand
out per-group schemas. The test `schema_snapshot` compares it with
[`schema/protocol.snap`](schema/protocol.snap), and `wire_examples_snapshot` compares one
message of every variant with [`schema/examples.snap`](schema/examples.snap). Any API
change shows up as a diff of those files, and that diff is the API review
([R-TEST-002](../../docs/RULES.md#R-TEST-002)). After an intended change, regenerate them
with `INSTA_UPDATE=always cargo test -p jstream-protocol` (or `cargo insta review`), and
review the diff.

Compatibility follows [R-API-001](../../docs/RULES.md#R-API-001), at 0.x exactly as
after 1.0:

- **Minor (additive):** a new command, query, event, error code or `Call` kind; a new
  optional field. Old clients keep working, and a newer server still serves them
  (`can_serve`), because unknown fields are ignored (no type denies them), and because
  unknown server messages, events and error codes parse as an `Unknown` variant that
  clients skip. An unknown error code still has its `message`. From 0.9, an output's
  `OutputKind`, `RtmpTarget` and `RecordContainer` parse an unknown `type` or value as
  `Unknown` too, so later output kinds won't break a 0.9 client. Not yet covered: a new
  value of a string enum (`OutputState`, `EntityKind`) inside a known message breaks an
  older client's parse, so adding one needs open question 8 settled first. The same
  break happened once, in 0.9 itself: `OutputKind` had no fallback in 0.7 and 0.8, so a
  0.7 or 0.8 client can't parse `output.list` or `state.get` once a `record` output
  exists (the whole response fails, and `WsControl` ends the session). Whether that
  counts as a minor bump is part of open question 8.
- **Major (breaking):** removing or renaming anything, changing a field's type or
  meaning, or making an optional field required. It needs an ADR and one minor release
  that deprecates the old form first.
- Every public enum is `#[non_exhaustive]`, so Rust clients must handle new variants too.

### Item geometry

[ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md) defines an item's box as
`ItemGeometry` ([`geometry.rs`](src/geometry.rs)): `center` and `size` in canvas pixels,
`fit_mode` (`stretch`, `fit` or `fill`, default `fit`), clockwise `rotation_deg` about
the centre, and 3D `tilt_deg`, `turn_deg`, `depth_px` and optional `perspective_px`.
The 3D fields default to neutral: zeros, and perspective absent. The formula is a pure
function in this crate so the compositor and the UI share one implementation.

- `place` and `project` map a local-frame point, and the box's four corners, through
  rotation about z, then turn about y, then tilt about x, then depth, then per-item
  perspective, then add `center`. Corner order is top-left, top-right, bottom-right,
  bottom-left of the unrotated box. A point with z at or beyond `perspective_px` is
  behind the viewer. With the 3D fields neutral this is rotation about the box centre
  in the canvas plane. Depth alone, with no perspective, does not move the orthographic
  projection.
- `content` maps a cropped source into that local frame, before the placement above.
  `stretch` fills the box. `fit` is the largest rectangle of the source's aspect ratio
  inside the box, centred, and the rest stays transparent. `fill` is the smallest such
  rectangle that covers the box, centred, and may extend past it; the compositor clips.
  A crop that leaves no pixels draws nothing. Crop stays [`Crop`](src/command.rs), in
  source pixels, applied before fitting.
- `legacy_quad` and `ItemGeometry::from_legacy` evaluate a 0.3 `ItemTransform`: crop,
  then scale, top-left at `position`, clockwise about that corner. `from_legacy`
  returns that outline as a box with `fit_mode = stretch` and neutral 3D, so a
  non-uniform scale stays exact, plus whether a negative scale flipped an axis.
  A negative scale is not refused. Geometry has no flip field, so projecting that box
  matches the outline and not the reflected corner order. `legacy_quad` is the
  reflected quad.

These types are serde-shaped as the ADR's wire fields, and they are not messages yet.
`item.add` still takes `ItemTransform`. `PROTOCOL_VERSION` is 0.10 because of groups;
the `record` output kind is 0.9, the kind queries and settings 0.8, the outputs 0.7,
`stats.tick` the 0.6 addition and `video.snapshot` the 0.5 one. ADR-0022
names the geometry additions as 0.4, which already shipped with `events.dropped`;
the geometry wire surface ships as a later minor, under
[R-API-001](../../docs/RULES.md#R-API-001). The accepted ADR is not edited
([R-DOC-009](../../docs/RULES.md#R-DOC-009)). The geometry change in the schema
snapshots ([R-TEST-002](../../docs/RULES.md#R-TEST-002)) is `ItemTransform`'s
description, in `protocol.snap` and in the MCP tool list (`tools.snap`): it no longer
says that 3D fields will be added to that struct with defaults. `video.snapshot` is
the additive 0.5 definition beside that. No geometry definition was added or removed.
`is_neutral_3d` is the predicate a stored geometry uses to reject a non-neutral 3D
field with `invalid_argument` until the compositor draws it. That rejection belongs
on the edit that stores the geometry, which is not on the wire yet. This crate still
evaluates the formula, so the definition is locked by unit tests; it does not draw.

### Binary streams (deferred)

Preview video and audio meters
([ARCH › Control protocol](../../docs/ARCHITECTURE.md#protocol)) stay WebSocket binary
frames, not designed yet except the meter frame's fields. Preview is the M2 low-res
H.264 stream ([ROADMAP › M2](../../ROADMAP.md#m2)). They will travel with a small fixed
header (stream ID, stream kind, the `rev` or media timestamp they show) and be opened
and closed with JSON subscriptions. That header is part of the preview work.

`video.snapshot` is not one of those frames. M1a uses it so an agent can check a scene
over the JSON API and over MCP ([ROADMAP › M1](../../ROADMAP.md#m1),
[ARCH › MCP](../../docs/ARCHITECTURE.md#mcp)), so the PNG is a field of the query
result. A binary snapshot frame would need a second channel those clients do not have.

<a id="open-questions"></a>
## Open questions
<!-- @section
-->

Decisions for the maintainer. Until one is settled, the first cut does what is
described above.

1. **Framing.** A custom envelope (as here), or JSON-RPC 2.0 framing (`method`,
   `params`, `id`)? JSON-RPC is what MCP speaks and has client libraries, but it has no
   handshake or typed error details of its own, and its errors are numeric.
2. **Single edits.** Every edit travels in a batch. Should there also be a shorthand
   that sends one edit without the `edits` array?
3. **IDs.** Clients choose the IDs of what they create (this design). Alternatives:
   server-assigned IDs, with batches referring to created objects by local names. And
   should the ID types move to `jstream-types`, once it has IDs and an optional serde
   feature?
4. **Undo and redo on the wire.** Settled by
   [ADR-0032](../../docs/adr/0032-live-edits-and-undo.md), not built yet:
   `history.undo` and `history.redo` are a fourth kind of call, `history`, which
   carries no command (it applies a batch of edits the engine already holds), so
   R-ARCH-006 needs no change. The same ADR adds live values for continuous gestures
   (`live.set`, `live.cancel`, `live.changed`), committed by one batch.
5. **Actions that change collection state.** `studio.transition` changes the program
   scene, which is saved in the collection. Is it an action that also bumps `rev`, or an
   edit plus a separate "run transition" action?
6. **Idempotency.** How long the engine remembers keys (count or time), and whether a
   reused key with a different batch is an error (`idempotency_key_reused`).
7. **Event subscriptions.** Every client gets every event in this cut, including
   `stats.tick` (0.6). High-rate events (`stats.tick`, meters) will need opt-in
   subscriptions. Until then every client receives a tick about once a second while
   the pipeline runs.
8. **Unknown values in string enums.** `OutputState` and `EntityKind` have no `Unknown`
   fallback, so adding a value breaks older clients that parse it. Add fallbacks, or treat
   a new value as a major change? `OutputKind`, `RtmpTarget` and `RecordContainer` have
   fallbacks from 0.9, but 0.9's `record` kind already breaks 0.7 and 0.8 clients reading
   outputs (see Compatibility); kept as a minor bump until this is decided.
9. **Schema delivery.** The schema lives in an `insta` snapshot. Should `xtask gen-schema`
   also publish a plain `.json` file for non-Rust clients?
10. **ADR.** Should the envelope, tagging and compatibility policy above be recorded as an
    ADR ([R-ARCH-004](../../docs/RULES.md#R-ARCH-004)) once they are settled?
11. **Source kinds.** Settled by
    [ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md) and built in 0.7:
    `builtin` stays, its `kind` names a kind the engine describes (`kind.list`,
    `kind.describe`), and settings travel beside it as a map validated against that
    kind's descriptor ([Kinds and settings](#design)). No new `SourceKind` variants. Not
    built yet: the `kind.changed` event, filter settings, `asset.list`, storing an
    absolute path under a known root in relative form, and kind versions on stored
    settings, which come with persistence.
12. **Deleting referenced objects.** Settled by
    [ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md): rejection stays
    the only delete, with no cascading edit. The error will list every blocker as a
    structured detail, and a `delete.plan` query will return the batch that removes the
    target and its blockers. Not built yet: today the error's `reason` names the
    blockers, and the client removes them itself.
