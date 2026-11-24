<!-- @doc
id = "ADR-0236"
title = "An output's start is part of the first seconds the model size governor waits out"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0236: An output's start is part of the first seconds the model size governor waits out

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0197](0197-3d-models-draw-with-fewer-pixels-when-the-gpu-is-short-of-time.md) has
the render thread draw 3D models with fewer pixels while the GPU can't keep its frames
in time, and [ADR-0200](0200-the-size-3d-models-are-drawn-at-is-a-rendering-switch.md)
decision 7 has it decide nothing for its first 3 seconds, whose frames upload and
compile.

An output that starts later has first frames of its own: its pipelines, layers and
encode targets are made on its first draw, and the GPU falls behind for a moment. On a
streaming PC, an output started 0.8 s into a show stepped a scene that fits at 100% down
to 85%. It never stepped back up: the full show takes 88% of the frame's budget at 100%,
and ADR-0200 keeps a scene that close to the step-down mark at 85 on purpose. Fewer
pixels would not have shortened that moment, so the step down bought nothing.

<a id="decision"></a>
## Decision
1. **An output's first frame starts the first seconds again.** When a frame for an
   output (not a picture only this machine sees) is drawn for the first time, the
   governor decides nothing for the next 3 seconds, as at the render thread's start
   (`Governor::output_started`). The frames it timed before say nothing about the ones
   after, so they are forgotten, and its count of long frames and of calm ticks starts
   again.
2. **Nothing else changes.** The marks of ADR-0197 decision 4 and ADR-0200's margin
   stand: a scene that does fall behind after an output's start steps down as before.

<a id="consequences"></a>
## Consequences
Starting a stream or a recording during a show no longer lowers the size models are
drawn at for the rest of the show. A scene that truly can't keep up waits up to 3
seconds longer to step down after an output starts, during which the output's frames
may come late; the start's own hitch already does that. A local preview starting does
not restart the wait, so a page opening previews can't hold the governor's choice off.

Regression test: `resolution::tests::an_output_starting_does_not_step_down`.
