<!-- @doc
id = "ADR-0171"
title = "Live data from a DAW drives scenes: JTunnel state, data bindings, source settings and grids"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0171: Live data from a DAW drives scenes: JTunnel state, data bindings, source settings and grids

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
JTunnel now carries a DAW's state beside its audio and MIDI. A sender's **state packets**
(packet type 4) hold small JSON messages under keys: `clock` (tempo, playing, bar, beat
and beat phase, about 30 times a second, from any CLAP host), and, from a controller
extension in the DAW, `transport`, `grid`, `scenes`, `track/0` to `track/N` (each
column's name, colour, mute, solo, volume, and its clip `slots` with `state` and
`queued`), `mixer`, `meters`, `notes`, `device`, `clip` and `markers`. Each message is the
whole of its key, never a change to it; a message over 1,200 bytes goes in up to 64
pieces; changes go at once and everything is resent about once a second; a new stream ID
means the sender started again. Receivers that don't know the type ignore it. The sender's
side is JTunnel's, in [Jclap](https://github.com/jeffmhopkins/Jclap) PR #317
(`plugins/jtunnel/src/protocol.h` and `state.h`).

The maintainer wants that state to drive scenes in general: a light or a model's glowing
parts following a track, a grid of pads showing the clip launcher, a launched clip
switching scenes. New scenes should map it with no code of their own. It is a telemetry
source and a trigger source at once.

What exists to build on, and where it stops:
- **Value bindings.** Motion modifiers ([ADR-0099](0099-native-motion-modifiers-and-timelines.md))
  move an item's placement and opacity, and drive its effects' number and colour settings
  ([ADR-0138](0138-motion-drives-effect-settings.md)). The `midi` modifier
  (ADR-0168, in PR #1589) follows a controller with a
  smoothing, a curve and a `from..to` range, read without a lock. Nothing reads JTunnel
  state.
- **Action bindings** ([ADR-0122](0122-controller-input-midi-and-one-binding-system.md)):
  a key chord or a MIDI note, CC or program change runs any protocol action.
- **Lights** ([ADR-0143](0143-scene-lights-and-depth-aware-scene-filters.md)): a light's
  strength is its `intensity` times its item's opacity, so anything that drives opacity
  dims it. Its `color` and `intensity` are source settings, which no modifier reaches.
- **Models** ([ADR-0107](0107-3d-models.md)): `color` tints every material at once;
  `texture` and `texture_material` put another source's picture on one material by name.
  No setting reaches one material's colour or glow, and nodes are not addressable
  (ADR-0168 decision 6).
- **Shared values** ([ADR-0102](0102-page-api-for-browser-sources.md) decision 7) hold
  at most 256 names and announce every change to every client, so meters at 30 Hz don't
  belong there.
- **The JTunnel receiver** ([ADR-0123](0123-lan-inputs-jtunnel-and-visca.md)) is opt-in,
  LAN only, and takes datagrams only from senders the person accepted and subscribed to.
  A sender's MIDI drives bindings only once "Use this sender's MIDI" is on, and never
  reaches the protocol. JStream's parser refuses packet type 4 today, and counts it.

The forces:
- Everything stays general: a key, a field and a value, never a DAW's or a show's names.
- The render thread neither locks nor allocates ([R-RT-003](../RULES.md#R-RT-003)).
- A sender may do no more than ADR-0123 lets it, so this stays inside that ADR's bound
  and is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)): it changes no licence,
  security or privacy posture, compatibility promise or spending.
- One way to bind, as ADR-0122 decided: data joins MIDI and keys as a trigger and a value
  source, with no second list of targets.

<a id="decision"></a>
## Decision
**1. JStream receives JTunnel state.** `jstream-jtunnel` parses packet type 4: stream ID
(4 bytes), message ID (4), piece (1), piece count (1, 1 to 64), key length (1, 1 to 63),
the key, data length (2, at most 1,200) and the data.
- **Whole messages only.** Pieces are put together per sender and key, all or nothing,
  as SysEx is. The message that completes last replaces the key's. A new stream ID from a
  sender forgets everything it sent before. A key not refreshed for 3 seconds reads as
  absent, since the sender resends everything about once a second.
- **Bounded.** A key is 1 to 63 of letters, digits, `/`, `_`, `.` and `-`. A message is
  one JSON value, at most 76,800 bytes and nested no deeper than 32, parsed on the
  receive thread. A sender holds at most 1,024 keys and 4 MiB of state, and at most 128
  messages part-way assembled. Anything over a bound, malformed, or from an address
  JStream didn't subscribe to is dropped and counted, as audio and MIDI are.
- **Opt-in per sender.** An accepted sender gains **Use this sender's state**, off by
  default, beside "Use this sender's MIDI". Off, its state packets are counted and
  dropped.

**2. Live data.** The engine keeps each sender's newest message per key: not collection
state, not saved, not an edit.
- A **field** inside a message is a dotted path of object names and list indexes from 0:
  `volume`, `slots.2.state`, `tracks.3.0`. A missing field reads as absent.
- **Reading without a lock.** Planning registers each binding's `(input, key, field)` as
  a watch. When a message lands, the receive thread evaluates the watches on that key and
  stores each result in a lock-free cell: a number, a colour, or a short text's hash. The
  render thread only loads cells ([R-RT-003](../RULES.md#R-RT-003)).
- **Over the protocol** (actions and queries, never edits,
  [R-ARCH-006](../RULES.md#R-ARCH-006)): `data.list` names the senders with state and
  their keys, and `data.get { input, key }` returns one message. MCP gets the same two as
  tools. A client only reads; nothing sets live data.

**3. A `data` motion modifier.** `MotionKind` gains `data`, beside `midi`, with:

| Field | Meaning | Default |
|---|---|---|
| `input` | A JTunnel sender by name; omitted for any | any |
| `key`, `field` | Where the value is | required |
| `range` | `[low, high]`: a number at `low` reads 0 and at `high` reads 1, clamped | `[0, 1]` |
| `states` | A table from text to a 0 to 1 value, for a field such as `state` or `queued` | none |
| `smoothing_ms`, `curve`, `from`, `to` | As ADR-0168's | 50, `linear`, 0, 1 |

`true` reads 1 and `false` 0; a text reads its `states` entry, or 0; an absent field reads
0, so the item rests at `from`. The follower smooths it per item and modifier ID as it does
a `midi` modifier (ADR-0168 decisions 3 and 4), and an unfollowed shot reads it as it is.
Cameras refuse it, as they refuse `midi`. So everything a modifier moves follows the DAW.

**4. Modifiers drive a source's settings.** `MotionProperty` gains `source`, with `param`
(a setting of the item's source kind, by its ID in the kind's descriptor) and, for a
model, an optional `material` (decision 6). A number has the value added and kept within
the descriptor's range; a colour is lightened or darkened as ADR-0138 decision 3 does.
- It changes how this item draws its source, not the source: two items placing one light
  can follow different faders. Nothing is stored.
- Only settings the kind marks live take part; a setting that reloads its source (a file,
  a shape) and one of another type add nothing, as ADR-0138's missing parameter does. The
  Motion section offers only the live number and colour settings.
- So a light's `color` and `intensity`, and a model's `color`, `metallic` and
  `roughness`, follow any modifier: a wiggle, the audio, a fader or the DAW.

**5. Colour from data.** On a colour target, a `data` modifier may **set** the colour
instead of lightening it: `color_from = "field"` takes a `#rrggbb` text from the field,
and `palette` (a table from text to colour) picks one by the field's text. While the
field has a usable value, it replaces the setting's own colour, before any modifier adds
to it; otherwise the setting's own colour shows. Smoothing blends between colours in the
stored sRGB values, as ADR-0127 blends them.

**6. A model's materials, by name.** The `model` kind gains `materials`, a list of at most
32 overrides, each live:

| Field | Meaning | Default |
|---|---|---|
| `material` | The file's material name; every material with that name | required |
| `color` | Multiplies the base colour | white |
| `emissive` | Replaces the emissive colour | the file's |
| `emissive_strength` | Multiplies the emissive strength, 0 to 64 | 1 |
| `emissive_texture` | Another source whose picture is the emissive texture, as `texture` is the base colour's | none |

A `source` modifier with `material` drives `color`, `emissive` or `emissive_strength` of
that material, with or without a stored override. A name the file doesn't have changes
nothing and is logged once. Nothing here knows what a model depicts: any glTF with named
materials works.

**7. Data triggers.** `Trigger` gains `data { input?, key, field, equals }`, where
`equals` is a text, number or boolean. It fires when the field's value **becomes** equal,
from a different value or absent: a resend of the same value doesn't fire. The first value
after a sender's state stream starts sets the baseline and doesn't fire, so connecting
replays nothing. A key that went stale (decision 1) keeps its last value as the
baseline, so one that comes back with the same value doesn't fire, and a sender that
drops out for a moment doesn't switch scenes again. `key` and `field` may hold `*`
wildcards, as decision 8 describes. A binding fires at most once every 50 ms. It runs
any action a hotkey runs (ADR-0122 decision 1).

**8. An `event_text` source kind: what just happened, as words that fade.** It shows a
line of text each time a rule fires, holds it, then fades it out:
- **Rules**, at most 32, each with `enabled`, `key`, `field`, `equals` and `text`. A rule
  fires as a data trigger does (decision 7). `key` and `field` may hold `*`, matching any
  one key part or list index, so one rule covers every slot of every column:
  `track/*` and `slots.*.state` equal to `recording`. Data triggers take `*` the same way.
- **The text** is a template: `{1}` and `{2}` are what each `*` matched and `{1+}` that
  plus one, `{value}` the new value, `{.name}` a field beside the matched one, and
  `{/name}` a field at the top of the message. So
  `Clip recording started (track {1+}, clip {2+}): {.name}` names the clip. Values from a
  sender are untrusted text: drawn as plain text, control characters replaced by spaces,
  each cut to 100 characters, and a line to 300.
- **Presets.** The kind offers ready-made rules for the keys JTunnel's state documents
  (a clip recording, playing, stopping or queued; mute, solo and arm; play and stop; a
  tempo change), each a checkbox, so a person ticks what to show and edits the words.
  They are rules like any other, not special cases.
- **Timing.** `hold_ms` (0 to 60000, default 4000) keeps a line fully shown, then
  `fade_ms` (0 to 10000, default 500) fades it. At most `max_lines` (1 to 20, default 5)
  show, newest at the `newest` end (`top` or `bottom`); a new line past the limit pushes
  the oldest out. Lines come at most 8 a second, and any more are dropped and counted.
- **Look.** The `text` kind's font, size, colour, outline, alignment and wrapping.
- The receive thread makes the lines; the render thread draws them and fades them on the
  media clock, with nothing allocated there.

**9. A `grid` source kind.** `grid` draws cells from live data as a picture, which an item
shows flat or a model takes as `texture` or `emissive_texture` (decision 6):
- **Where the cells are.** `input`, a `columns_key` with `{column}` in it (`track/{column}`),
  a `cells_field` that is a list in each column's message (`slots`), and `columns` and
  `rows` (1 to 64) from a `column_offset` and `row_offset`. Each cell reads a
  `state_field` and a `color_field` from its list entry.
- **What a cell looks like.** `looks`: a table from the cell's state text to a look:
  a colour or the cell's own colour, a brightness, and a blink rate. Defaults name nothing
  but an `absent` look, so a grid shows plain cells until a look is set. `cell_px`,
  `gap` and `radius` set the pads' shape, and `dim` the brightness of a cell with no look.
- **Bars.** `mode = "bars"` reads a list of numbers instead (a field such as
  `tracks`), one bar per column, for meters and levels.
- **Drawing.** The receive thread writes the cells into a triple buffer per grid source;
  the render thread draws them with one small pass, blinking on the media clock. Nothing
  allocates there.

**10. Pages.** The `browser` kind gains `page_api_data` (off by default, shown while
`page_api` is on, with ADR-0124's warning) and `page_api_data_input`. A granted page gets
`jstream.data()` (every key of that input, as JSON) and a `data` event per changed key, at
most once per page frame, as ADR-0147 sends MIDI readings. So a layout no built-in kind
draws is still possible.

**11. Within ADR-0123.** The JTunnel sockets, what JStream sends, the accepted-senders
list and the subscribed-address check are unchanged. A sender's state reaches only what
its MIDI reaches, and only once its own switch is on: bindings (decisions 3 to 7), `event_text` and `grid`
sources, and pages the person granted. It never runs a protocol command; protocol clients,
who are already authorised, may read it. The parser is fuzzed with the other packets.

**12. People map it without typing paths.** The Motion section offers **DAW data** beside
MIDI, and the Bindings list offers a data trigger beside a key chord and a MIDI message.
Both pick the sender, key and field from a **live field picker**: a tree of the keys and
fields arriving now, each with its current value, filled from `data.list` and
`data.get`. A list index can be picked as one entry or typed. The property list offers a
source's live settings (decision 4) and, on a model, each material's colour, glow colour
and glow strength by name (decision 6). For example, a shape that shows only while the
transport plays is one `data` modifier on opacity, `key = "transport"`,
`field = "playing"`, `from = -1`, `to = 0`, `smoothing_ms = 0`.

**13. Delivery and the wire.** Each piece lands in its own PR, each wire addition an
additive minor version ([R-API-001](../RULES.md#R-API-001)), in this order: decisions 4
and 6 (no data needed); 1 and 2; 3 and 5; 7; 8; 9; 10. Each PR brings its part of
decision 12.

<a id="consequences"></a>
## Consequences
- **Easier:** a scene maps a DAW's transport, mixer, notes and clips onto lights,
  materials, placement, effects and scene changes from the Motion section and the Bindings
  list, with no code. The `clock` key's beat phase follows the song without MIDI clock.
- **Easier, without any DAW:** light colour, model colour and one material's glow follow
  any modifier, including `midi` and `audio`.
- **Easier:** a clip launcher or a meter bridge on a model is one `grid` source and one
  `emissive_texture` or `texture`, not a binding per cell.
- **Easier:** an on-screen log of what the DAW just did ("Clip recording started (track 1,
  clip 1)") is one `event_text` source with its presets ticked, and fades by itself.
- **Harder:** live data is a second input store beside MIDI's, with its own bounds,
  counters and fuzzing.
- **Harder:** a modifier list still holds at most 8, so a rig with many driven parts uses
  groups or `grid`.
- **Not decided here:** addressing nodes inside a model, OSC input, writing data back to
  the DAW, and live data from anything but JTunnel. Each is additive.
- **Must do:** the maintainer's hardware run checks a real DAW end to end over JTunnel;
  CI feeds packets in tests.
