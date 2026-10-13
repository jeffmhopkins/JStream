<!-- @doc
id = "ADR-0007"
title = "Bus-matrix audio and scene-independent outputs"
kind = "adr"
status = "accepted"
-->

# ADR-0007: Bus-matrix audio and scene-independent outputs

<a id="context"></a>
## Context
Video and audio routing must be independent: for example, music on the stream but not in
the VOD, or a different scene on the virtual cam than on the stream.

<a id="decision"></a>
## Decision
Audio sources exist independently of scenes, and feed buses through a send matrix with a
gain per send. Buses carry filter chains and feed outputs: stream and recording tracks,
and monitor devices. Video outputs consume a *video mix* (Program, Preview, or any scene),
not "the current scene".

A **virtual microphone** is out of scope. Windows has no user-mode API for creating an
audio endpoint, so it would need a signed kernel driver, which requires its own ADR.

<a id="consequences"></a>
## Consequences
- Covers OBS's track model as a special case.
- The UI must present the matrix simply (sends per source, a strip per bus).
