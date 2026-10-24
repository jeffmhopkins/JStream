<!-- @doc
id = "ADR-0073"
title = "Sources drawn at the size they are shown"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0073: Sources drawn at the size they are shown

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
The `text` source draws its string once, at its `size` in pixels. The compositor scales that
frame like any other texture, so a text item scaled to twice its size shows a bitmap
resampled to twice its size, and it looks blurry (#749, from the maintainer's Windows pass).
That is true on program, in the preview and in recordings.

A source can't fix this alone. It doesn't know its items' placements: several items, in
several scenes, can show one source at different sizes, and the placements are the engine's
([ADR-0022](0022-item-transform-and-canvas.md)). An item's crop and a legacy transform's
scale count in the source frame's pixels, so a source that simply drew a bigger frame would
make every item that shows it bigger, and move its crops.

There were two ways to go:
1. The engine tells a source how enlarged it is shown, and the source draws more pixels for
   the same size.
2. Resizing a text item in the UI changes the source's `size` instead of the item's scale.
   That is simpler, but a resize then means something else for one kind, every other client
   still gets blurry text, and two items that show one source can't differ in size.

<a id="decision"></a>
## Decision
The engine draws a source at the **density** its most enlarged item needs, for the kinds that
can draw at one. A source's frame then has more pixels than its own size, and is placed at
that own size, its **extent**, so nothing a client sees changes: its items' crops, transforms
and boxes still count in the extent's pixels.

1. **The seam.** `Source::set_density(density)` asks a source to draw its frames `density`
   times as many pixels each way as its own size. It returns that own size, the extent, for a
   kind that draws at a density, and `None` (the default) for the others. The `text` kind is
   the one that does: it lays the text out as before and draws it with every length
   multiplied by the density, so its line breaks and extent stay the same. It draws at
   densities from 1 to `MAX_DENSITY`, 8, and halves a density whose frame would be over its
   limits until it fits.
2. **The compositor.** A `jstream-render` image has an extent besides its pixel size
   (`Image::extent`, `ResourceCache::set_extent`). The compositor places an image by its
   extent and samples its texture across it. A video chain's layers carry the extent of
   their input through the filters' sizes.
3. **Measuring.** Each plan of the program mix measures how much every frame its passes
   draw is enlarged: canvas pixels per pixel of the frame's extent, the most of any item that
   shows it, in the program's passes and the video mixes'. It measures the placements the
   collection holds, not a gesture's live values ([ADR-0032](0032-live-edits-and-undo.md)),
   so a drag resamples the frame it has, and the text is drawn sharp once the drag commits.
   That is the rate limit. A source that a video chain runs over counts as 1, because its
   filters' settings are in the frame's pixels. A source drawn only in a transition's tracks,
   or not drawn, keeps the density it has.
4. **Drawing again.** The density is the next quarter step of a doubling at or above the
   scale (1, 1.19, 1.41, 1.68, 2, …), so a small change in scale draws nothing again. A new
   density is a build on the `jstream-sources` thread, which creates the source and sets its
   density there, off the collection lock, as a settings change does. Until it is installed
   the source shows its old frame. A source with a build in flight waits for it before it is
   asked for another, and the plan that installs it measures again.

What it doesn't measure: a nested scene's own scale (an item inside a scene placed at twice
its size is measured inside that scene), the program output's size against the canvas, and
the 3D placements the compositor doesn't draw yet.

<a id="consequences"></a>
## Consequences
- A text item scaled up is sharp on program, in the preview and in recordings, for every
  client, with no protocol change. A text item at its own size draws exactly as before.
- Memory grows with the square of the density: a text drawn at 4 holds 16 times the pixels.
  `MAX_DENSITY` and the source's limits bound it. A text enlarged and then hidden keeps its
  density, up to 64 times its pixels, until a plan shows it again: drawing it smaller on
  hiding would only draw it large again when it is shown.
- A source's extent must not depend on its density, or the measured scale moves with each
  build. The `text` kind takes its bounds from its outlines at a density of 1.
- The size a client sees for a source is its extent. When ADR-0022's `source.sizes` and
  `source.size_changed` are built, they report the extent, not the frame's pixels, or crop
  handles would be off by the density.
- During a drag the text is resampled until the gesture commits, then drawn again.
- Another kind can draw at a density by implementing `set_density`: an SVG image, or a
  browser page at a device pixel ratio. A filtered text source isn't drawn sharper; that
  needs its filters' settings scaled with it, which would be its own decision.
