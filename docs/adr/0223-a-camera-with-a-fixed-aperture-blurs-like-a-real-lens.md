<!-- @doc
id = "ADR-0223"
title = "A camera with a fixed aperture blurs like a real lens as it zooms and focuses"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0223: A camera with a fixed aperture blurs like a real lens as it zooms and focuses

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0108](0108-camera-depth-of-field.md) gives a scene camera a depth of field:
`focus_px`, the distance in focus, and `blur_px`, the blur radius of a point infinitely
far behind it, so a point `D` away blurs by `blur_px · |D − focus_px| / D`. That is the
thin-lens circle of confusion with the aperture's effect folded into `blur_px`, and it
holds `blur_px` fixed whatever the camera's field of view or focus distance.

A real lens doesn't. With a fixed f-number the aperture's diameter grows with the focal
length, and the blur far behind the focus is that diameter times `f / focus`, so it
grows with the square of the focal length and falls as the focus moves away. That is
what makes a dolly zoom read: the camera moves back while it zooms in, the subject keeps
its size, and the background swells and softens behind it. With ADR-0108's model the
background swells but its softness doesn't change. The maintainer asked for the zoom and
the focus distance to change the depth of field like a real camera's, so that such moves
work.

The forces:
- Every existing camera must keep its look; a show's cameras were tuned by eye under
  ADR-0108.
- The blur must stay capped at 64 px, as ADR-0108 decision 2 draws it.
- Autofocus ([ADR-0176](0176-controllers-and-live-data-drive-scene-cameras.md) decision
  5), camera motion, moves between cameras and canvas rescaling already change
  `focus_px`, `fov_deg` and `blur_px`; the new behaviour must follow all of them without
  each needing to know about it.

It is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)): the maintainer asked for the
feature, and it changes no licence, security or privacy posture, compatibility promise
or spending. The protocol gains one optional field. It extends ADR-0108, whose decisions
all still hold for a camera without it.

<a id="decision"></a>
## Decision
1. **An opt-in field.** `SceneCamera` gains `fixed_aperture` (since protocol 0.191,
   default `false`, not written while false). Off, a camera draws exactly as ADR-0108
   says.
2. **`blur_px` is the aperture, measured on the framing camera.** With `fixed_aperture`
   on, `blur_px` is the blur far behind the focus of the framing camera (ADR-0094
   decision 3: 45°, focused at its framing distance, which equals its focal length
   `f₄₅`). A view with focal length `f` (from its field of view and the canvas height)
   focused at `focus_px` blurs far away by
   `blur_px · f² / (f₄₅ · focus_px)`, capped at 64 px, and every point by ADR-0108
   decision 2 with that in place of `blur_px`. Halving the field of view's tangent
   quadruples the blur; focusing twice as far halves it.
3. **Worked out where the view is.** `SceneCamera::view` applies it, after everything
   else has moved the camera: motion, aim, a group's anchor and autofocus all change
   `fov_deg` or `focus_px` first, and the view draws with what they leave. A move
   between cameras interpolates `blur_px`, `fov_deg` and `focus_px` as before and takes
   `fixed_aperture` from the camera it moves to, so a glide between two fixed-aperture
   cameras blurs as a lens moving between them would. Rescaling the canvas scales
   `blur_px` and the positions alike, and the blur scales with them.
4. **Turning it on keeps the look.** The Scene properties dialog's Real lens check,
   under Depth of field, sets `blur_px` to the aperture that draws the blur the camera
   draws now at its field of view and focus, and turning it off sets back the blur the
   aperture draws there, each capped at 64. Turning the depth of field off turns it off.

<a id="consequences"></a>
## Consequences
- A dolly zoom on a fixed-aperture camera softens or sharpens its background as it goes,
  with no keyframed blur.
- A fixed-aperture camera zoomed far in reaches the 64 px cap sooner; the cap is where
  ADR-0108's drawing (taps over the item's mips, a pushed-out polygon) stays cheap, so it
  isn't raised.
- An agent or client that sets `blur_px` on a fixed-aperture camera sets the aperture,
  not the blur seen, which the MCP guidance and the protocol README say.
- Nothing changes for a camera that doesn't turn it on, so no stored scene changes.
