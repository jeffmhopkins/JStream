<!-- @doc
id = "CRATE-jstream-anim"
title = "jstream-anim"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-anim

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-anim/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-anim", "docs/adr/0049-keyframe-scene-model-and-evaluation.md#decision", "docs/adr/0099-native-motion-modifiers-and-timelines.md#decision", "docs/adr/0100-audio-reactive-motion-modifiers.md#decision"]
-->

Layer-0 model of keyframe scenes and native motion
([ADR-0049](../../docs/adr/0049-keyframe-scene-model-and-evaluation.md), on
[ADR-0024](../../docs/adr/0024-scene-structure.md) §2): what a variant of a scene
stores, what its items look like, and what a switch between two keyframes of one scene
animates. No dependencies, no `unsafe` and no I/O. Geometry, crop and effects are type
parameters, so it doesn't depend on `jstream-protocol`: `jstream-graph` instantiates
it with its placement, the protocol's `Crop` and its filter chains for a keyframe
scene's overrides, and a
client can use it to preview a keyframe as the engine resolves it. The engine pairs a
switch within a family by item in its own transition planner, not through
`plan_switch` yet.

Contents:
- `Field`: the fields a variant overrides, with their wire names: the four keyable
  ones, `geometry`, `crop`, `visible` and `opacity`, then `effects`, the item's whole
  effects list ([ADR-0088](../../docs/adr/0088-keyframe-scenes-override-item-effects.md)),
  and `motion`, its whole list of motion modifiers (ADR-0099), each a type parameter
  that defaults to `()`. `Props` holds an item's values for the four
  keyable ones, which are all a switch animates, and `Value` one field's value. `Value::problem` refuses an opacity that isn't finite or is outside
  `0..=MAX_OPACITY` (4, the protocol's limit, #1101); geometry and crop are the graph's to check.
- `Override`: the fields a variant overrides for one item, each whole.
  `Override::apply` gives the effective values over the parent's values as passed in,
  so an absent field follows the parent live.
- `Keyframe`: a variant's overrides keyed by item ID. `effective` resolves one item.
  `write` (refusing an invalid value and changing nothing) returns `Inverse::Clear`
  when the field was absent and `Inverse::Write` of the old value when it was present.
  `clear` returns the write that undoes it, or `None` for an absent field, which
  changes nothing. `reset` clears everything and returns the writes that restore it.
  `drop_unless` drops the overrides of items outside the variant's tree and returns
  them whole; `restore` puts them back, or changes nothing and reports the item if one
  already has overrides. `undo` applies an `Inverse`. `effects_mut` changes an
  overridden effects list in place, so a filter edit on a keyframe's copy needs no
  whole write. No empty override is ever stored.
- `plan_switch`: the plan for a switch between two members of a family (`None` for the
  parent itself). Items pair by ID, one `Step` each in the order given, with their
  effective values at both ends and a `Motion`: `Still` (visible, equal), `Move`
  (visible, different), `Appear`, `Disappear` or `Hidden`. It doesn't interpolate:
  ADR-0029's transition sampler does, from these ends.

- Motion ([ADR-0099](../../docs/adr/0099-native-motion-modifiers-and-timelines.md)):
  `Modifier` is one motion modifier on an item: a `Property` (`x`, `y`, `scale`,
  `rotation`, `tilt`, `turn`, `depth`, `opacity`, the four crop edges), a `Kind`
  (`Wiggle`: seeded smooth noise with octaves; `Oscillate`: a sine, triangle, square or
  saw `Shape` with period and phase; `Drift`: a rate, optionally wrapping; `Impulse`: a
  decaying sine that lasts `duration_ms`, for a shake; `Audio`, below) and a `Clock`
  (the animation time, time since the item was shown, or `Triggered`: time since its
  last trigger, and nothing before one). `evaluate_with` takes each triggered
  modifier's time and each audio modifier's level from the caller; the wire has no
  trigger yet, so the engine passes none. `check_list` refuses more than
  `MAX_MODIFIERS`, duplicate or empty IDs and out-of-range parameters. `evaluate` sums an
  item's modifiers at one moment into `Offsets`, without allocating, so the render
  thread calls it per item per frame: positions, angles and crop add, scale and opacity
  multiply, and a step that would leave a sum non-finite is skipped. `Offsets::blend` mixes two sides' outputs across a keyframe move, which keeps
  every wave's phase.
- Audio-reactive modifiers
  ([ADR-0100](../../docs/adr/0100-audio-reactive-motion-modifiers.md)): `Kind::Audio`
  holds an `Audio`: a `Measure` (`Level`, `Peak`, `Onset`, or a `Band` between two
  frequencies), a dB floor and ceiling, attack and release times, a `Curve` (`linear`,
  `ease_in`, `ease_out`, `ease_in_out`, none overshooting) and a `from`..`to` range in
  the property's unit. `Audio::normalize` maps a measure onto `0..=1` in dB (an onset as
  it is), `Envelope::advance` follows it with one-pole attack and release smoothing that
  doesn't depend on the frame rate, and `Audio::value` bends and maps the envelope.
  What is heard (a bus or a source) and its measuring stay outside this crate: the
  evaluator keeps an `Envelope` per item and modifier and hands each level to
  `evaluate_with` by modifier ID. `evaluate` and `value_at` read an audio modifier as
  silence, its `from`.
- `Timeline`: cues sorted by time, a duration and a `Looping` (`Once`, `Loop`, or `Range`:
  an intro, then a loop). `check` refuses unsorted, unreachable or too many cues and bad
  bounds. `due` gives the cues that fire in a window of the animation clock, each with
  its exact time (`Fire`), so the control thread fires every cue once a pass however it
  ticks; `position` gives the pass and time; `seek` gives the cues to run on a jump to a
  pass and time: the last state per `CueAction::state_key` in play order, earlier passes
  and a range's intro included, without events. An unchecked zero-length loop never
  plays, and a modifier whose unchecked parameters give a non-finite value (a period
  that isn't positive or is tiny, a zero wrap, a NaN) gives 0, so bad input can't spin
  or put NaN on the render thread.

**Tests** (in `keyframe.rs`, `switch.rs`, `modifier.rs`, `audio.rs` and `timeline.rs`): live follow of absent fields, each
write and clear inverse, the no-op clear, refused opacities, reset and its restore,
drop and restore, a refused restore changing nothing, the wire names, every `Motion`
case, ends between two keyframes and in reverse, ends following the parent's current
values, and pairing by item. And a seeded walk of 5000 random writes, clears, resets,
drops and restores, each of which must undo exactly and leave no empty override.
For motion: each waveform's landmarks, period and phase, wrapping drifts, a seeded wiggle
that stays within its amplitude with no jump between frames and stays precise days in,
how `evaluate` adds, multiplies and skips disabled modifiers, the shown clock, blending,
every refused parameter, an unchecked bad period and other unchecked input (a tiny
period, a zero wrap, NaN and infinite parameters) giving 0, sums near the float range
staying finite, an oscillator unchanged three days in, and an impulse decaying inside
its envelope and waiting for its trigger; for audio, each curve's ends, clamping and
lack of overshoot, the dB map and its clamps, onsets, the range and its inversion,
attack and release time constants, zero times, ignored bad steps, the same envelope at
30 and 120 frames a second, every refused parameter, and audio modifiers read by ID, as
silence when unheard; and for timelines a six-cue sequence fired once each under uneven
ticks, loops, a range loop's intro and repeats, `Once` ending, seeks within a pass and
into later passes, unchecked zero-length loops that never spin, every refused timeline,
and 200 random timelines whose cues fire the same under random ticks as in one window.
