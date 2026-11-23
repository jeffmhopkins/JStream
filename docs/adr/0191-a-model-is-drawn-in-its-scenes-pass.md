<!-- @doc
id = "ADR-0191"
title = "A model is drawn in its scene's pass when nothing reads its layer"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0191: A model is drawn in its scene's pass when nothing reads its layer

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0107](0107-3d-models.md) decision 6 draws every model item into a transparent layer
the size of the canvas, and its scene's pass then blends that layer like any item. The
layer is what a source's filters and an item's effects run over, and what opacity and a
blend mode are applied to, so one path serves every case.

It also means every model costs a full-canvas half-float layer and a full-canvas blend,
whatever it covers. A scene built from many models pays that once per model, each frame:
at 1080p a layer is 16.6 MB that is cleared, written and read back through a sampler, and
the cost grows with the number of models, not with what they cover. Most models in such a
scene have no effects and are placed fully opaque in the normal blend, so nothing reads
their layer except the blend that copies it into the pass.

Two things constrain a cheaper path:
- **One submit.** The compositor records a pass into one command encoder and submits it
  once, and what a frame stages is attached to that submit. A model drawn inside a pass
  has to be recorded into the same encoder, so it can't write a uniform buffer between
  two draws of the pass: its frame uniform and its materials' values must be settled
  before the pass is recorded.
- **Depth.** Decision 7 has a model's layer write the model's depth into the scene's
  depth target, and [ADR-0143](0143-scene-lights-and-depth-aware-scene-filters.md)
  decision 4 draws the model's depth into the scene's view depth. Both read the model's
  own depth layer, drawn beside its colour, not its colour layer.

This is `agent` tier: it changes how the renderer draws, with no setting, no protocol
field and no picture a scene could tell apart. It amends ADR-0107 decision 6, which stays
the rule for every model the decisions below leave to its layer.

<a id="decision"></a>
## Decision
**1. A model draw can be made ready.** `Compositor::prepare_model` takes what
`draw_model` takes and returns a `PreparedModel`: the same draw with everything the
frame decides settled, in buffers of its own (its frame uniform, the order of its
blended primitives, the values its materials are driven to, the textures and maps it
binds, and the layers its surface is drawn into when its scene reads a surface buffer,
[ADR-0170](0170-generic-reflections-and-3d-rendering-switches.md) decision 4). It draws
nothing. A draw made ready is then recorded one of two ways:
- **In a pass.** A scene's `Item` may carry it (`Item::model`). The pass shades the model
  straight into its target at the item's place in the order, through the same pipelines,
  with the model's own depth test in the compositor's private depth buffer, cleared for
  it, exactly as its layer was drawn. Nothing else in the pass tests against that
  buffer.
- **In its layer.** `Compositor::draw_prepared` draws it into a layer, which holds what
  `draw_model` would have drawn.

`prepare_model` returns nothing for a draw that needs its layer whatever places it: a
camera with depth of field ([ADR-0165](0165-depth-of-field-on-models.md)), which blurs
the model's layer by its depth, and, on a compositor that mixes encoded colour
([ADR-0126](0126-one-set-of-blend-modes-and-a-blending-space.md)), a model with a
blended primitive.

A compositor that mixes encoded colour is what a new collection has, so it is the case
that matters most. Its pass holds sRGB-encoded colour while its items draw, and a
model's fragments are written into it encoded, as the pass's own draws are: an opaque
fragment replaces what is behind it, so the encoded fragment is exactly what its
layer's texel, encoded and blended, would have left. A blended primitive is different.
Inside a layer its fragments mix with the model's own in linear light, and only the
result is encoded; in the pass they would mix in encoded values. So a model with one
keeps its layer there.

**2. What a pass refuses.** An item that carries a draw must place a layer the size of
the canvas, fully opaque, in the normal blend, and no item before it in the pass may
carry a draw of the same uploaded model, since one submit can drive an upload's
materials only one way. Anything else is `RenderError::Invalid` before GPU work. Opacity
and the other blend modes apply to a model's whole picture, not to each of its
fragments, so they need the layer.

**3. The picture is the same.** A model shaded in its pass and one blended from its layer
show the same pixels to within the rounding of one blend: a fully opaque normal blend of
a premultiplied layer writes `source + destination × (1 − source alpha)`, which is what
the model's own pipelines write into the pass. A model moves from one path to the other
without a visible change, so which path draws it is never part of what a scene means.

**4. Depth is unchanged.** A model's depth is still drawn into its own depth layer when
its box is drawn, and the pass's item still carries that layer (`Item::depth`): the pass
writes it into the scene's depth target at the model's place in the order, and the view
depth takes it, as before. A model is not tested against the scene's depth target, as
its layer never was: sharing that target would hide a model behind an earlier one where
its layer would have drawn over it, and the model would change when it gained an
effect.

**5. The engine chooses per model, each frame.** When it draws a model's box it makes
the draw ready instead of drawing the layer, unless:
- a chain reads the layer: the item's effects, or its source's video filters
  (ADR-0107 decision 6);
- a transition is running, whose tracks place layers;
- its layer has fewer pixels than the canvas
  ([ADR-0197](0197-3d-models-draw-with-fewer-pixels-when-the-gpu-is-short-of-time.md)):
  a pass shades at the canvas's pixels, so the smaller layer is what saves the time;
- `prepare_model` returns nothing (decision 1);
- more than one pass showed the model in the frame before.

Each pass that draws a scene's colour then settles every model made ready. The first
pass of the frame to show a model carries it, when decision 2 allows. Any other pass,
and any item decision 2 refuses, has the draw recorded into the model's layer first,
once a frame, and places the layer as ADR-0107 does. So a model is shaded once a frame
however many passes show it: a scene two mixes draw, or one a filter reads, shades it in
the first pass and blends its layer in the rest for one frame, and draws its layer up
front from the next.

**6. A model's layer is still made.** The layer stays allocated while the model is
shown, though a frame that carries the model draws nothing into it: the passes that
draw a model again through another camera (a floor's reflection, a reflection probe's
capture) find its item by that layer, and a model that needs its layer the next frame
has it. A layer that held a picture is emptied in the first frame that leaves it
undrawn, so nothing that reads it shows the model where it was.

**7. What is not changed.** The model's source filters and item effects, opacity, blend
modes, depth of field, sorting with cards, picking, shadows, the view depth, the surface
buffer, floor reflections and probes all draw as they did. A scene with no model draws
exactly as before.

<a id="consequences"></a>
## Consequences
- A model with nothing over its layer costs its own fragments, not two passes over the
  canvas: no layer clear, no layer write beyond what it covers, and no full-canvas
  blend.
- The layer's memory is still held (decision 6). Letting a carried model's layer go is
  possible later; it needs the passes that find a model by its layer to find it another
  way.
- A model's shading is recorded in its scene's pass, so a profile counts its GPU time
  under the scene's pass, not under the item. Its depth and the making ready still count
  under the item.
- A model is carried only at full size. When the engine lowers a model's pixels
  (ADR-0197) a plain model goes from no layer to a smaller one, which costs a small
  model a little more than it saves and a large one much less. The step that lowers
  the pixels was tuned with every model in a layer; it is worth measuring again with
  this in place.
- Where encoded colour is mixed, a model with a see-through material keeps the cost of
  its layer. Drawing such a model's blended primitives into a small layer of their own
  would lift that.
- Two paths draw a model, and decision 3 is what keeps them one picture. The render
  crate tests them against each other for lit, unlit, blended, camera and surface
  draws; a new feature of the model's draw has to hold in both.
- A model's first frame in two passes shades it twice (decision 5), once in a pass and
  once into its layer. It is one frame, and the picture is the same.
- Models of one upload shown by several items of a pass take the layer path for all but
  the first (decision 2). Giving each draw its own material buffers would lift that.

<!-- Once accepted, only `status` and `superseded_by` may change (R-DOC-009), and the
     generated `adr-status` banner, which only `cargo xtask docs generate` writes (ADR-0038).
     A changed decision is a new ADR that supersedes this one. Not a tracked doc. -->
