<!-- @doc
id = "ADR-0185"
title = "How reflection probes and screen-space reflections landed, off by default"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0185: How reflection probes and screen-space reflections landed, off by default

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0170](0170-generic-reflections-and-3d-rendering-switches.md) decided on reflection
probes (decision 3) and screen-space reflections (decision 4), each with a switch in the
app-wide 3D rendering setting (decision 1). It says a switch a later version adds is on
when an older file or server leaves it out.

Both have now been built, and some of the details differ from ADR-0170's text. On an
integrated GPU, a 3D scene drawn at 1080p30 went from about 21 ms of GPU time a frame to
about 39 ms with screen-space reflections on. Most of that was each model being shaded a
second time for the surface buffer. Had the switch been on by default, existing
reflective scenes would have dropped below their frame rate as soon as they were
upgraded.

<a id="decision"></a>
## Decision
ADR-0170's decisions stand, with these changes.

**Screen-space reflections are off until turned on.** `screen_space_reflections` defaults
to off (`RenderFeatures::DEFAULT`), and a file or server that leaves it out reads it as
off. It is the one exception to ADR-0170's rule that a new switch reads as on. Every other
switch keeps that rule. Its quality, `screen_space_steps`, defaults to the fewest steps,
16 (Low), likewise.

**A model is shaded once for its surface.** In a scene whose chain reads a surface buffer,
each `scene`-lit model whose `reflections` is on writes its surface into three layers of
its own in the same draw as its layer. The buffer then merges them by their `1 / w`. A
model whose layer drew no surface in the frame is drawn into the buffer again, so the
result is the same. A model that doesn't reflect writes no surface at all: where it is
in front of one that does, the view depth is nearer than the surface there, and the
pass leaves the pixel as it is.

**Where the delivery departs from ADR-0170's text:**

- **Probes (decision 3).**
  - A capture is spread over six frames, one face a frame. The faces are resampled into
    the panorama in the frame that draws the last, and surfaces read it from the next
    frame. Until then they read the probe's previous capture. A refresh therefore costs
    a sixth of a capture a frame, not a spike.
  - One probe captures at a time, scene-wide. The slot goes to the due probe that has
    waited longest.
  - The panorama's mip chain is read by a ring of taps at the level the roughness
    spans, as the sky's is.
  - The probe has a binding of its own, beside the sky's.
  - A box's faces fade into the environment over its `fade_px`.
  - Each compositor captures its own probes.
- **Screen-space reflections (decision 4).**
  - The pass is a built-in filter kind that no chain lists. The engine puts it first in
    the chain of a 3D scene whose `reflections` is on, while the switch is on, so it
    runs after the scene is composited and before the scene's own filters.
  - The surface buffer is drawn of the scene's models only. Each model writes its
    surface into three layers of its own in the same draw as its layer, and the buffer
    merges them by their `1 / w`, so no model is shaded twice for it. The surface
    buffer is three layers:
    - the shading normal with `1 / w`;
    - the split-sum BRDF with the roughness;
    - the environment's reflection as the shader added it, which already includes a
      probe's, with a flag for where it may be replaced.
  - A lit card is not drawn into the surface buffer. It reflects its probe and the
    environment only, though it shows in what models reflect.
  - The "quality switch" is a step count of 16, 32 or 64 (`screen_space_steps`).
  - Roughness blurs the hit over a disc of 12 taps of the picture, as wide as the ray
    has spread by the time it gets there, instead of reading a blurred mip. The scene's
    composite has no mip chain at that point.
  - A scene with this pass has a chain, so a transition shows it as one picture, as it
    shows any scene with filters (ADR-0065 §6).

<a id="consequences"></a>
## Consequences
- A scene with `reflections` on looks as it did before until someone turns screen-space
  reflections on in Settings › 3D rendering. The setting's info and the scene dialog's
  tip both say so.
- The pass costs one extra full-screen draw per reflecting model, the march itself, and
  three canvas-sized half-float layers per reflecting model (about 48 MB at 1080p) while
  it runs. Measured on the same integrated GPU after the single shading, it added about
  10, 11 and 13 ms of GPU time a frame at Low, Medium and High; Low held 30 fps.
- Lit cards reflect only their probe and the environment, never what is on screen.
