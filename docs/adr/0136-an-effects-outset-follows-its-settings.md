<!-- @doc
id = "ADR-0136"
title = "An effect's outset follows its settings"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0136: An effect's outset follows its settings

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0125](0125-item-effects-layer-grows-where-it-is-needed.md) decision 2 pads an
item's effects layer, at the first effect that draws past the box, by the outset of it
and every effect after it. That outset was each kind's `max_outset_px`, the most it
draws at any settings ([ADR-0065](0065-scene-chains-and-item-effects.md) §5): 300 px
for a drop shadow, 65 for an outline. A shadow and an outline at their defaults, which
reach 16 and 5 px, then grew a 320×216 item's layer to about 1050×946, and every blur
and distance pass ran over all of it. The perf suite's `effects_many` scene, 30 such
items, fell under its lavapipe floor (#1362).

<a id="decision"></a>
## Decision
1. **An effect's outset is what it draws at its settings**, as render measures it
   (`jstream_render::video_filter_outset`): a drop shadow its larger offset plus its
   blur, a glow or a bloom its radius, an outline outside the shape its width plus a
   pixel, and one inside it nothing. It is rounded up to a 16 px step, so a Move that
   blends a setting ([ADR-0127](0127-moves-interpolate-effect-settings.md)) resizes the
   layer only when it crosses a step, and it never passes the kind's `max_outset_px`.
   A kind render doesn't measure, such as an effect pack's, takes its `max_outset_px`
   whole.
2. **A Move pads for both looks.** While it blends an item's effects, each effect's
   outset is the larger of the two looks', so the layer holds what it draws at every
   point of the blend.
3. **`max_outset_px` keeps its meaning:** the bound an outset never passes, as ADR-0065
   §5 and the protocol have it.

<a id="consequences"></a>
## Consequences
- **Easier:** an item's effects cost what they draw. At their defaults a shadow and an
  outline pad a box by 32 px a side, not 365.
- **Harder:** a kind that draws past its input needs an outset function beside its
  descriptor, and one that reaches farther than it says is cut at the layer's edge.
  A change to a setting that crosses a step re-plans the item's layer at a new size.
- **Amends** ADR-0125 decision 2's outset. The rest of ADR-0125 holds.
- **Docs to change:** core › Snapshots, render › Filters.
