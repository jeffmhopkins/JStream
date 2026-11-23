<!-- @doc
id = "ADR-0183"
title = "Channels: named live values that controls set and anything follows"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0183: Channels: named live values that controls set and anything follows

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Controls reach the collection in three ways today, each tied to the control that drives it:

- **Motion modifiers** follow a MIDI control (`midi`, [ADR-0168](0168-midi-controllers-drive-motion.md))
  or a JTunnel sender's live data (`data`, [ADR-0171](0171-live-data-from-a-daw-drives-scenes.md)),
  on items, their effects and source settings, and cameras
  ([ADR-0176](0176-controllers-and-live-data-drive-scene-cameras.md)).
- **Value bindings** ([ADR-0178](0178-value-bindings-drive-any-number.md)) let the same two
  readers replace any live setting, fader or send.
- **Action bindings and hotkeys** ([ADR-0122](0122-controller-input-midi-and-one-binding-system.md))
  run one protocol action when a key chord, a note, a CC crossing or a program change fires.

What a show built on these runs into:

- **No value lives in JStream itself.** Every reader names a controller or a data field. Ten
  lights that should fade together carry ten copies of one CC, and rebinding the controller
  means editing all ten. A hotkey can't drive a value at all: an action is instant, so a key
  can't fade a group of lights up, and a CC reads 0 until it is first sent.
- **No processing between a control and what follows it** apart from each reader's own
  smoothing: no averaging of a jittery data field, no separate rise and fall fade, no
  on/off that holds a level.
- **One control can't set how much another applies.** A knob that scales how strongly a
  camera leans with a controller, or a switch that gates a whole set of modifiers, has no
  place to live.
- **Timed changes of a few values.** A scene keyframe ([ADR-0099](0099-native-motion-modifiers-and-timelines.md))
  repositions a whole scene and becomes what is shown; a timeline's cues run instant
  actions. Neither fades a handful of values over time on its own track while the scene
  stays as it is, such as a power-on sequence that brings groups of lights up in order.

The engine already has every piece needed underneath: lock-free control readers, the one-pole
smoothing, a `jstream-values` thread that ticks value bindings 30 times a second under the
engine lock, and atomics the render thread loads without locking
([R-RT-003](../RULES.md#R-RT-003)).

This is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)): it adds a collection object and
protocol additions, minor and additive ([R-API-001](../RULES.md#R-API-001)), and changes no
product scope, licence, security or privacy posture, compatibility promise or spending. No
channel reaches further than the controls already bound to it do.

<a id="decision"></a>
## Decision
**1. A channel is a collection object.** `channel.set { channel }` sets one whole, by
`channel_id`, and `channel.delete { channel_id }` removes it; both are edits with their undo.
`state.get` lists them as `channels` and the collection file saves them (schema 20). At most
256 per collection. Its fields:

| Field | Meaning | Default |
|---|---|---|
| `channel_id` | 1 to 64 bytes of ASCII letters, digits, `_` and `-` | required |
| `name` | Its display name, at most 256 bytes | empty |
| `scene_id` | A scene it belongs to; omitted for the whole collection | none |
| `default` | Its value when the engine starts following it, 0 to 1 | 0 |
| `inputs` | At most 8 readers (decision 3) | none |
| `combine` | How its inputs and actions make one value: `latest`, `max`, `min`, `sum`, `product` or `average` | `latest` |
| `average_ms` | A moving average over this window, 0 to 10000; 0 averages nothing | 0 |
| `rise_ms`, `fall_ms` | How long a full rise from 0 to 1, or fall from 1 to 0, takes, 0 to 60000; 0 jumps | 0, 0 |
| `curve` | `linear`, `ease_in`, `ease_out` or `ease_in_out`, as a modifier's | `linear` |
| `on` | Whether it starts switched on | `true` |
| `off_value` | What it reads while switched off, 0 to 1 | 0 |

A channel's **value** is always 0 to 1. Whatever follows it maps that onto its own range, so
one channel can drive a light's intensity, a rotation in degrees and a fader in dB at once.

**2. Its value is live, never stored.** The engine keeps each channel's value, its switch and
whatever actions set it as runtime state, like the program scene: never an edit, never undone,
never saved. A collection opens with every channel at its `default` and switched as `on` says.
A channel with a `scene_id` returns to its `default` and its `on` each time that scene goes on
program, so a control scoped to one scene starts fresh there. That return is immediate: it
skips the channel's `rise_ms` and `fall_ms`, since the scene arrives with its channels as
they start. Deleting the scene leaves the
channel in place, unattached, as ADR-0178 decision 5 leaves a value binding.

**3. Inputs.** Each input is a reader with the shape a modifier's kind has, flattened: `midi`
(ADR-0168) or `data` (ADR-0171), with their fields, smoothing and curve, or `channel` (decision
5), another channel. `MotionMidiControl` gains `note`, with the note number in `controller`:
1 while that note is held, 0 once it is released, so a pad gates a channel; any `midi`
reader takes it, a modifier's and a value binding's too. Its `from` and `to` map the reader onto the channel's 0 to 1 and are kept
within it. A JTunnel sender's modulation params are a `data` input on key `params`, field
`values.N`, so a DAW's automation feeds a channel with nothing new. A channel can't read
itself, directly or through others: `channel.set` refuses a cycle with `invalid_argument`. As a
value binding does (ADR-0178 decision 3), an input takes part only once it first reads
something other than it read when the engine started following it, so a controller at rest
overrides nothing.

**4. One value, then processing, in this order, once a tick.**
1. **Combine** the inputs that have moved and the value actions last set (decision 6):
   `latest` takes whichever changed last, as a DAW's pickup does; `max` and `min` the largest
   or least; `sum` adds, kept within 0 to 1; `product` multiplies, so one input gates another;
   `average` takes the mean. With nothing moved and no action yet, it is `default`.
2. **Switch.** While the channel is off, the value is `off_value` instead.
3. **Average** over the last `average_ms`.
4. **Fade** toward that at no more than `1 / rise_ms` a millisecond upward and `1 / fall_ms`
   downward, a straight ramp, so a switch fades a group in and out at a set pace.
5. **Curve.**

The `jstream-values` thread ticks channels with the value bindings, about 30 times a second,
while the collection holds either. Each channel's value goes into a lock-free cell that the
render thread, value bindings and other channels load; nothing on the render thread locks or
allocates.

**5. Anything follows a channel.** `MotionKind` gains `channel { channel_id, smoothing_ms,
curve, from, to }`: the channel's value, smoothed and mapped as a `midi` modifier's is
(ADR-0168 decisions 2 and 3). It goes everywhere a `midi` or `data` kind goes: item placement
and opacity, its effects' and source's settings (ADR-0138, ADR-0171 decision 4), a model's
materials, a camera's motion, value bindings (so any live setting, fader or send), and a
channel's inputs. A channel that doesn't exist reads 0, so what follows it rests at `from`. A value binding
reading a channel keeps ADR-0178 decision 3: it drives only once the channel's value first
differs from what it was when the engine started following the binding, so a channel made
from a parameter and set to that parameter's value as its `default` moves nothing until it
is moved.
Scene and source previews read it as it is, unsmoothed, as they read MIDI.

**6. Actions set channels.** Three actions, each runnable by a client, a hotkey, a MIDI
trigger and a timeline cue (they join [`Action::cue_problem`]'s set):
- `channel.set_value { channel_id, value, fade_ms? }` sets the action value to `value`, 0 to 1,
  ramping over `fade_ms` (0 to 60000) when given, on top of the channel's own fade.
- `channel.switch { channel_id, to }` with `to` = `on`, `off` or `toggle`.
- `channel.nudge { channel_id, by, fade_ms? }` adds `by`, -1 to 1, kept within 0 to 1, for an
  encoder or a pair of keys.

Under `latest`, an action is the input that changed last. A key bound to `channel.switch`
`toggle` with a `rise_ms` of 800 fades every light that follows the channel. `channel.values`
reports each channel's value now and whether it is on, which MCP offers as a tool. A
momentary key needs the key's release, which hotkeys don't report yet. A momentary MIDI pad
is a `midi` input with the new `note` control (decision 3): it reads 1 while its note is
held, on its channel or any, and 0 once released. `channel.values` also says what drives
each channel now: an input by its index, an action, or nothing yet (its `default`).

**7. One control scales another.** Every motion modifier (item and camera) and every value
binding gains an optional `amount`, a `channel_id`. A modifier's value is multiplied by the
channel's value, so `amount` at 0 leaves the property as stored and at 1 applies it fully. A
value binding's mapped value is pulled toward its `from` the same way:
`from + (value − from) × amount`. A switched channel with a fade is therefore a gate that fades.
An `amount` naming a channel that doesn't exist applies fully, so deleting a channel never
silently stops what it scaled.

**8. Outline, decided by a later ADR: sequences and the UI.** Timed changes of a few
channels belong apart from scene keyframes, which reposition a whole scene: lanes of
channel automation that play, stop, loop and start on a trigger (a channel crossing a
level, a scene going on program, a keyframe being drawn), each lane touching only its
channel, so a knob moved meanwhile takes that channel over under `latest`. The UI never
lists every channel at once: a channel is found by search and opened in an inspector, and
the learn button beside each live number offers making or following a channel. The wire
shape of those, and how they reuse timelines, is the follow-up's; nothing here depends on
it. [ADR-0184](0184-sequences-lanes-and-triggers.md) decides sequences and their triggers.

**9. Delivery.** This ADR's decisions 1 to 7 land in one PR, an additive minor protocol
version (0.169) and collection schema 20, with MCP getting `channel.values` and the
channel calls with every other call. Sequences, triggers and the UI follow.

<a id="consequences"></a>
## Consequences
- **Easier:** one named value drives any number of lights, materials, cameras, filters and
  faders, and rebinding it to another controller is one edit. A hotkey fades a group in and
  out; a CC that hasn't been sent no longer pins anything at 0, since the channel holds its
  `default`.
- **Easier:** a knob scales or gates any modifier or value binding through `amount`, and a
  timed sequence of a few values runs without touching the scene's keyframes.
- **Harder:** a third live store beside MIDI's and live data's, ticked on the `jstream-values`
  thread; a channel's response is at most one tick (about 33 ms) behind its control, which a
  follower's own smoothing hides.
- **Harder:** `amount` and the `channel` kind add a cell load per modifier per frame for
  modifiers that use them, and nothing for those that don't.
- **Not decided here:** key release for momentary hotkeys, channels whose value is not 0 to 1,
  writing a channel back to a DAW, and a scripting language. Each is additive.
- **Must do:** the maintainer's hardware run moves a real controller onto a channel that
  several lights follow, through a local port and through JTunnel; CI feeds messages in tests.
