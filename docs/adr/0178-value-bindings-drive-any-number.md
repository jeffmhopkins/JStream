<!-- @doc
id = "ADR-0178"
title = "Value bindings: a controller or live data drives any number"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0178: Value bindings: a controller or live data drives any number

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Decides the wire shape of [ADR-0122](0122-controller-input-midi-and-one-binding-system.md)
decision 1's value bindings, which that ADR left to the protocol PR that adds them.

<a id="context"></a>
## Context
ADR-0122 decided one binding system: a trigger fires an action (`binding.set`, a hotkey's
twin), and a value binding lets a controller drive a number, "applied as it is drawn,
never stored, never an edit". [ADR-0168](0168-midi-controllers-drive-motion.md) built the
second half for items only, as `midi` motion modifiers, [ADR-0171](0171-live-data-from-a-daw-drives-scenes.md)
added `data` ones and the item's own source's light and model settings, and
[ADR-0176](0176-controllers-and-live-data-drive-scene-cameras.md) added cameras.

The maintainer wants a controller assignable to every number in JStream, through that one
system, for the next alpha. An audit found what still can't take one: filters on a
source, a scene, the master stage or an output (only an item's own effects can); the
settings of every source kind but a light and a model (the modifier is accepted and
changes nothing); a source's fader, a bus's, a send and every audio filter setting; and an
item's width and height apart. A motion modifier lives on an item or a camera and adds to
what it draws per frame; none of these has a motion list, a fader is heard rather than
drawn, and a source is shared by every item that places it.

The engine already has a path that draws and hears the collection with values that are
not stored: a gesture's live source settings (ADR-0091) become a "shown" graph, the
collection with those setters applied, which the live sources, the mixer and the render
plan follow.

<a id="decision"></a>
## Decision
1. **A value binding is a collection object.** `binding.set_value` sets one by
   `binding_id`, whole, and `binding.clear_value` removes it; `state.get` lists them as
   `value_bindings` and the collection file saves them (schema 18). A binding reads
   exactly what a `midi` or `data` motion modifier reads, with the same fields, ranges,
   smoothing and curve (its `kind` and parameters, flattened), so an agent learns one
   shape. At most 256 per collection. A minor protocol version, 0.168
   ([R-API-001](../RULES.md#R-API-001)).
2. **Targets.** `source_setting` and `filter_setting` (any chain owner, video or audio)
   name a setting its kind marks `live` of type `int`, `float` or `bool`; `source_gain`,
   `bus_gain` and `send_gain` name a fader or an existing send. `binding.set_value`
   checks the target exists and its setter takes it. An item's placement, its effects and
   its own source's settings, and a camera, keep their motion modifiers, which add to
   what is stored per item and keyframe; a value binding is for what has no motion list.
3. **It replaces the value.** A binding's mapped value is the setting's while it drives
   (kept within the setting's range, rounded for an `int`, on at one half for a `bool`), as
   a knob mapped in a DAW sets its parameter, rather than adding to it as a modifier does.
   It drives once its control first reads something other than it read when the engine
   started following it, so a collection opens as it was saved and a controller at rest
   overrides nothing.
4. **Drawn and heard as live values.** A `jstream-values` thread ticks every binding
   about 30 times a second while the collection holds one: it reads each control from the
   same atomics a modifier does, smooths it with the same one-pole smoothing on the tick's
   time, and turns each driving binding into its target's setter edit. When those change,
   the engine draws and hears the collection with them on top of the open gestures'
   values, through the shown graph a live source setting already uses. Nothing is stored:
   `state.get`, undo and the file keep the stored value, and clearing the binding gives it
   back. A moved control wins over a gesture on the same setting.
5. **Left in place when its target goes.** Unlike an action binding, which leaves with
   the object it names (ADR-0040 §3), a value binding whose source, filter, bus or send
   is deleted stays and drives nothing, as a timeline's cue does, and drives an object
   with the same ID again. A file loads its value bindings without looking their targets
   up, so a binding to a filter of a pack not installed survives.
6. **Learn.** `midi.last_controls` reports each MIDI input's latest control change and how
   many it has sent. The UI puts a learn button beside every live number and switch of a
   source's or filter's settings, every fader and every send: it binds the first control
   whose count grows over the setting's whole range (a fader's over -60 to +6 dB), shows
   the binding's control, and unbinds it on a click.
7. **Width and height.** `MotionProperty` gains `width` and `height`, which stretch an
   item's box on one axis about its centre, multiplied by `1 + value` after `scale`, so a
   controller, or any modifier, can stretch an item.

<a id="consequences"></a>
## Consequences
- **Easier:** every number the audit found takes a controller or a DAW's data, through
  one call an agent uses and one button a person clicks, alongside the modifiers that
  already drive items and cameras.
- **Harder:** a driving binding redraws through the shown graph, so each tick that changes
  a value clones the collection and replans, as a gesture on a source setting does; ticks
  that change nothing cost a read of each control. Two value bindings on one setting:
  the later in ID order wins.
- **Harder:** a control that reads where it started, or flicks out and back within one
  tick, has not moved, so it drives nothing until it moves again.
- **Not decided here:** a data field learned from the UI (an agent sets one today), and a
  value binding on a camera or an item, which take modifiers.
- **Must do:** the maintainer's hardware run moves a real fader through JTunnel onto a
  filter, a source's setting and a bus fader; CI feeds messages in tests.
