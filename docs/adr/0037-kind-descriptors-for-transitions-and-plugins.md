<!-- @doc
id = "ADR-0037"
title = "Kind descriptors for transitions and CLAP plugins"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0037: Kind descriptors for transitions and CLAP plugins

<a id="context"></a>
## Context
[ADR-0025](0025-typed-settings-kind-descriptors.md) describes the settings of sources and
filters with kind descriptors that the engine reports and validates against. Two ADRs
accepted after it leave gaps that issue #93 records, from the reviews of ADR-0025:

- **Transition presets (N5).** [ADR-0029](0029-transitions.md) fixes a preset as
  `{ id, name, kind, duration_ms, easing, unmatched, stagger_ms }` with
  `kind: Cut | Fade | Move`. A closed enum means every new kind (a stinger that plays a
  media file, a WGSL wipe from an effect pack) is a protocol change, and `unmatched` and
  `stagger_ms` mean nothing to a kind that doesn't work per item. The maintainer decided
  N5 in the builder session, relayed on #93: **"Yes, use descriptors"**: transition
  presets get kind descriptors and typed settings like sources and filters, Cut, Fade and
  Move are the first three kinds, and new kinds need no protocol change.
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

This is an agent-tier decision ([ADR-0017](0017-adr-decision-tiers.md)). The one product
choice in it, descriptors for transitions, is the maintainer's, quoted above. The rest is
a protocol and data-model shape within ADR-0025, ADR-0029 and ADR-0035. It adds to
ADR-0025 and ADR-0029, which stay accepted and unchanged, and supersedes no ADR. Nothing
it touches is built yet: the protocol has no transition presets, kind descriptors or
CLAP filters, so every change is additive under [R-API-001](../RULES.md#R-API-001).

<a id="decision"></a>
## Decision

**1. Transition kinds are kinds.**
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

**2. Plugin-provided kinds (CLAP).**
- ADR-0025's kinds gain a third source: **plugin kinds**, one per CLAP plugin the engine
  finds ([ADR-0035](0035-clap-plugins-out-of-process.md) §4). Each is an audio filter
  kind. Its ID is `clap/<plugin id>`, where the plugin ID is the plugin's own stable
  CLAP identifier. No effect pack may be named `clap`, so a plugin kind never shadows a
  pack kind, and the `/` keeps it apart from built-ins.
- **The descriptor is built from the plugin's parameters.** The engine never loads a
  plugin (ADR-0035 §1), so the plugin host process reads them and reports them to the
  engine. Each parameter the plugin exposes and doesn't mark hidden becomes a
  `ParamDescriptor`:
  - `id`: the plugin's parameter ID in decimal, which CLAP requires to be stable;
  - `type`: `bool` for a stepped parameter with exactly two values, `int` for any other
    stepped parameter, `float` otherwise, each with the plugin's range and default;
  - `label` from the parameter's name, and `group` from its module path;
  - `live`: true when the plugin marks the parameter automatable, false otherwise.

  Plus one parameter, `state`, of the new `state` type (below). The descriptor's
  `version` is always 1. A plugin versions its own state, so the engine ships no
  migrations for plugin kinds. A plugin update that changes the parameter list changes
  the descriptor, and `kind.changed` reports it.
- **Settings and the plugin's state.** The plugin's saved state is authoritative: the
  host restores the plugin from `state`, and the parameter values are the plugin's view
  of it, which the engine reads back after a restore. A parameter edit reaches the plugin
  through the parameter ring (ADR-0035 §6). When the user closes the plugin's own editor,
  the engine reads the new state and parameter values and applies them as one edit,
  attributed to the client whose action opened the editor, so it is one undo step and
  one audit entry ([ADR-0013](0013-command-model.md)). While the editor is open its
  changes are runtime state, like live values
  ([ADR-0032](0032-live-edits-and-undo.md)).
- **A missing plugin** is a missing kind: the filter's stored settings, `state`
  included, are kept verbatim and saved back, and the filter is bypassed and reported
  (ADR-0025's unknown-data rule). A stored parameter that the installed plugin no longer
  exposes is unknown data under the same rule.
- **The `state` parameter type.** ADR-0025's `ParamType` gains `state`: opaque bytes
  that only the kind interprets.
  - The value is a JSON string holding the bytes in standard, padded base64. The
    descriptor gives a `max_bytes`, and the engine caps it at 2 MiB, so a settings map
    still fits in one protocol message. Validation checks only the encoding and the size.
  - A `state` parameter is never `live`, is never a reference (the graph doesn't look
    inside it), and its default is empty (no bytes).
  - Clients don't edit it as a field. They may copy it whole, which is how a plugin
    preset is copied between filters. A `state` the plugin rejects is a runtime error on
    the filter, never an edit failure, like a missing asset.
  - Any kind may declare one, but built-in and pack kinds prefer typed parameters.
    Clients that don't know the type show it read-only, through ADR-0025's `unknown`
    fallback, so adding it is a minor change.

**3. ADR-0005 is now ADR-0035 (N7).** Where ADR-0025 cites ADR-0005 for effect packs, read
ADR-0035, which supersedes ADR-0005 and keeps its decision on WGSL effect packs and WASM
plugins unchanged. Normative docs cite ADR-0035.

**4. Every edge-adding edit runs the cycle check (N8).** ADR-0025's list is not
exhaustive. The edits that can add a dependency edge, and so run the one shared cycle
check, include:
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
- **Opaque state is a blind spot.** JStream can't validate or inspect it, so it can't
  check that a plugin keeps no secrets in it. The add-plugin warning that ADR-0035 asks
  for also says that the plugin's state is saved in the collection file.
- **Protocol follow-ups,** all additive (one minor version each under
  [R-API-001](../RULES.md#R-API-001)), with schema snapshot updates
  ([R-TEST-002](../RULES.md#R-TEST-002)):
  - the transition preset shape and edits, when ADR-0029's model is built in M1b, take
    this ADR's shape from the start, so ADR-0029's `unmatched` and `stagger_ms` are never
    preset fields on the wire;
  - the `transition` kind category, with the kind queries of ADR-0025;
  - the `state` parameter type and plugin kinds, with CLAP filters in M3.
- **Built-in descriptors** for `cut`, `fade` and `move` join the kind snapshot test
  ADR-0025 asks for.
- **The plugin host** gains a scan mode that reports a plugin's descriptor without
  processing audio. The engine caches descriptors and rescans when the plugin search
  paths change.
- **If transition presets are persisted before this lands,** moving `unmatched` and
  `stagger_ms` into `settings` is a `schema_version` bump with a migration and a fixture
  test ([R-API-003](../RULES.md#R-API-003)).
- **Follow-on doc edits** (made with this ADR, since it is accepted):
  - [ARCH › Data model](../ARCHITECTURE.md#data-model): the `TransitionPreset` shape, the
    transitions and settings bullets (transition and plugin kinds, the `state` type), and
    ADR-0037 in `depends_on`;
  - [ARCH › Control protocol](../ARCHITECTURE.md#protocol): the preset edits carry a kind
    and settings;
  - [ROADMAP › M2](../../ROADMAP.md#m2) and [› M3](../../ROADMAP.md#m3): transition
    kinds and CLAP filter settings.
- **Rejected alternatives:**
  - **Keep `kind: Cut | Fade | Move` as fixed protocol fields** (the shape ADR-0029
    wrote). Every new kind would be a protocol release, and pack transitions couldn't be
    expressed at all. The maintainer chose descriptors.
  - **Move every field into settings.** Duration and easing are common to every timed
    transition and are what UIs show on a preset card. Keeping them fixed means a kind
    change keeps the preset's timing, and clients need no descriptor to show it.
  - **A separate blob field on CLAP filters beside the settings.** ADR-0035 puts the
    state in the settings, and a parameter type keeps one map, one patch shape and one
    undo path for everything a filter stores.
