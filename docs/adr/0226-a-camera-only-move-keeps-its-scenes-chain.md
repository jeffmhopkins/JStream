<!-- @doc
id = "ADR-0226"
title = "A Move that moves only a 3D scene's camera keeps the scene's chain running"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0226: A Move that moves only a 3D scene's camera keeps the scene's chain running

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A 3D scene's views are often its keyframe scenes that differ only in their camera
([ADR-0049](0049-keyframe-scene-model-and-evaluation.md),
[ADR-0094](0094-scene-cameras.md)). A Move between two of them glides the camera.
[ADR-0180](0180-a-move-within-a-3d-family-glides-through-its-chain.md) makes that Move
pair item by item even when the family has a video chain, and pauses the chain for the
Move's length. So depth effects such as ambient occlusion and depth bloom
([ADR-0143](0143-scene-lights-and-depth-aware-scene-filters.md),
[ADR-0151](0151-the-rest-of-richer-3d-scenes.md)) drop out for the whole glide and come
back when it ends. On a set lit by its bloom, the glow dips on every camera move. A
camera moved some other way (camera motion, ADR-0113, or a channel, ADR-0183) keeps the
chain, so the same move looks different depending on how it is made. ADR-0180 left
running the chain over the moving set for later.

When nothing but the camera moves, the Move's tracks draw exactly what the scene's own
pass draws through a camera at the Move's position. That pass already runs the chain with
the scene's depth, lights, shadows and reflections.

This is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)). It changes how an existing Move
is drawn and nothing on the wire.

<a id="decision"></a>
## Decision
1. **A camera-only Move is drawn as the scene through a gliding camera.** A Move within
   a 3D family (ADR-0180 decision 1) whose destination has a video chain, where every
   track is an item at the same place at both ends and at every `via` stop (or, for an
   item that can't glide because its effects cover the canvas, such as a model's layer,
   [ADR-0106](0106-item-effects-in-canvas-space.md), one that goes and comes back at the
   same place with the same effects)
   ([ADR-0166](0166-moves-curve-through-keyframes-turn-as-one-and-may-orbit.md)), no
   effect setting blends ([ADR-0127](0127-moves-interpolate-effect-settings.md)), and the
   family's camera rides with no 3D group
   ([ADR-0164](0164-3d-groups-share-their-scenes-space.md) §5), is not drawn track by
   track. The program scene is planned as at rest, with its passes and its chain. Every
   camera of its family draws the Move's camera for the Move's length: the same path,
   timing, `via` stops and `camera_path` a track would be seen through. The family's
   camera motion, live aim and autofocus then apply to it each frame, as they do to a
   camera at rest.
2. **Everything else is unchanged.** A Move that also moves, shows, hides or re-crops an
   item, blends an effect, or leaves a camera anchored to a group still plans item by
   item with its chain paused, as ADR-0180 says. So does a Move between families, a Fade
   and a cut.
3. **The Move is still a transition.** The studio runs it as before. It reports the same
   start and end, a switch while it runs starts from what is on screen
   ([ADR-0029](0029-transitions.md) §7), and the shot is planned again when it ends.

This amends ADR-0180 for Moves that move only the camera.

<a id="consequences"></a>
## Consequences
- A view glide between camera-only keyframes looks like the same camera move made with
  camera motion or a channel: the chain, depth effects, reflections and lights run
  throughout, with no dip at either end.
- A camera-only glide costs a frame what the scene at rest costs, plus its chain, instead
  of a transition's tracks.
- A Move that changes anything besides the camera still drops the chain for its length.
  Running the chain over a set whose items move is left for later.
