<!-- @doc
id = "ADR-0255"
title = "A whole picture turned over or flipped has no layer either"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0255: A whole picture turned over or flipped has no layer either

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0249](0249-a-nested-scene-is-drawn-at-the-size-it-is-shown.md) decision 4 places a
nested scene of one whole picture (a capture, or a picture over no background) as that
picture, with no layer, when it covers the canvas's box exactly, unrotated and
unflipped. A webcam mounted upside down is shown turned by 180 degrees over the canvas,
and a geometry saved from a client can carry a centre a rounding off the canvas's
(959.99994 for 960). Either misses the exact match, so the scene is drawn into a
layer of the canvas's size every frame: about 1.5 ms of GPU time for a 1080p webcam
on an integrated GPU (Radeon 780M), where its unturned twin costs none.

<a id="decision"></a>
## Decision
ADR-0249 decision 4 stands, with these changes:

1. **Within a rounding.** A picture covers the canvas's box when its centre and size are
   within a hundredth of a pixel of the box's and its turn within a thousandth of a
   degree of a whole number of half turns.
2. **Turned over or flipped.** A picture turned by a half turn, or flipped across or
   down, passes through too. Its key holds the picture's image as before, and the items
   of the scenes that place it are placed mirrored to match (a half turn mirrors it
   both ways, and each flip mirrors its own way again), so what they show is what the
   layer would have held.
3. **Only items are told.** A mirror is carried by an item's placement. While a
   transition's track, an item's effects box or a model's texture shows the scene, a
   mirrored one is drawn into a layer as before. An unmirrored one passes through as
   ADR-0249 has it.

No setting, switch, protocol message or file changes.

<a id="consequences"></a>
## Consequences
- **An upside-down webcam costs no pass.** A full-frame capture turned over is its
  texture wherever an item places it.
- **A rounding no longer costs a layer.** A geometry a hundredth of a pixel off still
  passes through, drawn as if exact.
- **What stays.** A mirrored scene read by a transition, a box or a model keeps its
  layer, and any other turn or crop draws a layer as before.
