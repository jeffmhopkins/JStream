<!-- @doc
id = "ADR-0218"
title = "Autofocus meets a 3D model at the surface it draws"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0218: Autofocus meets a 3D model at the surface it draws

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0176](0176-controllers-and-live-data-drive-scene-cameras.md) decision 5 measures
autofocus through each item's box, so a model was met at its item's flat rectangle at
`depth_px`, a plane through the middle of the model, and not at the surface the camera
sees. A camera inside or close to a model (an interior, a vehicle, a room) met that
plane in the wrong place, or, once past it, met nothing and held a focus far from the
surface filling the frame. With a depth of field on, close views of a model drew out of
focus. ADR-0176 named this limit and a depth readback as one way past it (#1770).

<a id="decision"></a>
## Decision
1. **A model is met at its surface.** Autofocus carries the camera's viewing axis into
   the model's own space, through the inverse of the map its item places it by (as it
   draws: fit, flips, angles, depth, and a billboard turned to the camera), and meets it
   against the model's triangles. Other items are met at their box, as before.
2. **What counts as surface** is what draws opaque: a triangle of a blended material is
   seen through and passed over; a masked one counts as opaque (its texture isn't
   sampled); a single-sided triangle counts only from its front, which a mirroring
   placement swaps; a double-sided one from either side.
3. **On the CPU, not a readback.** An unposed model is searched through a bounding
   volume hierarchy over its rest-pose triangles, shared by the model's clones, so each
   frame's measure is one ray through it (microseconds). The hierarchy is built on a
   thread of its own the first time the model is measured (tens of milliseconds for a
   model of 150,000 triangles), so the render thread never waits for it. A posed
   (rigged) model is skinned by its palette now and searched triangle by triangle. No
   GPU work or readback is added, and nothing waits on the GPU.
4. A model whose mesh the frame doesn't have yet, or whose hierarchy is still being
   built, is not met, so the focus holds (at first `focus_px`), as for nothing at the
   centre.

<a id="consequences"></a>
## Consequences
- **Easier:** a camera inside or near a model focuses on what fills its frame.
- **Harder:** a model measured by autofocus keeps a copy of its triangles on the CPU
  (about 40 bytes a triangle, with the hierarchy), and for a few frames after its first
  measure it isn't met; a rigged model's measure is linear in its triangles.
- **Unchanged:** items in a 3D group are still passed over, and a masked material's
  cut-out holes still stop the axis.
