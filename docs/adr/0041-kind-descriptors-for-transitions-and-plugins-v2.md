<!-- @doc
id = "ADR-0041"
title = "Kind descriptors for transitions and plugins, v2: kind origins and the blob type"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0041: Kind descriptors for transitions and plugins, v2: kind origins and the blob type

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

Supersedes [ADR-0037](0037-kind-descriptors-for-transitions-and-plugins.md). It restates
ADR-0037's decisions on transition kinds (N5), the ADR-0005 citation (N7) and the
cycle-checked edits (N8) unchanged. It changes the shape of plugin kinds and their opaque
state (N6) to the maintainer's choice: kinds gain an explicit `origin`, whose third value
is `plugin`, and the opaque parameter type is `blob`.

<a id="context"></a>
## Context
[ADR-0025](0025-typed-settings-kind-descriptors.md) describes the settings of sources and
filters with kind descriptors that the engine reports and validates against. Two ADRs
accepted after it leave gaps that issue #93 records, from the reviews of ADR-0025:

- **Transition presets (N5).** [ADR-0029](0029-transitions.md) fixes a preset as
  `{ id, name, kind, duration_ms, easing, unmatched, stagger_ms }` with
  `kind: Cut | Fade | Move`. A closed enum means every new kind (a stinger that plays a
  media file, a WGSL wipe from an effect pack) is a protocol change, and `unmatched` and
  `stagger_ms` mean nothing to a kind that doesn't work per item.
- **CLAP plugins (N6).** [ADR-0035](0035-clap-plugins-out-of-process.md) §6 makes a CLAP
  plugin's parameters the filter's settings and stores the plugin's saved state as an
  opaque blob in those settings. ADR-0025 › Kinds names only built-in and pack kinds, and
  its parameter types have no type for opaque bytes.
- **Citations (N7).** ADR-0025 cites [ADR-0005](0005-plugins-wgsl-and-wasm.md) for effect
  packs. ADR-0035 supersedes ADR-0005, and restates its decision on WGSL packs and WASM
  plugins unchanged.
- **Cycle-checked edits (N8).** ADR-0025 › Typed references › Cycles lists the edits that
  run the cycle check as `item.add`, `source.create`, `source.set_settings` and the filter
  edits. It reads as exhaustive, but `item.set_source`
  ([ADR-0023](0023-placements-references-and-clones.md)) and the variant → parent edge
  ([ADR-0024](0024-scene-structure.md)) also add edges.

ADR-0037 settled all four as an agent-tier ADR, on the maintainer's first answer to N5
("Yes, use descriptors") and with N6 as a technical shape: plugin kinds with IDs
`clap/<plugin id>` and an opaque parameter type named `state`. The maintainer then
decided N5 and N6 explicitly, relayed on #93 with each option as chosen:
- N5, **"Kind descriptors"**: transitions (Cut, Fade, Move and later ones) become kinds
  with typed settings, like sources and filters, so a new transition needs no protocol
  change. This is ADR-0037's decision 1.
- N6, **"Plugin kinds + opaque blob"**: kinds get a third origin, `plugin`, whose
  descriptor is generated from the plugin's parameter list at load. A new opaque `blob`
  parameter type (base64, size-capped, not editable in the UI) carries the plugin's saved
  state.

N6 differs from ADR-0037 in names and in one field: the type is `blob`, not `state`, and
a kind's origin is reported, not only implied by its ID. ADR-0037 is accepted and
immutable ([R-DOC-009](../RULES.md#R-DOC-009)), and changing what it decided needs a
superseding ADR ([R-ARCH-004](../RULES.md#R-ARCH-004)). Superseding an ADR is Tier 1
([ADR-0017](0017-adr-decision-tiers.md)), so this ADR is too. It restates the whole of
ADR-0037 so that one ADR holds the decision, as earlier v2 ADRs do.

Nothing it touches is built: the protocol has no transition presets, kind descriptors,
kind origins or CLAP filters, and ADR-0037's `state` type never shipped. Every change is
therefore additive under [R-API-001](../RULES.md#R-API-001), and the rename costs no
deprecation.

<a id="decision"></a>
## Decision

**1. Transition kinds are kinds (N5, as ADR-0037 decided).**
- ADR-0025's kind categories (source, video filter, audio filter) gain **transition**.
  `kind.list` and `kind.describe` report transition kinds, `kind.changed` covers them, and
  their settings use ADR-0025's descriptors, values, validation, stored complete map,
  versioning and unknown-data rules unchanged. A client that doesn't know a category
  ignores kinds of that category, so a later category is additive too.
- **The first three kinds are built-ins** with bare IDs: `cut`, `fade` and `move`. Their
  behaviour is ADR-0029's §3 to §5, unchanged. A new kind (a stinger, a WGSL wipe from an
  effect pack) is a new kind ID with its own descriptor, with no protocol change. Pack
  transition kinds are namespaced by pack like other pack kinds. What a pack's shader
  receives (both pictures and the progress) is designed with the first pack transition.
- **How this fits ADR-0029's `TransitionPreset`.** The fields every kind shares stay
  preset fields, and the kind-specific ones move into the settings:
  - **Stay:** `id`, `name`, `kind` (now a kind ID string), `duration_ms` and `easing`. A
    kind that ignores them (Cut ignores both, as ADR-0029 says) still stores them, so
    switching a preset's kind keeps its timing.
  - **Added:** `settings`, the kind's settings map.
  - **Move into settings:** `unmatched` and `stagger_ms`, which only kinds that work per
    item use. `fade` and `move` declare both as parameters, with ADR-0029's values
    (`unmatched`: an enum of `fade`, `scale` and `slide`; `stagger_ms`: an integer of at
    least 0, in milliseconds) and defaults. `cut` declares no parameters.

  The preset is therefore `{ id, name, kind, duration_ms, easing, settings }`.
- **Unchanged from ADR-0029:** the default preset and its protection, deleting a preset
  clearing the overrides that use it, the resolution order of a switch, retargeting, and
  `SceneItem.transition { delay_ms, easing, glide, appear, disappear }`. The per-item
  settings belong to the placement, not to a kind, so they stay fixed fields. A kind that
  doesn't work per item ignores them.
- **Edits.** The preset create edit takes `kind` and an optional `settings` map; missing
  parameters take their defaults. The preset update edit changes the common fields and
  takes a settings `patch` with ADR-0025's patch rules. Changing a preset's `kind` replaces
  its settings with the new kind's defaults, overlaid by any settings the edit carries;
  the inverse restores the old kind and settings. Validation is ADR-0025's, with fields
  named `edits.N.settings.<id>` or `edits.N.patch.<id>`.
- **References.** A transition kind's settings may hold typed references (a stinger's
  media file, a matte scene). Scene and source references are delete blockers, and
  `delete.plan` clears them, as for any settings. They add no nesting edge, because a
  transition renders on the program mix, outside every scene.
- **A missing kind.** A preset whose kind is missing (a pack that isn't installed) is kept
  verbatim under ADR-0025's unknown-data rule. A switch that resolves to it cuts, and the
  engine reports the preset as unavailable, so a switch never fails or stalls.

**2. Kind origins, and plugin kinds (N6).**
- **Every kind reports its `origin`**: `builtin`, `pack` or `plugin`. `kind.list`'s
  summary and the `KindDescriptor` carry it. Built-in kinds come from `jstream-sources`
  and the built-in effects, pack kinds from effect packs (ADR-0025), and plugin kinds
  from native plugins the engine hosts. `origin` gets an `unknown` fallback, like
  `ParamType`, so a later origin is a minor change.
- **Plugin kinds today are CLAP audio filters**, one per CLAP plugin the engine finds
  ([ADR-0035](0035-clap-plugins-out-of-process.md) §4), with origin `plugin` and category
  audio filter. The ID is `clap/<plugin id>`, where the plugin ID is the plugin's own
  stable CLAP identifier. No effect pack may be named `clap`, so a plugin kind never
  shadows a pack kind, and the `/` keeps it apart from built-ins. A later plugin format
  gets its own reserved prefix and the same origin.
- **The descriptor is generated from the plugin's parameter list at load.** The engine
  never loads a plugin (ADR-0035 §1). Each time the plugin host process loads a plugin
  (to scan it, or to run it as a filter), and whenever the plugin asks its host to rescan
  its parameters, the host reads the parameter list and reports it. The engine generates
  the descriptor from it, and a descriptor that differs from the one it holds is
  reported by `kind.changed`. Each parameter the plugin exposes and doesn't mark hidden
  becomes a `ParamDescriptor`:
  - `id`: the plugin's parameter ID in decimal, which CLAP requires to be stable;
  - `type`: `bool` for a stepped parameter with exactly two values, `int` for any other
    stepped parameter, `float` otherwise, each with the plugin's range and default;
  - `label` from the parameter's name, and `group` from its module path;
  - `live`: true when the plugin marks the parameter automatable, false otherwise.

  Plus one parameter, `state`, of the new `blob` type (below). The descriptor's
  `version` is always 1. A plugin versions its own state, so the engine ships no
  migrations for plugin kinds.
- **Settings and the plugin's state.** The plugin's saved state is authoritative: the
  host restores the plugin from `state`, and the parameter values are the plugin's view
  of it, which the engine reads back after a restore. A parameter edit reaches the plugin
  through the parameter ring (ADR-0035 §6). When the user closes the plugin's own editor,
  the engine reads the new state and parameter values and applies them as one edit,
  attributed to the client whose action opened the editor, so it is one undo step and
  one audit entry ([ADR-0013](0013-command-model.md)). While the editor is open its
  changes are runtime state, like live values
  ([ADR-0032](0032-live-edits-and-undo.md)). A state larger than the cap (below) is not
  applied: the stored state stays the last one that fit, and the engine reports an error
  on the filter.
- **A missing plugin** is a missing kind: the filter's stored settings, `state`
  included, are kept verbatim and saved back, and the filter is bypassed and reported
  (ADR-0025's unknown-data rule). A stored parameter that the installed plugin no longer
  exposes is unknown data under the same rule.

**3. The `blob` parameter type.** ADR-0025's `ParamType` gains `blob`: opaque bytes that
only the kind interprets.
- **Value.** A JSON string holding the bytes in standard, padded base64. The default is
  the empty string (no bytes).
- **Size cap.** The descriptor gives a `max_bytes`, and the engine caps every blob at
  2 MiB of decoded bytes, so a settings map still fits in one protocol message. A larger
  value is rejected with `invalid_argument`. Validation checks only the encoding and the
  size.
- **Not a setting a person edits.** A `blob` parameter is never `live` and never a
  reference: the graph doesn't look inside it. Clients don't offer it as an editable
  field. They show its size, and may copy it whole or reset it, which is how a plugin
  preset is copied between filters. A blob the kind rejects is a runtime error on the
  filter, never an edit failure, like a missing asset.
- **Kept out of reads by default.** State reads and events that carry settings report a
  blob as a summary, `{"type":"blob","bytes":…,"digest":…}`, instead of its bytes. One
  query returns the bytes of one blob parameter, for a client that copies or exports it.
  Edits carry the base64 string, or `null` to reset it; a summary in an edit is
  `invalid_argument`. Logs and the audit log record a blob's size and digest, never its
  bytes. The collection file stores the bytes.
- **Who may declare one.** Any kind may, but built-in and pack kinds prefer typed
  parameters. Clients that don't know the type show it read-only, through ADR-0025's
  `unknown` fallback, so adding it is a minor change.

**4. ADR-0005 is now ADR-0035 (N7, as ADR-0037 decided).** Where ADR-0025 cites ADR-0005
for effect packs, read ADR-0035, which supersedes ADR-0005 and keeps its decision on WGSL
effect packs and WASM plugins unchanged. Normative docs cite ADR-0035.

**5. Every edge-adding edit runs the cycle check (N8, as ADR-0037 decided).** ADR-0025's
list is not exhaustive. The edits that can add a dependency edge, and so run the one
shared cycle check, include:
- `item.add` and `item.set_source` ([ADR-0023](0023-placements-references-and-clones.md)),
  which can make a scene place a `scene` source;
- `item.move` ([ADR-0024](0024-scene-structure.md)), which can put a `scene` source in
  another scene;
- `scene.create` with `variant_of`, which adds the variant → parent edge (ADR-0024);
- `source.create`, `source.set_settings` and the filter edits, which can add a scene
  reference in settings (ADR-0025).

Transition preset edits add no edge (decision 1). ADR-0025's test that every edge-adding
edit is checked covers this whole list, and grows with any later edit that adds an edge.

<a id="consequences"></a>
## Consequences
- **New transition kinds need no protocol release.** A stinger or a wipe is a kind with
  a descriptor, and UIs and agents render its settings generically, as for sources and
  filters. The price is ADR-0025's: no compile-time types for a kind's settings.
- **CLAP filters fit the generic settings form.** The web UI and agents read and change
  a plugin's parameters like any filter's, and a plugin's state travels with the
  collection.
- **Clients can tell where a kind comes from** without parsing its ID: a UI can group
  plugin kinds, mark them as native code, and show the add-plugin warning that ADR-0035
  asks for.
- **Opaque state is a blind spot.** JStream can't validate or inspect a blob, so it can't
  check that a plugin keeps no secrets in it (a licence key, an account token).
  [R-SEC-001](../RULES.md#R-SEC-001)'s redaction can't apply to bytes it can't read.
  Keeping blobs out of state reads, events, MCP responses and logs by default limits
  where such bytes travel; the collection file still holds them. The add-plugin warning
  also says that the plugin's state is saved in the collection file.
- **A blob is untrusted input to the plugin.** A collection file from elsewhere hands
  its bytes to native code. That is ADR-0035's trust model (crash isolation, not a
  sandbox, [R-SEC-003](../RULES.md#R-SEC-003)), unchanged: the host process isolates a
  crash on restore, and the engine checks only the encoding and the size.
- **The 2 MiB cap bounds a plugin's state.** A plugin whose state is larger (a sampler
  that embeds its samples) can't save it in JStream. If real plugins need more, a later
  ADR can store large blobs as asset files beside the collection.
- **Protocol follow-ups,** all additive (one minor version each under
  [R-API-001](../RULES.md#R-API-001)), with schema snapshot updates
  ([R-TEST-002](../RULES.md#R-TEST-002)):
  - the transition preset shape and edits, when ADR-0029's model is built in M1b, take
    this ADR's shape from the start, so ADR-0029's `unmatched` and `stagger_ms` are never
    preset fields on the wire;
  - the `transition` kind category and the `origin` field, with the kind queries of
    ADR-0025;
  - the `blob` parameter type, its read summary and the query that returns its bytes,
    with plugin kinds and CLAP filters in M3. The query's name and shape are set in that
    protocol change ([R-API-002](../RULES.md#R-API-002)).
- **Built-in descriptors** for `cut`, `fade` and `move` join the kind snapshot test
  ADR-0025 asks for.
- **The plugin host** reports a plugin's parameter list each time it loads one, and has a
  scan mode that loads a plugin to report its descriptor without processing audio. The
  engine caches descriptors and rescans when the plugin search paths change.
- **When the cap becomes a constant in code,** a normative doc that states it pins it as
  a fact with that file as its source ([DOCS › Facts](../DOCS.md#facts)).
- **If transition presets are persisted before this lands,** moving `unmatched` and
  `stagger_ms` into `settings` is a `schema_version` bump with a migration and a fixture
  test ([R-API-003](../RULES.md#R-API-003)).
- **Follow-on doc edits when this ADR is accepted** (not made while it is proposed):
  - ADR-0037: `status = "superseded"`, `superseded_by` pointing here, and
    `cargo xtask docs generate` for its banner and the ADR index;
  - re-point every reference to ADR-0037 here, since references to a superseded ADR fail
    the docs check: the links and `depends_on` in
    [ARCH › Data model](../ARCHITECTURE.md#data-model) and
    [ARCH › Control protocol](../ARCHITECTURE.md#protocol), and the links in
    [ROADMAP › M2](../../ROADMAP.md#m2) and [› M3](../../ROADMAP.md#m3);
  - ARCH › Data model's settings bullet: kinds have an origin (built in, pack or plugin),
    and a CLAP plugin's saved state is a parameter of the opaque `blob` type, replacing
    "`state` type".
- **Rejected alternatives:**
  - **Keep `kind: Cut | Fade | Move` as fixed protocol fields** (the shape ADR-0029
    wrote). Every new kind would be a protocol release, and pack transitions couldn't be
    expressed at all. The maintainer chose descriptors.
  - **Move every field into settings.** Duration and easing are common to every timed
    transition and are what UIs show on a preset card. Keeping them fixed means a kind
    change keeps the preset's timing, and clients need no descriptor to show it.
  - **Keep ADR-0037's shape** (origin implied by the ID prefix, a type named `state`).
    The maintainer chose an explicit `plugin` origin and a `blob` type. A type named for
    one use also reads oddly on any other kind that declares opaque bytes.
  - **A separate blob field on CLAP filters beside the settings.** ADR-0035 puts the
    state in the settings, and a parameter type keeps one map, one patch shape and one
    undo path for everything a filter stores.
  - **Typed plugin parameters only, without the saved state.** Many plugins keep state
    that no parameter exposes (a loaded impulse response, an analyser's learned profile),
    so restoring parameters alone doesn't restore the plugin.
  - **Blob bytes inline in every read and event.** Simpler, but every settings event
    could carry up to the cap in base64, and every agent reading state would receive
    bytes it can't use and that may hold a plugin's secrets.
