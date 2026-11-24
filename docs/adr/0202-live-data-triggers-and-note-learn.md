<!-- @doc
id = "ADR-0202"
title = "Live data triggers fire bindings, and a client learns notes"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0202: Live data triggers fire bindings, and a client learns notes

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A binding runs a protocol action when a MIDI note, a control change or a program change
arrives ([ADR-0122](0122-controller-input-midi-and-one-binding-system.md)), and the
engine fires them since [ADR-0207](0207-mute-actions-and-held-triggers.md). A JTunnel
sender's live data ([ADR-0171](0171-live-data-from-a-daw-drives-scenes.md)) can move
motion, channels and value bindings, but it can't run an action. The maintainer wants a
Bitwig scene being selected or launched, and a JTunnel Mod crossing a level, to run show
actions such as going to a bookmark, and the alpha.12 mapping design lists data triggers
and learning a note for the Triggers UI.

<a id="decision"></a>
## Decision
1. **A `data` trigger.** `Trigger::Data {input?, key, field, on}` is a fourth kind of
   binding trigger, and a minor protocol version, 0.178
   ([R-API-001](../RULES.md#R-API-001)).
   - It reads a `data` motion modifier's path: the sender (`input`, missing for any), the
     message's `key` and a dotted `field` (empty for the whole message).
   - A `*` that is a whole key segment matches any one segment, so `track/*` listens to
     every track. The condition holds while it holds for any matching key.
   - `on` (`DataOn`) is `equals {value}`, `changes`, `rises {level}` or `falls {level}`.
     `equals` compares the same text, or the number, `true` or `false` that `value`
     spells. `rises` holds at the level or above, and `falls` holds below it.
2. **It fires as a MIDI trigger does.** The engine's values thread reads each data
   trigger's field after the MIDI bindings on every tick, and runs the bound action
   through the engine's action path, credited to the engine.
   - `equals`, `rises` and `falls` fire when their condition starts to hold. When it
     stops, they run the action's release (`Action::on_release`). So a Mod held above a
     level works as push to talk.
   - `changes` fires on each new value and has no release.
   - A press is let go when its binding changes or is removed.
3. **Only from used state.** The trigger reads the engine's live data, which holds only
   the state of senders whose "Use this sender's state" is on (ADR-0171 decision 1). So
   a LAN sender the person hasn't let in fires nothing. That switch, not "Use this
   sender's MIDI" (ADR-0123 §3), gates it, because data is state.
4. **It starts from where things are.** A field's value when the engine starts
   following a trigger, or when a key first arrives, fires nothing. Connecting Bitwig
   with scene 3 already selected doesn't jump the show. A key that goes away counts as
   not holding.
5. **At most once every 50 ms.** A trigger fires at most once every 50 ms, so a field
   that flickers doesn't run its action on every message.
6. **`midi.last_notes`.** This query answers `MidiNoteInfo {input, channel, note,
   velocity, count}` for each MIDI input's latest note-on, where `count` is the input's
   note events. A client learning a note watches for the input whose count grows, as
   `midi.last_controls` does for a controller.

<a id="consequences"></a>
## Consequences
- **Easier:** Bitwig drives a show. Selecting scene 3 (`grid`, `selectedScene`,
  `equals 3`) or launching it on any track (`track/*`, `playing`, `equals 2`, the
  zero-based scene index) goes to a bookmark, and a Mod above half (`params`,
  `values.19`, `rises 0.5`) presses Continue.
- **Easier:** the Triggers UI learns a note the way it learns a controller.
- **Harder:** each tick reads each data trigger's message from the live data store under
  its lock. A `*` key also lists that sender's keys. The values thread already ticks 30
  to 60 times a second while a binding exists.
- **Not decided here:** a timeline that starts on a data trigger. Its own trigger list
  (`TimelineTrigger`) has no `data` kind yet; a binding can run `timeline.play`
  instead.
