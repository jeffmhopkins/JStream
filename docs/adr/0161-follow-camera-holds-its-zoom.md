<!-- @doc
id = "ADR-0161"
title = "A follow camera holds its zoom, and only pans at the edges"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0161: A follow camera holds its zoom, and only pans at the edges

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Amends [ADR-0113](0113-camera-motion-and-follow-pointer.md) decision 2 (the follow
camera's fields and algorithm) and [ADR-0120](0120-follow-pointer-is-a-filter.md)
decision 1 (the `follow_pointer` filter's parameters). Where the follow camera lives,
its state, its zoom action and its safe margin are unchanged.

<a id="context"></a>
## Context
ADR-0113's follow camera takes the mock's algorithm, and two of its steps change the
camera's distance on their own. A fast pointer pulls the camera back for a moment, and
where a corner of the frame would show past the followed item's box, the camera moves
nearer. The lean adds to both: at an angle the frame's footprint on the box grows, so
leaning near an edge moves the camera in further.

The maintainer's hardware run found this reads as the camera zooming by itself near the
box's edges. The zoom someone sets, or steps live with `scene.zoom_camera`, is the one
they want to see. Near an edge the camera should pan, and the frame should stop at the
edge.

<a id="decision"></a>
## Decision
1. **A setting.** `CameraFollow` gains `hold_zoom`, a boolean, true by default and when
   omitted. The `follow_pointer` filter gains the matching Hold zoom setting, a
   `bool` parameter with the same default. A filter saved without it holds the zoom.
2. **With the zoom held:**
   - The camera's distance is the zoom's. At a zoom where the box can't fill the frame,
     it is the farthest distance at which the box still fills it at the resting angle.
     The distance glides in log space only when the zoom changes.
   - A fast pointer doesn't pull the camera back.
   - Near the box's edges only the look point is held, so the frame stops at the box's
     edge. The camera never moves nearer for a corner.
   - The lean is scaled back toward the resting angle, just far enough that the frame
     from that distance still fits on the box. So a lean never shows past the box, and
     never moves the camera in.
   - The safe margin (ADR-0120 decision 4) still applies after each step, and the frame
     staying on the box still comes first, so at the box's very edge the pointer can sit
     between the margin and the frame's edge.
3. **Without it**, the camera is ADR-0113's, as before.
4. **The wire.** One minor protocol version: `CameraFollow.hold_zoom` and the new
   descriptor parameter. Collection files need no new schema version: the filter's
   settings are a map, and a missing `hold_zoom` reads as true.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - the zoom on screen is the zoom asked for, so zoom steps are predictable and the
    camera pans along an edge instead of moving in and out.
- **Harder:**
  - scenes saved before this hold the zoom too, so a follow camera that relied on the
    pull-back or the corner move-in changes until its filter's Hold zoom is turned off.
  - with the zoom held, a large lean near an edge is limited, so the camera leans less
    there than it did.
- **We must now:**
  - check the held zoom along the edges on Windows hardware with the follow camera.
