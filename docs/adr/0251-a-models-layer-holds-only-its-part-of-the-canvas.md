<!-- @doc
id = "ADR-0251"
title = "A model's layer holds only its part of the canvas"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0251: A model's layer holds only its part of the canvas

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0107](0107-3d-models.md) decision 6 draws each item that shows a model into a
layer of its own, the canvas's size, which a scene places flat over the canvas. With
anti-aliasing ([ADR-0242](0242-3d-models-are-drawn-multisampled.md))
that is a multisampled colour and depth target of the canvas's size to clear and
resolve for every model item, however small it is seen.
[ADR-0197](0197-3d-models-draw-with-fewer-pixels-when-the-gpu-is-short-of-time.md) can
draw it with fewer pixels, but only by the same share for all of them. On an integrated
GPU (Radeon 780M) at 1080p30, nine small screen quads, each its own model item, cost
1.7 to 2.5 ms each, about 20 ms of a 33 ms frame.

The renderer already knows the box a model can draw into, from its bounds' corners as
the camera sees them (#1070), and draws and places only there.

<a id="decision"></a>
## Decision
1. **A part of the canvas.** On the program stage and the preview (the caches that draw
   uploads small, ADR-0249), a model item's layer, and its view depth beside it, hold
   only the part of the canvas its box is in: each side the least of a few steps (64,
   96, 128, 192, 256, 384 and so on, by halves of a power of two) that holds the box,
   placed over the box and kept inside the canvas. ADR-0197's share still applies to
   that part's pixels. Where the part would be half the canvas or more, the layer holds
   all of it as before.
2. **Drawn as the whole canvas sees it.** The model is drawn through the camera of the
   whole canvas, its clip space cut to that part, so each pixel of the part is the
   pixel the whole canvas's layer would hold. `Placement::Layer` places the layer over
   its part (`Image::window`), and the view depth beside it is placed the same way.
3. **Where it doesn't apply.** The layer holds the whole canvas when its box can't be
   told (a rigged model, whose pose can take it past its bounds, or a corner of its
   bounds at or behind the camera), when filters run over it (its source's video
   filters or the item's effects, which work over the whole canvas's layer), when it
   draws a surface for its scene's screen-space reflections, when its camera has depth
   of field, and in a snapshot, still, frame sheet or probe.
4. **Targets shared.** The multisampled targets and depth buffers of the last four sizes
   are kept, not two, so the models of a scene, whose parts come in few sizes, draw
   into kept targets.

<a id="consequences"></a>
## Consequences
- **A small model costs its part.** A model seen at 100 pixels is drawn into a 128-pixel
  layer: its multisampled targets are a hundredth of the canvas's or less.
- **The same picture.** The model's pixels and depth are the ones the whole canvas's
  layer holds. A shadow's soft edge, whose sampling turns by the pixel, turns by the
  part's pixel instead.
- **A moving model may make a new layer.** A model whose box crosses a step is given a
  layer of the next size, as a layer drawn small is (ADR-0249).
- **More targets kept.** Four sizes of multisampled targets and depth buffers stay
  allocated in place of two.
