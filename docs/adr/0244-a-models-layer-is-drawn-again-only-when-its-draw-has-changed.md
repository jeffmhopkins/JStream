<!-- @doc
id = "ADR-0244"
title = "A model's layer is drawn again only when its draw has changed"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0244: A model's layer is drawn again only when its draw has changed

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A nested scene's layer is left as it is when it still holds the very draw it is asked
for ([ADR-0194](0194-a-layer-is-drawn-again-only-when-what-it-draws-has-changed.md)),
but that decision left models out: a 3D model item's layer is drawn every frame, with
its view depth and, for a model that reflects its scene, its surface layers. That is
usually the costliest pass a 3D scene has, and in a scene whose camera, lights and
models are still, such as a cockpit seen from a fixed seat, it draws the same pixels
every frame.

Everything a model's draw depends on is already known where it is drawn: the model,
the frame uniform built from its placement, camera, lighting, lights, shadow views,
sky, floor and probe, each material's drive, its live texture, and the pictures it
reads (shadow maps, sky, reflection, probe), each of which counts its writes
(`Image::mark_changed`). Shadow maps are already kept while their casters and light
stand still, so they don't move on in a still scene. Only a rig's pose was written
every frame, without saying whether it changed.

<a id="decision"></a>
## Decision
1. **A model's layer is left as it is when it still holds the very draw it is asked
   for.** That is when all of these are as they were at its last draw into that layer:
   the model, by identity, and how many times its pose changed; the frame uniform,
   byte for byte; each material's drive; the live texture's picture with its count of
   writes and the material it replaces; each shadow map, sky and probe picture read,
   with its count of writes, and whether it was read through a mip chain; the sample
   count; and the count of writes of the layer, its view depth and its surface layers,
   so nothing else drew into them since. Drawn with a view depth or surface layers and
   asked without, or the other way, is another draw.
2. **The comparison is exact.** What was drawn is kept beside the model pass, by the
   layer it went into, and compared as it is, not hashed.
3. **A pose counts only when it changes.** `Compositor::pose_model` with the palette
   already written writes nothing and doesn't move the model's pose count.
4. **A kept layer is not marked as changed,** nor its view depth or surface layers, so
   what reads them (mip chains, a nested scene per ADR-0194) sees no change either.
5. **Only a model's layer draw is covered.** A lit card's quad, a surface drawn on its
   own, a depth-only draw and a shadow map are drawn as before; the item's filters run
   every frame as they did.

No setting, switch, protocol message or file changes. The pixels are those a redraw
would give.

<a id="consequences"></a>
## Consequences
- **A still model costs no model pass.** Nothing is recorded or submitted for it; its
  filters, and the scene's passes over its view depth, still run.
- **Anything that moves redraws it.** A camera move, an animation, a light, a live
  texture that repaints, or a drive that changes draws it every frame, as before.
- **Memory.** Each model layer keeps a copy of its last frame uniform, its drives and a
  few words per picture, refilled in place, so a steady frame allocates nothing.
- **Risk is the write count, as in ADR-0194 decision 5.** A write into a picture a
  model reads that does not mark it would now leave the model's layer stale too.
