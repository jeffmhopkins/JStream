<!-- @doc
id = "ADR-0180"
title = "A Move within a 3D scene's family glides item by item, its chain paused"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0180: A Move within a 3D scene's family glides item by item, its chain paused

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A 3D scene's views are its keyframe scenes (ADR-0049), and a Move between them glides
the camera (ADR-0094, ADR-0113). Depth effects such as ambient occlusion and depth bloom
are a scene's video chain (ADR-0143, ADR-0151), which a keyframe shares with its parent
(ADR-0024).

[ADR-0065](0065-scene-chains-and-item-effects.md) §6 makes a scene with a chain one
picture in a transition: a Fade crossfades the two pictures and a Move falls back to
that. For a 3D set that is wrong twice over. The set's views then cannot glide at all,
which is what they are for; and each picture was already drawn through its scene's
camera, so drawing it through a camera again, as tracks of a 3D scene are, showed it
zoomed and the set missing, both in a glide and when an item was shown on the scene
with a transition.

A 3D model's layer has the same flaw in any Move: it is drawn through its own scene's
camera into a canvas-sized layer, which cannot glide, so the leaving and the arriving
view of the model crossfaded, each still, while the cards around it moved.

<a id="decision"></a>
## Decision
1. **A Move within a 3D family pairs by item.** A Move by item from a member of a 3D
   scene's family, or from what was on screen, to another member plans each side's
   items one by one, as a Move of a scene without a chain does, whether or not the
   family has a chain. The chain does not run while the Move does; the shown scene
   draws with it again once the Move ends.
2. **A whole scene's picture is drawn flat.** Wherever a transition still draws a scene
   as one picture (a Fade, a Move between families, a 2D side), that picture is drawn
   over the canvas through no camera, since its scene's camera already drew it.
3. **A model's layer follows its track's camera.** While a transition's track draws a
   model's layer, the model is drawn through the camera that track is seen through, so
   a Move's moving camera turns the model with everything else.

This amends ADR-0065 §6 for 3D families.

<a id="consequences"></a>
## Consequences
- View glides in a 3D set with depth effects glide, and a model moves with the cards
  on it. The effects drop out for the length of the glide and come back at its end,
  which is visible on strong effects; running the chain over the moving set instead is
  left for later.
- Showing or hiding an item on a filtered 3D scene with a transition, and a Fade into or
  out of one, show the scene as it is.
- A 2D scene with a chain is unchanged: it is one picture, as ADR-0065 §6 says.
