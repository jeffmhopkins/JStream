<!-- @doc
id = "ADR-0111"
title = "A scene camera's up at either pole"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0111: A scene camera's up at either pole

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Amends [ADR-0094](0094-scene-cameras.md): decision 3's up near the poles, and its
consequence about the UI's canvas handles.

<a id="context"></a>
## Context
ADR-0094 decision 3 makes the picture's up world `−y`, or world `−z` when the camera looks
within 0.01° of straight up or down, where `−y` is no longer defined. The framing camera,
and most cameras people set up, face `−z`, toward the canvas. Tilt such a camera down to
the pole (looking along `+y`) and the up it carries smoothly is `−z`, so the picture holds
still. Tilt it up to the other pole (looking along `−y`) and the up it carries is `+z`, so
`−z` turns the picture over in the last 0.01° (#1047). The UI's orbit drag stops 85°
short of the poles, but a camera typed in Scene Properties, set over the protocol or met
mid-Move can be there.

The same ADR says the UI's canvas handles in a 3D scene work in the item's plane through
`unplace`, so a drag moves an item in its own plane. What was built instead (#1053) shows
no handles in a 3D scene, and a move drag keeps the item at its depth, on the plane
`z = depth_px` through `CameraView::on_plane`.

<a id="decision"></a>
## Decision
1. **The up at either pole.** Within 0.01° of straight down the picture's up is world
   `−z`, as before, and within 0.01° of straight up it is world `+z`. Each is the up a
   camera facing `−z` carries as it tilts to that pole, so tilting such a camera to either
   pole keeps its picture upright. `roll_deg` still turns the picture after that.
2. **The UI's canvas in a 3D scene** shows no handles. Dragging an item moves it on the
   plane of constant depth through it (`z = depth_px`, `CameraView::on_plane`), not along
   screen axes and not in a tilted item's own plane.

Everything else in ADR-0094 stands.

<a id="consequences"></a>
## Consequences
- The view only changes within 0.01° of looking straight up. A camera saved exactly at
  that pole now shows its picture the other way up, as it would have shown it a hair
  before the pole. There is no wire change and no protocol bump.
- A camera that faces another way as it tilts to a pole (`+z`, toward the viewer from
  behind the canvas, or along `x`) still turns its picture there, as any fixed fallback
  up does for some direction. Carrying the up from the previous frame would avoid it, but
  the camera has no previous frame: it is a pose, set and keyed as one.
- `SceneCamera`'s view (`geometry/camera.rs`) picks the up by the sign of the forward's
  `y`, and a test tilts to both poles.
