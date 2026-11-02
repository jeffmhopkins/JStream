<!-- @doc
id = "CRATE-jstream-anim"
title = "jstream-anim"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-anim

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-anim/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-anim", "docs/adr/0049-keyframe-scene-model-and-evaluation.md#decision"]
-->

Layer-0 model of keyframe scenes
([ADR-0049](../../docs/adr/0049-keyframe-scene-model-and-evaluation.md), on
[ADR-0024](../../docs/adr/0024-scene-structure.md) §2): what a variant of a scene
stores, what its items look like, and what a switch between two keyframes of one scene
animates. No dependencies, no `unsafe` and no I/O. Geometry, crop and effects are type
parameters, so it doesn't depend on `jstream-protocol`: `jstream-graph` instantiates
it with its placement, the protocol's `Crop` and its filter chains for a keyframe
scene's overrides, and a
client can use it to preview a keyframe as the engine resolves it. The engine pairs a
switch within a family by item in its own transition planner, not through
`plan_switch` yet.

Contents:
- `Field`: the fields a variant overrides, with their wire names: the four keyable
  ones, `geometry`, `crop`, `visible` and `opacity`, and `effects`, the item's whole
  effects list ([ADR-0088](../../docs/adr/0088-keyframe-scenes-override-item-effects.md)),
  a type parameter that defaults to `()`. `Props` holds an item's values for the four
  keyable ones, which are all a switch animates, and `Value` one field's value. `Value::problem` refuses an opacity that isn't finite or is outside
  `0..=1`; geometry and crop are the graph's to check.
- `Override`: the fields a variant overrides for one item, each whole.
  `Override::apply` gives the effective values over the parent's values as passed in,
  so an absent field follows the parent live.
- `Keyframe`: a variant's overrides keyed by item ID. `effective` resolves one item.
  `write` (refusing an invalid value and changing nothing) returns `Inverse::Clear`
  when the field was absent and `Inverse::Write` of the old value when it was present.
  `clear` returns the write that undoes it, or `None` for an absent field, which
  changes nothing. `reset` clears everything and returns the writes that restore it.
  `drop_unless` drops the overrides of items outside the variant's tree and returns
  them whole; `restore` puts them back, or changes nothing and reports the item if one
  already has overrides. `undo` applies an `Inverse`. `effects_mut` changes an
  overridden effects list in place, so a filter edit on a keyframe's copy needs no
  whole write. No empty override is ever stored.
- `plan_switch`: the plan for a switch between two members of a family (`None` for the
  parent itself). Items pair by ID, one `Step` each in the order given, with their
  effective values at both ends and a `Motion`: `Still` (visible, equal), `Move`
  (visible, different), `Appear`, `Disappear` or `Hidden`. It doesn't interpolate:
  ADR-0029's transition sampler does, from these ends.

**Tests** (in `keyframe.rs` and `switch.rs`): live follow of absent fields, each
write and clear inverse, the no-op clear, refused opacities, reset and its restore,
drop and restore, a refused restore changing nothing, the wire names, every `Motion`
case, ends between two keyframes and in reverse, ends following the parent's current
values, and pairing by item. And a seeded walk of 5000 random writes, clears, resets,
drops and restores, each of which must undo exactly and leave no empty override.
