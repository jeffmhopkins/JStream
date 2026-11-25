<!-- @doc
id = "ADR-0245"
title = "A 3D extra that runs too long holds the extras at their cheapest"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0245: A 3D extra that runs too long holds the extras at their cheapest

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
The GPU-time guard of #349 times each effect pack's passes on the program stage and
quarantines a pack kind whose pass runs over half the frame budget for 30 ticks
running, before the OS takes the GPU for hung and resets it (Windows' TDR, about 2 s;
[ADR-0087](0087-effect-quarantine-after-gpu-device-loss.md)). The built-in filters were
not timed, on the reasoning that the engine's own shaders are bounded.

The built-in 3D extras are bounded per pixel, but not per frame: ambient occlusion,
volumetric fog and screen-space reflections each do work that grows with the canvas,
their quality settings and how much of the frame the 3D scene fills. On a weak or busy
GPU, at a high canvas size and their highest settings, one of them can run long enough
for every frame to be late, and in the worst case long enough for a reset. The
resolution governor reads the whole frame's time and draws models with fewer pixels
([ADR-0197](0197-3d-models-draw-with-fewer-pixels-when-the-gpu-is-short-of-time.md)),
and steps into economy first
([ADR-0243](0243-the-gpu-short-of-time-draws-3d-extras-cheaper-before-fewer-pixels.md)),
but it steps back out once frames have room, and never acts at a size the rendering
switches fix. A built-in kind is part of the engine, so quarantining it as a pack is
would be wrong: a person can't re-enable it with `effect.enable`, and it isn't
untrusted code.

<a id="decision"></a>
## Decision
1. **The 3D extras are timed.** `ambient_occlusion`, `volumetric_fog` and
   `screen_space_reflections` time their passes under their own kind, as a pack's are
   under `<pack>/<effect>`, and the same guard counts them by the same rule: over half
   the frame budget in 30 ticks running.
2. **One that runs too long holds economy.** The render thread takes a built-in kind
   the guard reports (a kind ID with no `/`), and the governor holds economy from then
   on: models with one sample a pixel, ambient occlusion and volumetric fog at their
   lowest quality, whatever the GPU's load and even at a size the rendering switches
   fix. A warning names the kind once.
3. **Until the program stage restarts.** The hold lives with the stage's governor, so
   it ends when the stage does; nothing is saved, and the collection's settings are
   untouched.
4. **Packs are unchanged.** A pack kind that runs too long is still quarantined
   (ADR-0087), and the engine never sees a built-in kind's report.

<a id="consequences"></a>
## Consequences
- A GPU that can't keep up with the 3D extras' settings gets their cheapest form for
  the rest of the stage's run, rather than late frames or a reset; the size ladder
  still drops pixels after that if frames are still long.
- Screen-space reflections have no cheaper form: one that runs too long holds the
  others' economy, which frees GPU time for it, but is not itself made cheaper. A
  device loss is still the last guard.
- Three more spans are timed, on the program stage only and only where the GPU has
  timestamp queries, out of the guard's 32 slots.
- A held economy is not reported over the protocol; the log says it.
