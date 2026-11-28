<!-- @doc
id = "ADR-0259"
title = "Faded and additive models in the shared pass are transparent"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0259: Faded and additive models in the shared pass are transparent

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0257](0257-models-in-one-scene-are-one-depth.md) draws the models of a 3D scene that
can share it into the scene's colour and depth in one pass. Its decision 2 has every
opaque or masked primitive write that depth, whichever blend the item uses, and its
decision 3 applies the item's opacity in the shading. Together they are wrong for an item
that is not fully drawn:

- An item at opacity 0.5 in front of another model writes depth at full strength, so the
  model behind it fails the depth test there and is never shaded. The pixel shows the sky
  through half of the faded item, not the model behind it. At opacity 0.01 an invisible
  item cuts a hole in every model behind it.
- An additive item with opaque materials does the same: its glow is added over the sky
  where a hull should be under it.
- An item whose blend is `screen`, `multiply` or a mode that reads the picture under it is
  drawn source-over, silently, because the pass has no picture of what is under each
  fragment.

ADR-0257's view depth also lost its half-opacity limit for a model that keeps its own
layer. That layer's depth is what the cards behind it are tested against, so a canopy at
opacity 0.15 hid a card behind it. Decision 1 of ADR-0257 said
the private layer keeps that limit; the shader no longer did.

ADR-0257 also left a frame that composites straight to the output taking a new layer of
the canvas's size every frame whenever the program scene had a model and a camera, even
one model, which does not share.

R-DOC-009 lets an accepted record change only by a new one, so this record restates
ADR-0257 with those points changed.

<a id="decision"></a>
## Decision
**Models in a 3D scene that can share it are drawn into the scene's colour and depth, in
one pass, not into a layer each, and only an item that is fully drawn with the `normal`
blend writes that depth.** [ADR-0257](0257-models-in-one-scene-are-one-depth.md) is
superseded. There is still no scene or camera option.

1. **Who shares.** A model seen through the scene camera, with no item filters, no filters
   on its source, no depth of field (`blur_px` above 0, or a fixed aperture), no
   screen-space reflection of its own surface, and the `normal` or `additive` blend, and
   only when another model in that scene shares too. One model keeps its layer. The
   others keep the private layer and the composite test of
   [ADR-0256](0256-models-in-one-scene-share-a-depth.md), including its half-opacity
   limit: a fragment under half opacity is not in that layer's depth. A filter, a blur, a
   reflection or a blend that reads the picture under the model needs an image of that
   model alone. A transition that composites the layer still draws it; the scene pass
   draws the shared geometry as well.
2. **Opaque, then transparent.** For an item at full opacity with the `normal` blend,
   every opaque or masked primitive is drawn first, depth tested and written (`Greater`
   on the reversed `Depth32Float`), whichever other materials that model has, and shaded
   where that depth is equal. A canopy does not stop the hull writing depth. Then every
   transparent primitive is drawn far to near by its centre, across models, depth tested
   and not written: the blended primitives of every item, and every primitive of an item
   under full opacity or with the `additive` blend. There is no alpha cutoff: a fragment
   at opacity 0.2 is tested at the rasterizer's depth.
3. **The item stays the item.** Opacity (colour by the whole value, alpha by at most 1),
   the blend (`additive` adds and keeps the coverage under it; `normal` is source-over)
   and the material drives are applied in that pass, per model.
4. **Order around them.** The sky and any item with no camera that sorts before the first
   camera item are drawn first, untested. The shared pass loads that colour and clears
   the depth. Cards and the private model layers follow, testing the same depth; a card
   does not write it. An item with no camera drawn after that is untested, so it stays
   on the screen. Flat cards in the scene use this depth, as they did.
5. **View depth.** After the cards' view depth is drawn, the scene depth is max-blended
   into it as `1 / w`. Opaque models are there without being drawn again. A transparent
   fragment wrote no scene depth, so fog does not see it, as a private layer's view depth
   ignores one under half opacity. A private layer's depth is still copied as before.
6. **Samples.** The shared pass draws one sample per pixel, because the depth the cards
   test is the scene's single-sample target. A private layer stays at the samples
   [ADR-0242](0242-3d-models-are-drawn-multisampled.md) set. A slanted edge in the shared
   pass is a hard step.
7. **Windowed layers.** [ADR-0251](0251-a-models-layer-holds-only-its-part-of-the-canvas.md)
   sizes a private layer to the model's box. A model alone keeps that window. The shared
   pass, used when two or more models share, draws into the scene target, which is the
   canvas, so the window does not apply to those models. Their texture slots
   ([ADR-0250](0250-a-model-shows-a-live-texture-on-each-part-it-names.md)) still apply
   in the shared pass.
8. **Program's layer.** A program scene drawn at the output's size composites straight to
   the output, as before, unless two or more of its models share. Then it is drawn into
   program's layer first, which the frame claims with the others and keeps between frames.

A 2D scene is unchanged.

<a id="consequences"></a>
## Consequences
- **Easier:** a faint additive plume in front of a hull shows, and the part behind the
  hull is hidden, at opacity 0.2 as at 1. A faded or additive model never hides what is
  behind it. Two transparent models interleave by primitive. Opaque models occlude per
  pixel in either centre order. One depth clear serves every model that shares, instead
  of a layer and a depth each.
- **Harder:** a faded model's own surfaces do not hide each other while it fades: its
  primitives are sorted by their centres, so a part of it behind another part can show
  through until it is fully drawn again. The shared pass is one sample, so a slanted edge
  is harsher than a 4x layer. It is several submits (one frame uniform), grouped so a run
  of one model's transparent primitives is one submit. A model with filters, depth of
  field, a screen-space reflection or a blend that reads under it is still a layer, and a
  fragment of that layer under half opacity is still far. Fog does not see a transparent
  fragment of a shared model. An item with no camera that used to sit between two models
  is drawn with the backdrop or after every shared model, not between their fragments. A
  card drawn after the models is drawn over their transparent fragments, wherever its
  centre sorts. While the GPU is short of time, a shared model is still drawn at the
  scene's resolution; the smaller layer
  [ADR-0197](0197-3d-models-draw-with-fewer-pixels-when-the-gpu-is-short-of-time.md)
  describes is the private one.
- **Not decided here:** a multisampled scene target whose colour and depth resolve into
  the single-sample ones the cards and the filters read; sorting transparent fragments
  rather than primitives; interleaving cards with the transparent primitives; a faded
  model drawn opaque into a layer of its own and faded as one picture.
- **Follow-up:** none. The pass, the choice of which models share, and the tests land
  with this record.

<!-- Once accepted, only `status` and `superseded_by` may change (R-DOC-009), and the
     generated `adr-status` banner, which only `cargo xtask docs generate` writes (ADR-0038).
     A changed decision is a new ADR that supersedes this one. Not a tracked doc. -->
