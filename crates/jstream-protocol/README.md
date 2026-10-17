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
  (`welcome`, `response`, `error`, `event`), `Request`/`Call` and `Response`/`CallResult`,
  and `ActionResult`, the data an action may answer with (`pairing.start`'s code,
  `youtube.sign_in`'s consent URL, a YouTube broadcast).
  `ClientMessage::parse` maps every parse failure to `malformed`.
- **History** (`history.rs`, [ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)):
  `HistoryCall` with its `HistoryOp` (`history.undo`, `history.redo`), the fourth call
  kind, and `HistoryDirection` (`undo`, `redo`), the `cause` of a `state.changed` and
  the `direction` of a `history_empty` error.
- **Live values** (`live.rs`, [ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)):
  `LiveOutcome` (`committed`, `cancelled`, `expired`), `GestureInfo` (one open gesture
  in a `live.list` result), the limits `MAX_GESTURES_PER_CLIENT` (16) and
  `MAX_LIVE_VALUES` (256), and `is_live_adjustable`, which says which setters `live.set`
  takes (`item.set_transform`, `item.set_geometry`, `item.set_crop`), so every
  implementation agrees.
- **Commands** (`command.rs`): `Edit` (`scene.create`, `scene.rename`, `scene.delete`,
  `scene.reorder`, `scene.set_role`, `item.add`, `item.remove`, `item.reorder`,
  `item.move`, `item.set_geometry`, `item.set_transform` (deprecated), `item.set_crop`,
  `item.set_visible`, `item.set_source`, `item.set_opacity`, `item.set_blend`,
  `canvas.set_size`, `source.create`, `source.rename`, `source.set_settings`,
  `source.duplicate`, `source.delete`, `filter.add`, `filter.remove`,
  `filter.set_enabled`, `filter.set_settings`, `filter.reorder`, `output.create`,
  `output.set_target`, `output.set_encoder`, `output.delete`, `encoder.create`,
  `encoder.update`, `encoder.delete`, `source.set_audio`, `bus.create`, `bus.rename`,
  `bus.set_fader`, `bus.delete`, `send.set`, `send.remove`, `hotkey.set`, `hotkey.clear`,
  `transition.create`, `transition.update`, `transition.delete`,
  `transition.set_default`, `scene.set_transitions`, `item.set_transition`), with
  `ItemTransform` (deprecated), `Crop`, `SourceKind`, `SceneRole` and `BlendMode`;
  `Action` (`output.start`, `output.stop`, `output.set_key`, `output.clear_key`,
  `scene.switch` with its optional `preset_id`, `studio.set_preview`,
  `audio.reset_loudness`, `preview.subscribe`, `preview.unsubscribe`,
  `pairing.start`, `device.revoke`, `youtube.sign_in`, `youtube.sign_out`,
  `youtube.create_broadcast`, `youtube.bind_broadcast`, `youtube.transition_broadcast`,
  `live.set`, `live.cancel`), with `Action::named_objects` and `Action::bind_problem`,
  which every implementation uses to check and follow a hotkey's action; `Batch`, whose
  optional `ends_gesture` commits a gesture; and the `Command`/`CommandClass`
  classification view.
- **Filters** (`filter.rs`, [ADR-0026](../../docs/adr/0026-source-filter-chains.md)):
  `FilterOwner` (tagged `type`; only `source` so far), `FilterChain` (`video`, `audio`),
  `FilterInfo` and `FilterChains`, the chains `SourceInfo` reports, and
  `MAX_CHAIN_FILTERS` (8).
- **Outputs** (`output.rs`): `OutputKind` (`rtmp`, `record` and `virtual_cam` so far), `RtmpTarget`
  (a generic `url`, or a `youtube` broadcast), `RecordContainer` (only `mp4`), `OutputInfo`, and `SecretString`, the write-only stream key, which
  never prints and never comes back from the engine
  ([R-SEC-001](../../docs/RULES.md#R-SEC-001)). `OutputKind::key_server` names the
  server a stream key is bound to (scheme, host and port), which the engine and the
  mock compare to remove a key that would otherwise follow its output elsewhere (#343).
- **Hotkeys** (`hotkey.rs`, [ADR-0024 §5](../../docs/adr/0024-scene-structure.md)):
  `Chord`, a key chord string with one spelling per chord (`Chord::problem`, shared by
  every implementation), and `HotkeyBinding`, a chord and its action as reads report it.
- **Encoder profiles** (`encoder.rs`): `EncoderProfile` (H.264 and AAC, with rate
  control, keyframe interval and hardware preference), its range checks
  (`EncoderProfile::problem`, shared by every implementation), `EncoderProfileInfo`,
  and `EncoderSessionInfo`, the per-output sharing `output.status` reports
  ([ADR-0027](../../docs/adr/0027-program-master-and-output-chains.md)).
- **Audio** (`audio.rs`, [ADR-0028](../../docs/adr/0028-audio-model-v2.md)):
  `SourceAudio` and `AudioFollow`, a source's audio settings; `BusInfo` and `SendInfo`,
  the buses and the send matrix as reads report them; the fader range and its shared
  check (`gain_db_problem`); the bus limit (`MAX_BUSES`), which the graph, the mock and
  the UI use and the engine's mixer is held to; and `SourceMeter`, `BusMeter` and `Loudness`, the readings
  a `meters.tick` carries.
- **Queries** (`query.rs`): `state.get`, `scene.list`, `source.list`, `item.list`,
  `output.list`, `output.status`, `encoder.list`, `bus.list`, `video.snapshot`, `kind.list`,
  `kind.describe`, `delete.plan` (with its `DeleteTarget`: a scene, a source or a bus),
  `studio.get`, `device.list` and `live.list`, with `QueryResult` and the
  `SceneSummary`, `SceneInfo`, `SourceInfo`, `ItemInfo` and `DeviceInfo` shapes the
  reads report. `state.get` carries the `Canvas`, and `ItemInfo`
  the item's `placement` form and its `geometry`, `SceneInfo` a scene's transition
  overrides, `ItemInfo` its `transition` and `state.get` the presets. `video.snapshot`
  returns the program mix as a PNG. `youtube.status`, `youtube.list_broadcasts` and
  `youtube.list_streams` read the YouTube account.
- **YouTube** (`youtube.rs`, [ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md)):
  `YoutubeAuth`, `BroadcastInfo`, `StreamInfo` and the enums they carry, mirroring the
  Live API with an `other` value for anything newer.
- **Transitions** (`transition.rs`, [ADR-0029](../../docs/adr/0029-transitions.md),
  [ADR-0037](../../docs/adr/0037-kind-descriptors-for-transitions-and-plugins.md)): the
  built-in transition kinds (`cut`, `fade`, `move`) and their descriptors, `Easing` with
  `Easing::apply`, the one easing function every implementation shares (golden-tested),
  `ItemTransition` (an item's delay, easing, glide and appear and disappear effects) with
  its range check, `TransitionPresetInfo` and `TransitionsInfo` as `state.get` reports
  them, and `TransitionStarted`, what a `studio.changed` says a switch ran.
- **Kinds and settings** (`kind.rs`, `settings.rs`,
  [ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)): `KindDescriptor`,
  `ParamDescriptor` and `ParamType`, the shapes that describe a source, filter or
  transition kind's parameters, and `Settings`, a map from parameter ID to a JSON value, whose `scene`,
  `source`, `bus` and `asset` references are tagged objects (`Reference`). The per-value checks
  (`ParamDescriptor::check`, `check_asset_path`) and the settings schema
  (`KindDescriptor::settings_schema`) are pure functions here, so every implementation
  shares them. No source or filter kind is defined here; the transition kinds are
  (Transitions above).
- **Events** (`event.rs`): `state.changed`, `output.state_changed`, `studio.changed`
  (program, preview and the transition a switch started), `events.dropped`,
  `stats.tick`, `meters.tick`, `youtube.status_changed`, `youtube.health_changed`,
  `live.changed` and `live.ended`.
- **Preview streams** (`preview.rs`, [ADR-0045](../../docs/adr/0045-leptos-web-ui.md)):
  `PreviewCodec` (`h264`, `jpeg`), `PreviewSettings` (`preview.subscribe`'s fields with
  their defaults and range checks, which the engine applies), and the binary
  frame: `BinaryFrame`, its fixed `BinaryHeader` with `StreamKind`, the byte layout's
  `to_bytes`/`parse` and `BinaryError`, and `KeyframeGate`, the drop rule every hop that
  queues frames applies ([Binary streams](#design)).
- **Errors** (`error.rs`): `ProtocolError`, a `thiserror` enum whose variants have stable
  codes, `Blocker`, the `blockers` detail of a refused delete, and `WireError`, its wire
  form with a message.
- **IDs and versions** (`id.rs`, `version.rs`): opaque string IDs (`BusId::master()` is
  the one fixed ID, `master`; a `GestureId` is chosen by the client), `Rev`, and
  `PROTOCOL_VERSION`.
- **Numbers** (`finite.rs`): `Finite32`, the only float type on the wire, which rejects
  NaN, infinity and out-of-range numbers at construction and deserialization. Computed
  results (`CanvasPoint`, `ContentRect`, `LegacyQuad`) use plain `f32`.
- **Geometry** (`geometry.rs`): [`ItemGeometry`](src/geometry.rs), `Canvas`,
  `PlacementForm` and the shared placement formula
  ([ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md)). `project` places the
  box, rotation about its centre, with the 3D fields in the formula and defaulting to
  neutral. `content` maps a cropped source with `stretch`, `fit` or `fill`.
  `transform_view` is the deprecated 0.3 view of a geometry that `ItemInfo.transform`
  reports. `legacy_quad` and `ItemGeometry::from_legacy` evaluate a 0.3 transform. A
  negative scale is not refused: `from_legacy` returns the drawn outline and flags the
  reflection, which geometry cannot store. `PROTOCOL_VERSION` is 0.27 because of
  transition presets and studio mode's preview (the `transition.*` edits,
  `scene.set_transitions`, `item.set_transition`, `scene.switch`'s `preset_id`,
  `studio.set_preview`, `studio.changed`'s `preview_scene` and `transition`, and the
  `transition` entity kind and kind category); 0.26 added live values
  ([Live values](#design): `live.set`, `live.cancel`, `live.list`, the `live.changed`
  and `live.ended` events, and `Batch.ends_gesture`); 0.25 added
  hotkeys (`hotkey.set`, `hotkey.clear`, `state.get`'s `hotkeys`, `delete.plan`'s
  `hotkeys` and the batch result's `removed_hotkeys`); 0.24 added bus
  references (the `bus` parameter type and reference, which the `waveform` and
  `spectrum` kinds' `audio` setting uses, a `bus` `delete.plan` target, and
  `bus.delete` refused while one refers to its bus); 0.23 added YouTube
  ([YouTube](#design): the `youtube.*` commands and events, the `youtube`
  RTMP target, and the YouTube actions' `ActionResult`s,
  [ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md)), 0.22
  opacity, blend and filter chains (`item.set_opacity`, `item.set_blend`, the
  `filter.*` edits, `SourceInfo.filters` and the `filter` entity kind,
  [ADR-0023](../../docs/adr/0023-placements-references-and-clones.md),
  [ADR-0026](../../docs/adr/0026-source-filter-chains.md)), 0.21 pairing
  (`pairing.start`, `device.list`, `device.revoke`, `ActionResult`, the
  `unsupported` error and the `device` entity kind,
  [ADR-0057](../../docs/adr/0057-server-answers-credential-calls.md)), 0.20
  the preview streams (`preview.subscribe`, `preview.unsubscribe` and the binary
  frame), 0.19 audio (sources' audio, buses, sends, `bus.list`, `meters.tick` and
  `audio.reset_loudness`), 0.18 the `virtual_cam` output kind, 0.17
  `scene.create`'s optional `role`, 0.16 `scene.switch`, `studio.get` and
  `studio.changed`, 0.15 `delete.plan` and the
  `blockers` detail on `invalid_argument`, 0.14 references
  and clones (`item.set_source`, `duplicated_from`), 0.13 encoder
  profiles and session sharing, 0.12 groups (`scene.set_role`,
  `item.move`, a scene's `role` and the placement edits' `scene_id`), 0.11 the canvas
  and the geometry wire surface ([Item geometry](#design)), 0.10 the `history` calls,
  0.9 the `record` output kind, 0.8 the kind queries and settings, 0.7 the output
  edits, `output.list`, the key actions and `key_set`, 0.6 `stats.tick` and 0.5
  `video.snapshot`.
- **Schema** (`schema.rs`): `json_schema()`, snapshot-tested in
  [`schema/protocol.snap`](schema/protocol.snap), with every fixture message's wire form in
  [`schema/examples.snap`](schema/examples.snap).
- **Parity fixture** (`parity.rs`, only with the non-default `fixtures` feature): test
  support, not wire types. One batch that builds a small collection (a non-default
  canvas, legacy items, one geometry item, two RTMP outputs, a virtual camera, a recording output, two
  encoder profiles and two hotkeys, one of them rebound), the exact answer to every collection read of it (`output.list`
  included), and the exact error for batches rejected against it (geometry, canvas, RTMP
  URL, recording path and encoder profile and hotkey checks, and `item.set_source` and
  `duplicated_from`, opacity, blend and filter checks, included). The collection also
  holds a clone made with `source.duplicate` and `item.set_source`, a
  `duplicated_from` naming a missing source, items at opacity 0.25 and 0.5, a desktop
  audio source with an audio chain of two gain filters, one disabled, and audio: a
  microphone with its fader, mute and sends, two buses (one deleted again with its
  send), a renamed and faded bus, a duplicated microphone with a send removed, and a
  disabled nested scene. The rejected batches cover the bus limit, the master bus,
  fader ranges, and sources with no audio or no fader of their own.
  `jstream-graph` and `jstream-control`'s mock engine are both layer 2, so neither can
  test against the other ([R-ARCH-002](../../docs/RULES.md#R-ARCH-002)); both enable
  the feature in dev-dependencies only and are held to these answers. `kinds()` names
  the kinds an implementation must describe for the batch to apply (`webcam`, with
  video and no audio, and `desktop_audio` and `mic`, with audio and no video, none with
  parameters, so every implementation reports the same empty settings and audio
  defaults, and the audio filter kind `gain`, whose one parameter the batch always
  sets). It changes no
  message, so it never changes `PROTOCOL_VERSION`.

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-protocol/src/**", "crates/jstream-protocol/schema/**"]
depends_on = ["docs/adr/0013-command-model.md", "docs/RULES.md#R-API-001", "docs/RULES.md#R-API-002", "docs/RULES.md#R-API-004", "docs/RULES.md#R-ARCH-006"]
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
{"type":"hello","protocol":{"major":0,"minor":26},"client_name":"jstream-cli"}
{"type":"welcome","protocol":{"major":0,"minor":26},"client_id":"cli_7","rev":12}
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

- `type` names the message (`request`), the command, query, history op or result
  (`scene.create`, `history.undo`), or the event (`state.changed`). Command, query,
  history and event names are `noun.verb` in
  snake_case ([R-API-002](../../docs/RULES.md#R-API-002)), so the tag *is* the name that
  MCP tools, the CLI and logs use.
- `kind` names the class of a call and its result: `batch`, `action`, `query` or
  `history`. It is a
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
  devices, files, secrets, time or runtime state (starting an output, going live,
  storing a stream key, OAuth, switching the program scene). Never undoable or
  batchable. The response says the action was accepted, and carries an
  `ActionResult` only for an action that answers with data (`pairing.start`'s code,
  0.21; `youtube.sign_in`'s consent URL and the broadcast a YouTube broadcast action
  made or changed, 0.23; every other action omits `result`); progress arrives as events
  (`output.state_changed`).

The classification rule: **if the command's whole effect is a change to the collection
state, and its inverse can be computed from the state before it, it is an edit.
Otherwise it is an action.** A command that does both is split into an edit and an
action. A **query** (`Query`) changes nothing and is neither.

The types enforce the split: a `Batch` holds only `Edit`s, an action travels alone, and
`Call` (tagged by `kind`) has exactly four forms: `batch`, `action`, `query` and
`history`. A `history` call carries no command: it names a direction and applies edits
the engine already holds (see *Undo* below), so it is neither class and R-ARCH-006
still covers every command.
`Command` is a classification view over both enums, with `class()`, `is_undoable()` and
`is_batchable()`. The tests `names_are_valid` and `every_command_is_classified` check
every name in the generated schema.

Creating edits carry a client-chosen ID (`scene_id`, `item_id`, `output_id`). A batch can then refer
to an object it creates earlier in the same batch, a retry can't create a duplicate,
and an undo can re-create a deleted object under its old ID. IDs are opaque strings.

Sources are shared instances, independent of scenes. `source.create` adds one, and
`item.add` places an existing source by reference, so one source can appear in many
scenes, or several times in one, as the same capture. `source.duplicate` makes a
separate copy under a new ID, with the same kind, settings and filters, and records the source it
copied in the copy's `duplicated_from`; `source.rename` renames one. An item placing a
shared source is a **reference**, and one placing a copy is a **clone**
([ADR-0023](../../docs/adr/0023-placements-references-and-clones.md)). `item.set_source`
(0.14) changes which source an item places and keeps the item's ID, placement and stacking
position, so a client converts between the two in one batch: `source.duplicate` then
`item.set_source` to the copy, or `item.set_source` back to `duplicated_from` then
`source.delete` of the unplaced copy. `duplicated_from` is a plain link, not a
dependency: deleting the original is allowed, `SourceInfo` then omits the link, and
`source.create` takes it (never naming the new source itself) so that undoing a
`source.delete` restores it. A `SourceKind` is `builtin` (a kind the engine describes) or `scene`, which
nests another scene. Positions are indexes: scenes in display order, items in stacking
order with 0 at the bottom.

### Groups and placement addressing

[ADR-0024](../../docs/adr/0024-scene-structure.md) §1 makes a group a scene with
`role: group` (protocol 0.12). `SceneRole` is `scene` or `group`. `scene.create` takes
an optional `role` (omitted means `scene`; protocol 0.17), and `scene.set_role { scene_id, role }`
changes it; the role changes no engine behaviour.
`scene.list` and `state.get` report `role`, omitted for `scene`, so a 0.11 reader sees
no change for ordinary scenes. `item.move { item_id, scene_id, index }` moves an item
to a position in another scene, keeping its ID and every placement property, under the
same cycle check as `item.add` (field `scene_id`). `index` is the item's position after
the move: up to the number of items already there for another scene, and below it for
the item's own scene, where the move is a reorder. Grouping is a client-built batch:
`scene.create` with `role: group`, `source.create` of a `scene` source,
`item.add` of it with `item.move` to the lowest selected index, then an `item.move` of
each selected item into the group, bottom first.

The placement edits (`item.set_geometry`, `item.set_transform`, `item.set_crop`,
`item.set_visible`, `item.set_opacity`) take an
optional `scene_id`, fixed now so that keyframe scenes can arrive additively (ADR-0024
§2). Absent, or the item's own scene, changes the item. Until keyframe scenes exist,
any other existing scene is `invalid_argument` on `edits.N.scene_id`, and a missing one
`not_found`. An inverse keeps the addressing of the edit it undoes.

### Audio

[ADR-0028](../../docs/adr/0028-audio-model-v2.md) makes audio part of the collection
(0.19). An **audio source** is one whose kind has audio (`KindDescriptor.audio`), or a
`scene` source, whose audio is what the scene shows. Its `SourceInfo.audio` holds
`enabled`, `follow` (`always` or `when_shown`), its fader `gain_db` and `muted`; every
other source omits `audio`. A new source takes its kind's defaults: on, at 0 dB,
unmuted, following `always` for a kind with audio and no video (an audio input) and
`when_shown` for every other audio source, a `scene` source included.
`source.set_audio { source_id, enabled?, follow?, gain_db?, muted? }` sets any of them
and needs at least one. A `scene` source has no fader, mute or sends of its own: its
`gain_db` and `muted` stay 0 and false, and setting either is `invalid_argument`.

**Buses** are collection objects with an ID, a name and a fader (`gain_db`, `muted`).
Every collection has the master bus, ID `master`, named `Master`; it can be renamed and
faded but never deleted. `bus.create`, `bus.rename`, `bus.set_fader` and `bus.delete`
manage the others, at most 16 buses in all (`MAX_BUSES`). **Sends** route a source's audio into a
bus, post-fader, at a gain: `send.set { source_id, bus_id, gain_db }` creates one or
changes its gain, `send.remove` removes it. Only a source with a channel of its own
sends, so not a `scene` source. A new channel source sends to the master at 0 dB.
Deleting a bus removes the sends into it, and deleting a source removes its sends;
the inverses put them back. Since 0.24 a setting can refer to a bus (a `bus`
reference, which the `waveform` and `spectrum` kinds' `audio` uses to draw it), and
`bus.delete` is refused while one does, as for a scene or source. Every gain is in dB from −100 to +24; silence is `muted`.

Buses never feed buses (ADR-0028), so a send always runs from a source to a bus, and
an audio routing cycle can't be expressed. Filter chains (0.22) belong only to
sources (`FilterOwner` is `source`), so none feeds one bus from another either; a
routing check comes with the first thing that could. `follow`'s activity gain is the engine's (D6); the
protocol only stores it.

Levels and loudness are runtime state: `meters.tick` carries them (see
[Events](#design)), never a query, and `audio.reset_loudness` restarts the master's
integrated loudness, an action because it changes no collection state.

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
  `accept`; `scene`; `source` with an optional `media`; `bus`, since 0.24), a `default`, a `label`, an
  optional `description`, the UI hints `group`, `advanced` and `visible_if`, and `live`.
  An unknown `type` parses as `ParamType::Unknown`, which a client shows read-only.
  `step`, `multiline` and the hints never affect validation.
- Values are plain JSON. Colours are `#rrggbb`, or `#rrggbbaa` when `alpha` is set.
  References are tagged objects: `{"type":"scene","scene_id":…}`,
  `{"type":"source","source_id":…}`, `{"type":"bus","bus_id":…}` (0.24) and
  `{"type":"asset","root":…,"path":…}`, with
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
  reference to a missing scene, source or bus is `not_found`. A `source` reference that
  requires `media` rejects a source whose kind doesn't produce it. Asset paths under
  `collection` and `app` are relative, with `/` separators and no empty or `..` segment,
  drive letter, leading `/` or `:` (which names an NTFS alternate data stream on Windows).
  Under `absolute` the path is `/…`, `\…`, `C:\…` or `C:/…`, with no `..` segment, no
  second leading separator (`\\host\share`, `//host/share`, `\\.\…` and `\\?\…` name
  a UNC share or a device, and opening one on Windows hands the engine's credentials to
  that host), no `:` but the drive letter's, and no `?` or `*` (Win32 never allows
  them in a file name, and `\??\UNC\…` or `\??\GLOBALROOT\Device\…` is an NT object
  path that reaches the same hosts and devices). Under every root, the last component
  must not name a DOS device, which Win32 opens in place of a file in any folder: a
  stem (the name before the first `.`, without trailing spaces, in any case) of `CON`,
  `PRN`, `AUX`, `NUL`, `COM0`–`COM9`, `COM¹`–`COM³`, `LPT0`–`LPT9`, `LPT¹`–`LPT³`,
  `CONIN$` or `CONOUT$`. So `C:\x\nul`, `\CON .png` and `/x/aux.txt` are refused under
  `absolute`, and `CON.png` and `media/nul` under `collection` or `app` (joined to the
  root folder, they still open the device), while `CONSOLE.png` or `COM10.png` is not.
  Earlier components may use these names: Win32 fails to open such a path, and elsewhere
  they are ordinary folders. An asset that doesn't exist yet is accepted.
- **References are dependency edges.** Items placing sources, `scene` sources showing
  scenes, and scene, source and bus references in settings (a filter's settings count
  as its source's) form one dependency graph. An edit that would close a cycle
  (`item.add`, `item.move`, `item.set_source`, `source.create`, `source.set_settings`,
  `filter.add`, `filter.set_settings`) is
  rejected with `invalid_argument` naming it (for example scene `a` -> source `sa` ->
  scene `a`). A bus depends on nothing, so a bus reference never closes one. Asset
  references are not edges.
- **Deletes are refused while anything refers to the target**, with no cascading edit
  ([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)).
  `source.delete` is refused while an item places the source, and `scene.delete` while a
  `scene` source shows the scene; either, and `bus.delete` (0.24), is refused while a
  setting refers to its target. The `invalid_argument` lists every blocker in `blockers`, each a `Blocker`
  `{entity, id, field}`: the item (`source_id`), the `scene` source (`kind.scene_id`)
  or the source whose setting refers to it (`settings.<param>`, or
  `filters.<filter_id>.<param>` for a filter's setting). `delete.plan` returns
  the batch that removes them with the target (see [Queries](#design)). Hotkey
  bindings are never blockers: a delete removes the bindings that name its target
  (below).

`SourceInfo` reports `settings`, omitted when empty (a `scene` source, or a kind with no
parameters), so a reader of 0.7 messages sees no change for those.

### Opacity, blend and filters

All of this is added in 0.22. A placement has an `opacity`, an alpha multiplier from 0 to 1 (default 1), and a
`BlendMode` (`normal`, `additive`, `multiply`, `screen`, `overlay`, `darken`, `lighten`,
`difference`, `exclusion`; default `normal`). `item.add` takes both, optional;
`item.set_opacity { item_id, scene_id?, opacity }` and `item.set_blend { item_id, blend }`
set them. Out of range is `invalid_argument` on `edits.N.opacity`, and every mode but
`normal` is `invalid_argument` on `edits.N.blend` until the compositor draws it.
`ItemInfo` reports both.

A source has a video and an audio filter chain
([ADR-0026](../../docs/adr/0026-source-filter-chains.md)). The `filter.*` edits address a
filter by `owner` (`{"type":"source","source_id":…}`) and `filter_id`, unique within the
owner: `filter.add { owner, chain, filter_id, kind, settings?, enabled?, index? }`,
`filter.remove`, `filter.set_enabled { enabled }`, `filter.set_settings { patch }` (the
rules of `source.set_settings`) and `filter.reorder { index }` within the chain. A
source has a video chain if its kind produces video and an audio chain if it produces
audio and is not a `scene` source; `kind` must be a `video_filter` or `audio_filter`
kind for that chain (`invalid_argument` on `edits.N.kind`), and a chain holds at most
8 filters (`edits.N.chain`). A missing filter is `not_found` with entity `filter`.
`SourceInfo.filters { video, audio }` reports them, each omitted while empty.

Every float on the wire (`Vec2`, `ItemTransform`, `ItemGeometry`, an opacity) is a `Finite32`:
an `f32` that is never NaN or infinite. Computed geometry results use plain `f32`. serde
would otherwise narrow an out-of-range JSON number such as `1e39` to infinity, which
then serializes as `null` and no longer parses. So a
non-finite or out-of-`f32`-range number fails deserialization, and
`ClientMessage::parse` rejects the whole message as `malformed`. In-process clients
can't build a non-finite value either: `Finite32::new` and `Vec2::new` return `None`
for one. Settings values keep non-integer JSON numbers as `f64`, parsed to the nearest
one (serde_json's `float_roundtrip`), so a message with a long number still serializes
and parses back to itself.

Outputs are collection state too: `output.create` adds one, stopped, with its
`OutputKind`; `output.set_target` changes where an RTMP output sends, from its next
`output.start`; `output.delete` removes one, and is refused with `invalid_argument`
while it runs. A target URL must be an `rtmp://` or `rtmps://` server URL with a host and an
application name, and is refused if it could carry a credential (`user:password@`, a
query or a fragment). A key pasted into the path can't be told from a nested
application name, so it is stored as given. A `record` output (0.9) records to a file
on the engine's machine: `path` must be absolute (`/…`, `\…`, `C:\…` or `C:/…`), have
no `..` segment and end in a file name. As for an `absolute` asset path, it must not
start with a second separator (a UNC share or device, `\\host\share\…`, `\\.\…`,
`\\?\…`), nor contain `:` but the drive letter's, `?` or `*` (`\??\UNC\…`), nor
end in a DOS device name (`NUL`, `nul.mp4`, `CON`, `COM1`, `LPT1`, …), by the same rule
as an asset path's last component. And
`container` is `mp4` (fragmented). `output.start` writes `<path>.part` and fails with
`action_failed` when `path` or `<path>.part` already exists, so a recording never
overwrites; `output.stop` renames it to `path`. A recording output has no RTMP target,
so `output.set_target` refuses it with `invalid_argument`; delete and re-create it to
record elsewhere. A `virtual_cam` output (0.18) has no fields: it offers the program
video to other apps on the engine's machine as a webcam, with no audio, at the canvas
size it has when it starts and 30 fps. `output.start` fails with `action_failed` when
the engine has no virtual camera, the camera isn't registered on the machine, or it
can't offer that size. It has no target either, so `output.set_target` refuses it too,
and it encodes nothing, so its encoder profile is stored but unused. A 0.9 to 0.17
client reads it as `Unknown`. The
stream key is not collection state: it is a secret held in the OS credential store,
so it is set with the `output.set_key` action and removed with `output.clear_key`. A
`youtube` target (0.23) names a broadcast instead of a URL: its ingest and key come
from YouTube on each `output.start` ([YouTube](#design)), so it needs no
`output.set_key`. Its ID must be ASCII letters, digits, `-` and `_`, at most 128 bytes.
It is write-only ([R-SEC-001](../../docs/RULES.md#R-SEC-001)): no response, event,
error, query or log carries it, and `output.status` reports only `key_set`. A key is
bound to the server it was set for (#343): when an applied batch, undo or redo deletes
the output, or points it at another server (`OutputKind::key_server`: scheme, host and
port; a new path on the same server keeps the key), the engine removes the key from the
credential store, and refuses that output's `output.start` with `action_failed` until
the key is gone. So a retarget never sends the old key to the new server, and an output
re-created with a deleted one's ID starts with no key. Undoing the delete or the
retarget restores the target but not the secret: set it again with `output.set_key`.
A start already opening when the retarget lands opens the server it checked, so while
an output's `output.start` is opening, `output.set_key` for it fails with
`action_failed`: a key sent for the new server waits until the start has settled.
The two stay edits under the classification rule above: their effect on the collection
state, and its inverse, are unchanged, and the key, which is not collection state, is
removed by the engine guarding it, as a deleted output's runtime state is dropped.
`output.clear_key` succeeds for an output no longer in the collection. No message
changed, so this changes no `PROTOCOL_VERSION`. Its errors carry fixed reasons that
never quote the key.

Encoder profiles (0.13) are collection state as well
([ADR-0027](../../docs/adr/0027-program-master-and-output-chains.md)).
`encoder.create` and `encoder.update` take a `profile_id`, a `name` and an
`EncoderProfile`: H.264 video with `cbr` or `vbr` rate control (100..=100000 kbit/s;
a `vbr` peak at least the average and at most 200000), a keyframe interval of 1..=10 s
and a hardware preference (`prefer`, `require` or `avoid`, the default), and AAC audio
at 32..=320 kbit/s. Anything else, including a value a newer server sent as `unknown`,
is refused with `invalid_argument` naming the field. `encoder.delete` is refused while
an output uses the profile. `output.set_encoder` points an output at a profile, or with
no `profile_id` back at the engine's default (H.264 CBR 2500 kbit/s, a keyframe every
2 s, software, AAC 160 kbit/s). Both apply from the output's next `output.start`.
Running outputs with the same mix and the same profile, as it was when each started,
share one encoder session, and `output.status` reports the sharing.

Hotkey bindings (0.25) are collection state too
([ADR-0024 §5](../../docs/adr/0024-scene-structure.md),
[ADR-0040 §3](../../docs/adr/0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md)):
a `Chord` bound to an `Action`, which a client that listens for keys sends when the
chord is pressed. The engine only stores bindings; the desktop host (OS-global
shortcuts) and the web UI (in-page ones) dispatch them, and the headless daemon none.
- **Chords.** Zero or more of the modifiers `Ctrl`, `Alt`, `Shift` and `Meta`, each at
  most once and in that order, then one key, joined by `+`: `F1`, `Ctrl+Digit1`,
  `Ctrl+Shift+KeyM`. The key is a W3C UI Events
  [`KeyboardEvent.code`](https://www.w3.org/TR/uievents-code/) value from
  `Chord::KEYS` (letters, digits, punctuation, function keys F1–F24, editing and
  arrow keys, the keypad and the media keys): the physical key, whatever the layout,
  which is what browsers and Tauri's global-shortcut API report. A chord has exactly
  one spelling, so two bindings are on the same keys exactly when their strings are
  equal. Anything else is `invalid_argument` on `chord`, with `Chord::problem`'s reason.
- **`hotkey.set {chord, action}`** binds a chord; its inverse is `hotkey.clear`. It is
  refused with `already_exists` (entity `hotkey`, the chord as `id`) for a chord that
  is bound: rebinding is a batch of `hotkey.clear` then `hotkey.set`. `action` is an
  action request, checked as an `action` call is, and only actions can be bound, so a
  hotkey never adds to an undo history. `output.set_key` can't be bound (its stream key
  would be stored in the collection, [R-SEC-001](../../docs/RULES.md#R-SEC-001)), nor
  can the preview actions (they serve only the sending session), `live.set` and
  `live.cancel` (their gesture belongs to the sending session), `pairing.start` and
  `device.revoke` (the server answers them for the client that asks), or the
  `youtube.*` actions (YouTube is driven through the UI,
  [ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md)): each is
  `invalid_argument` on `action.type`. A scene, output or preset the action names must
  exist, else `not_found`.
- **`hotkey.clear {chord}`** removes a binding, or is `not_found` (entity `hotkey`); its
  inverse is `hotkey.set` with the action.
- **Deleting what a binding names removes the binding**, in the same edit:
  `scene.delete`, `source.delete`, `output.delete`, `encoder.delete` and
  `transition.delete` drop every binding whose action names their target
  (`Action::named_objects`; today scenes, outputs and the preset a `scene.switch`
  names), and their inverses restore the
  bindings after the target. If another client has bound one of those chords since,
  that `hotkey.set` fails and so does the undo, rather than overwriting the binding.
  A batch's result lists the bindings it removed (`removed_hotkeys`), and `delete.plan`
  lists the ones its batch would (`hotkeys`), so a client can say "this also removes
  the hotkey Ctrl+Digit1".
- Reads report a binding as a `HotkeyBinding {chord, action}` whose `action` is plain
  JSON, so a client older than an action type still reads the collection; it keeps the
  binding and can't run it. `HotkeyBinding::action` parses it.

### Transitions and studio mode

[ADR-0029](../../docs/adr/0029-transitions.md) and
[ADR-0037](../../docs/adr/0037-kind-descriptors-for-transitions-and-plugins.md) decide
the model (0.27). A transition preset is collection state:
`{preset_id, name, kind, duration_ms, easing, settings}`, where `kind` is a transition
kind `kind.list` reports (category `transition`: `cut`, `fade` and `move` so far) and
`settings` are checked against its descriptor as a source's are (`unmatched`, the
effect for items nothing pairs, and `stagger_ms`, an extra delay per incoming item by
stacking order, for `fade` and `move`; [ADR-0052](../../docs/adr/0052-transition-mechanics-and-studio-preview.md)).

- `transition.create` adds a preset, `transition.update` changes its fields or patches
  its settings (a new `kind` resets the settings to that kind's defaults, overlaid by
  the patch), and `transition.delete` removes one, clearing the scene overrides and
  removing the hotkey bindings that name it in the same edit; its inverse re-creates
  the preset and restores them. The
  default preset can't be deleted (`invalid_argument` on `preset_id`);
  `transition.set_default` moves it. A new collection holds one preset, `cut`, as its
  default.
- `scene.set_transitions {scene_id, transition_in?, transition_out?}` sets both of a
  scene's overrides (absent clears one), and `item.set_transition {item_id,
  transition}` an item's `ItemTransition {delay_ms, easing, glide, appear,
  disappear}`, omitted from `ItemInfo` while it is the default.
- `scene.switch {scene_id, preset_id?}` resolves the preset in order: the one it names
  (`not_found` when there is no such preset), else the new scene's `transition_in`,
  else the current scene's `transition_out`, else the default. A switch while a
  transition runs starts the new one from what is on screen.
- `studio.set_preview {scene_id}` puts a scene on preview, studio mode's next scene.
  Like a switch it changes no `rev` and is never undone; a switch leaves preview where
  it is.

`Easing::apply` is the one easing function every implementation evaluates, so the UI's
preview of a transition and the engine's render agree.

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
  produced, with `removed_hotkeys` when its deletes removed hotkey bindings (0.25;
  omitted when there are none). Queries that read collection state report the `rev` they read.

### Queries

Queries read and never change `rev`. Those that read the collection state carry the
`rev` they read, so a client can send its next batch with that `expected_rev`:

- `state.get`: the whole collection at one revision: the `canvas` (missing from a
  pre-0.11 server, it parses as 1920×1080), every source, every scene in display order
  with its items, every output (`outputs`, ordered by ID; missing from a pre-0.7
  server, it parses as empty) and every encoder profile (`encoders`, ordered by ID;
  missing from a pre-0.13 server, it parses as empty), the `buses` and `sends`
  (as `bus.list`; missing from a pre-0.19 server, both parse as empty), every
  hotkey binding (`hotkeys`, ordered by chord; missing from a pre-0.25 server, it
  parses as empty), and the transition presets with the default (`transitions`;
  missing from a pre-0.27 server, it parses as the one initial `cut`). A client reads
  it on connecting, and again after a
  `state.changed` it didn't cause.
- `scene.list`: the scenes in display order, each with its name, `role` (omitted for a
  `scene`) and item count.
- `source.list`: the sources, ordered by ID (sources have no display order), each with
  its name, `SourceKind`, settings, filters and, for an audio source, its `audio`.
- `bus.list`: the buses, ordered by ID (the master among them), and every send,
  ordered by bus and then by source, at a `rev` (0.19).
- `item.list {scene_id}`: one scene's items in stacking order, bottom first, or
  `not_found` for a missing scene.
- `output.list`: the outputs, ordered by ID (outputs have no display order), each with
  its `OutputKind` and `encoder_profile` (absent for the default), at a `rev`.
- `encoder.list`: the encoder profiles, ordered by ID, at a `rev` (0.13).
- `output.status {output_id}`: an output's runtime state and `key_set`, whether the
  credential store holds its key (missing from a pre-0.7 server, it parses as
  `false`). While it runs, `encoder` names its session's profile (absent for the
  default), the other running outputs that share the session (`shared_with`, by ID)
  and whether the session is a hardware one; `encoder_sessions` and
  `hardware_sessions` count the video sessions open across every output, since
  hardware encoders cap them per GPU vendor and driver and the engine doesn't guess
  the cap (0.13; missing, they parse as absent and 0). None of it is collection state,
  so it carries no `rev`.
- `kind.list` and `kind.describe {kind_id}`: the kinds the engine describes, above.
  Not collection state, so no `rev`.
- `studio.get`: the scene on program (`program_scene`) and on preview
  (`preview_scene`, 0.27), each omitted only while the collection has no scene. Runtime
  state, not collection state
  ([ADR-0030](../../docs/adr/0030-studio-state-and-scene-switching.md)), so no `rev`.
  Added in 0.16.
- `device.list`: the devices paired with the listener this client reached, each with
  its `device_id`, `name`, and when it was paired and last used (Unix seconds). Not
  collection state, so no `rev`. Added in 0.21 (see *Pairing* below).
- `delete.plan {target}`: the batch that deletes a scene, source or bus (`target` is
  `{"type":"scene","scene_id":…}`, `{"type":"source","source_id":…}` or, since 0.24,
  `{"type":"bus","bus_id":…}`) with everything that refers to it, as `edits` and the
  `rev` it was planned at, or `not_found` for a missing target (and
  `invalid_argument` on `target.bus_id` for the master, which is never deleted). In order, the edits remove the items that place a
  deleted source, set every settings reference to a deleted object to `null` (one
  `source.set_settings` per source, which keeps the source, and one
  `filter.set_settings` per filter), delete the `scene` sources
  that show a deleted scene, and last delete the target. Deleting a scene deletes the
  `scene` sources that show it, and the cascade stops there, since deleting a source
  deletes no scene. A bus is held only by settings references, which the plan clears;
  its `bus.delete` takes its sends. A target nothing refers to plans just its own
  delete. `hotkeys` lists the bindings the batch removes with what they name, ordered
  by chord (0.25; omitted when there are none); they need no edit of their own. The client
  shows the plan, then sends it as an ordinary batch with that `expected_rev`, so it is
  refused if the state moved on, and its undo is the batch's inverse.
- `video.snapshot`: the current program mix as a PNG. `png` is the file in standard
  base64 with padding and no line breaks. `width` and `height` are that image's size;
  the longest side is at most 1280. `rev` is the collection revision of the layout.
  Source pixels are runtime state and can change without a new `rev`. The query takes
  no arguments and changes nothing.
  **Size.** The PNG is RGBA8 and nothing caps its byte size, so a noise-like frame,
  which PNG can't compress, can be as large as its raw pixels: the image is the
  canvas, scaled down so its longest side is at most 1280. The default 1920×1080
  canvas gives 1280×720, 3.7 MB raw, and `png` can reach about 4.9 MB of base64; a
  square canvas about 8.7 MB. A typical frame is far smaller. The 4 MiB limit `jstream-server` sets applies only to messages
  it receives, not to its answers
  ([jstream-server › Design](../jstream-server/README.md#design)), so a client must
  accept an answer of that size: `jstream-control`'s WebSocket client keeps
  tungstenite's default limits, which are well above it.

**Pairing** (0.21, [ADR-0044](../../docs/adr/0044-pairing-remote-browsers.md)).
`pairing.start` opens a one-time code and answers it in `result`
(`{"type":"pairing.start","code":"XXXX-XXXX","expires_in_secs":120}`), `device.list`
lists the paired devices, and `device.revoke {device_id}` forgets one (`not_found` with
entity `device` for an unknown one). They concern whom a listener admits, so the server
in front of the engine answers them
([ADR-0057](../../docs/adr/0057-server-answers-credential-calls.md),
[jstream-server › Design](../jstream-server/README.md#design)); a `Control` with no
server in front answers `unsupported`. The code is a `SecretString`: it never prints,
and nothing but this answer carries it ([R-SEC-001](../../docs/RULES.md#R-SEC-001)).

An item is reported with its `source_id`, `placement` form, `transform`, `geometry`,
`crop`, `visible`, `opacity`, `blend` and `transition`, and a source with its name, kind, settings and
filters: every property an
edit sets, so a client can compute any edit, or its inverse, from what it read
([Item geometry](#design) says what `transform` and `geometry` hold for each form). The shapes (`SourceInfo`, `SceneInfo`, `ItemInfo`) mirror `jstream-graph`'s model,
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
that sets it: `scene.reorder` and `item.reorder` restore positions,
`item.set_visible` and `item.set_crop` restore what `item.add` doesn't take,
`item.set_opacity` and `item.set_blend` restore what they set,
`canvas.set_size` restores the canvas, and `item.set_geometry` and
`item.set_transform` each restore the previous placement in its own form. So undoing
`item.remove` is `item.add` (with `geometry` or `transform`, matching the removed
item's form) plus whichever of `item.reorder`, `item.set_crop`, `item.set_visible`
and `item.set_transition` the removed item needs, and undoing `scene.delete` is
`scene.create` with the scene's `role` (omitted for `scene`), `scene.reorder` unless it
was last, `scene.set_transitions` if it had an override, and each item's restore,
bottom to top. Undoing `transition.delete` is `transition.create`, then a
`scene.set_transitions` for each override it cleared, and a `hotkey.set` for each
binding it removed. `item.move` and `scene.set_role` are each undone by the same
edit with the old scene and index, or the old role. Undoing
`source.delete` is `source.create` with the complete stored settings and its
`duplicated_from`, then `source.set_audio` and `send.set` (or `send.remove` of the
default master send) for whatever audio differs from the kind's defaults, then a
`filter.add` per filter; undoing a filter edit is its opposite (`filter.remove` and
`filter.add` with the filter's place, switch and settings) or the previous value,
undoing `item.set_source` is `item.set_source` back, and undoing
`source.set_settings` is a patch of the old values. Undoing `bus.delete` is
`bus.create`, `bus.set_fader` unless the fader was at its default, and a `send.set` for
each send the bus had; `source.set_audio`, `bus.set_fader` and `send.set` are each
undone by the same edit with the old values, and a new send by `send.remove`. Deleting a source that items still
place or a setting refers to, a scene that a `scene` source shows or a setting refers
to, or a bus a setting refers to, is rejected, so no edit removes more than the object
it names (a bus's sends are part of it), apart from the scene overrides a deleted preset
clears and the hotkey bindings a delete removes, which its inverse restores. `jstream-graph`'s property
test checks that applying a random batch and then its inverse restores the state
exactly. A new edit that can't state its inverse in edits needs new edits first.

**Undo and redo requests** follow [ADR-0032](../../docs/adr/0032-live-edits-and-undo.md):

```json
{"type":"request","id":"r4","call":{"kind":"history","op":{"type":"history.undo"},"expected_rev":15}}
{"type":"response","id":"r4","result":{"kind":"history","rev":16,"step_rev":13,"undo_depth":4,"redo_depth":1}}
```

- `history.undo` applies the inverse of the client's most recent step that isn't
  undone, and `history.redo` applies its most recently undone step again. Each is one
  `rev` bump and emits `state.changed` with `cause` `undo` or `redo`, but makes no new
  step: the step moves between the client's undo and redo stacks. The client's next
  batch clears its redo stack. Another client's batches never touch either stack.
- `expected_rev` and `idempotency_key` work as for a batch, and keys are shared with
  the client's batches. The engine checks in this order:
  1. **The key.** A key the client already used gets that call's stored result, whatever
     `expected_rev` says now, so a retried undo whose response was lost never undoes a
     second step. A batch's retry works the same way.
  2. **`expected_rev`.** A stale one fails with `rev_conflict` and leaves both stacks
     as they were. This is what ADR-0032 means by "checked first": no part of the
     history is read or changed before it passes.
  3. **The stack.** An empty one fails with `history_empty`.
- `rev` is the revision produced, `step_rev` the revision the step's batch originally
  produced (it names the step), and the depths say what can be undone and redone next.
- `undo_conflict` (`step_rev`, and the `client_id` and `rev` of the conflicting batch)
  refuses an undo or redo when another client's batch after it changed a property the
  step sets, created or deleted an object the step touches, touched an object the step
  creates or deletes, also reordered the scenes or also set the canvas size, or made its edits
  no longer apply. An item's transform and its geometry are one property, its
  placement, and which source it places (`item.set_source`) is another. Each audio
  field a `source.set_audio` or `bus.set_fader` sets is its own property, and a send is
  an object of its own, which uses its source and its bus. An
  edit touches the objects it creates, deletes or edits and the ones it places or
  names, such as an item's source. What the step touches is taken both from when it was recorded and from
  the edits it would apply now. Nothing is applied and the step is dropped, so the next
  undo reaches the step before it.
- A step whose edits fail for a reason no other client caused keeps its place and the
  call fails with the graph's own error. The usual case is a running output that blocks
  undoing its `output.create`, which fails with `invalid_argument` until the output
  stops.
- `history_empty` (`direction`) means there is nothing to undo or redo.
- Actions (outputs, stream keys, live values), queries and history calls are never
  steps; a gesture's step is its committing batch. The engine
  keeps 100 steps per client by default, in memory, for the session's lifetime.

### Live values

Live values (0.26) keep a drag, a fader move or a live dialog to one batch and one undo
step ([ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)). A client opens a **gesture**,
named by a `gesture_id` it chooses, and sends its values as it goes:

```json
{"type":"request","id":"r7","call":{"kind":"action","action":{"type":"live.set","gesture_id":"drag_1","values":[{"type":"item.set_geometry","item_id":"itm_1","geometry":{"center":{"x":800,"y":450},"size":{"width":480,"height":270}}}]}}}
{"type":"response","id":"r7","result":{"kind":"action"}}
{"type":"request","id":"r9","call":{"kind":"batch","edits":[{"type":"item.set_geometry","item_id":"itm_1","geometry":{"center":{"x":820,"y":450},"size":{"width":480,"height":270}}}],"ends_gesture":"drag_1"}}
```

- `live.set {gesture_id, values}` opens the gesture if it is new and sets its values:
  setter edits, the same ones a batch carries. Each overrides one parameter on output
  from the next render tick, and replaces the gesture's earlier value for it. A live
  value is runtime state: it never changes `rev`, is never an undo step, is never saved,
  and `state.get` and `item.list` never show it. The live-adjustable setters are
  `item.set_transform`, `item.set_geometry` (one parameter, the placement) and
  `item.set_crop`; audio faders and kind parameters marked `live` follow as their
  real-time paths exist. Values are checked as a batch's edits are, named
  `values.<n>.<field>`. Any other edit (`values.<n>.type`), an empty list, more than 256
  values, or a 17th open gesture of the client is `invalid_argument`, and nothing in the
  message is applied. The engine never refuses a `live.set` for arriving fast; a client
  keeps one per gesture in flight and sends its newest values when the answer comes.
- **Commit:** the client sends one ordinary batch with the final values and
  `ends_gesture`. It is one `rev`, one undo step whose inverse restores the values from
  before the gesture, and one audit entry. The gesture ends with it. A batch whose
  `ends_gesture` names no gesture the client has open is `invalid_argument` (field
  `ends_gesture`), and a batch that fails leaves the gesture open.
- **Cancel:** `live.cancel {gesture_id}` ends the gesture without a commit; it succeeds
  when there is nothing to cancel, so a retry is safe. A gesture also ends,
  `expired`, when its client's session ends.
- **Two gestures, one parameter:** a parameter is held by one gesture at a time. A
  `live.set` naming a parameter another gesture holds takes it over (last writer
  wins); the other keeps the rest. A gesture left holding nothing stays open, and
  counts toward the client's 16, until the client commits it, cancels it or
  disconnects, so its `ends_gesture` still works. A batch that changes a held parameter changes the
  collection, but the live value still shows until the gesture ends. A batch that
  deletes an item drops its live values.
- `live.list` returns every open gesture, ordered by client and gesture ID, with the
  values each holds now, for a client that connects during one. It carries no `rev`.

`live.set` and `live.cancel` are actions: never undoable, never batchable, answering
`{"kind":"action"}`.

### Events

After `welcome`, the server pushes every event to every client.

- `state.changed {rev, client_id, request_id?, cause?}` follows every applied batch,
  undo and redo; `cause` is `undo` or `redo` for a history call (0.10). A client
  matches it to its own response by `request_id`; there is no ordering promise between
  the two. Every other client re-reads what it shows. Since each batch adds exactly one
  to `rev`, a gap shows that an event was missed.
- `output.state_changed {output_id, state}` reports runtime state, which is not part of
  the collection, so it carries no `rev`.
- `studio.changed {program_scene?, preview_scene?, transition?, client_id, request_id?}`
  reports a new scene on program or preview, with the request that caused it: a
  `scene.switch`, a `studio.set_preview`, or a batch, undo or redo that created the
  collection's first scene (it goes on program and preview) or deleted the program or
  preview scene (it moves to the first scene left, or to none). For edits it follows
  their `state.changed`. A switch, or setting preview, to the scene already there emits
  nothing. After a switch, `transition` names the preset it ran and how long the
  transition lasts in milliseconds (`duration_ms`, 0 for a cut), so a client can show
  progress; no event marks the end. Program and preview are runtime state, so it
  carries no `rev` ([ADR-0030](../../docs/adr/0030-studio-state-and-scene-switching.md)).
  Added in 0.16; `preview_scene` and `transition` in 0.27.
- `live.changed {gesture_id, client_id, values}` carries the newest value of each
  parameter a gesture changed since its last one. The engine coalesces it per gesture
  to at most 30 a second, so a slow client gets fewer, newer values; the values a
  gesture had not sent yet always go out before it ends. Until
  [open question 7](#open-questions) every client receives it.
- `live.ended {gesture_id, client_id, outcome, rev?}` says a gesture ended: `committed`
  with the committing batch's `rev`, whose `state.changed` comes first, `cancelled` or
  `expired`. A client that doesn't know these events ignores them and shows committed
  state only.
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
  ([ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md)).
- `meters.tick {sources, buses}` is one reading of the audio levels (0.19), about ten
  a second while the mixer runs; an idle engine emits none. Each routed source and
  each bus has `peak` (the largest absolute sample since the previous tick) and `rms`
  (over the latest mixer block), linear full scale per channel, left then right. The
  master bus also has `true_peak` (BS.1770, since the previous tick) and `loudness`:
  `momentary`, `short_term` and `integrated` LUFS, each absent until there is enough
  audio above the absolute gate. Each tick is a full reading, so a client that misses
  one loses nothing; nothing numbers them.
- `youtube.status_changed {auth}` and `youtube.health_changed {output_id, broadcast_id,
  stream_id, status, health, issues}` (0.23) are described under [YouTube](#design).

### YouTube

JStream signs in to one Google account, once, with the full `youtube` scope
([ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md), keeping ADR-0041 §4), and every
YouTube command is an action or a query (0.23). They are for the UI: the MCP server
offers none of them, refuses a `youtube` output target and drops YouTube events, since
YouTube is for people, not agents (ADR-0058 §3):

- `youtube.sign_in` returns `{"type":"youtube.sign_in","url":…}`, Google's consent
  page. Once the user agrees, Google redirects the browser to a listener the engine
  opened on `127.0.0.1`, and the engine finishes the sign-in itself. No token ever
  travels in the protocol ([R-SEC-001](../../docs/RULES.md#R-SEC-001)): the refresh
  token goes to the OS credential store. Sent again while a sign-in waits, it returns
  the same URL.
- `youtube.sign_out` revokes the token and forgets it.
- `youtube.status` reports `auth`: `unavailable` (the engine has no Google OAuth client, as
  in a build whose built-in client is still empty, so YouTube's API features are off
  and clients hide them),
  `signed_out`, `signing_in` or `signed_in`. `youtube.status_changed {auth}` reports
  each change, including Google refusing the account's token.
- `youtube.list_broadcasts {status?, broadcast_type?, page_token?}` and
  `youtube.list_streams {page_token?}` read pages of up to 50. Both default to
  everything, the persistent "Stream now" broadcast included. A stream's key is never
  reported.
- `youtube.create_broadcast`, `youtube.bind_broadcast` and
  `youtube.transition_broadcast` return the broadcast as YouTube left it.
  `bind_broadcast` without a `stream_id` creates a reusable stream first.
- An output with the `youtube {broadcast_id}` target publishes to the `rtmps://`
  ingest of its broadcast's bound stream. While it runs, the engine polls the stream's
  health and sends `youtube.health_changed` when it changes.
- A Live API failure is `action_failed`, queries included, and a used-up quota says so
  in its reason (ADR-0041 §5, kept by ADR-0058).

Values mirrored from YouTube (`life_cycle`, `privacy`, `status`, `health`, `severity`)
have an `other` value, so a value YouTube adds later still parses. Text from YouTube is
untrusted and shown as plain text. Chat, the featured message and audience stats come
later.

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
| `invalid_argument` | A field's value was rejected | `field`, `reason`, and `blockers` for a refused delete |
| `undo_conflict` | An undo or redo would overwrite another client's later work; the step is dropped | `step_rev`, `client_id`, `rev` |
| `history_empty` | Nothing to undo or redo | `direction` |
| `action_failed` | An accepted action could not be carried out | `reason` |
| `unsupported` | A valid call this implementation doesn't serve (the pairing calls without a server in front; 0.21) | `call` |
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
  counts as a minor bump is part of open question 8. 0.13 adds the `EntityKind` value
  `encoder`, which only an error answering an encoder edit names, so a client older
  than 0.13, which sends none, never reads it. 0.19 adds `bus` the same way: only an error answering a bus or send edit
  (`bus.create`, `bus.rename`, `bus.set_fader`, `bus.delete`, `send.set`, `send.remove`)
  names it. An older client's undo of its own `source.delete` restores the source's
  sends, but if one's bus has since been deleted by another client, the undo fails with
  `undo_conflict`, which names no entity. 0.21 adds `device` the same way: only an error
  answering `device.revoke` names it, and 0.22 adds `filter` on the same terms for the
  filter edits. 0.27 adds the `EntityKind` value `transition` in the same way, but also
  the `KindCategory` value `transition`, which `kind.list` reports for every transition
  kind, so a client older than 0.27 can't parse `kind.list` from a 0.27 server. From
  0.27 `KindCategory` parses an unknown value as `Unknown`, so a later category won't
  break a 0.27 client. The encoder enums (`VideoCodec`,
  `RateControl`, `HardwarePreference`, `AudioCodec`) parse an unknown value as
  `Unknown` from the start.
- **Major (breaking):** removing or renaming anything, changing a field's type or
  meaning, or making an optional field required. It needs an ADR and one minor release
  that deprecates the old form first.
- Every public enum is `#[non_exhaustive]`, so Rust clients must handle new variants
  too, except `FilterChain`: an owner has a video and an audio chain and no other
  ([ADR-0026](../../docs/adr/0026-source-filter-chains.md)), so code that picks a
  chain matches it exhaustively.

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

**On the wire (0.11).** ADR-0022 names these additions as 0.4, which had already
shipped with `events.dropped`, so they ship as 0.11, an additive minor under
[R-API-001](../../docs/RULES.md#R-API-001). The accepted ADR is not edited
([R-DOC-009](../../docs/RULES.md#R-DOC-009)).

- **Canvas.** `state.get` reports `canvas { width, height }` in whole pixels, default
  1920×1080, and `canvas.set_size` changes it. Zero, or a side over the engine's
  maximum (`jstream-graph` accepts up to 8192 each way, so 7680×4320 and its portrait
  form fit), is `invalid_argument` on `edits.N.width` or `edits.N.height`. Items keep
  their canvas-pixel placements. Inverse: `canvas.set_size` with the previous size.
- **Two placement forms.** An item stores either a geometry or a legacy 0.3 transform,
  and only an edit changes which. `item.add` takes an optional `geometry` beside the
  deprecated `transform`; sending both is `invalid_argument` on `edits.N.geometry`, and
  sending neither stores the identity transform, its 0.3 meaning, until 1.0.
  `item.set_geometry` stores a geometry and the deprecated `item.set_transform` a
  transform. Each one's inverse restores the previous placement in its own form.
- **Validation.** A stored geometry has a size greater than zero
  (`edits.N.geometry.size`), a `perspective_px` greater than zero if present
  (`edits.N.geometry.perspective_px`), and finite corners (`edits.N.geometry`). Until
  the compositor draws 3D, a non-neutral `tilt_deg`, `turn_deg`, `depth_px` or any
  `perspective_px` is `invalid_argument` naming that field; `is_neutral_3d` is that
  predicate. Accepting them later is not a breaking change.
- **Reads.** `ItemInfo.placement` is `legacy` or `geometry` (missing from a pre-0.11
  server, it parses as `legacy`). A legacy item reports its transform exactly as
  written and no `geometry`, because its box depends on the source's runtime size. A
  geometry item reports its `geometry`, and a deprecated `transform` computed from it
  alone (`transform_view`): the box's top-left corner after rotation about the centre,
  its rotation, and scale (1, 1). Every read is a function of stored state, so it
  changes only with `rev`.
- **Deprecated** since 0.11, removed in 1.0: `ItemTransform`, `item.set_transform`,
  `item.add`'s `transform` and `ItemInfo.transform`. Their doc comments, and so the
  schema descriptions in `protocol.snap` and the MCP `tools.snap`, say so
  ([R-TEST-002](../../docs/RULES.md#R-TEST-002)).
- **Not in 0.11 yet:** `source.sizes` and `source.size_changed` (decision 4), which need
  the engine's runtime source sizes, and live geometry values, which arrived in 0.26
  with `live.set` ([Live values](#design)). Both are later minors.

### Binary streams

Preview video travels as WebSocket binary messages, one `BinaryFrame` each, alongside
the JSON text messages of the same connection (0.20,
[ARCH › Control protocol](../../docs/ARCHITECTURE.md#protocol),
[ADR-0045](../../docs/adr/0045-leptos-web-ui.md)). Audio meters travel in the JSON
`meters.tick` event; a binary meter stream would be another kind of the same frame.

- **Opening and closing.** `preview.subscribe {channel, max_width?, max_height?, fps?,
  codecs?}` is an action: it opens a stream of the program mix for the calling
  session only, tagged with the `channel` number the client chose (a `u32`, not an ID:
  it only tells the session's streams apart in the frame header). The picture is the
  canvas scaled down to fit `max_width` × `max_height` (default 640 × 360, at most
  1920 × 1080) with even sides, so it follows `canvas.set_size`. A picture is sent when
  what the program mix draws changes, at most `fps` times a second (default 15, at
  most 30), and at least every 2 s as a keyframe. `codecs` lists what the client can
  decode, most preferred first (default `["h264", "jpeg"]`); the engine uses the first
  it can encode, and may fall back to a later one if an encoder fails to open, so each
  frame names its codec. A session has at most 4 streams. `preview.unsubscribe
  {channel}` closes one (and succeeds for a channel that isn't open); so does the
  session's end. The stream shows the mix before any master stage, which is the
  default ARCH asks for; a post-master option comes with that stage. Over MCP, which
  has no binary channel, `preview.subscribe` fails with `action_failed`.
- **The frame.** A fixed 32-byte little-endian header, then the payload:

  | Offset | Size | Field |
  |---|---|---|
  | 0 | 1 | version, 1 |
  | 1 | 1 | kind: 1 = preview |
  | 2 | 1 | codec: 0 = none (an end frame), 1 = H.264, 2 = JPEG |
  | 3 | 1 | flags: bit 0 keyframe, bit 1 end of stream |
  | 4 | 4 | channel |
  | 8 | 2 | width |
  | 10 | 2 | height |
  | 12 | 4 | reserved, 0 |
  | 16 | 8 | presentation time in µs since the stream opened (signed) |
  | 24 | 8 | the `rev` the picture shows |

  An H.264 payload is one access unit in Annex B form, with the parameter sets in band
  at keyframes, which WebCodecs' `VideoDecoder` takes with no `description`. A JPEG
  payload is a whole baseline JFIF picture, always a keyframe. An end frame (flag bit
  1, codec 0) says the engine ended the stream; its payload is the reason as UTF-8, and
  the channel is free again. A client ignores a frame whose version, kind or codec it
  doesn't know, and the reserved bytes and spare flag bits.
- **Overload.** Frames are never buffered without bound. Each hop that queues them (the
  engine, the server, the WebSocket client) drops a frame that finds its queue full,
  and every later frame of that channel until a keyframe finds room (`KeyframeGate`),
  so what a client receives always decodes. The 2 s keyframe bounds the gap.
- **Compatibility.** The header's version byte changes only with a new protocol major.
  A new kind, codec or flag is a minor change, and a frame a client can't read is
  skipped.

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
   [ADR-0032](../../docs/adr/0032-live-edits-and-undo.md) and built in 0.10:
   `history.undo` and `history.redo` are a fourth kind of call, `history`, which
   carries no command (it applies a batch of edits the engine already holds), so
   R-ARCH-006 needs no change. The same ADR adds live values for continuous gestures
   (`live.set`, `live.cancel`, `live.changed`), committed by one batch; those are built
   for item placements and crops ([Live values](#design)), not yet for audio faders.
5. **Actions that change collection state.** Settled by
   [ADR-0030](../../docs/adr/0030-studio-state-and-scene-switching.md): the program
   scene is runtime state, not collection state, so `scene.switch` is an action that
   changes no `rev`, and no action changes collection state.
6. **Idempotency.** How long the engine remembers keys (count or time), and whether a
   reused key with a different batch is an error (`idempotency_key_reused`).
7. **Event subscriptions.** Every client gets every event in this cut, including
   `stats.tick` (0.6) and `live.changed`. High-rate events (`stats.tick`,
   `live.changed`, meters) will need opt-in subscriptions. Until then every client
   receives a tick about once a second while the pipeline runs, and up to 30
   `live.changed` a second per gesture while someone drags.
8. **Unknown values in string enums.** `OutputState` and `EntityKind` have no `Unknown`
   fallback, so adding a value breaks older clients that parse it. Add fallbacks, or treat
   a new value as a major change? `OutputKind`, `RtmpTarget` and `RecordContainer` have
   fallbacks from 0.9 and `KindCategory` from 0.27, but 0.9's `record` kind already breaks 0.7 and 0.8 clients reading
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
    built yet: the `kind.changed` event, `asset.list`, storing an
    absolute path under a known root in relative form, and kind versions on stored
    settings, which come with persistence.
12. **Deleting referenced objects.** Settled by
    [ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md) and built:
    rejection stays the only delete, with no cascading edit. The error lists every
    blocker in `blockers`, and the `delete.plan` query returns the batch that removes
    the target and its blockers ([Edits, actions and queries](#design)).
