<!-- @doc
id = "ADR-0078"
title = "Text density bounded by its frame's pixels, not at 8"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0078: Text density bounded by its frame's pixels, not at 8

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0073](0073-sources-drawn-at-their-shown-size.md) draws a `text` source at the density
its most enlarged item needs, from 1 to `MAX_DENSITY`, 8. The maintainer's alpha pass found
that too low: a text source left at its 64 px default, with a short word typed in, and
dragged to fill a 1920x1080 canvas is shown about 15 times its own size. At 8 it is still
resampled to about twice its pixels, and it looks soft. A smaller font, or a 4K canvas, needs
more.

The cap was there for memory. A fixed density is a poor bound for that: 8 lets a long
paragraph draw a frame wider than a GPU's largest texture, while a single short word at 8
holds fewer pixels than one canvas.

<a id="decision"></a>
## Decision
This amends ADR-0073 §1, the densities the `text` kind draws at.

1. `MAX_DENSITY` is 32. The engine's quarter steps of a doubling run up to it, as before.
2. The bound is the frame's pixels. A density whose frame would hold more pixels than a 4K
   canvas (3840 x 2160), or be wider or taller than 8192 (the largest texture side every
   GPU the compositor runs on takes), is lowered to the highest density that fits. The
   source still reports the density's extent, so the engine places the frame as before, and
   the engine doesn't ask again: it keeps the density it asked for.
3. The text applies that bound each time it draws, on an update as well as on a new
   density, so a longer text at a high density draws a smaller density rather than fail.
   ADR-0073's halving for a frame over the source's limits stays, below this bound.

<a id="consequences"></a>
## Consequences
- A short line filling the canvas is drawn at the size it is shown, up to 32 times its own
  size, and stays sharp on program, in the preview and in recordings.
- A text's frame at a density holds at most about 8.3 million pixels: 33 MB of RGBA, and
  twice that as the compositor's half-float texture. ADR-0073's "up to 64 times its pixels"
  for a text enlarged and then hidden becomes "up to a 4K canvas's pixels".
- A text larger than a 4K canvas when drawn at the density it is shown at is resampled the
  rest of the way. That is a text cropped to a small part of itself, or shown on a canvas
  larger than 4K.
