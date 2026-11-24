<!-- @doc
id = "ADR-0237"
title = "An item shown or hidden on a 3D scene keeps the scene's chain running"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0237: An item shown or hidden on a 3D scene keeps the scene's chain running

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Showing or hiding an item on program runs a Move by item
([ADR-0101](0101-item-show-hide-actions.md)). On a 3D scene that Move stays within the
scene's family, so [ADR-0180](0180-a-move-within-a-3d-family-glides-through-its-chain.md)
draws it track by track with the scene's video chain paused.
[ADR-0226](0226-a-camera-only-move-keeps-its-scenes-chain.md) keeps the chain only for a
Move that moves nothing but the camera, so an item appearing or going still drops it.

On a streaming PC, a 3D set whose depth effects (ambient occlusion, depth of field,
bloom) hold its look lost them for the 0.6 to 0.9 s a hologram card took to show or
hide. Without occlusion the walls read brighter and the stars behind showed through
them, so the whole set looked see-through on every cue.

An item that appears or goes on its own, where the scene places it, needs no track of
its own to be drawn: the internal show scene already fades its items in and out inside
the scene's pass ([`ShowFade`](../../crates/jstream-core/src/transition.rs)), sampled
over the item's pose as each frame is drawn.

This is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)). It changes how an existing Move
is drawn and nothing on the wire.

<a id="decision"></a>
## Decision
1. **Such an item fades in the scene's pass.** A Move that ADR-0226 decision 1 would
   draw as the scene through a gliding camera, but for tracks of items that appear or
   go without a partner, is drawn that way too. Each of those items must be one of the
   destination scene's own items (not one placed through a 3D group, and not a group
   moving its children), and the destination must still hold it. In the destination's
   pass, each such item is sampled with its track's effect and timing from the Move's
   start, as the show scene's items fade on their own. One that goes is still drawn
   until its fade ends. The plan wakes at the end of each fade and at the end of the
   Move, and then plans the scene at rest.
2. **Its other items are as the scene draws them now.** Such a Move starts from the
   program scene as it was drawn, at rest or partway through another item's showing
   or hiding (`Running::in_place`). Its other items can still differ at their two ends,
   and in their effects' settings, because the live values that drive them (a
   sequence's channel lanes, mappings) have moved since the cue. Those differences
   don't count as the Move moving them: the scene drawn as it is shows them as they
   are now, as it would at rest. On a streaming PC, a hologram's hide ran its lanes
   while its cards went, and without this its Move dropped the chain.
3. **Everything else is unchanged.** A Move whose items also move, re-crop, blend an
   effect or leave a camera anchored to a group, an item that appears or goes inside a
   group, a Move between families, a Fade and a cut are drawn as ADR-0180 and ADR-0226
   say.

This amends ADR-0180 and ADR-0226 for items that only appear or go.

<a id="consequences"></a>
## Consequences
- Showing or hiding a card on a 3D set with depth effects keeps the set's look: the
  chain, depth, lights, shadows and reflections run while the card fades in or out.
- The item draws in the scene's pass, so it is lit, shadowed and depth-sorted with the
  scene throughout, as it is at rest.
- A Move that moves items as well as showing one still drops the chain for its length.
  Running the chain over a set whose items move is still left for later.
