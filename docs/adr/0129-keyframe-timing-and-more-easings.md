<!-- @doc
id = "ADR-0129"
title = "A keyframe times its items in moves into it, and three more easings"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0129: A keyframe times its items in moves into it, and three more easings

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
An item's transition settings (`delay_ms`, easing, glide, appear and disappear) belong
to the item, so every keyframe of a family shares them
([ADR-0049](0049-keyframe-scene-model-and-evaluation.md) §3). Choreography often needs
an item to lead in one move and trail in the next: a panel that arrives first in one
keyframe and leaves last in another. A keyframe can override where an item is, but not
when it moves there (#1067).

The easings are also only `ease_in_out`, `ease_out`, `linear` and `spring`
([ADR-0052](0052-transition-mechanics-and-studio-preview.md)). There is no `ease_in`, no
curve of the user's own, and no step that holds still and then jumps.

<a id="decision"></a>
## Decision
This amends ADR-0049 §3 and ADR-0052's list of easings. The rest of both stands.

**1. A keyframe may time an item in moves into it.** A keyframe scene's override of an
item gains `timing` (`KeyframeTiming`): an optional `delay_ms`, `easing` and
`duration_ms`. When a switch, a placed keyframe or a hotkey moves program into that
keyframe, the item takes the keyframe's delay and easing in place of its own, and the
keyframe's duration in place of the preset's. A field left out follows the item and the
preset. The preset's `stagger_ms` × the item's index still adds to the delay. The timing
applies on both sides of the switch: to the item arriving or gliding in the keyframe, and
to it leaving when the keyframe hides it. A transition into any other scene, including
the family's parent and its other keyframes, doesn't use it. Glide, appear and disappear
stay the item's own. A `stinger`, `cut` or zero-duration preset ignores the timing, as it
ignores the item's own.

**2. Setting it.** `item.set_timing` writes a keyframe's timing for an item, whole. It
takes only a keyframe scene, with the errors the placement edits give a keyframe; the
item's own scene is `invalid_argument` on `scene_id`, since nothing moves into it by its
own timing. A timing with no field set removes the override. `item.clear_override` of
`timing` removes it too, and "reset keyframe" clears it with the rest. Its inverse is the
old timing, or a clear when there was none. A `delay_ms` or `duration_ms` over 60000 is
refused, as an item's delay is.

**3. Three more easings.** `Easing` and `ItemEasing` gain:
- `ease_in`: a cubic ease in, the mirror of `ease_out`.
- `hold`: no progress until the end, then the whole jump. With a delay, it places an
  item's jump at a chosen time, which is how a strip of frames shows a stagger exactly.
- `cubic_bezier { x1, y1, x2, y2 }`: CSS's curve, `x1` and `x2` in `[0, 1]` and `y1` and
  `y2` in `[-2, 3]`, so it may overshoot either end. A point out of range is
  `invalid_argument` on `easing.cubic_bezier.<field>`, on a preset, an item or a
  keyframe's timing. The engine samples it by bisection on `x`.

A preset, an item and a keyframe's timing may each use any of them.

**4. Where it lives.** The planner looks the timing up in the keyframe's overrides when
it plans a switch into the keyframe, so the render thread only samples tracks, as
before. A group's children still move with the group item's progress (ADR-0049 §3),
whatever their own timing.

**5. Compatibility.** A protocol minor bump: `item.set_timing`, `ItemOverride.timing`,
the `timing` override field and the three easings are additions. A client older than
this minor can't parse a preset, item or override that uses a new easing, as with any
new enum value (open question 8 in the protocol README); it ignores a `timing` that
uses only the older ones, as an unknown field. The collection file gains `scenes[].overrides[].timing` at a new schema version, which an older file
never has.

<a id="consequences"></a>
## Consequences
- Two keyframes of one scene can stagger the same items in opposite orders, which a
  frame strip shows (`two_keyframes_stagger_three_items_in_opposite_orders_in_a_frame_strip`).
- The Inspector and the preset form offer `ease_in`, `hold` and a custom curve, which
  starts from a back-out curve (`Bezier::BACK_OUT`). The curve's points, and a keyframe's
  timing, are edited through the API and MCP for now; a UI for them is a follow-up.
- A keyframe's timing is one more field of an override, stored, undone, reported by
  `state.get` and dropped with the override as the others are.
