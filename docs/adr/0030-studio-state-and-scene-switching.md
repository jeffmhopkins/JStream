<!-- @doc
id = "ADR-0030"
title = "Studio state and scene switching"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0030: Studio state and scene switching

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
The M1 demo needs two scenes and a way to choose which one goes out. ARCH's data model
lists `studio { program_scene, preview_scene }` inside the collection, and the protocol
leaves open how switching works
([jstream-protocol › Open questions](../../crates/jstream-protocol/README.md#open-questions),
question 5): an action that also bumps `rev`, or an edit plus a separate "run transition"
action.

[ADR-0029](0029-transitions.md), accepted, already states the answer it builds on:
"Scene switching itself is an action, not an edit, and program and preview are runtime
state". That is register entry D26
([D. Transitions and studio](../../design/ui-prototype/decision-register.md#d-transitions-and-studio)),
a technical decision within [ADR-0013](0013-command-model.md) that ADR-0029 left to its
own ADR. This is that ADR.

The forces:
- Switches happen on air, often by hotkey. If `program_scene` were an edit, Ctrl+Z would
  switch scenes on air, and each client's undo history would interleave switches with
  layout edits.
- An action that bumps `rev` breaks "`rev` counts edits", which agents rely on for
  `expected_rev`, and every switch would make every client re-read the state.
- Program must always show something sensible while the collection changes under it: a
  fresh collection's first scene, or a program scene that another client deletes.

<a id="decision"></a>
## Decision
**Program is runtime state.** The engine keeps the program scene beside the outputs'
state, not in the collection. It is not counted by `rev`, not undoable, and not part of
`state.get`.

**Protocol (0.13):**
- Action `scene.switch { scene_id }` puts a scene on program with a cut. It fails with
  `not_found` for a scene not in the collection. Switching to the scene already on
  program succeeds and emits nothing. ADR-0029's optional `preset` is added with the
  transitions it names; until then every switch is a cut.
- Query `studio.get` returns `{ program_scene }`, with no `rev`. `program_scene` is
  omitted only while the collection has no scene.
- Event `studio.changed { program_scene, client_id, request_id }` reports every change of
  program, naming the request that caused it.

**Rules that keep program valid:**
- The collection's first scene goes on program when it is created.
- Edits that delete the program scene, whether a batch, an undo or a redo, move program
  to the first scene left in display order, or to none when no scene is left.
- Reordering scenes never moves program.
- Edits that move program emit their `state.changed` first, then `studio.changed`.
- Undo and redo never restore a program scene: undoing a `scene.switch` is not
  possible (it is an action), and undoing a `scene.delete` brings the scene back
  without putting it on program.

**What follows program:** `video.snapshot` composites the program scene, and so does
every output that renders a scene once the render thread composites scenes (E1, C1b). A
`VideoMix` with input `Program` (ARCH › Data model) follows it when mixes are built.

**Not decided here:** the preview scene and `studio.set_preview` (studio mode, M2); where
the last program scene is saved for a restart, which comes with persistence; and
transitions, which [ADR-0029](0029-transitions.md) specifies.

This settles protocol open question 5: switching is an action that changes runtime state,
and no action changes collection state.

<a id="consequences"></a>
## Consequences
- An agent's `expected_rev` does not protect against a concurrent switch. That is
  intended: a switch is not an edit.
- A client learns about program from `studio.changed` and `studio.get`, not from the
  collection. A client that missed events (`events.dropped`) re-reads both.
- A restart puts the first scene on program until the last program scene is persisted.
- Every implementation of the protocol (engine and mock) applies the same rules for the
  first scene and for a deleted program scene, and the engine's differential test holds
  them to each other.
- [ARCH › Data model](../ARCHITECTURE.md#data-model) drops `studio` from the collection
  tree, and the protocol's open question 5 points here.
