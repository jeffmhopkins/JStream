<!-- @doc
id = "ADR-0165"
title = "Depth of field on 3D models"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0165: Depth of field on 3D models

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Extends [ADR-0108](0108-camera-depth-of-field.md) (depth of field for scene cameras) to the
models of [ADR-0107](0107-3d-models.md). It changes nothing for a card, a 2D scene, or a
camera without depth of field.

<a id="context"></a>
## Context
ADR-0108 blurs each card as it draws: the card's fragment knows its `w`, so the shader
averages the card's mips over a disc of radius `blur_px · |D − focus_px| / D`. A model
doesn't take that path. ADR-0107 decision 6 draws it with its own depth test into a
canvas-sized layer, and the layer is then placed flat (`Placement::Layer`), with no blur.
So a model stays sharp however far it is from the focus. A test render on `main` showed it:
a cube and a card at the same depth, focused four times farther away with `blur_px` 16,
drew the card soft and the cube with a one-pixel edge.

The maintainer asked for depth of field to apply to 3D models. A scene built from models
needs it most where a camera pulls focus past a near model to something far behind it.

What exists to build on:
- **The model's layer and depth.** The model is drawn alone into a canvas-sized layer, and
  `Compositor::draw_model_depth` already draws its view depth (`1 / w = f / D` in a
  half-float layer, 0 where it isn't) for shadow maps and the scene view
  ([ADR-0143](0143-scene-lights-and-depth-aware-scene-filters.md) decision 4).
- **The filter harness.** Multi-pass filters draw fullscreen passes into scratch layers
  through one runtime, and depth-aware filters already read a `ViewDepth` beside their
  input.
- **The circle of confusion** is one shared function, `CameraView::blur_at`.

The forces:
- A model has depth inside itself: its near parts may be soft while its far parts are
  sharp, in one layer. One radius per model is wrong, and so is a gather by each pixel's
  own radius alone: a soft near part would keep a hard edge against a sharp far part.
- A sharp near part must keep a sharp edge over a soft far part, as ADR-0108 keeps a
  sharp card's.
- The silhouette must soften outward, as a card's does (ADR-0108 decision 4).
- A scene with no model, or a camera with no depth of field, must cost and draw exactly as
  today.

It is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)): the maintainer asked for it, and it
changes no licence, security or privacy posture, compatibility promise or spending. It
adds nothing to the protocol: the camera's existing `focus_px` and `blur_px` drive it.

<a id="decision"></a>
## Decision
**1. A model is blurred by its own depth.** When the camera that draws a model has depth
of field, each pixel of the model's layer is blurred by the circle of confusion of the
surface it shows: radius `r = blur_px · |D − focus_px| / D`, capped at 64 canvas pixels,
the function cards use. Without depth of field, or without a camera, the model draws as
before, with no extra pass.

**2. Drawn as a gather that acts like a scatter.** `Compositor::draw_model` draws the model
into a layer the compositor keeps and its view depth into another, then fills the item's
layer from them in five fullscreen passes:
1. **Tiles.** For each 8 × 8 block of pixels, the largest `r` in it.
2. **Reach.** For each tile, the largest `r` of the tiles whose blur reaches it, so a
   pixel knows how far it must look.
3. **Pack.** Each pixel's `1 / w` beside its reach `R`.
4. **Gather.** Each pixel reads a spiral of 48 taps over a disc of radius `R`, turned per
   pixel. A tap counts where its own disc reaches this pixel, so a soft pixel spreads
   over its neighbours as a lens would, weighted by the inverse of its disc's area. Taps
   narrower than the tap spacing (`R / √48`) don't spread, and the pixel's own colour
   stands for them.
   - **Occlusion.** A tap farther than this pixel spreads no farther than this pixel's own
     radius. So a sharp near part keeps its edge over a soft far part, and a soft near part
     spreads over a sharp far part.
   - **Hidden background.** Where taps fall on a sharp nearer part, the farther pixel's
     sum is scaled up by the share of taps left, standing in for the background the near
     part hides.
   - **Empty pixels** are infinitely far, so a soft edge spreads into them and the
     silhouette softens outward.

   Where `R` is under half a pixel the pass copies the pixel, so an in-focus model draws as
   it would without depth of field.
5. **Smooth.** Each pixel whose own radius is 1.5 px or more becomes the mean of the 5 × 5
   pixels around it, each weighted by how soft it is itself, which evens out the
   gather's grain without spreading a sharp neighbour into the blur. Sharper pixels are
   copied.

**3. Order.** The blur happens as the model's layer is drawn, before its source's filters
and its item's effects run over that layer (ADR-0107 decision 6). A card's filters run
before its blur, since they run on the flat picture. The two orders differ only for a
filter that moves pixels, and such a filter over a model's layer already works in canvas
pixels.

**4. What it doesn't change.** The model's view depth, its shadow maps, the scene's view
depth and the depth that hides cards behind a model (ADR-0107 decision 7) stay sharp, as
ADR-0143 decision 5 keeps depth sharp for cards. Sorting, lights and blending are as
before. A Move, a live value or an agent that changes the camera's focus changes a model's
blur as it does a card's.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - a camera pulls focus across models as it does across cards, so a scene built from
    models has depth of field with nothing to set beyond the camera's focus.
- **Harder:**
  - **Quality.** Like ADR-0108's disc, this is an approximation. A layer holds only the
    model's nearest surface, so the background a soft near part hides is guessed from what
    is around it. Large radii show the 48 taps as fine grain, which the per-pixel turn
    spreads into noise rather than pattern and the smoothing pass evens out. Golden tests
    bound it by its effect: an in-focus model draws as without depth of field, a defocused
    one keeps its mean light while its edges ramp, its silhouette softens outward, and a
    sharp near part keeps its edge over a soft far part.
  - **Cost.** Five passes per model with depth of field, the gather at 48 taps and the
    smoothing at 25 a pixel over the canvas; nothing for a model drawn without it.
- **Not decided here:**
  - **Between items.** A soft model in front of a sharp card or another model still
    spreads only within its own layer's coverage, as ADR-0108 says of cards. A
    scene-wide pass over a depth buffer would lift that, and is its own ADR.
  - **Bokeh shapes and highlights,** as in ADR-0108.
- **Follow-up implementation:** `jstream-render` only: the passes, `draw_model` taking them
  when the camera has depth of field, and golden tests on lavapipe. The MCP design notes'
  depth of field paragraph says that models blur too.
