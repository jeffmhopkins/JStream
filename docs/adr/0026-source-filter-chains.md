<!-- @doc
id = "ADR-0026"
title = "Source filter chains and the filter edits"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0026: Source filter chains and the filter edits

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Sources need filters: a colour correction on a camera, a noise gate and a compressor on a
microphone. Three accepted ADRs already say what filters are, but none says how a client
adds one:

- [ADR-0023](0023-placements-references-and-clones.md) puts filters on the **source**, so
  they apply everywhere it is placed, and keeps per-placement effects out of v1.
- [ADR-0025](0025-typed-settings-kind-descriptors.md) gives filters kind descriptors in
  two categories, `video_filter` and `audio_filter`, and settings in the same map and
  patch shape as a source's. It leaves "the filter edits themselves" to the filter-chain
  design, and says every edit that can add a dependency edge, the filter edits included,
  runs the cycle check.
- [ADR-0028](0028-audio-model-v2.md) runs each audio source through its own audio filter
  chain before its fader, and ADR-0027 adds master and per-output chains.

The UI prototype has separate video and audio chains, each filter with an ID, a kind, an
on/off switch and parameters, reorderable within its chain only. The decision register
proposes that model for sources, scenes, outputs and buses
([D9](../../design/ui-prototype/decision-register.md#b-filters-and-the-render-pipeline)),
and ARCHITECTURE's data model lists `Source.filters[]` beside `SourceAudio.filters[]`
without saying how one is edited.

The mixer can now run audio filter chains: D7b built gain, noise gate, compressor, limiter
and delay kernels, chains of up to 8 filters on every input and bus, and latency
compensation. The compositor has no video filter yet.

This ADR decides the source chains and their edits. Bus, scene, master and output chains
reuse the shape later with another owner. It is agent tier
([ADR-0043](0043-narrow-maintainer-tier.md)): a protocol and data-model shape inside
ADR-0023, ADR-0025 and ADR-0028, with no product-scope, security or compatibility
promise, and every change additive under [R-API-001](../RULES.md#R-API-001).

<a id="decision"></a>
## Decision
**1. A source has two chains.** `filters.video` and `filters.audio`, each an ordered list
of filters, applied first to last. A filter is `{ filter_id, kind, enabled, settings }`.
- A source has a video chain if its kind produces video (a `scene` source does), and an
  audio chain if it produces audio and is not a `scene` source: ADR-0028 gives a scene
  source `enabled` and `follow` only. A filter on a chain the source lacks is
  `invalid_argument`.
- The audio chain is the one ARCHITECTURE called `SourceAudio.filters[]`: it lives on the
  source beside the video chain, so one set of edits addresses both.
- A chain holds at most **8** filters, the mixer's capacity per chain.

**2. Filter IDs are unique within their source.** They are chosen by the client, like
item IDs, and a filter is addressed as (source, filter). Unlike the register's proposal,
they are not global: `source.duplicate` copies both chains with their IDs, so a clone's
filters match the original's one for one, and nothing has to mint new IDs.

**3. A filter's kind matches its chain.** `kind` is a `video_filter` kind for the video
chain and an `audio_filter` kind for the audio chain, from `kind.list`. Its settings are
checked against the descriptor and stored complete, exactly as a source's are
(ADR-0025). A kind the engine doesn't describe, or one of another category, is
`invalid_argument` on `kind`, as an unknown source kind is. `source.create` likewise
refuses a filter kind.

**4. The edits.** Each has an exact inverse, computed from the state before it.
- `filter.add { owner, chain, filter_id, kind, settings?, enabled?, index? }` adds a
  filter at `index` (default: last), enabled unless `enabled: false`. Inverse:
  `filter.remove`.
- `filter.remove { owner, filter_id }`. Inverse: `filter.add` with its chain, kind,
  settings, `enabled` and index.
- `filter.set_enabled { owner, filter_id, enabled }`. A disabled filter stays in its
  chain and keeps its settings; it is bypassed. Inverse: the previous value.
- `filter.set_settings { owner, filter_id, patch }`, with `source.set_settings`'s patch
  rules (`null` resets a parameter). Inverse: the patched parameters' previous values.
- `filter.reorder { owner, filter_id, index }` moves a filter within its chain. Inverse:
  `filter.reorder` back.
- `owner` is a tagged object, `{"type":"source","source_id":…}` for now, so buses,
  scenes and outputs can own chains later without new edits.
- `source.delete`'s inverse re-adds every filter after `source.create`, and
  `source.duplicate` copies both chains. `SourceInfo.filters` reports both chains; it is
  omitted while both are empty.

**5. References in filter settings are edges of the source.** A scene or source reference
in a filter's settings is an edge from the filter's source, like one in the source's own
settings. `filter.add` and `filter.set_settings` run the cycle check, a referenced object
can't be deleted while the filter refers to it (the blocker's field is
`filters.<filter_id>.<param>`), and `delete.plan` clears such a reference with
`filter.set_settings`.

**6. What runs.** The audio chain drives the mixer's chain on the source's input, in
order, with disabled filters left out. The built-in audio filter kinds are the D7b
kernels: `gain`, `noise_gate`, `compressor`, `limiter` and `delay`. A filter the mixer
refuses at run time (its chain's latency over the mixer's bound, say) is a runtime error
on that filter, logged and bypassed, never an edit failure, as a missing asset is under
ADR-0025. No video filter kind is built in yet, so the video chain can only hold pack
kinds the engine describes, and the compositor does not draw them yet.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - one edit family serves both chains, and later buses, scenes and outputs;
  - a clone keeps its filter IDs, so a client's selection survives reference ↔ clone;
  - undo, persistence and the cycle and delete checks reuse what source settings
    already do.
- **Harder:**
  - two filters on two sources may share an ID, so a client keys filters by source too;
  - the engine has to map audio filter kinds onto mixer kernels and keep the mixer in
    step after every batch, undo and redo.
- **Not decided here:**
  - bus, scene, master and output chains (ADR-0027 for the last two), which reuse this
    shape with another `owner`;
  - sidechain keys on audio filters (ADR-0028 §keys): a key is a source reference in
    settings, so it is an edge by point 5, and the routing rules come with it;
  - video filter kinds and the render passes that draw them (register D11), blend mode
    maths (D13), and video delay (D15).
- **Docs to change:** ARCHITECTURE's data model (`Source.filters { video, audio }`, the
  audio chain no longer under `SourceAudio`), the protocol, graph, core and control
  READMEs, and the CLI's edit list.
