<!-- @doc
id = "ADR-0211"
title = "A built-in filter's pixel settings count in its input's extent"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0211: A built-in filter's pixel settings count in its input's extent

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Amends [ADR-0197](0197-3d-models-draw-with-fewer-pixels-when-the-gpu-is-short-of-time.md)
decision 2, where an item's effects ran over a smaller model layer at its size. It changes
how built-in effects draw and promises nothing new to packs, so it is agent tier
([ADR-0043](0043-narrow-maintainer-tier.md)).

<a id="context"></a>
## Context
An image can hold more or fewer texels than the size it is placed at, its extent
([ADR-0073](0073-sources-drawn-at-their-shown-size.md)): a source drawn sharp for an item
that shows it enlarged holds more, and a model's layer drawn smaller while the GPU is short
of time (ADR-0197) holds as few as half each way, a size that can change mid-stream.

The built-in filters' descriptors say their `px` settings (a blur's radius, a drop shadow's
offset and blur, an outline's width, a pixelate's block size, a scroll's speed in px/s, and
so on) count in source pixels, which is the extent. Their passes counted them in the input's
texels. So a blur reached twice as far across a model's half-size layer, and stepped when
the size changed, and half as far across a source drawn sharp at twice its size (#1680).
Depth of field on models already scaled its blur and cap by the layer's texels per canvas
pixel ([ADR-0165](0165-depth-of-field-on-models.md), `model/focus.rs`).

Three paths count pixels around a filter. `Pass::reach` says how far past its input's
content a pass draws, in texels, for the bound that cuts its draws (#1070). `filter::outset`
says how far past its input a kind draws at its settings
([ADR-0136](0136-an-effects-outset-follows-its-settings.md)), and core sizes an item's
effects layer and the chain's pad by it ([ADR-0125](0125-item-effects-layer-grows-where-it-is-needed.md)).
`VideoFilter::set_pad` grows the input by texels.

Effect packs' filters and shader sources
([ADR-0093](0093-shader-sources-from-effect-packs.md)) have parameters too, but nothing
tells the engine which of them count in pixels.

<a id="decision"></a>
## Decision
1. **A built-in filter's pixel settings count in its input's extent.** Before a filter
   draws, every `px` and `px/s` parameter of a kind `filter::descriptors` lists is multiplied
   by the input's texels per extent pixel, and the pass reads those settings. A parameter
   along one axis (an ID ending `_x` or `_y`, a scanline's spacing and roll, a colour
   fringe) takes that axis's scale; a length in any direction (a radius, a width) takes the
   geometric mean of the two, which is either when the image is scaled alike both ways. A
   whole-number setting is rounded. A scaled value past the pass's own cap is held there,
   which keeps the pass's cost bounded, so a source drawn with many more texels than its
   extent can draw a very large radius smaller than asked.
2. **What is not scaled.** A border's 9-slice frame corner counts in the frame image's own
   pixels. The depth-aware kinds count in their scene's pixels and depths and draw over its
   canvas-sized composite. Effect packs' filters and shader sources are left as they are:
   their parameters reach their shaders unscaled, and a pack's WGSL sees only texel sizes.
   A pack effect on an image with fewer texels than its extent reaches further across the
   picture, as every filter did before. Giving packs the scale would be a new value in the
   prelude every pack validates against, for a case only a model's smaller layer and an
   enlarged sharp source reach; it can be added later without changing this.
3. **Reach, outset and pad.** A pass's reach counts in texels, from its scaled settings, as
   the bound it feeds does. `filter::outset` counts in extent pixels, as the settings it reads
   do, and core turns it into the item layer's pixels for the pad. The pad counts in texels.
   Only an item's effects are padded, over a layer placed at its own size, whose texels are
   its extent pixels, so the pad and the scaled reach agree and nothing is cut.
4. **The cost.** The scaled settings are worked out only when the input's scale changes, and
   kept: a steady input costs a comparison per filter per frame.

<a id="consequences"></a>
## Consequences
- An effect on a model item covers the same share of the picture whatever size the model's
  layer is drawn at, with no step when that size changes. ADR-0197 decision 2's "at its
  size" now holds only for pack effects.
- A source drawn sharp at more texels than it is shown keeps its filters' look as it is
  enlarged, where before its blur and shadow shrank on the picture.
- A new `px` parameter is scaled unless it is named as counting in something else; a test
  fails until each is one or the other.
- Pack authors' pixel parameters still count in the input's texels; the pack docs say so.
