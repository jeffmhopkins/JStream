<!-- @doc
id = "ADR-0245"
title = "A GPU short of time draws 3D extras cheaper before it draws fewer pixels"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0245: A GPU short of time draws 3D extras cheaper before it draws fewer pixels

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
While the GPU can't keep its frames in time, the program's render thread draws 3D
models with fewer pixels
([ADR-0197](0197-3d-models-draw-with-fewer-pixels-when-the-gpu-is-short-of-time.md)),
in steps from 100 to 50 percent of each side, no lower than the floor the rendering
switches set ([ADR-0241](0241-a-floor-for-the-model-size-and-any-whole-percentage.md)).
That was the only thing it gave up. Every pixel left is still drawn with everything the
rendering switches ask for: 4 samples a pixel by default
([ADR-0242](0242-3d-models-are-drawn-multisampled.md)), and a scene's ambient occlusion
and volumetric fog ([ADR-0151](0151-the-rest-of-richer-3d-scenes.md)) at the quality
each filter's settings name.

Those extras are among a 3D scene's costliest passes. Volumetric fog at its default
quality marches 48 steps a ray; ambient occlusion at High takes 16 samples a pixel
against Low's 8; and multisampling costs four times the model layer's colour and depth
memory traffic. Each is a refinement the eye misses far less than a softer picture:
edges smoothed by 1 sample instead of 4, fog marched with 24 steps instead of 48, and
occlusion with 8 samples, against a model drawn at 85 percent and enlarged.

<a id="decision"></a>
## Decision
1. **Economy comes first.** Before its first step down in size, the program's governor
   takes one step into economy at full size: from then on, while it lasts, a frame's 3D
   models are drawn with one sample a pixel, its `ambient_occlusion` filters take Low's
   8 samples, and its `volumetric_fog` filters march as quality Low does (24 steps, at
   half size), whatever their settings ask. The filters learn it from the scene's view
   depth (`ViewDepth::economy`), as they learn the camera.
2. **It is a rung of the same ladder.** Economy is full size again, one rung below full
   size, and every smaller size stays in economy: stepping down goes full, economy,
   85, 75, 67, 50; stepping up undoes them in that order, economy last, by the same
   rules (three long frames or a long frame while behind to step down; the wait, which
   doubles when a step up is undone, to step up). What the step into economy saved is
   learned from the frame timed after it, as a step down in size is, taken to be at
   most half of the frame until one is timed.
3. **The floor holds the size, not the extras.** A floor of 100 keeps models at full
   size but still lets the governor take its economy step. A size the rendering
   switches fix (`model_resolution`) is drawn with everything they ask for, never in
   economy.
4. **Only the program stage's governor does it.** A snapshot, a preview and a probe's
   capture draw as before; nothing in the protocol changes, and the log says when the
   extras go cheaper and back.

<a id="consequences"></a>
## Consequences
- A scene short of time first loses its edge smoothing and some fog and occlusion
  detail, then its models' pixels; one that only just misses its frames keeps its full
  size.
- A scene whose cost is all in its models' shading gains little from economy, and steps
  down in size one rung later than before: the frames between take the governor's
  usual three long frames, or one long frame while behind.
- Clients can't yet see that economy is on: the log says so, and
  `profile.get`'s `model_resolution` still shows the size.
- A new extra worth giving up under load joins economy by reading
  `ViewDepth::economy` (a depth-aware filter) or `Chains::economy` (the engine).
