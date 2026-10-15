<!-- @doc
id = "ADR-0049"
title = "Keyframe scenes: the override model, live values and switching between keyframes"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0049: Keyframe scenes: the override model, live values and switching between keyframes

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0024](0024-scene-structure.md) §2 makes a **keyframe scene** a first-class
variant of a scene: it has no items of its own, shows its parent's items, and stores
per-item overrides `{ geometry?, crop?, visible?, opacity? }`, an absent field following
the parent. Placement edits address `(scene, item)` and write an override when the scene
is a variant; `item.clear_override` removes one field. A keyframe "switches like a
scene, usually with a Move transition". [ADR-0040](0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md)
§1 and §2 settle overrides that leave the variant's tree and overrides on a shared
group's children. [ADR-0029](0029-transitions.md) defines transitions: preset
resolution, pairing, interpolation, easing, clamping and retargeting.

Those ADRs fix the model's shape and the edits' addressing. They leave open what is
needed to build it:
- when an absent field "follows the parent", whether it follows the parent's value at
  the moment the keyframe was made or its value now (the **live value**);
- what a switch between two keyframes of one parent animates, and how it relates to
  ADR-0029's Move, which pairs items by source and moves an unflattened group as one
  item;
- what happens when the parent, or the keyframe on program, is edited while it shows,
  or while a switch into it is running;
- what writing, clearing and dropping overrides return as their inverses, which the
  graph needs in one place before the protocol edits exist;
- whether a variant may set its own transition overrides, which ADR-0024 lists as not
  decided.

The roadmap item (K2) asks for the design and a pure model with an evaluator, with the
protocol later. Keyframe scenes themselves are already in scope (ADR-0024, accepted), so
this ADR decides internal design only and is agent tier
([ADR-0043](0043-narrow-maintainer-tier.md)). It adds no feature: in particular it does
not add timeline animation (keyframes that play by themselves over a clock), which would
be a product-scope decision.

<a id="decision"></a>
## Decision

**1. Overrides are deltas over live parent values.**
- A variant stores only its overrides. Its item's effective values are computed when they
  are read: each overridden field takes the override, each other field takes the
  **parent's current value**. Nothing is copied when a variant is created or a field is
  overridden.
- So an edit to the parent shows at once in every variant where that field is not
  overridden, and in no variant where it is. "Reset keyframe" (a batch of
  `item.clear_override`) makes the variant follow the parent again.
- The keyable fields are exactly ADR-0024's four: `geometry`, `crop`, `visible` and
  `opacity`. Each field is overridden whole: a variant that overrides `geometry`
  overrides all of it, the angles included, never a single coordinate. This keeps an
  override equal to one placement edit and its inverse.
- An override's `opacity` must be finite and within `[0, 1]`, like the item's own.
  Geometry and crop are checked by the same checks as the item's own placement edits.

**2. Writing and clearing, with their inverses.**
- Writing a field that was absent is undone by `item.clear_override` of that field.
  Writing a field that was present is undone by writing the old value (ADR-0024 §2).
- Clearing a present field is undone by writing its old value. **Clearing an absent field
  is allowed and changes nothing**, and its inverse is empty, so "Reset keyframe" can be
  sent without first reading which fields are set.
- An item whose overrides are all cleared has no entry left, so a variant never stores an
  empty override.
- Dropping overrides (ADR-0040 §1, and ADR-0024 §2 on `item.remove`) returns each dropped
  item's whole override, which the inverse restores unchanged.

**3. A switch between members of one family is a Move by item.**
- A **family** is a parent scene and its variants. A switch between two members of a
  family (a variant and its parent, or two variants of it) is a scene switch like any
  other ([ADR-0030](0030-studio-state-and-scene-switching.md)), and uses the preset
  ADR-0029 §2 resolves.
- Under a `Move` preset, the two sides show the same items, so they are paired **by item
  ID**, not by source. This gives the same pairs as ADR-0029 §4 (same items, same
  sources, same order) and stays exact when one source is placed twice.
- Each item then falls in one of four cases, by its effective visibility on each side:
  - visible on both sides: it **moves** from its effective values on the outgoing side to
    those on the incoming side, interpolated and eased as ADR-0029 §5 says (angles the
    shortest way, opacity linear, clamping after easing). An item whose values are equal
    on both sides is still, and costs no interpolation;
  - visible only on the incoming side: it **appears** with its appear effect;
  - visible only on the outgoing side: it **disappears** with its disappear effect;
  - visible on neither: it is hidden throughout.
- An item whose glide is `Never` (ADR-0029 §6) never pairs, as for any switch: it
  disappears and appears. Per-item transition settings belong to the item, so every
  member of a family shares them. They are not keyable.
- **Groups.** ADR-0029 §4 moves a group that renders to its own texture as one item.
  Between members of a family the group is the same scene on both sides, so its
  children also have the same identity on both sides: overrides on the children are
  interpolated inside the group's texture, with the group item's progress. A group that
  ADR-0029 flattens behaves as it does there.
- Under `Fade` and `Cut` presets nothing changes from ADR-0029: a keyframe switch fades
  or cuts like any switch.
- A switch out of a family (to an unrelated scene) uses each side's effective values as
  ADR-0029 pairs them. Variants add nothing to it.

**4. Live edits while a keyframe shows.**
- An edit to the parent, or to the variant's overrides, while the variant is on program
  or preview takes effect on the next tick, with no animation, as an edit to any visible
  scene does.
- During a switch, each item's end values are the effective values when the switch
  starts, in the plan the control thread sends (ADR-0029 §4). An edit that changes them
  mid-switch is sent as a new plan, and the render thread **retargets from what is on
  screen** (ADR-0029 §7). It never jumps, and never waits for the switch to end.

**5. Variants may set their own transition overrides.**
- A variant is a real scene, so it has its own `transition_in` and `transition_out`
  (ADR-0029 §2), which do not follow the parent. That lets one keyframe glide in more
  slowly than its siblings. A variant with none set uses the normal resolution: the
  other scene's, then the default.

**6. A pure crate holds the model and the evaluator.**
- `jstream-anim` (layer 0, no dependencies, no `unsafe`) holds: a variant's override set
  with writing, clearing, resetting, dropping and restoring and the inverses of §2;
  effective values (§1); and the switch plan of §3, which classifies each item and gives
  its two ends.
- It is generic over the geometry and crop types, so it doesn't depend on
  `jstream-protocol`. The graph instantiates it with the protocol's `ItemGeometry` and
  `Crop` when variants are built, and a client can use it to preview a keyframe exactly
  as the engine resolves it.
- Interpolation and easing stay with ADR-0029's transition sampler. The plan supplies its
  ends; it does not interpolate.

<a id="consequences"></a>
## Consequences
- An edit to the parent never needs to touch its variants, and a variant costs only the
  fields it changes. Reading a variant's item costs one lookup per item.
- Keyframes animate through the transition code that already exists. The only new
  behaviours there are pairing by item within a family and interpolating children inside
  an unflattened group, both small.
- Because overrides are whole fields, nudging one coordinate in a variant stores its
  whole geometry, which then no longer follows the parent's angles or size. That matches
  what a placement edit sends.
- The protocol and graph work comes later and takes a protocol-version slot:
  `variant_of`, `overrides`, `item.clear_override`, the `scene_id` addressing in
  variants, the dropped-overrides result (ADR-0040 §1), `schema_version` with a
  migration and fixture, and the render hookup of §3. Each changes its crate's README.
- **Not decided here:** timeline animation (keyframes that advance on their own, or
  per-property curves over time). It would change product scope, so it needs its own
  maintainer-tier ADR if it is wanted.
