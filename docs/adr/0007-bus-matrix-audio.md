---
id: ADR-0007
title: "Bus-matrix audio and scene-independent outputs"
kind: adr
status: accepted
decided: 2026-10-13
rev: 1
review_every: 180d
depends_on: []
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version, recorded from design Q&A with the maintainer"}
---

# 0007: Bus-matrix audio and scene-independent outputs


## Context
Video and audio routing must be independent (e.g. music on stream but not in the VOD,
a different scene on the virtual cam than on the stream).

## Decision
Audio sources exist independently of scenes and feed buses through a send matrix
(per-send gain). Buses carry filter chains and feed outputs (stream/record tracks,
monitor, virtual mic). Video outputs consume a *video mix* (Program, Preview, or any
scene), not "the current scene".

## Consequences
- Covers OBS's track model as a special case.
- The UI must present the matrix simply (per-source sends, per-bus strips).
