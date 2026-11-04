<!-- @doc
id = "ADR-0116"
title = "Keyframe tracks: a placed scene switches its own keyframe, as studio state, with a Move"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0116: Keyframe tracks: a placed scene switches its own keyframe, as studio state, with a Move

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

This ADR builds #1068, the keyframe-track part of the native motion plan under
[ADR-0099](0099-native-motion-modifiers-and-timelines.md). It sits next to
[ADR-0049](0049-keyframe-scene-model-and-evaluation.md), which defines the
keyframe override model, and [ADR-0040](0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md)
§2, which covers overrides on a shared group's children. It is agent tier: it adds an
action within the scope ADR-0099 accepted.

<a id="context"></a>
## Context
A keyframe scene ([ADR-0049](0049-keyframe-scene-model-and-evaluation.md)) is
a state of a whole scene. A program on air often has independent dimensions, such as a
background, a panel layout and an active effect. Only the whole scene can switch
keyframes, so each combination of those dimensions has had to be its own keyframe
scene. Item hotkeys (ADR-0101) fix the cases that only show or hide something. They
don't fix a group that moves between layouts of its own while the background changes
separately.

Several questions are open:
- **Where the state lives.** Is it in the collection (an edit, undone and saved) or
  in the studio (like the program scene)?
- **What draws it.** A scene can be placed in many scenes. The placing scene can also
  be drawn through a keyframe of its own that overrides the same items.
- **How a switch animates.** The issue asks for a Move, from a hotkey, from MCP and
  from a timeline cue (#1059).

<a id="decision"></a>
## Decision

**1. `scene.set_keyframe { scene_id, keyframe_id?, preset_id? }`** switches the keyframe
scene that `scene_id` is drawn through wherever another scene places it. Omitting
`keyframe_id` switches it back to the scene's own values.
- It applies to any scene that isn't itself a keyframe scene, whatever its role. A group
  is the usual case.
- Each placed scene switches independently of the scene placing it and of every other
  placed scene. Each one is a "track": three placed scenes, with two keyframes each,
  give eight combinations without eight keyframe scenes.
- It is refused with `not_found` for a missing scene, keyframe scene or preset. It is
  refused with `invalid_argument` when `scene_id` is a keyframe scene, or when
  `keyframe_id` isn't one of `scene_id`'s keyframe scenes.

**2. It is studio state, like the program scene.**
- It carries no `rev`, is never undone and is never saved. `studio.get` reports it as
  `keyframes` (`PlacedKeyframe`).
- `scene.keyframe_changed` reports each change, including the release that follows a
  batch, undo or redo that deletes the scene or the keyframe scene. An undo that brings
  the keyframe scene back doesn't switch to it again.
- Keeping it out of the collection is what makes a switch cheap and safe to fire from a
  hotkey or a cue: it never adds to an undo history or conflicts with another client's
  edit.

**3. A placed scene is drawn through the keyframe as a view, the way a keyframe of the
placing scene already draws a group.** The planner has drawn a group's children through
a keyframe scene of the placing scene since ADR-0040 §2. It does that through a view of
the group: the group's items at the keyframe's effective values. A placed scene with a
keyframe switched on is drawn through the same kind of view, so the following all work
unchanged: snapshots, video mixes, previews, audio activity (a child the keyframe hides
is not heard), live values addressed to the keyframe, and the Move machinery.
- **Precedence.** When the placing scene is drawn through a keyframe of its own that
  overrides an item in the placed group's tree, that keyframe draws the group, as it
  always has. The issue asks that "the placing scene's own keyframes keep working".
  Otherwise the placed scene's switched keyframe draws it.
- **Not merged field by field.** The two are not merged one field at a time. A view
  names one keyframe, and merging would need a keyframe scene the collection doesn't
  hold.
- **Program.** It applies only where the scene is placed. A switched scene that is itself
  on program draws its own values, since `scene.switch` already chooses its keyframes
  there.
- **Copies.** The engine hands the planners a copy of the collection that carries the
  switched keyframes, alongside the open gestures' live settings (ADR-0091). That copy
  is the one thing that holds the studio state. The collection itself never holds it.

**4. On program, a switch plays as a Move from what is on screen.**
- When program draws the scene, the engine takes what is on screen, switches, and runs
  a transition from that screen to program as now drawn, pairing each item with itself.
  This is how `item.show` plays an item's effect (ADR-0101).
- A Move pairs a flattened group's children by item. A group drawn as one image (boxed,
  filtered, or not covering the canvas) moves its children inside its layer from one
  view to the other (ADR-0049 §3). A transition in flight is retargeted from where it is.
- The Move takes the timing of preset `preset_id`, or the default preset's when it is
  omitted. Only a `move` or `fade` preset animates. Any other kind, such as the initial
  `cut`, switches at once.

**5. Hotkeys, cues, MCP and plugins.**
- **Hotkeys.** The action can be bound to a hotkey. Its binding names the scene, the
  keyframe scene and the preset, so ADR-0040 §3 removes the binding when any of them is
  deleted.
- **Timeline cues.** It joins the timeline cue set ([ADR-0115](0115-timelines-on-the-wire.md)
  decision 4, as ADR-0115 reserved). It is a state for `timeline.seek`, keyed by the
  placed scene.
- **MCP.** It counts as reversible, because another switch puts the keyframe back and
  nothing stores it, so the `scene` tool stays without `destructiveHint`.
- **Plugins.** Like `scene.switch`, it is in the plugins' `live` class.

**6. Compatibility.** Every change here is an addition, released as a minor protocol
version. The collection file doesn't change.

<a id="consequences"></a>
## Consequences
- **Easier.** A scene whose groups each have a few keyframes switches each group on its
  own, from a hotkey, a cue or a call, with a Move. The number of keyframe scenes grows
  with the number of states, not with their combinations.
- **Harder.** What a placed scene shows depends on studio state that isn't in
  `state.get`, so a client that draws the collection itself has to read `studio.get`'s
  `keyframes` and `scene.keyframe_changed` too. The web UI doesn't show tracks yet.
- **Not decided here:**
  - keeping switched keyframes across a restart;
  - merging the placing scene's keyframe with the placed one field by field;
  - a UI for tracks;
  - a switch per placement rather than per scene.
