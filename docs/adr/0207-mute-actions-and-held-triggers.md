<!-- @doc
id = "ADR-0207"
title = "Mute actions, held triggers that release, and switches as value targets"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0207: Mute actions, held triggers that release, and switches as value targets

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A source's or bus's mute is collection state, changed by the edits `source.set_audio`
and `bus.set_fader` ([ADR-0028](0028-audio-model-v2.md)). Keys and MIDI triggers run
actions only, never edits ([ADR-0024 §5](0024-scene-structure.md),
[ADR-0122](0122-controller-input-midi-and-one-binding-system.md) decision 1), so no key
or pad could mute anything. The only way round it was a channel bound to a fader, which
can't say "muted".

Three things a live show needs follow from that gap:

- **A mute key**: one press mutes, the next unmutes.
- **Push to talk**: the microphone is live only while a key or pad is held. Its
  opposite, **push to mute** (a cough key), mutes only while held.
- **A mute or a visibility driven by a control**: a MIDI button, a Mod or a channel
  switching a mute, or whether an item shows, the way it moves a fader
  ([ADR-0178](0178-value-bindings-drive-any-number.md)).

Holding needs a release, and nothing reported one. A hotkey's client sends its action on
the key's press. And although `binding.set` has stored MIDI action bindings since 0.96,
and the protocol says the engine runs them, nothing in the engine did: the MIDI each
input sent was recorded for the visualization sources and for value bindings, and no
code matched it against a binding's trigger.

The only MIDI input today is a JTunnel sender. The maintainer-tier
[ADR-0123](0123-lan-inputs-jtunnel-and-visca.md) §3 bounds what one can do: its MIDI
drives bindings only once the person turns on "Use this sender's MIDI" for it, and
[ADR-0142](0142-feeding-the-midi-sources-and-their-beat.md) §2 kept JTunnel's MIDI away
from the binding system because that switch didn't exist yet. It still doesn't.

Muting on air must never click or drop a frame. The mixer already reads each input's and
bus's gain and mute from atomics on every block and ramps from the last block's gain to
the new one over the block, so a mute set between two blocks fades in one block (about
10 ms) and reopens nothing.

<a id="decision"></a>
## Decision
1. **New actions `source.mute { source_id, to }` and `bus.mute { bus_id, to }`**, `to`
   one of `muted`, `unmuted`, `toggle`, `push_to_talk` and `push_to_mute` (`MuteTo`).
   Protocol 0.187. They are collection state exactly as `item.show` is
   ([ADR-0101](0101-item-show-hide-actions.md) decisions 2 and 3): the engine applies
   the `source.set_audio` or `bus.set_fader` they stand for, `rev` moves on by one,
   `state.changed` follows and the collection saves it, and they are never an undo step,
   though another client's undo that would overwrite them fails with `undo_conflict`.
   Setting the mute a target already has changes nothing and emits nothing. A source with
   no audio, and a `scene` source (which has no mute of its own), are refused as the edit
   refuses them. The change reaches the mixer through the same path as the edit, so it
   ramps over one block as any mute does.
2. **A held trigger releases.** `Action::on_release` gives the action a held key or note
   runs when it is let go, or `None`. Only three cases have one: `push_to_talk` (pressed
   it unmutes, released it mutes), `push_to_mute` (the opposite), and `channel.switch`'s
   new `hold_on` and `hold_off` (on or off while held, back on release), so a channel can
   be held too. Sent as a plain call, with nothing held, each is its press alone. The
   release is derived from the bound action, so `hotkey.set` and `binding.set` keep their
   wire shape and nothing new is stored: a collection that binds a key to push to talk is
   one binding.
3. **Who runs a release.** For a hotkey, its client: the web UI sends the release when the
   chord's last key goes up, and sends every held release when the page loses the focus
   or is hidden, so a missed key-up never leaves a microphone open. The desktop app's
   global hotkeys report key-up the same way (each platform's listener, in its own PR).
   For a MIDI binding, the engine (decision 4).
4. **The engine runs MIDI action bindings.** Each input's state keeps a log of its control
   and program changes beside its note log, lock-free as the note log is
   ([ADR-0122](0122-controller-input-midi-and-one-binding-system.md) decision 2). The
   `jstream-values` thread, which ticks about 30 times a second (60 while a value moves)
   while the collection holds a binding, reads each log since its last look and runs, in the order heard: a note
   binding's action on note-on and its release on note-off; a control change binding's
   action when the controller crosses 64 upward and its release when it falls back below;
   a program change binding's action. A press and release between two ticks both run.
   Nothing sent before the first look runs. The actions run as their calls would, credited
   to the engine, with no audit entry, as a timeline's cue has none. **Only an input the person let in is read**, as ADR-0123 §3 decided:
   each accepted JTunnel sender gains "Use this sender's MIDI", off by default (the
   desktop app's checkbox beside "Use this sender's state", `midi` in its `jtunnel.json`,
   the daemon's `--jtunnel-midi <name>`), and the engine reads only the senders that have
   it on (`MidiInputs::bindable`). A binding with no `input` listens to each of them, not
   to the every-input state, which also holds the senders left out. This replaces
   ADR-0142 §2's "it doesn't reach the binding system" for action bindings, within
   ADR-0123's bounds; value bindings and channels, which read a sender's MIDI without the
   switch today, are left to their own change. **A held press never outlives its
   source.** The engine keeps each press whose action has a release until that release is
   heard, and runs the release itself as soon as the input is no longer let in, the
   binding is cleared or runs another action, or the MIDI inputs go. A JTunnel sender
   marks its input alive with every frame it really sends, MIDI or not, so the engine
   also lets its presses go once it has sent nothing for 500 ms; a MIDI port, which keeps
   no link alive, holds a key until its note-off. Firing on the values tick, not on the render tick of the message's
   time, adds up to one tick of latency; that is fine for a mute and leaves
   beat-accurate firing to a later change.
5. **Switches are value targets.** `ValueTarget` gains `source_mute`, `bus_mute` and
   `item_visible { item_id, scene_id? }`. A value binding drives them as it drives a
   `bool` setting: on at one half or more, applied as a live value through the shown
   graph, never stored. A mute set this way is heard as any mute is; a visibility set
   this way cuts, with no appear or disappear effect, since it is a live value and not an
   `item.show`. Since 0.198 a fourth switch, `filter_enabled { owner, filter_id }`,
   turns a filter on any chain on or off as `filter.set_enabled` would, so a sequence's
   Switches lane can drive it (#1819).
6. **Bindings name what they mute.** `named_objects` names the source or bus, so deleting
   it removes the bindings with it (`bus.delete` now does this too, as
   [ADR-0040 §3](0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md) has
   the other deletes do). Plugins get the actions with the `edit` class, as
   `source.set_audio`. They are in MCP's `REVERSIBLE_ACTIONS`
   ([ADR-0098](0098-mcp-destructive-hint-for-what-undo-cant-take-back.md)): the opposite
   mute puts it back.

<a id="consequences"></a>
## Consequences
- **Easier:** a mute key, push to talk and a cough key are each one binding, on a key or a
  MIDI pad, and any MIDI note, control change or program change binding now fires.
- **Easier:** a MIDI button, a Mod or a channel can drive a mute or an item's visibility
  through the same control button as a fader.
- **Harder:** a mute pressed from a key is not in anyone's undo history; pressing again is
  the way back, as with `item.toggle`.
- **Harder:** a MIDI pad does nothing until its sender's "Use this sender's MIDI" is
  ticked; the binding's UI should say so when none is.
- **Harder:** a release depends on the client hearing the key go up. The web UI releases
  on blur; a desktop listener that can't hear key-up on its platform can't offer a held
  key there.
- **Must do:** put value bindings and channels behind "Use this sender's MIDI" too, as
  ADR-0123 §3 says.
- **Must do:** the desktop app's global hotkey listeners report key-up and send releases.
  If several actions per trigger land, the runner and the clients run every action's
  press and every action's release, in order.
