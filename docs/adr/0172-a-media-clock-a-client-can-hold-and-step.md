<!-- @doc
id = "ADR-0172"
title = "A media clock a client can hold and step"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0172: A media clock a client can hold and step

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Everything the engine animates reads one media clock: transitions and Moves (ADR-0099,
ADR-0113), motion modifiers, timelines (ADR-0115), keyframe tracks (ADR-0116), a sky's
warp (ADR-0167), animated sources, and the pictures `video.snapshot` and `video.frames`
draw. That clock is the platform's, so it follows real time.

A machine that can't composite a scene in real time can't show its motion. The engine
draws whatever time it is when a frame is done, so a 3D scene with depth of field on
models (ADR-0165) and a sky at warp, at a few seconds a frame on a software rasterizer,
skips most of a 3-second Move. The maintainer asked for a way to render such scenes as
video on a machine without a GPU.

Two workarounds were tried, and both fall short:
- **A slowed recording.** The client stretches every duration it sets and retimes the
  recording. A Move's duration is capped at 60 s, so this can't stretch a 3 s Move more
  than 20 times. The engine's own rates (a sky's warp ramp and travel, a motion's
  period set in real time) don't stretch at all.
- **Stepping by hand.** The client sets the camera for each frame and takes a snapshot.
  This replaces the engine's own animation with the client's copy of it. It can't step
  a sky, which advances by real time and resets on a gap over a second.

The forces:
- A render must be exact: frame *n* shows the scene at time *t₀ + n·Δ*, however long it
  took to draw.
- The clock never runs backward. Every consumer (sources, timelines, the sky, MIDI)
  assumes that (`MediaClock::now`).
- An output (stream, recording, virtual camera) follows real time with its encoder and
  audio, so it can't share a clock that stands still.
- No new state goes into the collection. A render is a session's job, not part of the
  show.

It is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)). It is an addition to the protocol
(a minor bump, R-API-001). It changes no licence, security or privacy posture, and no
spending. No compatibility promise is broken: the clock runs as before until a client
holds it.

<a id="decision"></a>
## Decision
**1. One clock the engine owns.** The engine wraps the platform's media clock (or its own,
before the host injects one) in a clock it can hold. The live sources, the engine's state
(the time each action and switch is stamped with, and what a snapshot draws at) and MIDI
read it. The output pipeline keeps the platform's clock.

**2. Three actions.** None of them changes `rev`, is undone or is saved.
- `clock.hold` stops the clock where it is. Holding again keeps the held time.
- `clock.step { ms }` moves the held time on by 1 to 10000 ms. It fails if the clock
  isn't held.
- `clock.release` lets the clock run again. Releasing a clock that runs does nothing.

A render is: hold; then for each frame, step by the frame time and take a snapshot;
then release. A step wakes the live sources, so a source due at the new time paints.

**3. Never backward.** Released, the clock waits at the held time until the platform's
clock passes it, then runs with it again. A render stepped faster than real time pauses
the engine's time until real time catches up. One stepped slower jumps forward to now,
as a long frame would.

**4. Outputs and a held clock exclude each other.** `clock.hold` fails with
`action_failed` while an output runs, and `output.start` fails while the clock is held
or, released, still stands ahead of real time waiting for it to catch up: the error says
how far ahead it is.
An output's pictures, encoder and sound follow real time, and the pipeline keeps the
platform's clock. Previews may show a held scene at real time meanwhile; snapshots and
frame sheets are what a render reads.

**5. Not for hotkeys or plugins.** The three actions can't be bound. A plugin is never
granted them, since holding stops every client's animation.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - Any scene, however slow to draw, renders exactly, frame by frame: Moves, keyframe
    tracks, motion, timelines, a sky's warp ramp and travel, and animated sources all
    see the stepped time. A hold, a step or a release runs the timelines' tick before
    it returns, so a cue the step passed has fired by the next snapshot. Nothing in a scene has to be stretched or copied by the client.
  - Tests and agents can look at a moment of an animation precisely.
- **Harder:**
  - While held, every client sees the scene stand still, and preview streams may not
    match the snapshots.
  - Only what reads the engine's clock is stepped. A capture device, a browser page's
    own JavaScript timers, and a media file's decoder pacing run in real time. A render
    of such a source shows whatever frame it has.
  - After a render stepped faster than real time, the engine's time stands still until
    real time catches up, and no output starts until it has.
  - Live inputs stamped on the platform's clock aren't moved: a gesture's live value
    and notes that JTunnel brings in keep real time, so while the clock is held, or
    ahead after a release, a MIDI note can read as early or late against the engine's
    time.
  - Steps are whole milliseconds. 30 fps is steps of 33, 33 and 34 ms.
- **Follow-up implementation:**
  - `jstream-core`: `clock.rs` (`HeldClock`), with the live sources and MIDI on it.
  - The three actions in `jstream-protocol` (0.134).
  - The `clock` MCP tool, not destructive.
  - Tests of hold, step, release, the floor, and the exclusion with outputs.
