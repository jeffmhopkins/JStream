<!-- @doc
id = "ADR-0249"
title = "A nested 2D scene is drawn with the pixels it is shown at"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0249: A nested 2D scene is drawn with the pixels it is shown at

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Every nested scene is drawn into a layer of the canvas's size, whatever size the
scenes that place it show it at. A set that shows several 2D scenes as small pictures
(screens, monitors, picture-in-picture) pays for every one of them at the full canvas.
On an integrated GPU (Radeon 780M) at 1080p30, under stream load, a 3D scene showing
about ten such 2D scenes on cards of 300 to 600 pixels took 30 ms of GPU time a frame
against 33 ms to spend: its scene pass was 20 ms, and the nested 2D scenes about 15 ms
of that, each 1.4 to 1.9 ms.

[ADR-0194](0194-a-layer-is-drawn-again-only-when-what-it-draws-has-changed.md) already
leaves a layer as it is when nothing it draws changed, and
[ADR-0201](0201-a-layer-is-redrawn-only-over-the-boxes-of-the-items-that-changed.md) redraws only the boxes
that did. A scene that shows a live source (a page that repaints, a capture, a video)
still changes every frame, and is drawn whole when its background is opaque.

An upload only items draw is already held at the size they draw it (#1139, ADR-0117 §2):
each draw records how much of the frame's resolution it needed, and the cache holds the
upload at a power-of-two fraction of the frame from that and what the placements plan.
An image can already stand for a picture of more pixels than it has
([ADR-0073](0073-sources-drawn-at-their-shown-size.md)), and
[ADR-0197](0197-3d-models-draw-with-fewer-pixels-when-the-gpu-is-short-of-time.md) draws
a model's layer that way, but its decision 3 keeps 2D scenes at the canvas's pixels.

<a id="decision"></a>
## Decision
ADR-0197 decision 3 stands, with this change: **a nested 2D scene's layer has as many
pixels as the scenes that place it show it with**, on the render thread and the preview
compositor, which already hold uploads at the size they are drawn.

1. **Which layers.** A layer drawn for a scene below the program, in 2D (no camera),
   whether a mix draws it or the chains do (a scene nested in one with a video chain,
   or in one a model's map shows), that places no layer drawn in the canvas's own
   pixels, and whose image no video chain or model reflection reads as texels: a
   chain's input or a filter's scene, or the layer a model reflects as a panorama.
   Those keep the canvas's pixels, which their filters work in; the scenes nested in
   them are still drawn small. The layer a model shows as its texture is drawn small
   like any other. Every other layer, the
   program's, a stage's, a 3D scene's and every one a still, a probe or a frame sheet
   draws, keeps the canvas's size.
2. **The size.** As an upload's: a power-of-two fraction of the canvas each way, 1 down to
   1/16, with a quarter of headroom over the most of the canvas's resolution the layer's
   draws needed the frame before and what the placements plan, chosen once a frame. It
   grows the next frame when it is drawn larger, and shrinks only once drawn at less
   than half of it.
   - **Planned:** the most a 2D scene enlarges it, times the share of the canvas that
     scene's own layer holds when it is drawn small too; all of it while a transition's track shows it. A
     new layer starts there, or at the canvas's size when nothing plans it.
   - **Measured:** each draw that places it records what it needed. Unlike an upload's,
     a layer seen in 3D records what its polygon shows where it is largest (the near
     end of each edge, from the ratio of the ends' `w`), so a scene on a small card is
     drawn small. An upload seen in 3D still needs its whole frame. A model that shows
     a layer as its texture records the most texels a pixel of it spans: the densest
     texture coordinates of its textured triangles (by area, per unit of its surface)
     against the most pixels a unit of the model covers at any corner of its bounds,
     so a camera that closes in on the model raises its screens to full resolution.
3. **How it is drawn.** The layer is placed at the canvas's size whatever its pixels.
   The scene's items are laid out on the canvas as before, then scaled onto the layer's
   pixels: each axis by its own ratio, an item's depth and perspective by their mean.
   Kept layers and redrawn boxes (ADR-0194, ADR-0201) work in the layer's pixels.
4. **A scene of one whole picture has no layer.** A layer of decision 1 whose scene has
   one item (no effects, move, fade, motion or groups, opaque, in the normal mode,
   uncropped), a capture or a picture over no background, that this frame, with its
   live values, covers the canvas's box (to a hundredth of a pixel, flat, stretched
   or of the canvas's aspect ratio, and turned by none or a half turn), is not drawn:
   its key holds that picture's image, placed at the canvas's size, and the scenes
   that place it sample the picture itself. A picture flipped or turned over is
   placed mirrored to match, which only items can be told: while a transition, a box
   or a model shows the scene, a mirrored one is drawn into a layer. A frame where it
   doesn't cover the box draws it into a layer as before.

No setting, switch, protocol message or file changes.

<a id="consequences"></a>
## Consequences
- **A small nested scene costs its pixels.** A scene shown at a quarter of the canvas
  each way is drawn into a layer of a quarter or a half each way, a sixteenth or a
  quarter of the work, and so are the scenes it nests and the uploads it shows, whose
  needs are measured against it.
- **One frame from too few texels.** A scene placed larger by a gesture or a live value
  is drawn from its smaller layer for a frame, as an upload is, then at the size it needs.
  A cut or a transition to it is planned, and drawn whole from its first frame.
- **Not exact.** A layer rounds its size up, so its two ratios differ by under a texel,
  and an item rotated in it is drawn as at their mean. Edges are filtered from fewer
  texels, as an upload drawn small is.
- **A full-frame capture costs no pass.** A scene that only shows a 1080p webcam over
  its canvas is the webcam's texture wherever it is placed.
- **A model's texture is sized from an estimate.** The densest triangle and the
  nearest corner of the bounds overstate what most of the model needs, so a textured
  model may keep more texels than it shows; never fewer, short of a pose that moves
  its surface past its rest bounds.
- **What stays.** Snapshots, stills, frame sheets and probes draw every layer at the
  canvas's size, so their pixels are unchanged. A nested 3D scene's layer is unchanged.
