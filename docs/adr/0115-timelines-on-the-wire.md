<!-- @doc
id = "ADR-0115"
title = "Timelines on the wire: global IDs, inert cues, studio-state playback and source.restart"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0115: Timelines on the wire: global IDs, inert cues, studio-state playback and source.restart

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Builds [ADR-0099](0099-native-motion-modifiers-and-timelines.md) decision 4 (timelines),
whose scope the maintainer accepted. ADR-0099 says that everything below its scope decision
is design, and that later changes to it are agent tier. This ADR records the choices
ADR-0099 left open, so the protocol PR doesn't have to make them.

<a id="context"></a>
## Context
ADR-0099 decision 4 gives a scene named timelines of timed cues, played by
`timeline.play`, `timeline.stop` and `timeline.seek`, and fired on the animation clock.
`jstream-anim` already holds the schedule (`Timeline::due`, `Timeline::seek`). Nothing is on
the wire yet, so a timed sequence (lock on, four volleys at set times, an explosion) still
has to be one browser page.

Several questions are still open:
- **Naming.** How does an action name a timeline: by scene and ID, or by ID alone? A hotkey
  bound to a timeline, or a cue that starts another timeline, has to name it.
- **References.** A cue names objects (an item, a scene, a source, a timeline). What
  happens when one of them is deleted? ADR-0040 §3 removes a hotkey binding with its
  target. A timeline is authored timing, though, not one binding.
- **Who a cue's action is credited to.** An `item.show` cue moves `rev`, and its
  `state.changed` needs a client.
- **Restarting a source.** ADR-0099 lists "restart a browser source" as a cue action. No
  action does that yet.
- **Plugin scope.** A cue can run actions the edit class can't run.

<a id="decision"></a>
## Decision

**1. A timeline's ID is unique across the collection.** `timeline.set { scene_id, timeline }`
stores a timeline in a scene, so the scene still holds it. Its `timeline_id` is unique
across the collection, though, so `timeline.play`, `timeline.stop` and `timeline.seek`, a
hotkey, and a cue all name it by ID alone. A second scene can't take an ID already in use
(`already_exists`). A scene's timelines are ordered by ID, so no write ever reorders them,
and an undo puts one back exactly. `timeline.delete { timeline_id }` deletes one.
`EntityKind::Timeline` names one in errors.

**2. The objects a cue names are not checked, and a deleted one leaves the cue inert.**
`timeline.set` checks a timeline's shape: its ID, its length, its loop, cues in time order
and reachable (`TimelineInfo::problem`, which agrees with `jstream-anim`'s check), and each
cue's action in the closed set (decision 4). It doesn't check that the item, scene, source
or timeline a cue names exists. Deleting an object never edits a timeline. A cue whose
object is gone stays where it is. When it fires, its action fails as the call would fail,
and `timeline.cue_fired` carries the error. ADR-0113's follow camera already treats a
deleted item this way, and for three reasons it fits here too:
- deleting an item never shifts a sequence's timing;
- undoing the delete makes the cue work again;
- an undo or a collection load can restore timelines and the objects they name in any
  order.

Hotkeys are different. A binding to `timeline.play`, `timeline.stop` or `timeline.seek` is
removed with its timeline, as ADR-0040 §3 removes any binding with its target, and comes
back on undo.

**3. Playing is studio state, and each cue runs as its call would run.**
- **What playing is.** The engine keeps what is playing beside the program scene: no
  `rev`, never undone, never saved, and `studio.get` reports it. An undo that brings a
  deleted timeline back doesn't play it again.
- **Starting and stopping.** `timeline.play` starts a timeline from 0, or from 0 again if
  it is playing. `timeline.stop` stops it where it is. `timeline.seek { at_ms }` plays it
  from `at_ms` on its first pass.
- **How a cue runs.** A cue runs its action through the same path as a client's call, with
  that call's effects and events. It is credited to the client and request that started
  the timeline. While it runs, the engine takes the cue's exact time as now, so a
  transition, an item's appear effect or a fired motion that the cue starts begins at the
  cue's time, however late the tick. A cue that fails doesn't stop the timeline. A cue
  isn't a call, so it gets no audit or activity entry of its own: the `timeline.play`
  or `timeline.seek` that started the timeline has one, and `timeline.cue_fired`
  reports each cue.
- **Ticks.** The live ticker fires cues. It already runs on the control thread for live
  values, and it sleeps until the next cue or the end of a pass. A client's
  `timeline.play` runs the first tick before it returns, so a cue at 0 has run by then.
- **Seek.** A seek first runs the state cues before `at_ms`: the last one for each thing a
  cue sets, as `Timeline::seek` gives them. The program scene, one placement's
  visibility, and one shared value are states. A toggle, a fired motion, a restart, and
  another timeline's play, stop or seek are events, which a seek doesn't run again. A cue
  exactly at `at_ms` fires as the timeline plays on from there.
- **Rounds.** A cue that plays another timeline starts it at the cue's time, so that
  timeline's own first cues fire in the same tick. Timelines that start each other at 0
  would never settle, so one tick runs at most 16 rounds of cues, and the rest wait for
  the next tick.
- **Removal.** A batch, undo or redo that deletes a playing timeline, or its scene, stops
  it with `timeline.changed` `removed`. Changing a playing timeline keeps its start, and
  its cues fire at their new times from then on.
- **Events.** `timeline.changed` reports a start, a seek, a loop, an end, a stop and a
  removal. `timeline.cue_fired` reports each cue, with its error if it has one. Neither
  is ever sent once a frame.

**4. The cue actions are a closed set:** `scene.switch`, `item.show`, `item.hide`,
`item.toggle`, `values.set`, `source.restart`, and `timeline.play`, `timeline.stop` and
`timeline.seek`, as ADR-0099 lists them, plus `item.trigger_motion`. That last one fires a
one-shot motion (#1107), which is a sequence's most common step after a show.
- `Action::cue_problem` refuses every other action, so a cue never goes live, never stops
  an output, and never touches a key or a device.
- None of the allowed actions is an edit, so playing a timeline adds nothing to an undo
  history.
- ADR-0099 also names "switch a group to a keyframe" (#1068). That cue joins the set with
  the keyframe-track action, in its own PR.

**5. `source.restart { source_id }`** restarts a source whatever its `restart_on_show`
setting. A `browser` page loads again, a `media` file plays from its start, and an effect
pack's shader source starts its program again. Other kinds have nothing to restart, so for
them it succeeds and does nothing. It is runtime state like a switch's restart. ADR-0098
counts it as reversible for MCP: a `scene.switch` to a scene that sets `restart_on_show`
restarts sources the same way.

**6. The plugin scope is "never" for now.** A cue can run `scene.switch` and `values.set`,
which the edit class alone can't. So `timeline.set`, `timeline.delete`, `timeline.play`,
`timeline.stop` and `timeline.seek` are "never" for plugins until a PR decides which class
may use them. `source.restart` changes what is on air, as a switch does, so it is in the
live class. For MCP, the `timeline` tool is `destructiveHint`, since its cues may set a
shared value that a page may send off the machine.

**7. Compatibility.** Every change here is an addition and lands as a minor protocol
release. The collection gains `scenes[].timelines` at `schema_version` 6, with a migration
and the version 5 fixture. `TimelineChange` parses an unknown value as `unknown`. A
cue's action is plain JSON on the wire, as a hotkey binding's is, so a client that
doesn't know an action a later version adds to the cue set still reads the collection;
a server refuses a cue whose action it doesn't know.

<a id="consequences"></a>
## Consequences
- **Easier:** an agent builds a timed sequence from native items with one `timeline.set`
  and plays it from a hotkey or a call. It reads what happened from `timeline.cue_fired`,
  which carries each cue's error.
- **Harder:** a typo in a cue's item ID isn't refused when the timeline is written. It
  shows up as an error when the cue fires. Agents should read `timeline.cue_fired` after a
  first play.
- **Not decided here:**
  - a UI timeline editor;
  - the cue that switches a keyframe track (#1068);
  - curves per property (ADR-0099 leaves them out);
  - whether a timeline should stop when its scene leaves program. Today a timeline plays
    whatever is on program, since its cues name their own targets.
