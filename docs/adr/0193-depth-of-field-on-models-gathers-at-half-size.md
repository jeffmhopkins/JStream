<!-- @doc
id = "ADR-0193"
title = "Depth of field on models gathers at half the layer's size"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0193: Depth of field on models gathers at half the layer's size

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0165](0165-depth-of-field-on-models.md) blurs a model by its own depth in five
fullscreen passes. Its decision 2 fixes two of them at the layer's size: a gather of 48
taps at every pixel, two reads a tap, and a smoothing pass of 25. Measured at 1080p30 on
an integrated GPU, that was 6 to 10 ms of a 33 ms frame for one model that fills the
canvas, the largest single cost left in such a scene.

A blur several pixels wide has no detail a half-size picture can't hold, so most of
those reads buy nothing. Reading fewer taps at full size was tried first
([ADR-0192](0192-cheaper-sampling-in-3d-scenes.md)) and showed as a dot pattern near
the focus. A gather at half size alone was tried next: wide blurs matched, and parts a
pixel or two out of focus came out visibly softer than they should, because a half-size
picture can't hold a blur that small.

<a id="decision"></a>
## Decision
ADR-0165 stands: what is blurred, by how much, its occlusion rules, its order and what
it leaves sharp. This replaces how its decision 2 draws passes 4 and 5, which become
three.

1. **The blur is split in two by its radius.** A pixel whose radius `r` is 2 px or more
   spreads wholly in a wide layer, one under 0.75 px wholly in a narrow layer, and one
   between in both, its share of the wide layer rising smoothly with `r`. A pixel
   narrower than the gather's tap spacing (`R / √48`) is all in the narrow layer, as its
   own colour stood for it before.
2. **The wide layer is gathered at half the layer's size.** Each texel gathers for one
   pixel of its block of two by two, alternating the corner, with ADR-0165's 48 taps
   against the full-size colour and depth, each tap weighted by its share of the wide
   layer as well.
3. **Its grain is smoothed at half size too.** A texel whose pixel is 1.5 px or more
   soft becomes a mean of the 5 × 5 texels around it, weighted by their softness as
   before and by a cone as wide as the grain: one and a half tap spacings, and at most
   0.4 of the pixel's own radius.
4. **A last pass at the layer's size puts the two together.** It takes the wide layer
   from the 3 × 3 texels around the pixel, each weighted by how near its pixel is and by
   how alike that pixel's radius is to this one's, so what was gathered for a soft part
   isn't laid over a sharp part beside it. It spreads the narrow layer itself: each of
   the 3 × 3 pixels around it gives its narrow share, over itself alone at radius 0 up
   to all nine at about 1.8 px, a farther pixel spreading no farther than this pixel's
   own radius. The narrow layer goes under the wide one, into the alpha the wide one
   left.
5. Where no blur reaches a pixel it is copied, as before, so a model in focus draws
   exactly as without depth of field.

No setting, switch, protocol message or file changes.

<a id="consequences"></a>
## Consequences
- **Cost.** Each pixel takes about 70 texture reads where it took about 120: a quarter
  of the gather's, a quarter of a smoothing pass's, and 36 in the last pass. How much GPU
  time that saves is not yet measured on the integrated GPU the context's numbers come
  from.
- **A slight blur is drawn as what it is.** Before, a pixel narrower than the tap
  spacing stayed fully sharp, and the spacing grows with the widest blur near it, so a
  part 2 px out of focus was sharp beside a wide blur and soft away from one. Now it is
  blurred by its own radius either way. Beside a wide blur, parts a pixel or two out of
  focus are a little softer than they were.
- **A wide blur's grain is coarser and fainter.** It is made at half size and smoothed
  over about twice the width. Small bright lights blurred by tens of pixels show it
  most, as they showed the finer grain before.
- **The wide layer has the detail of one pixel in four.** The last pass keeps it to
  pixels of a like radius, so an edge between a sharp part and a soft one stays where it
  is; within a blur of 2 px or more nothing finer was there to keep. Where a wide blur
  spreads over a sharp part, the rim of its disc was a hard step and is now a ramp a few
  pixels wide, reaching up to 3 px farther. One of ADR-0165's tests read the sharp part
  4 px outside such a rim and now reads it 14 px outside.
- The pass keeps two half-size scratch layers where it kept one of the layer's size.
- ADR-0165's other tests hold unchanged. One more checks that the gathered layers are half
  the layer's size and that stripes one pixel wide, 0.6 px out of focus, are softened
  and still there.
