<!-- @doc
id = "ADR-0257"
title = "Models in one 3D scene are one depth"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0257: Models in one 3D scene are one depth

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0256](0256-models-in-one-scene-share-a-depth.md) composites each model's layer with a
per-texel depth test. The depth it tests is the model's view depth, and that image drops
a fragment under half opacity. A faint plume in front of a hull is then at the far plane
and the hull hides it. The layers are also ordered as whole models, so two transparent
models cannot interleave: every fragment of one is drawn before any fragment of the other.

A 3D scene is one space. The models in it should be drawn the way a scene's own geometry
is: one colour target, one depth, opaque surfaces first, then transparent ones far to near
at their real depth.

<a id="decision"></a>
## Decision
**Models in a 3D scene that can share it are drawn into the scene's colour and depth, in
one pass, not into a layer each.** [ADR-0256](0256-models-in-one-scene-share-a-depth.md) is
superseded for that path. There is still no scene or camera option.

1. **Who shares.** A model seen through the scene camera, with no item filters, no filters
   on its source, no depth of field (`blur_px` above 0, or a fixed aperture) and no
   screen-space reflection of its own surface. The others keep the private layer and the
   composite test of ADR-0256, including its half-opacity limit: a filter, a blur or a
   reflection needs an image of that model alone. A transition that composites the layer
   still draws it; the scene pass draws the shared geometry as well.
2. **Opaque, then transparent.** Every opaque primitive of every full-opacity `normal`
   model is drawn first, depth tested and written (`Greater` on the reversed
   `Depth32Float`), and shaded where that depth is equal, so a pixel is shaded once at
   the nearest surface of any of them. Then every transparent primitive — a blended
   material, an item opacity below 1, or a blend other than `normal` — is drawn far to
   near by the primitive's centre, across models, depth tested and not written. The
   fragment's depth is the rasterizer's, so a fragment under half opacity is tested where
   it is.
3. **The item stays the item.** Opacity (colour by the whole value, alpha held to 1), the
   blend (`additive` adds; anything else is source-over) and the material drives are
   applied in that pass, per model.
4. **Order around them.** The sky and any item with no camera that sorts before the first
   camera item are drawn first, untested. The shared pass loads that colour and clears
   the depth. Cards and the private model layers follow, testing the same depth; a card
   does not write it. An item with no camera drawn after that is untested, so it stays
   on the screen. Flat cards in the scene use this depth, as they did.
5. **View depth.** After the cards' view depth is drawn, the scene depth is max-blended
   into it as `1 / w`. Opaque models are there without being drawn again. A transparent
   fragment wrote no scene depth, so fog does not see it; the view depth already ignored
   one under half opacity. A private layer's depth is still copied as before.
6. **Samples.** The shared pass draws one sample per pixel, because the depth the cards
   test is the scene's single-sample target. A private layer stays at the samples
   [ADR-0242](0242-3d-models-are-drawn-multisampled.md) set. A slanted edge in the shared
   pass is a hard step.
7. **Windowed layers.** A private layer may be only the pixels the model covers. The
   shared pass draws into the scene target, which is the canvas, so that window does not
   apply to a model that shares.

A 2D scene is unchanged.

<a id="consequences"></a>
## Consequences
- **Easier:** a faint additive plume in front of a hull shows, and the part behind the
  hull is hidden, at opacity 0.2 as at 1. Two transparent models interleave by primitive.
  Opaque models occlude per pixel in either centre order. One depth clear serves every
  model that shares, instead of a layer and a depth each.
- **Harder:** the shared pass is one sample, so a slanted edge is harsher than a 4x
  layer. It is several submits (one frame uniform), grouped so a run of one model's
  transparent primitives is one submit. A model with filters, depth of field or a
  screen-space reflection is still a layer, and a fragment of that layer under half
  opacity is still far. Fog does not see a transparent fragment of a shared model.
  An item with no camera that used to sit between two models is drawn with the backdrop
  or after every shared model, not between their fragments. While the GPU is short of
  time, a shared model is still drawn at the scene's resolution; the smaller layer
  [ADR-0197](0197-3d-models-draw-with-fewer-pixels-when-the-gpu-is-short-of-time.md)
  describes is the private one.
- **Not decided here:** a multisampled scene target whose colour and depth resolve into
  the single-sample ones the cards and the filters read; sorting transparent fragments
  rather than primitives; interleaving cards with the transparent primitives.
- **Follow-up:** none. The pass, the choice of which models share, and the tests land
  with this record.

<!-- Once accepted, only `status` and `superseded_by` may change (R-DOC-009), and the
     generated `adr-status` banner, which only `cargo xtask docs generate` writes (ADR-0038).
     A changed decision is a new ADR that supersedes this one. Not a tracked doc. -->
