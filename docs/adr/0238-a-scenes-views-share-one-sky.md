<!-- @doc
id = "ADR-0238"
title = "A scene's views share one sky"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0238: A scene's views share one sky

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A 3D scene's sky backdrop
([ADR-0167](0167-a-sky-backdrop-for-3d-scenes.md)) keeps state on the render thread
from frame to frame: its warp as it ramps, how far its stars have travelled, its
layout once a warp is aimed ahead
([ADR-0187](0187-a-warp-can-travel-straight-ahead.md)), and the arrival picture it
picked after the last warp
([ADR-0188](0188-a-backdrop-arrives-at-pictures-pinned-in-its-sky.md)). That state was
kept per drawn scene. A scene's views are its keyframe scenes
([ADR-0049](0049-keyframe-scene-model-and-evaluation.md)), each drawn under its own
ID, so each view had a sky of its own.

On a streaming PC, a cockpit with twelve views warped in one view and arrived at a
planet. Switching to another view showed a different planet, or a nebula, because that
view's sky had made its own pick. Its warp and stars were also out of step with the
first view's.

This is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)). It changes how an existing
backdrop is drawn, and nothing on the wire.

<a id="decision"></a>
## Decision
1. **One sky state per scene family.** The sky state is kept by the scene's family,
   which is the parent of a keyframe scene and the scene itself otherwise. This is the
   same family that keys camera motion and live aim
   ([ADR-0152](0152-a-ptz-controller-drives-scene-cameras.md)). Every view of a scene steps the same warp and
   travel once a frame, shows the arrival it picked, and is lit by that arrival.
2. **Each view draws it through its own camera.** Each drawn scene still has its own
   sky layers, drawn through its own camera. It places them only in a frame that drew
   them. So a Move between two views, which draws both ends' skies, shows one sky seen
   through two cameras.
3. **An arrival is fixed in the sky.** An arrival is pinned in the sky's layout, not
   ahead of a camera, so it stays where it is in the world as the views change. The
   sky's layout follows its environment light's item, so it changes between views only
   if a keyframe overrides that item's rotation.

This amends ADR-0167 and ADR-0188.

<a id="consequences"></a>
## Consequences
- After a warp, every view of a set shows the same planet or nebula in the same place,
  and the warp and stars run on unbroken across view switches.
- Two different scenes, which are not views of each other, still keep a sky each.
- The shared sky steps once a frame. When two views drawn in one frame set different
  warps on their keyframes, the first view drawn sets that frame's step.
