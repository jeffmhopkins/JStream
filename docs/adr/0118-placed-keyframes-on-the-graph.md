<!-- @doc
id = "ADR-0118"
title = "Placed keyframes live on the engine's graph, not on a copy rebuilt each batch"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0118: Placed keyframes live on the engine's graph, not on a copy rebuilt each batch

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0116](0116-keyframe-tracks.md) decision 3's Copies bullet says the engine hands the
planners a copy of the collection that carries the switched keyframes, and that this copy
is the one thing that holds that studio state. The collection itself never holds it.

While any keyframe is placed, that copy is rebuilt after every batch
(`Graph::with_placed_keyframes` clones the collection), and every gesture start, end or
commit redraws, because the copy exists. Before keyframe tracks, a copy existed only
while a gesture had live source settings ([ADR-0091](0091-source-settings-as-live-values.md)).

<a id="decision"></a>
## Decision
This amends ADR-0116 decision 3's Copies bullet only. Everything else in that decision
stands, including precedence, program, and the Move.

The switched keyframes are a runtime field on the engine's graph
(`State::placed_keyframes`, written by `Graph::place_keyframe`).
- `Graph::apply` keeps the field. It is not an edit, not a revision, and not an undo step.
- The collection file does not store it. `studio.get` and `scene.keyframe_changed` are
  unchanged.
- Planners, audio activity and the page feed read the graph they already draw: the
  collection graph, or the live-settings copy (`Graph::with_values`) while a gesture has
  source settings. That copy carries the field because it clones the graph.
- The engine does not clone the collection on each batch only to attach the keyframes.

<a id="consequences"></a>
## Consequences
- A batch while a keyframe is placed no longer clones the collection for the planners.
  A gesture that changes no source settings no longer redraws only because a keyframe
  is placed.
- ADR-0116 keeps its text and status (R-DOC-009). A reader of its Copies bullet should
  take this ADR instead.
- What a placed scene shows is still absent from `state.get`. A client that draws the
  collection itself still reads `studio.get`'s `keyframes` and `scene.keyframe_changed`.
