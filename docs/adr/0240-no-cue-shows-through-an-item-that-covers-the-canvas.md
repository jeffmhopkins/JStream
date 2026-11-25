<!-- @doc
id = "ADR-0240"
title = "No cue shows through an item that covers the canvas"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0240: No cue shows through an item that covers the canvas

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
An item whose effects run over the canvas, as a 3D model's layer does, can't glide
([ADR-0106](0106-item-effects-in-canvas-space.md) decision 4): in a Move its two sides
never pair, so it disappears and appears. Within a family, both sides are the same
opaque layer. As a crossfade, the copy fading out under the copy fading in lets about a
quarter of what is behind them through half-way.

Within a 3D scene's family, a Move is drawn as the scene through a gliding camera when it
moves nothing but the camera
([ADR-0226](0226-a-camera-only-move-keeps-its-scenes-chain.md),
[ADR-0237](0237-an-item-shown-or-hidden-on-a-3d-scene-keeps-its-chain.md)). Otherwise it
is drawn track by track ([ADR-0180](0180-a-move-within-a-3d-family-glides-through-its-chain.md)),
with the crossfade. ADR-0237 let an item's show or hide that cuts another short keep the
scene drawn as itself. A Move to a view that cut a show or hide short did not get that,
nor the reverse: it started from what was on screen, where the model's two halves were
partway, so it was drawn track by track. On a streaming PC, a view change during a
hologram's hide showed the stars through the cockpit's model for the length of the glide,
and the glide also lost the scene's chain and its live sky.

Each earlier fix let one more kind of cue keep the scene drawn as itself. The crossfade
that shows through stayed, waiting for the next kind of cue.

This is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)). It changes how a Move is drawn,
and nothing on the wire.

<a id="decision"></a>
## Decision
1. **It swaps rather than crossfades.** Within a family, when an item that covers the
   canvas both goes and comes back in a Move, each of its halves swaps at once, at the
   Move's start, with no effect (`transition::plan`). Only one copy draws at a time, so
   nothing shows through it, however the Move is drawn. This amends ADR-0106 decision 4
   for Moves within a family. A Move between families, a Fade and an item that can't
   glide for another reason (its `glide` set to `never`) are unchanged.
2. **Its halves never stop the scene from being drawn as itself.** Drawn as the scene,
   the item shows its new side from the start, as its swap would. So the two halves of
   such an item count as still on any Move (`Planner::moves_only_the_camera`), even when
   their ends differ or a cut-short Move left more than one of them on screen. This
   widens ADR-0237 decision 2, which allowed this only in place.
3. **Every cue carries the fades it cuts short.** A switch to a view or a keyframe
   placed, as well as an item's show or hide, carries on the running fades of the Move
   it cuts short, when that Move was drawn as the scene (`Engine::running_fades`,
   `Running::carried`). Only an item's show or hide also starts in place: a view's other
   items can really move.

<a id="consequences"></a>
## Consequences
- Overlapping cues (shows, hides and view changes, each cutting the last short at any
  point) keep a 3D set drawn as itself, chain and sky included, when its views move only
  the camera. A test plays such sequences and checks every step
  (`no_overlap_of_cues_shows_through_what_covers_the_canvas`).
- A view that does move an item is still drawn track by track, with the chain paused.
  The model no longer shows through there either, but it shows its new side from the
  start of the glide rather than dissolving into it.
- A model's layer with different effects between two views also swaps, rather than
  dissolving between them.
