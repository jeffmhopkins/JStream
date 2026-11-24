<!-- @doc
id = "ADR-0203"
title = "A mapping adds to the value it drives or replaces it, and can be bypassed"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0203: A mapping adds to the value it drives or replaces it, and can be bypassed

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
The alpha.12 mapping design maps any source (a JTunnel Mod, a MIDI control, a channel,
an audio level) onto any parameter, as an **offset** or an **override**, several sources
per parameter, each one bypassable. The engine has two kinds of mapping, and each does
only one of these things:
- **Value bindings** ([ADR-0178](0178-value-bindings-drive-any-number.md)) drive a
  source setting, a filter setting, a fader or a send. Each one *replaces* its target's
  value, so two bindings on one target fight, and the last one evaluated wins every tick.
  A binding can't be switched off without removing it.
- **Reader motion rows** on items ([ADR-0099](0099-native-motion-modifiers-and-timelines.md),
  `midi`, `data`, `channel` and `audio`) and on scene cameras
  ([ADR-0176](0176-controllers-and-live-data-drive-scene-cameras.md)) *add* to the
  stored value, and so do the rows that drive an effect's or a source's settings
  ([ADR-0138](0138-motion-drives-effect-settings.md),
  [ADR-0171](0171-live-data-from-a-daw-drives-scenes.md)). A row can't set the
  value outright, for example an X position from a fader.

<a id="decision"></a>
## Decision
1. **A `mode` on both kinds of mapping.** `MappingMode` is `add` or `replace`. It is a
   minor protocol version, 0.186 ([R-API-001](../RULES.md#R-API-001)).
   - `ValueBinding.mode` defaults to `replace`, so every binding written before this
     version keeps working as it did.
   - `MotionModifier.mode` and `CameraMotionModifier.mode` default to `add`, for the same
     reason. Only a reader (`midi`, `data`, `channel` or `audio`) may be set to
     `replace`; for a wiggle, an oscillator or a drift it is refused at `motion.<n>.mode`.
   - A mode this version doesn't know is refused.
2. **`ValueBinding.enabled`**, default `true`. A binding that is off stays in the
   collection and in `state.get`, so anything listing what drives a setting still shows
   it, but it drives nothing. Motion rows already have `enabled`.
3. **A value binding target sums.** On each tick, a target takes this value, kept within
   its range:
   - its base is the value of the `replace` binding that moved last, or the stored value
     when no `replace` binding has moved;
   - every moved `add` binding's value is then added to it.

   An `add` binding's value is its mapped value as it is. So an `add` binding from 0 to
   6 dB lifts a fader by up to 6 dB, and one with a negative `to` lowers it. "Last
   moved" is the binding whose value changed most recently. That way, two overrides on
   one setting behave like two hands on one fader, and the design's rule of one override
   per parameter is the UI's to keep.
4. **A motion row that replaces sets the property itself.** Its value is the property
   in its own unit:
   - x, y, rotation, tilt, turn, depth and the crops: their position, angle or pixels;
   - opacity: from 0 to 1;
   - scale, width and height: the factor itself, not 1 + v;
   - an effect's or a source's setting: the setting's value;
   - a camera's position, target, field of view, roll, focus, blur and autofocus: their
     value. Pan and tilt turn the view from where the camera looks, so a replaced pan
     is the turn itself.

   On each property, the last enabled `replace` row in the list sets the value, and the
   `add` rows then add to it, or multiply it for scale, width, height and opacity. A
   row's `amount` channel pulls a replaced value toward the property's own value: at an
   amount of 0.5, it goes halfway.
5. **During a keyframe Move,** a replaced property blends when both sides replace it,
   as offsets do. When only one side replaces it, it switches at the halfway point,
   because there is no offset to blend from.
6. **A model's material drives stay add-only.** A material's values live in the model
   file, which the frame doesn't read, so a `replace` row on a material drives nothing.
   The same applies to a `replace` row on a colour setting, where painting already sets
   a colour.

<a id="consequences"></a>
## Consequences
- **Easier:** a parameter takes several sources, such as an override from a fader plus
  an offset from an LFO channel, and a source can be bypassed while it is set up. The
  mapping design's Offset, Override and Bypass are each one field.
- **Easier:** an item's X, a camera's field of view or a light's intensity can follow a
  fader exactly, with no arithmetic against its stored value.
- **Harder:** the value driver looks up each target's stored value on every tick that
  has an `add` binding. The lookup is a map read, and the tick already holds the state
  lock.
- **Harder:** a replace on a source setting is worked out in f32, so a setting above
  about 16 million loses its last digits while it is replaced.
- **Not decided here:** how the UI picks between an offset and an override, and where
  their bands show. The mapping design covers both (PRs 6 and 8).
