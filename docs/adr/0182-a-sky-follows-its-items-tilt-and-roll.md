<!-- @doc
id = "ADR-0182"
title = "An environment light's sky follows its item's tilt and rotation as well as its turn"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0182: An environment light's sky follows its item's tilt and rotation as well as its turn

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
An environment light's sky ([ADR-0151](0151-the-rest-of-richer-3d-scenes.md) decision 9),
and the backdrop and stars drawn from it
([ADR-0167](0167-a-sky-backdrop-for-3d-scenes.md)), are spun about the vertical by the
light's item's `turn_deg`, and by nothing else. The item's `tilt_deg` and `rotation_deg`
are stored, keyframed and driven by motion modifiers as on any item, and the sky ignores
them.

A set that is a vehicle needs all three: a ship that pitches or rolls shows its sky
pitching or rolling past the windows, and a controller or a DAW's data should be able
to drive each one, as it already drives the turn through a `turn` modifier. Every other
3D item already turns by all three angles, in ADR-0022's order, as one rotation
([ADR-0166](0166-moves-curve-through-keyframes-turn-as-one-and-may-orbit.md) decision 1).

It is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)): it changes no protocol, licence,
security or privacy posture, compatibility promise or spending. It extends ADR-0151
decision 9 and ADR-0167, whose sky a turn alone still orients exactly as before.

<a id="decision"></a>
## Decision
**1. The sky is oriented as its item is.** An environment light's sky (its gradient,
its picture, its nebulae, band and stars, and the direction of its warp's travel) turns
by its item's `rotation_deg`, `turn_deg` and `tilt_deg` together, in ADR-0022's order, as
a model placed by that item would. A direction in the scene reads the sky along that
direction taken back through the item's rotation. The backdrop, a scene-lit model's
light and reflections, and a reflection probe all read it so.

**2. A turn alone is unchanged.** With tilt and rotation at 0 the sky reads exactly as
before, so every scene, keyframe and golden image that only turns it stays as it is.

**3. In the renderer, the camera's axes carry it.** The sky and model shaders already map
a pixel's ray through the camera's right, down and forward axes. The renderer takes
those axes back through the sky's rotation on the CPU and passes no turn of its own, so
the uniforms and the cost per pixel stay as they were.

<a id="consequences"></a>
## Consequences
- **Easier:** a ship can pitch and roll against its sky, from keyframes, a move or any
  motion modifier on the light's `tilt` and `rotation`, with its lighting and
  reflections following.
- **Harder:** a scene that tilted or rotated an environment light's item before, which
  had no effect, now orients its sky by them.
- **Must now do:** `jstream-render`'s `Environment` gains `tilt_deg` and `roll_deg` and
  `sky_axes`; `jstream-core` fills them from the light's placement; a golden test
  tilts and rolls a sky, and a unit test checks the axes against ADR-0022's rotation.
