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
verified = { hash = "b3:df060caaf2ef569b", by = "agent:claude@session_01GLjL5hF5jZU4Zs8b6dKnau", note = "K2: new crate; re-read every bullet against lib.rs, keyframe.rs and switch.rs: Field names, Props/Value, opacity check, Override::apply live follow, write/clear/reset/drop_unless/restore/undo inverses, no empty override, plan_switch Motion cases and order, tests list incl. 5000-step seeded walk" }
-->

Layer-0 model of keyframe scenes
([ADR-0049](../../docs/adr/0049-keyframe-scene-model-and-evaluation.md), on
[ADR-0024](../../docs/adr/0024-scene-structure.md) §2): what a variant of a scene
stores, what its items look like, and what a switch between two keyframes of one scene
animates. No dependencies, no `unsafe` and no I/O. Geometry and crop are type
parameters, so it doesn't depend on `jstream-protocol`: the graph will instantiate it
with the protocol's `ItemGeometry` and `Crop` when variants are built, and a client can
use it to preview a keyframe as the engine resolves it. Nothing uses it yet.

Contents:
- `Field`: the four keyable fields, `geometry`, `crop`, `visible` and `opacity`, with
  their wire names. `Props` holds an item's values for all four, and `Value` one
  field's value. `Value::problem` refuses an opacity that isn't finite or is outside
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
  already has overrides. `undo` applies an `Inverse`. No empty override is ever stored.
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
