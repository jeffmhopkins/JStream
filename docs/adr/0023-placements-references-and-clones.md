<!-- @doc
id = "ADR-0023"
title = "Placements, references and clones"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0023: Placements, references and clones

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
M1b builds shared sources, duplicates, and "filters and effects"
([ROADMAP](../../ROADMAP.md#m1)). The maintainer-set M1b bullet reads: "source filters
apply everywhere the source appears, and item effects apply to one placement". Before the
graph, the protocol and the compositor build it, one question has to be settled: which
properties belong to a **placement** (a scene item), and which to the **source** it
places.

**Today.**
- [ARCH › Data model](../ARCHITECTURE.md#data-model) has `SceneItem { id, source_id,
  transform, crop, visible, blend, effects[] }`, and says "`SceneItem.effects[]` and
  `crop` belong to that one placement". Opacity is not modelled anywhere.
- Protocol 0.3 and `jstream-graph` build items with a source, a transform, a crop and
  `visible`. Blend, opacity and effects are not built.
- Sources exist independently of scenes ([ADR-0007](0007-bus-matrix-audio.md)).
  `item.add` of an existing source places the *same* instance, a **reference**.
  `source.duplicate` followed by `item.add` makes an independent copy, a **clone**.
- No edit changes which source an item places, and nothing records which source a clone
  was copied from.

**What the UI needs.** The chosen UI prototype
([design/ui-prototype](../../design/ui-prototype/README.md#settles), direction D) and its
[decision register](../../design/ui-prototype/decision-register.md) (entries D2 and D3)
show:
- a reference "shares everything with its source except its placement (position, size,
  rotation, crop)". The prototype routes opacity and blend to the **source**, so every
  placement of a source blends the same way;
- filters always apply "everywhere this source appears". There is no per-placement effect
  chain: to vary a look per placement, the user clones;
- visibility is per placement;
- a right-click toggles an item between "Reference to original" and "Independent clone".
  Going back to a reference discards the clone's changes. References show in italics,
  because they "reference a source owned by another scene".

**The forces:**
- Per-source blend is odd for a nested scene used as a background in one scene and as an
  overlay in another.
- The Move transition (register D25) animates per-item alpha, so the compositor needs a
  per-placement opacity anyway.
- A per-placement effect chain costs a render pass per placement. The prototype shows no
  one missed it.
- Removing and re-adding an item to switch between reference and clone loses the item's
  ID. Keyframe overrides and per-item transition settings are keyed by item ID
  (register D5 and D25), so the conversion must keep it.
- "Owned by another scene" contradicts ADR-0007's scene-independent sources, and would
  make deleting the owning scene awkward.

**The maintainer's choice.** For D2, the maintainer chose **"Hybrid"**: opacity and blend
belong to each placement, with position, size, rotation and crop. There is no
per-placement effect chain in v1, but the model keeps an `effects[]` slot so it can come
later without a migration. Source filters stay shared. A reference shares everything
except placement, opacity and blend. For D3, the maintainer chose the register's
recommendation: an edit that converts between reference and clone, and no ownership.

This ADR changes a maintainer-set ROADMAP bullet and the ARCH data model. It contradicts
no accepted ADR, so it supersedes none: ADR-0007 keeps sources independent of scenes,
and this ADR keeps that. It builds on [ADR-0013](0013-command-model.md) (edits and their
inverses). The item geometry it refers to (centre, size, rotation, 3D, fit) is ADR-0022's.

<a id="decision"></a>
## Decision

**1. A placement owns how the source is drawn there.** An item has:

| Field | Meaning | Default |
|---|---|---|
| `geometry` | Where and how big, ADR-0022's item geometry (`transform` until it lands) | ADR-0022's default |
| `crop` | Source pixels cut off each edge (unchanged) | none |
| `visible` | Whether the item is drawn (unchanged) | `true` |
| `opacity` | Alpha multiplier, a `Finite32` in [0, 1] | 1 |
| `blend` | How the item composites onto what is below it | `normal` |
| `effects[]` | Per-placement effect chain: **reserved, always empty in v1** | empty |

- **Opacity** multiplies the item's alpha after crop and fit. A value outside [0, 1] is
  rejected with `invalid_argument`.
- **Blend** is a string enum with `normal` as its default. The set of modes, and how they
  are computed in the linear FP16 working space, are decided with the render pipeline
  (register D13). Until the compositor draws a mode, the engine rejects it with
  `invalid_argument`, so stored state never claims something that isn't drawn.
- **`effects[]`** exists in the graph model and, once collections are persisted, in the
  on-disk format, always empty. No edit adds to it in v1, and the protocol doesn't report
  it until item effects are built. Adding them later is then an additive protocol change
  and needs no collection migration.

**2. The source owns what it is.** A source has its kind, its settings, its filter chains
and its name. Everything placed from it shares them, in every scene. Source filters
apply everywhere the source appears, as ROADMAP M1b already says. Organisation metadata
(labels, favourites) also belongs to the source; the scene-structure decision
(ADR-0024) defines it.

**3. References and clones.**
- A **reference** is an item that places a source which other items also place. It
  shares everything with them except its placement, opacity and blend (decision 1).
- A **clone** is an item that places a source made by `source.duplicate`. From then on
  the copy changes independently.
- Sources have **no owner**. No placement is "the original": every placement of a shared
  source is equal. A client shows how often a source is placed ("shared ×3") on each of
  its placements, rather than marking all but one as references. This replaces the
  prototype's italics rule.
- Cloning a `scene` source copies the source, with its filters, not the scene: both
  sources still show the same scene. Copying a scene's content is a scene duplicate
  (register D43), not a clone.

**4. Converting keeps the item.**
- New edit **`item.set_source { item_id, source_id }`**. It changes which source an item
  places, and keeps the item's ID, placement, opacity, blend and stacking position. Its
  inverse is `item.set_source` with the previous source. It is rejected with
  `not_found` if the source doesn't exist, and with `invalid_argument` if a `scene`
  source would close a cycle. The cycle check therefore runs on every edit that adds
  nesting, not only on `item.add`.
- New source field **`duplicated_from: Option<SourceId>`**, set by `source.duplicate` and
  reported in `SourceInfo`. It is a plain link: deleting the source it names is allowed,
  and the link then reads as absent. `source.delete`'s inverse restores it, so
  `source.create` gains an optional `duplicated_from`.
- **Reference → clone** is one batch: `source.duplicate`, then `item.set_source` to the
  copy.
- **Clone → reference** is one batch: `item.set_source` back to `duplicated_from`, then
  `source.delete` of the clone if no other item places it. That discards the clone's own
  settings and filters, as the prototype says. The option is offered only while
  `duplicated_from` names an existing source.

**5. Edits and compatibility.**
- New edits: `item.set_opacity { item_id, opacity }` and `item.set_blend { item_id,
  blend }`. Each has the same edit with the previous value as its inverse.
- `item.add` takes optional `opacity` and `blend`. `item.remove`'s inverse restores them,
  adding `item.set_opacity` and `item.set_blend` only where they differ from the
  defaults, as it already does for crop and visibility.
- `ItemInfo` reports `opacity` and `blend`, so it still "carries every property an edit
  can set", and a client-built copy of a scene is faithful (register D43).
- How placement edits address an item inside a keyframe scene (`scene_id` plus
  `item_id`) is ADR-0024's. Opacity is one of the fields a keyframe may override there.
- All of this is additive: a minor protocol release under
  [R-API-001](../RULES.md#R-API-001), with the schema snapshot as its API review
  ([R-TEST-002](../RULES.md#R-TEST-002)).

<a id="consequences"></a>
## Consequences
- **Easier:**
  - a nested scene can be a background in one scene and a blended overlay in another;
  - the Move transition and keyframes animate opacity per placement with no extra model;
  - one render pass per source, not per placement, since there are no item effects;
  - switching between reference and clone keeps the item's ID, so keyframe overrides and
    per-item transition settings survive it;
  - no ownership rule, so deleting a scene never orphans or re-homes a source.
- **Harder:**
  - an M1b deliverable shrinks: a different look per placement now needs a clone, until
    item effects are built;
  - the compositor applies opacity and blend per item, and blend modes need their own
    decision (register D13);
  - a reserved, always-empty `effects[]` has to be carried through the model and the
    persisted format;
  - the cycle check moves from one edit to every edit that adds nesting.
- **Not decided here:**
  - the blend mode list and its maths (register D13);
  - per-placement effects: when, and with what render cost. A future ADR decides it;
  - scene duplicate as an edit (register D43);
  - how a keyframe scene addresses and overrides placements (ADR-0024).
- **Docs to change once this is accepted** (they describe the current state, so they
  are not changed while this ADR is proposed):
  - **ROADMAP › M1b**, "Filters and effects": replace "item effects apply to one
    placement" with "each placement has its own opacity and blend; per-placement
    effects are not in v1, and the model reserves a slot for them". The "Shared
    sources" bullet gains the reference ↔ clone conversion that keeps the item.
  - **ARCH › Data model**: `SceneItem` becomes `{ id, source_id, geometry, crop,
    visible, opacity, blend, effects[] (reserved, empty) }`, and `Source` gains
    `duplicated_from?`. The "Filters and effects" bullet says that a placement owns
    geometry, crop, visibility, opacity and blend, and that `SceneItem.effects[]` is
    reserved and empty in v1. The shared-source bullet uses "reference" and "clone", and
    says sources have no owner.
  - **`design/ui-prototype/README.md` › What the prototype proposes**: the reference
    definition becomes "shares everything except placement, opacity and blend", and the
    italics rule becomes the shared count.
- **Follow-up implementation PR** (this ADR changes no code):
  - `jstream-protocol`: `item.set_source`, `item.set_opacity`, `item.set_blend`, the new
    `item.add` and `source.create` fields, `ItemInfo` and `SourceInfo` additions, and the
    snapshot updates;
  - `jstream-graph`: the fields, their validation and inverses, the cycle check on
    `item.set_source`, the reserved `effects[]`, and the property test extended to cover
    them;
  - the compositor in M1b: per-item opacity and `normal` blend, with golden tests.

  Each changes its crate README, and ARCH where it says what is built.
