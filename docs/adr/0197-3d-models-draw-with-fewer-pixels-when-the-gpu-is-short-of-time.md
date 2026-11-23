<!-- @doc
id = "ADR-0197"
title = "3D models draw with fewer pixels while the GPU is short of time"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0197: 3D models draw with fewer pixels while the GPU is short of time

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0117](0117-real-time-video-thread-model.md) keeps the GPU's queue short: when the
conversions of two ticks are still running, the tick's frame is not handed to the
outputs on the GPU path. That is the right thing to give up last. On an integrated GPU
at 1080p30, a 3D scene with one model, depth of field and screen-space reflections was
measured at 31 to 35 ms of GPU time a frame against 33 ms to spend, and its stream lost
about one frame in ten.

A 3D scene's frame there is bound by the pixels it shades
([ADR-0192](0192-cheaper-sampling-in-3d-scenes.md)), and most of them are a model's: in
that scene the model's layer with its depth of field took 17 ms of the 35. A model is
drawn into a layer of its own, the size of its canvas, which the scene then places
([ADR-0107](0107-3d-models.md) decision 6). An image can already hold a picture with
another number of pixels than the size it is placed at
([ADR-0073](0073-sources-drawn-at-their-shown-size.md)).

Two ways to spend less were weighed. Drawing the 3D scene on fewer ticks than the
outputs run at lowers the average but not the frame that is too long, moves in an
uneven cadence at 20 in 30, and slows everything the scene shows. Drawing with fewer
pixels lowers every frame and keeps motion at the output's rate.

<a id="decision"></a>
## Decision
ADR-0107 decision 6 stands, with this change: **a model's layer may have fewer pixels
than its canvas, and the program's render thread lowers that number while the GPU is
short of time.**

1. **The size.** A model's layer, its view depth and the surface layers drawn with it
   ([ADR-0170](0170-generic-reflections-and-3d-rendering-switches.md) decision 4) are
   100, 85, 75, 67 or 50 percent of the canvas's width and of its height, rounded up.
   Each still stands for the whole canvas: the model is placed through its camera on
   the canvas, and the scene places the layer over the canvas, filtered, where it placed
   it before. Its depth is read texel for texel, as before.
2. **What follows it.** The model's depth of field
   ([ADR-0165](0165-depth-of-field-on-models.md)) runs at the layer's size, its blur and
   its 64-pixel cap scaled, so it covers as much of the picture. A scene's surface
   buffer stays the canvas's size; a smaller model's surface is spread over it, each
   texel taking the one it falls in, unfiltered. An item's effects run over the layer
   at its size, as they do over any image drawn smaller than it is placed.
3. **What does not change.** Cards, the sky, the scene's pass and its chain (screen-space
   reflections, ambient occlusion, fog and every filter), 2D scenes, captures and the
   output keep the canvas's pixels. Shadow maps, probes and the mirrored draw of a
   floor reflection keep their sizes. Nothing keeps history, as before.
4. **Who chooses.** The program stage's render thread, once a tick, from two signals it
   already has: the GPU time of the frames it times
   ([ADR-0103](0103-per-item-performance-profile.md), one in four) and whether the GPU has
   fallen behind (ADR-0117 §1).
   - It steps down one size at once when the GPU is behind, and when three timed frames
     in a row took more than 88% of the frame's budget.
   - It steps up one size when the GPU time the larger size is expected to take has
     stayed under 78% of the budget for 5 seconds. The expectation is the worst case,
     all of the frame's GPU time growing with the model's pixels. A step up undone
     within 15 seconds doubles that wait, up to 80 seconds.
   - After a change nothing is decided for 10 ticks: the frames in flight were drawn at
     the size before.
   - On a device that can't time its frames, it steps down when the GPU is behind and
     up after the wait without that.
5. **Every other drawer** (a preview compositor of its own, a still, a probe) draws at
   100.
6. **The override.** The environment variable `JSTREAM_MODEL_RESOLUTION` set to `off`
   keeps 100, a size keeps that size, and `auto` or nothing chooses. A setting on the
   rendering switches replaces it when the protocol next changes.

Drawing the whole 3D scene smaller was considered. It saves more (the pass and
screen-space reflections too), but softens the cards, which often hold text, needs
every pixel-sized quantity in the compositor and in filters scaled, and pack effects'
can't be. It can follow if the model alone proves too little.

<a id="consequences"></a>
## Consequences
- While the GPU keeps up, every model is drawn at 100 and the picture is what it was.
- A model drawn smaller is softer by the enlarging, a plain filtered read: fine detail
  and a model's own texture lose sharpness in proportion, and its silhouette against a
  card is cut in steps of its layer's texels. A change of size shows as one step in
  sharpness.
- Expected, not yet measured on the integrated GPU above: of the 17 ms the model's
  layer took there, most follows its pixels, so 75% should give back about 7 ms and 50%
  about 12. The first measurement replaces this.
- A screen-space reflection on a smaller model reads normals and reflections that are
  constant over a block of the canvas's pixels; the resolve's weights by plane and
  normal still keep one surface from another.
- An item effect on a model's layer whose parameters count in pixels reaches further
  across the picture at a smaller size, as on any image drawn small.
- The compositor keeps the depth buffers of the last two sizes models were drawn at,
  since the mirrored and probe draws keep theirs.
- The choice is the render thread's and is not in the protocol: nothing reports the
  size but the log, one line a change.
