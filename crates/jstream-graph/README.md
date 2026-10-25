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
-->

The scene graph of a collection, and the only code that changes it. Layer 2: plain data,
no GPU, no I/O, no `unsafe`. It depends on
[`jstream-protocol`](../jstream-protocol/README.md): its edits are the protocol's `Edit`s,
its IDs are the protocol's ID types, and its errors are the protocol's `ProtocolError`
codes, so the engine passes them to clients unchanged. A keyframe scene's overrides are
[`jstream-anim`](../jstream-anim/README.md)'s `Keyframe`, instantiated with the
protocol's types (`Overrides`).

**Model** (`model/`, [ARCH › Data model](../../docs/ARCHITECTURE.md#data-model)):
- `Graph`: the `State` plus its `rev`, and the kind descriptors it validates `builtin`
  sources against. The engine supplies them (`Graph::with_kinds`); they are not
  collection state, and `Graph::new()` describes only the built-in transition kinds
  (`cut`, `fade`, `move`), which every graph has.
- `State`: the `canvas` (default 1920×1080), the `sources`, keyed by ID, the `scenes`
  in display order, the `outputs`, keyed by ID, the `encoders` (encoder profiles),
  keyed by ID, the `video_mixes`, keyed by ID, the `buses`, keyed by ID and always
  holding the master, the `sends`, keyed by bus and source, the master stage's filter
  `Chains` (`State::master`,
  [ADR-0027](../../docs/adr/0027-program-master-and-output-chains.md)), the `folders`
  and `labels`, each in its own order, the
  `hotkeys`: each `Chord` bound to the protocol `Action` it fires
  ([ADR-0040](../../docs/adr/0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md)
  §3), and the `transitions`: the presets, keyed by ID, and the default. A binding
  always names objects that exist. A new state holds one preset, `cut`, as its default.
  Each `Output` has its own `Chains` too; only their audio chains hold filters so far.
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
  ([ADR-0028](../../docs/adr/0028-audio-model-v2.md)). Every source carries a set of
  labels and a favourite flag (ADR-0024 §4).
- `Scene`: a name, a `role` (`scene` or `group`, ADR-0024 §1; it changes no behaviour
  here), its `transition_in` and `transition_out` overrides, the folder it is filed in,
  if any, its `SceneMeta` and its items in stacking order, bottom first. A keyframe
  scene has `variant_of`, naming its parent, its `Overrides` and no items;
  `State::shown_items` gives the items any scene draws, a keyframe scene's being its
  parent's at their effective values.
- `Folder` and `Label`: a folder has an ID, a name and an optional parent folder, and a
  label an ID, a name, a colour and an optional icon (ADR-0024 §3 and §4). They organise
  only.
- `Item`: the source it places, a `Placement`, a `crop`, `visible`, an `opacity` from
  0 to 1 and a `BlendMode`, its `ItemTransition`
  ([ADR-0029](../../docs/adr/0029-transitions.md)), and the reserved per-placement
  `effects`, an uninhabited `Effect` list that is always empty. `Placement` is
  one of [ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md)'s two forms: a
  `Geometry` (a box on the canvas) or a `Legacy` 0.3 `ItemTransform`, kept exactly as
  written. Only an edit changes the form.
  `item.set_source` changes the source and keeps the rest, the item's ID and its
  stacking position, so it converts the item between a reference and a clone
  ([ADR-0023](../../docs/adr/0023-placements-references-and-clones.md)).
- `Preset`: a transition preset ([ADR-0029](../../docs/adr/0029-transitions.md),
  [ADR-0037](../../docs/adr/0037-kind-descriptors-for-transitions-and-plugins.md)): an
  ID, a name, a transition kind, `duration_ms`, `easing` and its complete settings.
- `Bus`: an ID, a name, a fader (`gain_db`, `muted`) and an optional monitor
  (`monitor()`, the protocol's `BusMonitor`: an output device and a tap point, 0.42). The master bus (`master`,
  `MASTER_NAME`) is in every state; at most `MAX_BUSES` (16, the protocol's
  `MAX_BUSES`) buses in all. A send is a
  gain in dB from a source into a bus.
- `Encoder`: an encoder profile, an ID, a name and the protocol's `EncoderProfile`.
  `State::encoding_of` gives the encoding an output starts with.
- `VideoMix`: a video mix, an ID, a name and the protocol's `VideoMix` (an input, a
  resolution and a divisor of the base frame rate).
- `Output`: an ID, an `OutputKind` (so far RTMP to a generic URL or to a YouTube
  broadcast by its ID, a recording to an MP4 file, or the virtual camera, which has
  nothing to check), the encoder profile it uses, if not the default (a virtual
  camera encodes nothing, so it ignores its profile), and the video mix it shows, if
  not the program. Whether it
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
  `item.set_crop`, `item.set_visible` and `item.set_transition` only where the item
  differed from what `item.add` produces, and deleting a scene by `scene.create` (carrying its `role` for a
  group), `scene.reorder`, `scene.set_transitions` when it had an override, and each
  item's restore. The inverse carries no `expected_rev`: the engine's per-client undo
  history (in `jstream-core`) chooses it.
- **Touched sets** (`touched.rs`, [ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)):
  `Touched::of(edits, inverse)` names, from a batch's edits and its inverse's (so a
  delete also names what it removed), the objects created or deleted, the objects used
  (an item's scene and source, both sources of an `item.set_source`, a `scene`
  source's scene, a duplicate's original, the scene an `item.move` names, the profile
  an `output.set_encoder` names, the mix an `output.set_video_mix` names, a `scene`
  mix's scene, a send's source and bus, the preset a scene override or
  `transition.set_default` names), the properties set (names, a scene's role, an
  item's placement, crop, visibility, opacity, blend, index and source, each settings
  parameter, a filter's switch, settings parameters and index, an output's target,
  encoder and mix choices, a profile's encoding, a mix's spec, each audio field a
  `source.set_audio` or `bus.set_fader` sets, a bus's monitor, a send's gain, a preset's fields, a
  scene's transition overrides, an item's transition settings), and whether the
  scenes' order, the canvas size or the default preset was set. A send is an object of its own, so two sources'
  sends into one bus don't conflict, and deleting a bus conflicts with every change to
  its sends. An edit it doesn't know marks the set as conflicting with
  everything. `conflicts_with` is the undo rule the engine's history applies to other
  clients' later batches: one set creates or deletes an object the other touches, both
  set the same property, both set the scenes' order, both set the canvas size, or both
  set the default preset.
- **Settings** ([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)):
  `source.create` of a `builtin` source needs a source kind the graph describes (a
  filter or transition kind, such as a scanned CLAP plugin's, is `invalid_argument` on
  `kind.kind`, naming its category), checks each
  given value with the protocol's `ParamDescriptor::check`, and stores the complete map
  with defaults filled in. `source.set_settings` checks and applies a patch (`null`
  resets a parameter to its default) and its inverse is the patch of the old values.
  `source.rename` renames. `source.create` rejects a `duplicated_from` naming the new
  source itself, and accepts one naming a missing source, as `source.delete`'s inverse
  may need after the original went first. A reference to a missing scene, source or
  bus is `not_found`, and
  a `source` reference restricted to `media` needs a source whose kind produces it.
- **References are never left dangling.** `source.delete` is rejected while an item
  places the source or a setting refers to it, `scene.delete` while a `scene`
  source or a video mix shows the scene or a setting refers to it, and `bus.delete`
  while a setting refers to the bus (a `waveform` or `spectrum` drawing it, #362). The
  `invalid_argument` lists every blocker in `blockers` (`deps::blockers`, the reverse
  of the cycle check's edges, plus the mixes): the item (`source_id`), the `scene`
  source (`kind.scene_id`), the mix (`mix.input.scene_id`), the setting
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
  perspective and finite corners, each failure naming its field
  (`edits.N.geometry.size`). Any tilt, turn, depth and perspective the compositor can
  draw is accepted from protocol 0.34 (ADR-0050; `ItemGeometry::check_drawable`, else
  `edits.N.geometry`); before, a non-neutral one was `invalid_argument`.
  A geometry's `flip_h` and `flip_v` are part of it, so they are stored, undone and
  persisted with it (the collection file writes them only when set).
  `item.set_opacity` (optionally addressed by `scene_id`, like the placement edits) and
  `item.add`'s `opacity` accept 0 to 1; `item.set_blend` and `item.add`'s `blend`
  accept only `normal` until the compositor draws the other modes. Each is undone by
  the previous value, and removing an item restores both.
- **Filters** (`filter.rs`, [ADR-0026](../../docs/adr/0026-source-filter-chains.md)):
  `filter.add` checks, in order, the owner (a source, an output or the master stage),
  the filter ID (non-empty, unused on the owner), that the owner has the chain (a
  source: video if its kind produces video, audio if it produces audio and it is not a
  `scene` source; the master and an output that carries audio: audio only, ADR-0027),
  that the chain has room, that the kind is a filter kind of that chain (`kind`), that
  a master or output filter's settings hold no reference (a scene, source, bus or asset),
  the settings
  exactly as a source's, and the index; it is undone by `filter.remove`. `filter.remove` is undone by the
  `filter.add` that puts the filter back where it was, with its switch and settings;
  `filter.set_enabled`, `filter.set_settings` (a patch, `null` resets) and
  `filter.reorder` by the previous values. `source.duplicate` copies both chains with
  their IDs, `source.delete`'s inverse re-adds every filter after `source.create`,
  `output.delete`'s re-adds the output's after `output.create`, and `source.create`
  refuses a filter kind.
- **Outputs** (`target.rs` checks the URL): `output.create` and `output.set_target`
  reject, with `invalid_argument` and a fixed reason that never repeats the URL, one
  that isn't an `rtmp://` or `rtmps://` server URL with a host and an application
  name, and one that could carry a credential (`user:password@`, a query or a
  fragment). A `youtube` target's `broadcast_id` is rejected (field
  `kind.target.broadcast_id`, or `target.broadcast_id` in `output.set_target`) when it is empty, over 128 bytes, or has anything but ASCII
  letters, digits, `-` and `_`. A recording's `path` is rejected (field `kind.path`) when it is empty,
  over 4096 bytes, has control characters or a `..` segment, is not absolute (`/…`,
  `\…`, `C:\…` or `C:/…`, the same on every platform), starts with two separators (a
  Windows UNC or device path), has `:` other than a drive letter's, `?` or `*` (`\??\…`
  is an NT object path), or doesn't end in a file name, or ends in a DOS device name
  (`NUL`, `CON`, `PRN`, `AUX`, `COM0`–`COM9`, `COM¹`–`COM³`, `LPT0`–`LPT9`,
  `LPT¹`–`LPT³`, `CONIN$` or `CONOUT$`, in any case and with or without an extension,
  so `nul.mp4` too, by the rule of an asset path's last component), which Windows opens
  in place of a file; whether it is absolute on the engine's own machine, and
  whether the file can be written, is the engine's check at `output.start`. `output.set_target` on a recording or a virtual camera fails with
  `invalid_argument` (neither has an RTMP target). `output.delete` is undone by `output.create` with its kind, and
  `output.set_target` by the previous target. `Graph::apply_with_running` is `apply`
  for an engine whose outputs in the given set are running: deleting one of them fails
  with `invalid_argument`, in edit order. The `OutputDelete` inverse also restores the
  output's encoder profile and video mix, and then its filters.
- **Encoder profiles** (`encoder.rs`): `encoder.create` and `encoder.update` check the
  profile with the protocol's `EncoderProfile::problem` (`invalid_argument`, field
  `profile.<field>`) and the name; `encoder.delete` is refused with `invalid_argument`
  while an output uses the profile, naming it. `output.set_encoder` checks the output,
  then the profile (`not_found`), and needs no running check: it applies from the
  output's next start. Each is undone by its opposite (`encoder.delete` by
  `encoder.create`, `encoder.update` and `output.set_encoder` by the previous value).
- **Transitions** (`transition.rs`): `transition.create` needs a kind of category
  `transition` (else `invalid_argument` on `kind`) and checks its settings as
  `source.create` does; `transition.update` patches the fields it names, and a new
  kind resets the settings to that kind's defaults before the patch, its inverse
  carrying the old kind and the whole old map. `transition.delete` refuses the default
  (`invalid_argument` on `preset_id`) and clears every scene override naming the
  preset; its inverse re-creates the preset, then restores those overrides.
  `transition.set_default`, `scene.set_transitions` (a missing preset is `not_found`)
  and `item.set_transition` (its delay checked by `ItemTransition::problem`) are undone
  by the previous value. `scene.delete`'s and `item.remove`'s inverses restore the
  overrides and settings they dropped.
- **Video mixes** (`mix.rs`): `video_mix.create` and `video_mix.update` check the mix
  with the protocol's `VideoMix::problem` (`invalid_argument`, field `mix.<field>`),
  then that a `scene` input's scene exists (`not_found`), and the name;
  `video_mix.delete` is refused with `invalid_argument` while an output shows the mix,
  naming it. `output.set_video_mix` checks the output, then the mix (`not_found`), and
  needs no running check: the engine reads a mix's size and rate at an output's start.
  Each is undone by its opposite, as for encoder profiles. A mix is not a dependency
  edge: nothing can depend on a mix, so it can't close a cycle.
- **Audio** (`audio.rs`, ADR-0028): a new source takes its kind's audio defaults
  (`follow` `always` for a kind with audio and no video, `when_shown` for every other
  audio source, `scene` sources included), and a new source with a channel of its own
  (an audio source that is not a `scene` source) sends to the master at 0 dB.
  `source.duplicate` copies the audio and the sends. `source.delete` removes the
  source's sends, and its inverse adds `source.set_audio`, `send.set` and a
  `send.remove` of the default master send for whatever differs from the defaults.
  `source.set_audio` needs an audio source and at least one field, and refuses a
  `scene` source's `gain_db` or `muted`. `bus.create` checks the ID, the name and the
  bus limit; `bus.delete` refuses the master and a bus a setting refers to (above),
  and removes the bus's sends, and its inverse is `bus.create`, `bus.set_fader` if the fader differed,
  `bus.set_monitor` if it had a monitor, and the sends. `bus.set_monitor` needs the bus
  (`not_found`), then checks the monitor with the protocol's `BusMonitor::problem`
  (`invalid_argument` on `monitor.<field>`); a device is never looked up, and its
  inverse is the previous monitor, or none.
  `send.set` and `send.remove` check the source, then the bus (`not_found`), then that
  the source has a channel (a `scene` source sends nothing of its own). Every gain is
  checked with the protocol's `gain_db_problem` (−100 to +24 dB). A send always runs
  from a source to a bus, so buses can't form a cycle (buses never feed buses); a
  routing check comes with keyed (sidechain) filters.
- **Groups and moves** ([ADR-0024](../../docs/adr/0024-scene-structure.md) §1):
  `scene.set_role` changes a scene's role and is undone by the previous role.
  `scene.create` sets the `role` it names, or `scene` when it names none.
  `item.move` moves an item to an index in another scene (up to one past its top) or
  within its own (a reorder), keeping the whole item, and is undone by `item.move`
  back. The placement edits (`item.set_geometry`, `item.set_transform`, `item.set_crop`,
  `item.set_visible`, `item.set_opacity`)
  take an optional `scene_id`: absent or the item's own scene changes the item, a
  keyframe scene of the item's scene writes its override, any other scene is
  `invalid_argument`, and the inverse keeps the addressing.
- **Keyframe scenes** (`keyframe.rs`, [ADR-0024](../../docs/adr/0024-scene-structure.md)
  §2, [ADR-0049](../../docs/adr/0049-keyframe-scene-model-and-evaluation.md)):
  `scene.create_variant` appends a variant of a scene that isn't one, undone by
  `scene.delete`. A placement edit addressed to it writes one field of its override
  through `Keyframe::write`, after the same checks as the item's own edit, and is undone
  by `item.clear_override` of the field or by the old value. `item.clear_override` of an
  absent field changes nothing and has no inverse. An override names one of the
  parent's own items. A group's child is refused, naming #702. `item.add`, `item.move`
  into a variant and `scene.set_folder` of one are refused. `item.remove` of a parent's
  item and `item.move` of it out drop its overrides in every variant (`drop_unless`),
  report them in `Applied::dropped_overrides`, and restore them in the inverse after the
  item is back
  ([ADR-0040](../../docs/adr/0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md)
  §1). A role change orphans no override, since overrides name only a parent's own
  items, so `scene.set_role` drops none. A variant's `scene.delete` is undone by
  `scene.create_variant`, its role, position, transitions and metadata, then the writes
  of its overrides. A parent's delete is blocked by its variants, and its
  `delete.plan` deletes them (and the `scene` sources that show them) first.
- **Folders, labels and metadata** (`organise.rs`,
  [ADR-0024](../../docs/adr/0024-scene-structure.md) §3 and §4): folders and labels are
  ordered lists with unique, non-empty IDs and non-empty names. `folder.create` takes an
  optional `parent` that must exist, and `folder.set_parent` refuses a parent that would
  put the folder inside itself (`invalid_argument`, field `parent`, naming the chain).
  `scene.set_folder` names an existing folder or none. `label.create` and
  `label.update` check the colour (`#rrggbb`) and icon (1 to 64 bytes; ADR-0024 §4)
  with the protocol's `check_color` and `check_icon`, and `scene.set_meta` checks the
  same through `SceneMeta::problem`. `source.set_meta` replaces a source's labels (each
  must exist and appear once; stored ordered by ID) and favourite flag, and
  `source.duplicate` copies both to the copy. `folder.delete` is refused while a scene
  (`folder`) or folder (`parent`) sits in the folder, and `label.delete` while a source
  (`labels`) carries the label, listing them as `blockers`; their `delete.plan` moves
  what the folder holds up to its parent, or takes the label off those sources. Every
  edit is undone by the previous value; a delete by the create, then a reorder back
  unless it was last. `scene.delete` and `source.delete` undo the folder and metadata
  too. None of it is a dependency edge.
- **Hotkeys** (`hotkey.rs`): `hotkey.set` binds a chord to an action and
  `hotkey.clear` unbinds it, checked in this order: the chord's spelling
  (`invalid_argument` on `chord`), whether it is already bound (`already_exists`) or
  bound at all (`not_found`, entity `hotkey`), an action that can't be bound
  (`Action::bind_problem`: `output.set_key`, whose key is never stored, and the
  session-only (preview and live), server-answered and YouTube actions), and the objects the action names
  (`not_found`). Each is undone by the other. A rebind is `hotkey.clear` then
  `hotkey.set` in one batch. Deleting a scene, source, output, encoder profile or
  transition preset (a `scene.switch` may name one) removes the bindings that name it, so none dangles: the result's `removed_hotkeys` lists them,
  and the delete's inverse ends with a `hotkey.set` for each, after the object is back.
  The touched set names the hotkey, and the objects a `hotkey.set` action names as used.
- **No cycles** (`deps.rs`). One function lists every dependency edge: a scene depends
  on the sources its items place, a keyframe scene on its parent, a `scene` source on its
  scene, and a source on the
  scenes, sources and buses its settings, or its filters' settings, refer to. A bus
  depends on nothing, so an edge into one never closes a cycle; it only holds the bus
  against `bus.delete`. Asset references are not edges. Every edit that adds an edge
  runs the cycle check: `item.add`, `item.move`, `item.set_source` (from the item's
  scene to its new source), `source.create`, `source.set_settings`, `filter.add` and
  `filter.set_settings`; `source.duplicate` copies outgoing edges onto a source nothing
  depends on, so it can't close one. A cycle is `invalid_argument` naming it
  (for example scene `a` -> source `sa` -> scene `a`).

**Reading** (`read.rs`): `Graph::query` answers the protocol queries that read the
collection (`state.get`, with the folders and labels in their orders, `scene.list`,
`source.list`, `item.list`, `output.list`, `encoder.list`, `video_mix.list` and
`bus.list`)
with the protocol's result shapes, at the current `rev`, and the kind queries
(`kind.list`, `kind.describe`, which fails with `invalid_argument` on `kind_id` for an
unknown kind), and `delete.plan` (`plan.rs`,
[ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)). It returns `None` for a query the graph doesn't hold the answer to (`output.status` and `video.snapshot` read runtime state), and fails only with
`not_found` for an `item.list` of a missing scene or a `delete.plan` of a missing
target. `Graph::delete_plan` builds the plan from the same `deps::blockers` the delete
check uses: a scene's plan also deletes the `scene` sources that show it, the items
placing any deleted source are removed, settings references to anything deleted are set
to `null` (one `source.set_settings` per source, and one `filter.set_settings` per
filter), the video mixes that show a deleted scene are pointed at the program
(`video_mix.update`), and the target's own delete comes last. The plan also lists the
hotkey bindings those deletes remove. A bus's plan only clears references (its
`bus.delete` takes its sends); the master's is `invalid_argument`.
Sources come ordered by ID, scenes in
display order, items bottom first, outputs, encoder profiles, video mixes and buses
ordered by ID, and sends by bus and then source. `state.get` carries the canvas, the
buses, the sends, the hotkey bindings ordered by chord, and the transition presets
(ordered by ID) with the default, and an audio source's info its `audio`; `kind.list`
lists the transition kinds beside the source and filter kinds. A geometry item reads its `geometry`, and the deprecated `transform` view the
protocol computes from it (`ItemGeometry::transform_view`); a legacy item reads its
stored transform and no geometry. `Source::to_info` (given the `State`, to omit a
`duplicated_from` whose source is gone),
`Scene::to_info`, `Item::to_info`, `Filter::to_info`, `Output::to_info`,
`Folder::to_info` and `Label::to_info` convert single objects; a source and an output
report their chains in `filters` (`Chains::to_info`), omitted while both are empty,
`state.get` the master's in `master`, and an item its `opacity` and `blend`. Reads
change nothing.

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
`encoder_profile`, omitted for the default, its `filters` as a source's, and its
`video_mix`, omitted for the program), encoder profiles (`encoders`) ordered by ID,
omitted when there are none, video mixes (`video_mixes`) ordered by ID, omitted when
there are none, `buses` (omitted while the master is the only one and at its defaults;
each bus's `monitor` omitted for none)
and `sends`, written whenever there is a send or a source with a channel of its own (so
an empty list means every master send was removed) and omitted otherwise, the `master`
with its `filters`, omitted while it has none, the `hotkeys` ordered by chord, omitted
when there are none, and `transitions` (each preset with its `kind_version`, and the
default), omitted while it is the initial `cut` alone (so no `schema_version` bump: an
older file has none of these). A scene writes `transition_in` and `transition_out`
when set, and an item its `transition` when not the default. A source's `audio` is saved
only where it differs from its kind's defaults, except a `webcam`'s, which is always
saved, so a webcam without it can only come from before webcams had audio (#716). `folders` and `labels` are in their
orders, each omitted when empty. A scene's `folder`, `icon`, `color`, `favourite` and
`show_in_multiview`, and a source's `labels` and `favourite`, are each omitted while
they have their default, so a collection without them saves as it did before they
existed and needs no schema bump. A keyframe scene writes `variant_of` and its
`overrides` in item ID order, each omitted for any other scene, so it needs no schema
bump either. Loading creates the folders top level first, then
sets each nested one's parent, so a folder may come before its parent in the file;
labels come before the sources that carry them. Kinds, undo history and runtime state are not in it, and
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
  graph with the caller's kinds: the canvas size, folders and labels, scenes with their
  roles, folders and metadata (keyframe scenes after every other, then `scene.reorder`s
  to the file's order if that moved one, so a variant may come before its parent), then
  the buses (the master renamed or faded as saved) and their monitors,
  which a source's settings may refer to, then sources (each after the sources its
  settings refer to, then its labels and favourite), then every source's filters, then
  items in their placement form with their crop and visibility, opacity, blend and
  transition settings, then the keyframe scenes' overrides as placement edits addressed
  to them, then encoder profiles, then the presets and the default (the
  initial `cut` is updated in place when the file has it, and deleted when it doesn't),
  then each scene's overrides, then video mixes, then outputs with their profiles, mixes
  and filters, then the master's filters, then each source's audio and the sends (with a
  `send.remove` of a default master send the file doesn't list; a file without `sends`,
  such as one saved before sends existed, keeps every default master send), then
  hotkeys (`` hotkey `F1` ``). A
  dangling reference, a cycle, settings that don't fit the kind, an unknown kind or a
  URL with a credential fails as `LoadError::Invalid`, naming the object
  (`` item `i1` ``) and the protocol error (boxed, so `Result`s carrying a
  `LoadError` stay small); nothing is partly loaded. Then `rev` is set
  to the file's, so revisions keep increasing across restarts. A source, filter or preset whose
  `kind_version` differs from its kind's descriptor is refused
  (`LoadError::KindVersion`) until kinds ship settings migrations.
- **Errors** never quote a value beyond IDs, so they are safe to log. `serde_json`'s
  own messages quote the offending value, so a parse error keeps only a missing field's
  name, a fixed reason, and the line and column of a syntax error.

**Tests** (`tests.rs`; the reads' own in `read.rs`, the touched sets' conflict rule in
`touched.rs`, folders and labels in `organise.rs`): validation and error codes per
edit, shared sources, references and clones (`item.set_source` keeping the item, the
`duplicated_from` link through deletes and undo, and both conversions as one batch),
cycles (self, 2- and 3-cycles, legal diamonds, and through settings references, one
case per edge-adding edit), settings checks and defaults,
placement conversions and their inverses, a flip set and undone through
`item.set_geometry`, geometry and canvas validation,
delete rejection by references, roles, moves and the grouping and ungrouping batches,
`scene_id` addressing, keyframe scenes (`tests/keyframes.rs`: what a variant shows and
its live follow of the parent, each write and clear inverse and the no-op clear, the
edits it refuses, its parent's cycle, the overrides an item's removal or move drops and
their restore, a parent deleted only with its variants, a variant's delete undone, and
the collection file with a variant before its parent), output URL checks and the
refused delete of a running
output, encoder edits and their inverses (a profile freed and deleted in one batch),
video mix edits and their inverses, a mix blocking its scene's delete and the plan
pointing it at the program,
opacity and blend checks and inverses, the filter edits (`tests/filters.rs`: each
check's field, exact inverses, duplicate and delete, references in filter settings as
cycles, blockers and plans, the collection file, and the master's and outputs' audio
chains with their checks), audio defaults by kind, a
duplicate's audio and a delete's restore, a bus deleted with its sends and undone
exactly, bus references (#362: `a_bus_reference_must_name_a_bus`,
`a_drawn_bus_is_deleted_only_with_its_references_cleared`, and in `persist/tests.rs`
`a_source_drawing_a_bus_round_trips`), hotkey checks, rebinds and the bindings
deletes remove (and restore on undo), a `scene.switch` binding that names a preset
(`a_switch_naming_a_preset_follows_the_preset`), the transition edits (in
`transition.rs`: the initial `cut`, kind and settings checks, a source refusing a
transition kind, patches and kind changes, the default's delete refused and a delete
clearing overrides, item settings), exact inverses,
what each query reports, and parity with `jstream-control`'s mock: the reads and
rejected batches of `jstream_protocol::parity` (a dev-dependency with the `fixtures`
feature) must get exactly the fixture's answers, the same ones the mock is held to. And a
property test: a seeded generator applies 6000 random
batches of every edit, including moves, role changes, source switches, addressed
placement edits, filter edits on sources, outputs and the master, the audio edits, the
transition edits, and folders, labels and metadata. After each successful one, the
state must hold no cycle and its folders must form a tree, applying its
inverse must restore the state exactly, and applying the inverse's inverse (redo) must
restore the batch's result. After each failed one, the graph must be unchanged. And
after every batch, for each scene, source, folder and label: a plain delete is refused exactly when
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
1280×720 canvas, legacy items and a flipped geometry item, two encoder profiles that two outputs use,
two video mixes that two outputs show, scene `b` made a group, a clone, a
`duplicated_from` naming a missing source, opacities, a desktop audio source with two
gain filters, one disabled, a gain filter each on the master and the recording output,
and the fixture's buses, sends and source audio, less the virtual camera and the
hotkeys the parity collection gained later) plus a source whose
settings refer to a later one must produce exactly the checked-in
`persist/fixtures/v1.json`, and that fixture must load back to the same state and
`rev` ([R-API-003](../../docs/RULES.md#R-API-003): the fixture stays when the schema
moves on, and a migration has to load it); the fixture with its `webcam` sources'
settings taken out (as saved before the kind had a `device`) loading to the same state;
the fixture with its `display` source's `show_cursor` taken out (as saved before 0.36)
loading to the same state;
a file without `encoders` or any
`encoder_profile` (as saved before profiles existed) loading with none, and one without
`video_mixes` or any `video_mix` likewise, and one without bus `monitor`s (as saved
before 0.42) with none, while a monitor whose tap this version doesn't know is refused;
a file without `sends`, `buses` or source
`audio` (as saved before audio existed) keeping its audio sources' master sends at
0 dB, its webcams' with their audio off; a webcam with no audio settings (as saved
before webcams had audio, #716; a webcam's are written since, even at their defaults)
loading with its master send and its audio off, so an older collection never starts
recording a webcam's microphone, and saving that back; a file without `master` or any output's `filters` (as saved before their chains
existed) loading with those chains empty and the sources' filters intact; a removed
master send's round trip, a webcam's among them (its audio on, no send back); a profile's round trip; a geometry's flip loading back and a file without one loading unflipped; a virtual camera's round trip;
transitions' round trip (with the initial `cut` deleted, or kept but changed) and a
preset of another kind version refused; hotkeys' round trip, and a file whose binding
names a missing scene refused; a clone's
link surviving a round trip after its original is deleted; the migration runner over stand-in
migrations; and refused files (versions, malformed JSON, unknown fields, dangling
references (an output naming a missing profile or mix, and a mix naming a missing
scene, among them), cycles, unknown kinds, a credential in a URL, a kind version mismatch, an
item with both or neither placement form, an invalid geometry or canvas), and that
no error quotes a credential-like value placed where a number, key or tag belongs.

<a id="follow-ups"></a>
## Not yet
<!-- @section
-->

Planned, and kept possible by the private fields of the model types:

- **Effects and more chains:** `effects[]` on items (per placement), which a collection
  reserves and must leave empty for now, with their edits; filter chains on buses,
  scenes, the master and outputs
  ([ADR-0026](../../docs/adr/0026-source-filter-chains.md)).
- **Kind versions.** The engine's kinds come from `jstream-sources`. The collection file
  records each source's kind version but refuses a mismatch: settings migrations per
  kind, and keeping the data of a source whose kind is missing, come later.
- Blend modes other than `normal` on placements. Legacy placements go in 1.0, whose
  collection migration converts them
  ([ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md) decision 5).
- Overrides of a group's children in a keyframe scene (ADR-0024 §2), and the drops
  ADR-0040 §1 then needs: `scene.set_role` of the group, and removing or retargeting
  the item that places it (#702).
- Item lookup is a linear scan over scenes. An ID index can come once collections grow.
