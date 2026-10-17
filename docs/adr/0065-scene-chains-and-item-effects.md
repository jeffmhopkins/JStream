<!-- @doc
id = "ADR-0065"
title = "Scene filter chains and item effects"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0065: Scene filter chains and item effects

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A video filter today belongs to a **source**
([ADR-0026](0026-source-filter-chains.md)). It runs on the source's whole native frame,
before any placement crops or scales it. That is right for keying and colour correction,
which should apply wherever the source appears. It is wrong for a filter whose look
depends on the edges of the frame: a vignette, rounded corners and a border, a drop
shadow, an outline or glow. Put a vignette on a 1920×1080 camera, place the camera
cropped to its centre, and the darkened corners are cut away with the rest of the frame.
The user wanted the vignette around what the viewer sees: the cropped shot, or the whole
scene (issue #529).

Accepted decisions already leave room for two more levels, and build neither:
- [ADR-0024](0024-scene-structure.md) §1 says a group, which is a scene, "takes scene
  filters" and renders to its own texture, and that a keyframe variant follows its
  parent's filters. No edit adds a filter to a scene.
- [ADR-0026](0026-source-filter-chains.md) §4 makes the filter edits' `owner` a tagged
  object, so that "buses, scenes and outputs can own chains later without new edits". It
  leaves scene chains undecided. PR #303, queued on `wave/m1-2`, adds the `master` and
  `output` owners of [ADR-0027](0027-program-master-and-output-chains.md).
- [ADR-0023](0023-placements-references-and-clones.md) §1 reserves an always-empty
  `effects[]` on every placement, so per-placement effects can come without a collection
  migration. It leaves "when, and with what render cost" to a future ADR. Its maintainer
  choice keeps them out of v1, and ROADMAP M1b says so.
- [ADR-0050](0050-3d-transforms-rendering.md) §7 already fixes where item effects run:
  after crop, fit and the clip to the box, in the box's local frame, before projection,
  "so an effect turns with its item". Opacity and blend come after them.
- The UI decision register gives sources, scenes and outputs separate video and audio
  chains, and a scene no audio chain
  ([D9](../../design/ui-prototype/decision-register.md#b-filters-and-the-render-pipeline)).
  Its render graph (D11) has scene nodes that composite items, then run the scene's
  chain, each once per tick.
- [ADR-0032](0032-live-edits-and-undo.md) makes descriptor-marked `live`
  filter settings adjustable during a gesture without an undo step.

Two workarounds exist and neither is enough. A group around the cropped item takes the
filter on its `scene` source's chain, at the cost of one more layer the user has to
build. The master chain covers everything leaving JStream, but not one scene or one item.

**Tier.** This ADR is agent tier ([ADR-0043](0043-narrow-maintainer-tier.md)): it designs
features that accepted ADRs already anticipate (scene filters in ADR-0024, the reserved
`effects[]` in ADR-0023, their place in the render order in ADR-0050), with additive
protocol changes under [R-API-001](../RULES.md#R-API-001). One question in it is
maintainer level: whether item effects move into v1, which ADR-0023's maintainer choice
and ROADMAP M1b keep out. Following ADR-0043's split rule, this ADR does not answer it.
It fixes the design so that scheduling is the only question left.

### Options for the scene level

- **S1. A scene owns a video chain**, addressed by a new `owner`
  `{"type":"scene","scene_id":…}` on the existing filter edits. It runs on the scene's
  composite, wherever the scene renders.
- **S2. No scene chain.** Point users at the group workaround, or at the master chain.
  It adds nothing, but leaves "one click" as a hand-built nested scene, and the master
  chain covers every scene at once.
- **S3. Scene filters live on the `scene` source.** Its chain already exists, but it
  runs only where the source is placed, never when the scene itself is on program or in a
  video mix.

### Options for the item level

- **I1. An item owns a video chain**, addressed by `{"type":"item","item_id":…}` on the
  same filter edits, stored in the reserved `effects[]`. Its filters are ordinary
  `video_filter` kinds.
- **I2. A separate effect family:** `effect.*` edits and an effect kind category. It
  duplicates validation, undo, live values and the MCP tool for no gain, and a WGSL pack
  would need to register each effect twice.
- **I3. Placement-aware source filters:** run the source chain once per placement, with
  the placement's crop and box as uniforms. A source's chain would no longer run once per
  source per tick, which breaks the D11 render graph and ADR-0023's "source filters apply
  everywhere the source appears", and ADR-0050 §7 already places effects after crop.
- **I4. Groups only:** one group per item that needs a framed look. It is the workaround
  the issue asks to replace.

**Recommendation: S1 and I1.** Both reuse ADR-0026's edits, kinds, undo, live values and
MCP tool, and differ from a source chain only in what they run on.

<a id="decision"></a>
## Decision
**S1 and I1.** A filter can belong to a source, a scene or an item. Each level runs on a
different picture:

| Owner | Runs on | Runs | Typical use |
|---|---|---|---|
| source | the source's native frame | once per source per tick | keying, colour correction |
| item (`effects[]`) | that one placement's cropped, fitted box | once per item per tick | rounded corners, vignette on a zoomed shot, drop shadow |
| scene | the scene's whole composite | once per scene per tick | vignette or grade on one scene |

**1. Owners.** `FilterOwner` gains two variants, beside `source` and PR #303's `master`
and `output`:
- `{"type":"scene","scene_id":…}`: the scene's chains.
- `{"type":"item","item_id":…}`: the item's effects. Item IDs are unique across all
  scenes (ADR-0024), so no `scene_id` is needed.

Both have a **video chain only**. `chain: "audio"` on either is `invalid_argument` on
`chain`, as for a source without that chain. A scene's audio is a bus matter
(ADR-0028), and an item has none of its own.

**2. Filters are the same filters.** An item effect and a scene filter are
`{ filter_id, kind, enabled, settings }` with a `video_filter` kind from `kind.list`,
checked and stored exactly as ADR-0026 §3 says. Every video filter kind is valid at
every level. Filter IDs are unique within their owner. A chain holds at most 8 filters
(`MAX_CHAIN_FILTERS`).

**3. The edits are ADR-0026's.** `filter.add`, `filter.remove`, `filter.set_enabled`,
`filter.set_settings` and `filter.reorder` take the new owners with their existing
inverses. No edit is added. Beyond them:
- `item.remove`'s inverse re-adds the item's effects in order after `item.add`, and
  `scene.delete`'s re-adds the scene's filters after `scene.create`.
- An item's effects belong to the placement. `item.move` and `item.set_source` keep them
  with the item, as they keep its opacity and blend, so converting between reference and
  clone keeps them. `source.duplicate` copies only the source's chains.
- A keyframe variant follows its parent: it shows the parent's scene filters and its
  items' effects, and `effects` is not an override field. A filter edit whose owner is a
  variant is `invalid_argument`, as ADR-0024 §2 already says of filter edits on a
  variant.
- A scene or source reference in a scene filter's settings is an edge from that scene; in
  an item effect's settings, an edge from the item's scene. `filter.add` and
  `filter.set_settings` run the cycle check, and a referenced object can't be deleted
  while a filter names it. The blocker field is `filters.<filter_id>.<param>` for a
  scene and `effects.<filter_id>.<param>` for an item, and `delete.plan` clears them with
  `filter.set_settings`, as for sources (ADR-0026 §5).
- Undo conflicts ([ADR-0032](0032-live-edits-and-undo.md)) key a filter by
  (owner, `filter_id`).

**4. What clients read.** `SceneInfo.filters` reports the scene's chains (its `audio`
chain always empty) and `ItemInfo.effects` the item's effects, a list of `FilterInfo` in
order. Each is omitted while empty. `item.list`, `scene.list` and `state.get` carry them,
so a client-built copy of a scene stays faithful.

**5. Render order.** For one tick, from the inside out:
1. **Source:** the source's frame, through its video chain, in source pixels.
2. **Item:** crop, fit into the box and clip to the box, in the box's local frame
   (ADR-0050 §7). Then the item's enabled effects, in order, on a layer the size of the
   box in canvas pixels, plus an **outset** (below). Then the 3D placement and sampling,
   coverage times opacity, and blend, in stacking order. Opacity and blend apply to the
   effected layer as a whole, so a drop shadow fades with its item.
3. **Scene:** the background and the items, composited, then the scene's enabled
   filters, in order, on the canvas-sized result.
4. **Nested:** where a `scene` source places the scene, that source's own video chain
   runs next, and the result is placed like any source frame (step 2).
5. **Mix and out:** the mix (with any transition), the master chain, the output's chain,
   the privacy stage and colour conversion (ADR-0027).

So a vignette on an item follows that item's cropped box and turns with it, and the same
source placed elsewhere uncropped is untouched. A vignette on a scene darkens the corners
of that scene's picture, whatever is cropped inside it. Two `scene` sources showing the
same scene share its scene filters and may differ in their own chains.

- **Pixels.** Size parameters of an item effect (a corner radius, a feather) are in the
  box's canvas pixels, so the look scales with the item, not with the source's
  resolution. A layer larger than the adapter's texture limit renders scaled down
  uniformly to fit it.
- **Outset.** An effect that draws outside its input (a drop shadow, a glow, an outline)
  needs room. A `video_filter` descriptor gains an optional `max_outset_px`, the most it
  can draw outside its input at any settings, so a live edit never resizes the layer. The
  item's layer is padded by the sum over its chain's enabled filters. A WGSL pack effect
  has none until the pack manifest gains the field. On a source or scene chain the outset
  is ignored: the output is the input's size.
- **Disabled and empty chains** are bypassed. An item with no enabled effect draws
  straight from the source as today, and a scene with no enabled filter composites
  straight into its target, since the result is the same. ADR-0024's group flattening
  still applies only to a group with no enabled scene filter.

**6. Transitions.** A scene with an enabled scene filter is one picture: during a
transition it is drawn as one layer, like a group with filters under
[ADR-0029](0029-transitions.md) §4. Fade crossfades the two layers, and Move falls back
to that crossfade, since the items are inside a filtered picture. A look meant for
everything on program belongs on the master chain, which keeps Move. A paired item
glides with its effects only when both sides' effects are equal (the same filters, in
order, with the same settings and switches). Otherwise the two items don't pair, and
each takes its unmatched appear or disappear effect. Effect settings are never
interpolated.

**7. Cost.** No new external resource, so unlike an output chain (ADR-0027 §2) nothing
changes encoder sharing.
- An item with an enabled effect costs one box-plus-outset `Rgba16Float` layer and one
  scratch texture of the same size for ping-pong, plus one pass per enabled effect and
  one to draw the box into the layer. A 1920×1080 layer is about 16.6 MB.
- A scene with an enabled filter costs one canvas-sized scratch texture and one pass per
  enabled filter. The scene renders once per tick however many places show it (D11), so
  its chain runs once.
- Layers are allocated when the render plan is built and reused while their size holds,
  never per frame ([R-RT-002](../RULES.md#R-RT-002)). During a live resize the layer
  keeps its size and the box renders into it scaled, until the committing batch rebuilds
  the plan.
- The engine reports the cost: `stats.tick` counts offscreen layers, filter passes and
  their texture bytes, so a UI can say "adds a render pass" and an agent can check the
  budget. The per-item and per-scene budgets are measured when built
  ([R-RT-005](../RULES.md#R-RT-005)). Whether a cap on items with effects is needed
  follows from that measurement.

**8. Live values.** A `filter.set_settings` value whose parameters the descriptor marks
`live` is live-adjustable for every owner, so `live.set` scrubs a scene's vignette or an
item's corner radius without an undo step (ADR-0032). The parameter channel keys it by
(owner, `filter_id`, parameter). Adding, removing, reordering and switching filters
change the render plan, so they are never live.

**9. Agents.** The MCP `filter` tool is generated from the protocol, so its ops take the
new owners with no MCP change. An agent adds an effect to an item with
`filter {op:"add", owner:{"type":"item","item_id":"cam-close"}, chain:"video",
filter_id:"frame", kind:"rounded_corners"}` (once that kind exists), scrubs it with the
`live` tool, reads it back from `item.list`, and checks the picture with `video.snapshot`
of the scene.

**10. Compatibility.** Additive throughout: two new `FilterOwner` variants (the enum is
`#[non_exhaustive]`), the optional `SceneInfo.filters` and `ItemInfo.effects`, and the
optional descriptor field, as a minor protocol release with the schema snapshot as the
API review ([R-TEST-002](../RULES.md#R-TEST-002)). The collection file gains
`scenes[].filters` and accepts a non-empty `items[].effects`, with a `schema_version`
bump, a migration (an older file has none of either) and a fixture test
([R-API-003](../RULES.md#R-API-003)).

<a id="consequences"></a>
## Consequences
- **Easier:**
  - a vignette, rounded corners or a shadow follows what is on screen, at the scene or
    the item, without building a group;
  - one edit family, one kind list and one MCP tool serve every level, so a WGSL pack
    effect works on sources, scenes and items alike;
  - undo, live values, the cycle check and `delete.plan` work as they do for source
    filters;
  - the source chain still runs once per source per tick, however many places show it.
- **Harder:**
  - every item with an enabled effect costs a layer and its passes, so a scene with many
    framed items costs GPU time and memory in proportion; the cost is reported, and the
    budget is measured when built;
  - a scene with a scene filter loses per-item Fade and Move in transitions, which is
    why a program-wide look belongs on the master chain;
  - the render plan gains item-layer and scene-chain nodes, and the descriptor an outset
    field that built-in kinds which draw outside their input must declare.
- **Not decided here:**
  - **whether item effects are built in v1.** That is the maintainer's: ADR-0023's
    choice and ROADMAP M1b keep them out. Until the maintainer moves them into scope, the
    engine rejects the `item` owner with `invalid_argument` and `effects[]` stays empty,
    as today. Scene chains are in scope already (ADR-0024) and are built with the video
    chain runtime;
  - which built-in video filter kinds exist. A vignette, rounded corners and a border, a
    drop shadow, and an outline or glow are follow-up kinds for `jstream-render`'s
    filters, each with its descriptor and golden test;
  - keyframe overrides of effects, which would be a new override field;
  - a whole-picture crossfade for scenes without a scene filter (ADR-0029's later kind).
- **Implementation waits for the protocol queue.** Every part touches
  `jstream-protocol` (`FilterOwner`, `SceneInfo`, `ItemInfo`, the descriptor field), so
  it lands through the `wave/m1-2` protocol queue, after PR #303's `master` and `output`
  owners, and after the video chain runtime (roadmap V0) that runs source chains. This
  ADR changes no code. The follow-up PRs are:
  - `jstream-protocol`: the owners, the read fields, the descriptor field and the
    snapshot updates;
  - `jstream-graph`: scene chains and item effects in the model and the collection file,
    their validation and inverses, the edges, and the property tests;
  - `jstream-core` and `jstream-render`: the scene-chain and item-layer nodes, the
    outset, `stats.tick` counts, and golden tests (a vignette on a cropped item, on a
    scene, and on a rotated item);
  - the UI: "Scene filters" and "Effects" panels beside the source's filters.
- **Docs to change when built:** ARCH › Data model (`Scene.filters { video }`,
  `SceneItem.effects[]` as a video chain) and its "Filters and effects" bullet; ARCH ›
  GPU pipeline's render order; the protocol, graph, core and render READMEs. ROADMAP M1b's
  "per-placement effects are not in v1" changes only with the maintainer's scheduling
  decision.
