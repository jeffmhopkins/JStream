<!-- @doc
id = "ADR-0168"
title = "MIDI controllers drive motion: value bindings as a `midi` motion modifier"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0168: MIDI controllers drive motion: value bindings as a `midi` motion modifier

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Delivers the value bindings of [ADR-0122](0122-controller-input-midi-and-one-binding-system.md)
decision 1, and decides their wire shape, which that decision left to the PR that adds
them. Action bindings, triggers and the binding list are unchanged.

<a id="context"></a>
## Context
ADR-0122 decision 1 promises **value bindings**: a control change (or pitch bend, or
channel pressure) drives a number, an item property or a filter setting, mapped from the
controller's range onto a range the binding names. It is live, applied as it is drawn,
never stored and never an edit, so a fader fills no undo history.

The maintainer wants exactly that: a fader or knob, from a local port or a JTunnel
sender, that moves one object continuously between two poses, like a keyframe range
for a single item played live, with a smoothing time that can be 0 when the value
should jump.

Two systems already do most of it:

- **Motion modifiers** ([ADR-0099](0099-native-motion-modifiers-and-timelines.md))
  change an item's position, depth, angles, scale, opacity and crop each frame, without
  storing anything, on a group's item too, and in a keyframe scene's own list.
  [ADR-0138](0138-motion-drives-effect-settings.md) lets one drive a number or colour
  setting of the item's own effects.
- **Audio-reactive modifiers** ([ADR-0100](0100-audio-reactive-motion-modifiers.md))
  follow an input instead of the clock: the render thread's follower reads it once a
  tick, smooths it per item and modifier, and the modifier maps the result from `from`
  to `to` through a curve.

The engine's MIDI inputs ([ADR-0137](0137-midi-visualization-sources.md)) already keep
every controller's latest value per channel in atomics, which any thread reads without
a lock.

<a id="decision"></a>
## Decision
1. **A value binding is a `midi` motion modifier.** `MotionKind` gains `midi`, a minor
   protocol version (0.135, [R-API-001](../RULES.md#R-API-001)). Its parameters:

   | Field | Meaning | Default |
   |---|---|---|
   | `control` | `cc`, `pitch_bend` or `pressure` (channel pressure) | `cc` |
   | `controller` | The controller number, 0 to 127, with `cc` and only then | required with `cc` |
   | `channel` | 1 to 16, as a device's panel shows it; omitted for any | any |
   | `input` | A MIDI port or JTunnel sender by name, as a trigger names one; omitted for any | any |
   | `smoothing_ms` | The time constant of a one-pole smoothing, 0 to 10000; 0 jumps | 50 |
   | `curve` | `linear`, `ease_in`, `ease_out` or `ease_in_out`, as an audio modifier's | `linear` |
   | `from`, `to` | The value at the controller's lowest and highest, within ±100000; `to` below `from` inverts | 0, 1 |

   So every property a modifier moves, and every effect setting one drives, can follow
   a controller, with no second list of targets and no second way to name one. The
   binding is kept where the item's motion is: in the item, or in a keyframe scene's own
   list, so a Move between keyframes can hand a fader from one range to another.
   `item.set_motion` sets it, with its undo, and the Motion section edits it.
2. **The value.** A control change and channel pressure map 0 to 127 onto 0 to 1; a pitch
   bend maps -8192 to 8191 onto 0 to 1 with the wheel at rest at one half. With no
   channel the modifier reads the latest value sent on any channel, which the input's
   state keeps beside its per-channel values. The 0 to 1 value is smoothed, bent by the
   curve and mapped onto `from..to`. Before the controller sends anything it reads 0 (a
   pitch bend one half), so the item rests at `from`.
3. **Smoothing.** The render thread's follower (ADR-0100 decision 4) keeps a smoothing
   per item and modifier ID beside the audio envelopes, and once a tick moves it toward
   the control's value by `1 − e^(−dt/τ)`, with τ the smoothing time; 0 jumps. Rising and
   falling share the time, since a fader has no attack. Its first step lands on the
   value, so an item that appears shows the controller where it is instead of gliding
   from 0. It carries across a new shot with the same item and ID, as an envelope does.
4. **Where it reads.** Planning records each MIDI modifier with its input's state: the
   named input's, or every input's together. Reading is a few atomic loads, so the render
   thread neither locks nor allocates ([R-RT-003](../RULES.md#R-RT-003)). A shot no
   follower follows (a scene or source preview) reads the control as it is now,
   unsmoothed, so previews and the canvas show the controller too, unlike audio, which
   is silent there.
5. **Not on cameras.** A scene camera's motion refuses `midi`, as it refuses `audio`: no
   follower runs for cameras. A camera can ride with a moving group once
   ADR-0164 lets groups share a 3D scene's space, which a `midi` modifier
   on that group then drives.
6. **Parts of a model.** A modifier moves a whole item. A model's nodes (ADR-0107) are
   not addressable, and this ADR doesn't make them so: a part that moves on its own (a
   lever, a door) is a model file of its own, placed as its own item. A model turns
   about its bounds' centre, so a part that turns about another point (a lever about
   its base) is placed in a 3D group (ADR-0164) whose centre is that point, and the
   modifier drives the group. Addressing nodes inside one model is left to a later ADR if
   separate files prove too coarse.

<a id="consequences"></a>
## Consequences
- **Easier:** any number a modifier moves, on any item, effect or group, follows a
  fader, knob, wheel or pedal, from a local port or a JTunnel sender, and agents set it
  with the same `item.set_motion` they already use.
- **Easier:** a smoothing of 0 gives a jump for switches and cuts, and a longer one hides
  a controller's 128 steps on a large move.
- **Harder:** a modifier list holds at most 8, shared between motion and bindings; a rig
  with more uses a group, as motion already does.
- **Not decided here:** 14-bit controllers (MSB and LSB pairs), a learn mode that binds the
  next controller moved, and an input range narrower than 0 to 127. Each is additive.
- **Must do:** the maintainer's hardware run checks a real controller end to end, over a
  local port and over JTunnel; CI only feeds messages in tests.
