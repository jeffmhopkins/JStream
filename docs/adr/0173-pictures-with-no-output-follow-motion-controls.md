<!-- @doc
id = "ADR-0173"
title = "Pictures drawn with no output running follow motion controls"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0173: Pictures drawn with no output running follow motion controls

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
An audio, MIDI or data motion modifier follows its input over time: an audio one through
its envelope (ADR-0100), a MIDI or data one through its smoothing (ADR-0168, ADR-0171
decision 3). The render thread's follower steps each of them once a tick. That thread
runs only while an output runs, so with none running nothing follows. ADR-0168
decision 4 settled what a shot with no follower draws: a MIDI control as it reads at that
instant, unsmoothed, so previews and the canvas still show the controller.

That was fine while a follower was the normal case and an unfollowed picture was only a
preview. A held clock (ADR-0172) changed that. A render holds the clock, steps it a frame
at a time and takes a `video.snapshot` per frame, and ADR-0172 decision 4 forbids an
output while the clock is held. So every frame of a held render reads its controls raw,
and its smoothing does nothing. A scene of lights that follow a DAW's meters over
220 ms flashed on each drum hit in a render: the meters fall to 0 between hits, and
each frame drew them as they stood. Raising the smoothing to 3 s changed no pixel. A
preview of the same scene with no output running flickers the same way.

It is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)). It changes no protocol, licence,
security or privacy posture, or spending. It amends ADR-0168 decision 4 for the program
only, and keeps every other part of that ADR.

<a id="decision"></a>
## Decision
**1. The control side follows while the render thread doesn't.** While no render thread
adopts snapshots, the engine keeps one follower of its own for the program's shot. Each
picture the control side draws of the program, a `video.snapshot` or a preview hub
picture, steps it to the clock's time first. Its state carries from picture to picture
and across new plans, as the render thread's does. A picture at a time already followed
only carries levels to a new plan's bindings, and moves nothing.

**2. One follower at a time.** While a render thread runs, it alone follows, as before,
and the control side's pictures read what it stored. The control side's follower steps
only while none runs, so two followers never store into the same shot.

**3. What it follows.** The program's shot, as the render thread's follower does. A
scene or source preview's own shot still has no follower, and reads as ADR-0168
decision 4 says. With no output, the mixer doesn't run (ADR-0100), so an audio modifier
still hears silence. Its envelope now falls to silence over its release time instead of
standing at it.

<a id="consequences"></a>
## Consequences
- **Easier:** a held render draws a smoothed modifier as a stream of the same scene
  would, frame for frame. A preview shows the scene's motion as it will stream.
- **Harder:** a picture taken a long time after the last one steps the smoothing by that
  whole gap, so it lands on the control, as a long frame on the render thread does.
  A MIDI control on the canvas with no output now glides over its smoothing time, where
  it jumped before.
- **Must now do:** `jstream-core`: the follower on the engine's shared state, stepped by
  `video.snapshot` and the preview source while `RenderHub::following` is false; a test
  that steps a held clock under a preview and finds the smoothing part way.
