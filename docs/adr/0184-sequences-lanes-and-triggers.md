<!-- @doc
id = "ADR-0184"
title = "Sequences: lanes on timelines, and what starts them"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0184: Sequences: lanes on timelines, and what starts them

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Channels ([ADR-0183](0183-virtual-control-channels.md)) give a show named live values that
controls set and anything follows. ADR-0183 decision 8 left two things to a later ADR:

- moving a few channels over time, such as a power-on sequence that brings groups of lights
  up in order;
- starting such a sequence when something happens: a master switch, a scene going on
  program, a keyframe loading.

Timelines ([ADR-0099](0099-native-motion-modifiers-and-timelines.md) decision 4,
[ADR-0115](0115-timelines-on-the-wire.md)) already play, stop, seek and loop on the
animation clock. They run instant actions as cues, report what is playing in `studio.get`,
and a hotkey or a MIDI binding can play one. They have no way to move a value over time,
and nothing but a call or another cue starts them.

A keyframe repositions a whole scene and becomes what is shown, so it can't be the place
for "fade these three channels while the scene stays as it is". A sequence must start from
wherever its values are. Otherwise a "lights down" played halfway through "lights up"
jumps, and so does a move on a position or a camera that something else left elsewhere.

This is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)), and it amends ADR-0115, which is
agent tier by ADR-0099's own scope note. It adds protocol fields, minor and additive
([R-API-001](../RULES.md#R-API-001)), and changes no product scope, licence, security or
privacy posture, compatibility promise or spending.

<a id="decision"></a>
## Decision
**1. A sequence is a timeline with lanes.** On the wire it stays `timeline.*`. The UI,
docs and MCP guidance call one with lanes a sequence, as a scene `variant` is called a
keyframe. A timeline gains `lanes` (at most 32), and each lane has:

- a `target`, `{channel}`;
- `points` in time order (at most 256): `at_ms` (at most the timeline's `duration_ms`),
  `value` (0 to 1) and the `curve` into the point (`linear`, `ease_in`, `ease_out`,
  `ease_in_out` or `hold`, which stays and then jumps);
- `on` (a lane that is off does nothing).

A lane on any other setting is a lane on a channel that setting follows, through a value
binding or a `channel` modifier (ADR-0183 decision 5). So positions, sizes, materials and
cameras get lanes the same way faders and filter settings do.

**2. A lane is one more input of its channel.** It takes its channel when the timeline
starts or is sought. It keeps it until something else changes the channel: under the
default `latest`, a control moved, an action, or a lane of a timeline started later. The
last started wins, so an older timeline lets go of that channel and keeps running its cues.
The channel's switch, averaging, fade and curve apply after the lane, as after any input.
`channel.values` reports `{by: "sequence", timeline_id}` as the driver.

**3. A lane starts from the current value.** A lane with no point at 0 starts from the
channel's value at the moment its timeline starts (before the channel's curve). It moves
from there to its first point along that point's curve. "Lights down" is then one point
(0 at 3 s), and it works from any level. A position or a camera move driven through a
channel glides from where it is, with no jump. Each pass of a loop after the first starts
from the lane's last point. A seek moves each lane to its value at that time, from the
value it started from.

**4. At the end, channels hold.** When a `once` timeline ends, each lane sets its last
point's value and holds it. A stopped timeline's lanes hold where they are.

**5. Lanes move at frame rate.** While any lane drives a channel, the `jstream-values`
thread ticks about every 16.7 ms instead of 33.3 ms, so a fade shows no steps.

**6. `timeline.play` takes `delay_ms`** (0 to 60,000). The delay belongs to the play
action, so a hotkey, a binding, a cue, MCP and every trigger get it:

- a play that waits is runtime state, never saved or undone;
- another play starts the wait again, and `timeline.stop` cancels it;
- `studio.get` lists the waiting timelines as `waiting`, each with `due_in_ms`.

**7. Triggers: a timeline's `starts` and `stops`** (at most 16 each). Each entry is tagged
by `type`, carries its own `delay_ms`, and plays the timeline from its start, or stops it,
when it fires:

| `type` | Fields | Fires when |
|---|---|---|
| `channel` | `channel_id`, `edge` (`rises`, `falls`, `on`, `off`), `level` (default 0.5) | The value crosses the level, or the switch changes |
| `scene` | `scene_id`, `edge` (`shown`, `hidden`) | The scene goes on or off program, by any route |
| `keyframe` | `scene_id`, `keyframe_id` (omitted means any), `edge` (`loads`, `leaves`) | The scene starts or stops drawing through the keyframe, as program or as a placement |
| `timeline` | `timeline_id`, `edge` (`starts`, `ends`, `loops`) | Another timeline does that |

Triggers fire on a change only. The first check after the engine starts, or after a
collection opens, sees the state and fires nothing. A channel trigger fires at most once
every 50 ms, and must go back past a 0.02 dead band before the same edge fires again.

Two things are refused by `TimelineInfo::problem`:

- a timeline starting or stopping itself (it has a loop for that);
- a `starts` on a channel one of its own lanes drives.

Timelines that start each other in a ring are capped as cues are
([ADR-0115](0115-timelines-on-the-wire.md)).

**8. Keys and MIDI stay in the binding system.** A hotkey or a controller binding to
`timeline.play` or `timeline.stop` is how a key, a note or a CC starts a sequence. One
trigger runs one action there. State triggers live on the sequence, so they copy with it,
and many sequences can listen to the same thing.

**9. Delivery.** Protocol 0.170 and collection schema 21. `v20_to_v21` refuses lanes,
triggers and a play's delay in an older file. A note-off trigger, a falling-CC trigger,
item visibility, output, media and project triggers, switch lanes on filters and mutes, and
the UI follow. Each is additive.

<a id="consequences"></a>
## Consequences
- **Easier:** a power-on sequence is one timeline. Its lanes bring channels up in order,
  its cues show what must appear, and its `starts` names the master channel rising. A
  "lights down" played at any moment glides from where the lights are.
- **Easier:** one clock, one set of play actions and one report for cues and lanes, with
  the timeline's existing undo, labels, loops and bindings.
- **Harder:** a channel can now be driven by a sequence as well as by its inputs and
  actions. `channel.values`' driver says which, and `latest` decides between them.
- **Harder:** the `jstream-values` thread now runs while any timeline has a trigger or a
  wait, and ticks twice as often while a lane plays. Both are bounded by the limits above.
- **Not decided here:** project-level sequences, an offline preview of a sequence that
  doesn't reach program, key release for momentary keys, and recording a lane from a
  control. Each is additive.
- **Must do:** the maintainer's hardware run starts a sequence from a pad and from a
  keyframe after a delay, and watches a lane glide a light from wherever it was.
