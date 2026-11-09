<!-- @doc
id = "ADR-0138"
title = "Motion modifiers drive an effect's number and colour settings"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0138: Motion modifiers drive an effect's number and colour settings

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A border that pulses with the music, a glow that breathes, a blur that swells on each
kick: each is an effect setting that changes over time (#1294, after the `border` kind
of #1227, and #1404 for every effect: glow, blur, bloom).
Today one moves only by an edit, a Move between keyframes, or a timeline cue that sets
a live value, which steps rather than moves. [ADR-0099](0099-native-motion-modifiers-and-timelines.md) planned this as
its last step, "modifiers on live filter parameters", and
[ADR-0100](0100-audio-reactive-motion-modifiers.md) §1 said its `audio` kind drives
them with no decision of its own once they exist. ADR-0099 assumed it needs filter
settings to become live values first. It doesn't: a Move already hands an item's chain
new settings each frame, in place, so a stateful effect keeps its state
([ADR-0127](0127-moves-interpolate-effect-settings.md) §3), and an effect's outset
already follows its settings ([ADR-0136](0136-an-effects-outset-follows-its-settings.md)).
What is missing is a way for a modifier to name an effect and a setting, and the rules
for adding its value there.

<a id="decision"></a>
## Decision

**1. A modifier names an effect setting.** `MotionProperty` gains `effect`. A
modifier with it carries `effect: { filter_id, param }`: one of its item's own effects,
by its ID, and a parameter of that effect's kind, by its ID in the kind's descriptor.
Only property `effect` takes an `effect`, and it needs one; `item.set_motion` refuses
anything else with `invalid_argument` at `motion.<n>.effect`. Every kind and clock
applies, `audio` included, with everything ADR-0099 and ADR-0100 say of a modifier:
the limit of 8 an item, keyframe overrides, `item.trigger_motion`, one envelope per
item and modifier ID. The target is not checked against the item's effects when it is
written, because effects change on their own (a keyframe overrides them, an edit
removes one) and a modifier outlives them.

**2. What it drives.** The effect is looked up in the effects the item draws in that
scene: in a keyframe that overrides them, the keyframe's. The parameter must be a
number (`float` or `int`) or a colour of its kind. A modifier whose effect or
parameter isn't there, or is of another type (an enum, a string, a file), adds nothing, as an `audio` modifier that hears nothing
reads silence. It never errors and never moves the item's box.

**3. How a value applies.** Each frame the modifiers on one setting are evaluated on
the item's clocks, as the item's other modifiers are, and their values add up. The sum
is added to the setting the chain would draw otherwise: its own, or during a Move the
blend ADR-0127 gives it.
- A number is kept within the range the kind's descriptor gives the parameter and
  rounded for an `int`.
- A colour takes the sum as a fraction of full added to each of its red, green and
  blue (in the stored sRGB values, as ADR-0127 blends colours), each kept within 0 and
  full, and keeps its alpha: 0.2 lightens it, -1 takes it to black. One number can't
  name a hue, so a modifier lightens and darkens a colour; a colour that changes hue
  over time is a Move between keyframes.

A frame where every driven setting adds 0 draws the chain's own settings. Settings are
only ever added to, never multiplied, since a setting's 0 means something of its own
(no border, no blur).

**4. Where it runs.** Planning resolves each modifier against the item's chain once:
the filter's place and the parameter's range. The render thread evaluates the
modifiers and hands the chain its settings before it runs, once per frame of the
cache however many mixes draw it, by the path a Move uses (ADR-0127 §3), so every mix
of the frame draws the same settings. The collection never changes.

**5. Room to draw.** An item's effects layer is padded for the least and the most each
driven setting reaches (ADR-0136 §2's rule for a Move's two looks): a wave or a shake
within its amplitude, an `audio` modifier between its `from` and `to`, a drift within
its wrap or to the end of the range in its direction. A pulsing outer stroke or glow is
never cut.

**6. Timelines.** A cue doesn't drive a setting itself: it fires a `triggered`
modifier with `item.trigger_motion`, or switches to a keyframe whose list differs, so a
sequence moves a setting smoothly instead of stepping it cue by cue.

**7. During a Move.** The arriving item's modifiers drive the blended settings. Its
leaving side's effect modifiers stop as the Move starts, since the chain that draws
the pair is the arriving one's.

**8. Compatibility.** Additive, protocol 0.109: a new `MotionProperty` variant and an
optional `effect` on `MotionModifier`, omitted when absent. The collection file is
`schema_version` 13, which refuses such a modifier in a version 12 file. A 0.81 client reads
`property` `effect` as `Unknown` and ignores `effect`. The MCP design guidance gets a
recipe for an effect that pulses with the mix.

<a id="consequences"></a>
## Consequences
- **Easier:** a border's width, a glow's strength, radius or colour, a blur, a colour grade's
  brightness or a vignette follow a bus, a sine or a trigger with one modifier, over MCP
  or the protocol, with no page and no new effect kind.
- **Harder:** a chain with driven settings applies new settings each frame they change,
  the same work an edit does, and its layer is sized for the most they reach.
- **Limits that remain:** only an item's own effects are driven, not a source's or a
  scene's filters, since motion belongs to an item and a source's filters are shared
  by every item that shows it; a source shown through a group item with effects is the
  way to drive them. A colour only lightens and darkens. Speed in beats (#1222) waits
  for a tempo source. The Inspector shows such a modifier but adds none yet.
