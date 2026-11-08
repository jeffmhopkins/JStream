<!-- @doc
id = "ADR-0128"
title = "A Move between a 2D side and a 3D one glides through the framing camera"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0128: A Move between a 2D side and a 3D one glides through the framing camera

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0094](0094-scene-cameras.md) decision 7 runs a Move between a 3D scene and a 2D one
as a Fade, because the two sides have no one space to move in. Decision 6 also refuses a
camera on a keyframe scene whose parent has none, so a 3D keyframe needs a 3D family.
Together these mean a 2D scene can't flip a card into 3D and back, or push in on one
keyframe: every item cross-fades instead of gliding (#1066).

But decision 3 already gives the space they share. The framing camera
(`SceneCamera::framing`) draws every item with neutral 3D exactly on its 2D box. Seen
through it, a 2D scene is a 3D scene that looks the same.

<a id="decision"></a>
## Decision
This amends ADR-0094 decisions 6 and 7. The rest of ADR-0094 stands.

**1. A 2D side of a Move is seen through the framing camera.** A Move between a side
with a camera and a side without one moves: the camera moves from the 3D side's to the
canvas's framing camera, or from the framing camera to the 3D side's, with the Move's own
timing, as decision 7 moves it between two cameras. Items pair as they do between two 3D
scenes, and each pair moves in that moving space. When the Move ends on the 2D side, that
scene draws as 2D again, which looks the same as the framing camera's last frame.

**2. Where it still fades.** The framing camera draws a 2D item the same only when the
item is flat: no tilt, turn or depth. A tilted 2D item is drawn with its own perspective,
which projects about the item's own centre, while a camera projects about the canvas's
centre. Even with its perspective at the framing camera's focal length, an item off the
centre would jump. So a Move whose 2D side has a tilted, turned or pushed item runs as a
Fade of the same duration, as decision 7 said for every 2D-to-3D Move, and draws each
side whole as a Fade would. The engine decides this when it plans the transition, from
the visible items on that side. A Fade and a stinger are unchanged.

**3. A keyframe of a 2D scene may have a camera.** `scene.set_camera` on a keyframe scene
whose parent has none is accepted: the keyframe is 3D through its own camera, and its
parent and the family's other keyframes stay 2D. Removing a parent's camera still drops
its keyframes' overrides, but only when the parent had a camera to remove. A removal on a
2D parent leaves its keyframes' cameras alone. A live removal of a parent's committed
camera makes its keyframes 2D while it lasts, as committing it would. A 3D parent still
can't have a 2D keyframe: a keyframe's `camera: null` removes its override.

**4. Compatibility.** A protocol minor bump: an edit that was `invalid_argument` is
accepted now, which a client built for the older minor would not expect, so a client can
tell from the version whether a server takes a keyframe camera on a 2D scene. A Move that
faded may glide now. The schema's shapes and the collection file are unchanged; only doc
comments change.

<a id="consequences"></a>
## Consequences
- A 2D scene can push in, tilt or flip a card into 3D on one keyframe and come back with a
  Move, without making the whole family 3D.
- The UI's Own camera switch on a 2D scene's keyframe starts from the framing camera.
- A 2D side with tilted items still fades. Making them glide would need the 2D draw
  to project them about the canvas's centre, which would change how 2D scenes look.
