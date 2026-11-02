<!-- @doc
id = "ADR-0088"
title = "Keyframe scenes override an item's effects"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0088: Keyframe scenes override an item's effects

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A keyframe scene stores per item the fields it overrides: geometry, crop, visibility
and opacity ([ADR-0024](0024-scene-structure.md) §2,
[ADR-0049](0049-keyframe-scene-model-and-evaluation.md)). Item effects
([ADR-0065](0065-scene-chains-and-item-effects.md), in v1 by
[ADR-0068](0068-item-effects-in-v1.md)) came later. ADR-0065 §3 says a keyframe follows
its parent's effects and that "`effects` is not an override field", and lists keyframe
overrides of effects as not decided, since they "would be a new override field".
ADR-0068 leaves them open too.

So a keyframe can zoom into a camera but can't give the zoomed shot its own vignette or
rounded corners. The user has to make a second item, or a second scene with no Move
between them.

The question is not one of product scope: keyframe scenes and item effects are both in
v1. It is how one in-scope model covers a field of another, so this ADR is agent tier
([ADR-0043](0043-narrow-maintainer-tier.md)).

<a id="decision"></a>
## Decision
This amends ADR-0065 §3's bullet on keyframe variants, and ADR-0024 §2's list of
override fields, for item effects only. A keyframe scene still follows its parent's
scene filters, and every other part of ADR-0065 stands.

**1. `effects` is a fifth override field, overridden whole.** A keyframe's override of
an item may hold an effects list. Where it does, the keyframe draws the item with that
list in place of the parent's effects, in order, enabled or not. An empty list is an
override too: the item draws with no effects there. A keyframe that doesn't override
`effects` follows the parent's effects live, as for any other field. Filter IDs are
unique within the list, and it holds at most 8 filters (`MAX_CHAIN_FILTERS`), as an
item's own chain does.

**2. Two ways to write it.**
- **`item.set_effects { scene_id, item_id, effects }`** writes the whole list.
  `scene_id` must name a keyframe scene whose tree holds the item, with the errors of
  the placement edits addressed to a keyframe (ADR-0024 §2). The item's own scene, or
  any other scene, is `invalid_argument` on `scene_id`: an item's own effects are
  edited with `filter.*`, as ADR-0065 §3 says. `effects` is the whole list, each entry
  a `FilterInfo` (`filter_id`, `kind`, `enabled`, `settings`), each checked and its
  settings completed exactly as `filter.add` checks and completes an item effect's.
  Its inverse is `item.set_effects` with the old list, or `item.clear_override` with
  `field: "effects"` when the keyframe didn't override it. A filter whose kind is
  missing can't be written this way, since its settings can't be checked; only a load
  keeps one (§4).
- **The filter edits take a `keyframe { scene_id, item_id }` owner**, so a client
  edits one effect at a time and two clients editing different effects don't undo
  each other's. It names the item's effects as that keyframe shows them, with the
  same errors on `owner`. Where the keyframe follows the item, the edit first copies
  the item's own effects into the override and then applies to the copy, as one
  edit; its inverse is then `item.clear_override` of `effects`, so undo follows the
  item again. Where the keyframe overrides them, the edit changes the override in
  place and its inverse is the filter edit's own. A `scene` owner still refuses a
  keyframe scene (ADR-0065 §3): its scene filters are its parent's.
- `item.clear_override` takes `field: "effects"`, and its inverse is `item.set_effects`
  with the list it removed. "Reset keyframe" clears it with the other fields.
- There is no live value for it: filter settings have no live values yet, at any
  owner.

**3. What clients read.** `ItemOverride` gains `effects`, a list of `FilterInfo`,
omitted when the keyframe doesn't override it. `state.get` reports it, and so do the
dropped overrides an edit reports (ADR-0040 §1). `OverrideField` gains `effects`.

**4. Everything else is the override model's.**
- An override that leaves its keyframe's tree is dropped whole, effects included, and
  the inverse restores it (ADR-0040 §1).
- A scene or source reference in an overridden effect's settings is an edge from the
  keyframe scene, so `item.set_effects` runs the cycle check. A referenced object
  can't be deleted while an override names it: the blocker is the keyframe scene, with
  the field `overrides.<item_id>.effects.<filter_id>.<param>`, and `delete.plan` clears
  it with `item.set_effects`.
- Undo conflicts ([ADR-0032](0032-live-edits-and-undo.md)) key it as one property of
  the item, as the placement overrides are keyed.
- A Move between two keyframes of one family pairs the item only when its effects on
  both sides look the same ([ADR-0081](0081-effects-pair-by-their-look.md)). An
  overridden list that differs makes it disappear on one side and appear on the
  other. Effect settings are still never interpolated.
- The collection file stores the list in the keyframe's `overrides[]` entry, with each
  filter's kind version, and a filter whose kind is missing at load is kept verbatim,
  as an item's own effect is. That is a `schema_version` bump with a migration (an
  older file has no such list) and a fixture test
  ([R-API-003](../RULES.md#R-API-003)).

**5. Compatibility.** Additive: an optional `ItemOverride.effects`, a new
`OverrideField` value, a new `FilterOwner` variant and a new edit, as a minor protocol
release with the schema
snapshot as the API review ([R-TEST-002](../RULES.md#R-TEST-002)).

<a id="consequences"></a>
## Consequences
- **Easier:** one keyframe can frame its shot differently, with its own vignette,
  corners or shadow, and switching keyframes changes the look without a second item.
- **Harder:**
  - an item's effects can now differ per keyframe, so the render plan builds a chain
    per (keyframe, item), as it already keys item chains;
  - the first filter edit on a keyframe that follows the item copies the whole list,
    and an `item.set_effects` undo step holds the whole list;
  - a Move between keyframes whose effects differ fades the item out and in, rather
    than gliding it.
- **Not decided here:** per-filter overrides (a keyframe that changes one setting and
  follows the parent for the rest), live values for filter settings, and interpolating
  effect settings in a Move.
- **Docs changed with it:** ARCH › Data model (the override fields), the protocol,
  graph, core, control and UI READMEs where they list the override fields or the filter
  owners, and UI-FEATURES where it describes the Effects section.
