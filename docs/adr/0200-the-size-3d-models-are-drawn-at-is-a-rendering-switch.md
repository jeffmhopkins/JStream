<!-- @doc
id = "ADR-0200"
title = "The size 3D models are drawn at is a rendering switch"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0200: The size 3D models are drawn at is a rendering switch

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0197](0197-3d-models-draw-with-fewer-pixels-when-the-gpu-is-short-of-time.md) lets
the program's render thread draw 3D models with fewer pixels while the GPU is short of
time. It left three things open:

- Its override was an environment variable, set before the engine starts, until the
  protocol next changed (decision 6). An operator can't reach that from the app, and
  can't change it while a stream is live.
- Nothing reported the size but the log, so a client couldn't say that a soft model
  was the engine's choice.
- It stepped down whenever the GPU fell behind (decision 4), in a program that draws no
  model too, where fewer pixels in a model's layer save nothing and the log line
  misleads.

<a id="decision"></a>
## Decision
ADR-0197 stands, with these changes to its decisions 4 and 6.

1. **The setting.** The rendering switches
   ([ADR-0170](0170-generic-reflections-and-3d-rendering-switches.md) decision 1) gain
   `model_resolution`: 0, the default, lets the render thread choose as ADR-0197
   decision 4 says; 100, 85, 75, 67 or 50 always draws models at that percentage of
   their canvas's width and of its height. `studio.set_rendering` refuses any other
   value. It is kept and reported as the other switches are, and takes effect on the
   next tick.
2. **The environment variable is gone.** `JSTREAM_MODEL_RESOLUTION` is no longer read.
3. **Handing the choice back.** When the setting returns to 0 from a size, the render
   thread goes on from that size: it steps up after its wait if there is room, and
   down if there isn't.
4. **Only while a model is drawn.** The render thread steps down only when a model's
   layer was drawn since the tick before. Without one, long frames have another cause,
   the count of long frames starts again, and nothing is logged. Steps up are as before.
5. **The report.** `profile.get` reports, for each drawer, the size its models were
   drawn at at the end of the window (`model_resolution`, 100 for a drawer that never
   lowers it) and how many of the window's frames were drawn below 100
   (`model_frames_reduced`).

The setting is one value and not a switch beside a size, so that no pair of them can
disagree. An item filter whose parameters count in pixels still runs over the smaller
layer unscaled (ADR-0197's consequences): that holds for every image drawn smaller than
it is placed ([ADR-0073](0073-sources-drawn-at-their-shown-size.md)), and is for
filters to settle for all of them at once.

<a id="consequences"></a>
## Consequences
- The protocol's minor version rises: an older client keeps working and leaves the
  choice to the engine; an older engine ignores the setting.
- The Settings dialog's 3D rendering page can offer the choice, and a size can be tried
  on a live stream without restarting the engine.
- A machine that set the environment variable draws as if it were unset until the
  setting is made.
- A 2D program on a GPU that falls behind is left alone; its frames still give way as
  [ADR-0117](0117-real-time-video-thread-model.md) says.
