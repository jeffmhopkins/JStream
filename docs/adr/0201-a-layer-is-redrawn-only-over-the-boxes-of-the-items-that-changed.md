<!-- @doc
id = "ADR-0201"
title = "A layer is redrawn only over the boxes of the items that changed"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0201: A layer is redrawn only over the boxes of the items that changed

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0194](0194-a-layer-is-drawn-again-only-when-what-it-draws-has-changed.md) leaves
a nested scene's layer as it is when nothing the scene draws has changed. It is all or
nothing: a scene of many still items with a few that change every frame, such as a
grid of cells in which a meter or a clock moves, is still drawn whole each frame, every
still item with it.

ADR-0194 already keeps, on each layer, what was last drawn into it: the vertices, and
per draw its image, that image's count of writes, its mode and its mip chain. That is
enough to tell which draws differ, not only whether any does.

This ADR amends ADR-0194 by adding to it. Its decisions 1 to 5 stand as written, and
the decisions here apply only when its decision 1 found the layer not to hold the draw
it is asked for.

<a id="decision"></a>
## Decision
1. **A layer in which a few items changed is drawn again only over their boxes.** When
   the layer still holds its last draw, over a transparent background in the same
   space, and the scene has the same number of draws, each draw is compared with the
   one last drawn in its place. One that differs (other vertices, another image or
   mode, or an image whose texels changed) gives two boxes: the texels it could touch
   then and the ones it can touch now. Boxes that share a texel are joined, so no
   texel is in two. Each box is made transparent, every draw that reaches it is drawn
   again clipped to it, in order, and in the browser-style space
   ([ADR-0126](0126-one-set-of-blend-modes-and-a-blending-space.md)) the box is
   decoded. The rest of the layer is not touched.
2. **The whole layer is drawn instead** when a draw was added or removed, the
   background is not transparent, a draw reads its backdrop, the scene has a model's
   depth, there are more than 8 boxes, or the boxes cover more than half the layer.
3. **The layer is marked as changed, as a whole.** Mip chains and the scenes that
   place it follow as they do after a whole redraw.

No setting, switch, protocol message or file changes. The pixels are those a whole
redraw would give.

<a id="consequences"></a>
## Consequences
- **A scene with a few moving parts costs about what those parts cover.** A grid of
  cells in which a meter or a clock changes each frame redraws those boxes, not the
  grid. Every draw that reaches a box is drawn in it, so a box under many stacked
  items costs each of them there.
- **A parent redraws all of a changed layer it places.** By decision 3 the scene that
  places the layer draws the layer's whole content box again, not only the boxes.
  Passing the boxes up is a later step.
- **The same pixels.** Inside a box the draws, their order and their blending are those
  of a whole redraw, and outside it the layer holds what the last one left, so the
  result is bit for bit a whole redraw's on the same GPU.
- **Memory.** Each kept draw also keeps the box it could touch, four numbers per draw.
- **Risk.** A draw whose box did not bound every texel it can touch would leave stale
  texels outside it. The box is the one the compositor already trusts to bound what a
  browser-style pass decodes and what a parent draws of a layer.
