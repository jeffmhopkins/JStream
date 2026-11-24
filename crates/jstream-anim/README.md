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
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-anim", "docs/adr/0049-keyframe-scene-model-and-evaluation.md#decision", "docs/adr/0099-native-motion-modifiers-and-timelines.md#decision", "docs/adr/0100-audio-reactive-motion-modifiers.md#decision", "docs/adr/0168-midi-controllers-drive-motion.md#decision"]
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
  `motion`, its whole list of motion modifiers (ADR-0099), and `timing`, its timing in
  switches into the keyframe
  ([ADR-0129](../../docs/adr/0129-keyframe-timing-and-more-easings.md)), each a type
  parameter that defaults to `()`; nothing animates the last three. `Props` holds an item's values for the four
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
  `rotation`, `tilt`, `turn`, `depth`, `opacity`, the four crop edges, and `width` and
  `height`, which stretch one axis), a `Kind`
  (`Wiggle`: seeded smooth noise with octaves; `Oscillate`: a sine, triangle, square or
  saw `Shape` with period and phase; `Drift`: a rate, optionally wrapping; `Impulse`: a
  decaying sine that lasts `duration_ms`, for a shake; `Audio`, below) and a `Clock`
  (the animation time, time since the item was shown, or `Triggered`: time since its
  last trigger, and nothing before one). `evaluate_with` takes each triggered
  modifier's time and each audio modifier's level from the caller; the wire has no
  trigger yet, so the engine passes none. `evaluate_scaled` also multiplies each
  modifier's value by an amount the caller gives by ID (a channel's level, ADR-0183
  decision 7; 1 applies it as it is). `check_list` refuses more than
  `MAX_MODIFIERS`, duplicate or empty IDs and out-of-range parameters. `evaluate` sums an
  item's modifiers at one moment into `Offsets`, without allocating, so the render
  thread calls it per item per frame: positions, angles and crop add, scale, opacity, width and height
  multiply (`Offsets::size_of` gives the box's two factors, scale times each side's), and a step that would leave a sum non-finite is skipped. `Offsets::blend` mixes two sides' outputs across a keyframe move, which keeps
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
- MIDI-driven modifiers
  ([ADR-0168](../../docs/adr/0168-midi-controllers-drive-motion.md)): `Kind::Midi` holds
  a `Midi`: a smoothing time, a `Curve` and a `from`..`to` range. `Midi::normalize_cc`
  and `Midi::normalize_bend` map a controller's value onto `0..=1` (a bend's rest is one
  half), `Smoothing::advance` follows it with one one-pole time constant for rising and
  falling (0 jumps, the first step lands on the value, and a step within `SNAP`, 0.0005,
  of the value lands on it, so a held control rests exactly), and `Midi::value` bends and
  maps it. Which input, channel and controller it reads stay outside this crate: the
  evaluator hands each smoothed value to `evaluate_with` by modifier ID, as an audio
  level. `evaluate` and `value_at` read it at `from`. A `data` modifier (ADR-0171
  decision 3) is a `Kind::Midi` too: it follows a field of live data, which its
  evaluator reads as `0..=1` and hands in the same way, and so is a `channel` modifier
  (ADR-0183), which follows a channel's level.
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
- Shows (`show`, ADR-0210): the pure schedule of a show, generic over the caller's IDs
  and actions through `ShowKeys`. A `Plan` holds program clips end to end from 0,
  overlay, audio, sequence and keyframe entries, one-off events, bookmarks (pausing or not,
  with an arrive transition and an optional wait limit), loops (`Leave::AtOnce` or
  `FinishPass`) and an `EndOfShow`. `spanning` gives what spans a point on either
  `Edge` (just before it, or at it) and `state_events_before` the last state event per
  `CueAction::state_key` before it. `Runner` walks show time on the animation clock:
  `tick` runs the points due in order (a loop wraps at its end before a bookmark there
  could pause, a pausing bookmark pauses before anything at its time but a program clip
  starting there, which goes up as it pauses, then the spanning state, then the events), and `continue_`, `back`, `skip`, `go_to`, `jump`, `arm`,
  `off_script`, `back_to_script` and `stop` move it; `start_at_bookmark` starts as a
  jump to a bookmark would land, with its arrive transition; `looping` and `leaving` say
  which loop it is in and whether this pass leaves it. `pause` holds show time
  where the play head is by hand (`held`) and stops the sequences it started;
  Continue, or `unpause`, plays on from there, playing them again from where they
  were; program runs on through it and is not sought back. Every move puts up state by
  diffing the target `Spanning` against what it last put up (`Applied`), so a jump
  lands the state playing there would leave and re-applies only state events whose
  value it changed itself; one-shots are never replayed. `replan` carries a running
  runner over to an edited plan at the same show time: it maps what it put up onto the
  new plan (clips by scene, track or timeline, state events by key) and puts up what
  spans the play head there, so an overlay whose clip no longer covers the head goes
  off and one that now does comes on; a play head past the new end ends the show. It
  returns `Step`s: each `Effect` with the exact clock time it fell due. An audio clip
  (`AudioClip`, by source) comes up with `AudioOn` and goes with `AudioOff`; the caller
  works its fader out from the play head with `AudioClip::gain_db_at` (its level, with
  fades in and out linear in amplitude, `fade_db`, down to `AUDIO_FLOOR_DB`). One that
  ends while playing goes with no fade of its own; one a jump, a wrap or a stop cuts off
  goes over a short fade of 300 ms; one that stays up through a jump or a wrap comes up
  again at its new place, so its media seeks there.

**Tests** (in `keyframe.rs`, `switch.rs`, `modifier.rs`, `audio.rs`, `timeline.rs` and `show/`): live follow of absent fields, each
write and clear inverse, the no-op clear, refused opacities, reset and its restore,
drop and restore, a refused restore changing nothing, the wire names, every `Motion`
case, ends between two keyframes and in reverse, ends following the parent's current
values, and pairing by item. And a seeded walk of 5000 random writes, clears, resets,
drops and restores, each of which must undo exactly and leave no empty override.
For motion: each waveform's landmarks, period and phase, wrapping drifts, a seeded wiggle
that stays within its amplitude with no jump between frames and stays precise days in,
how `evaluate` adds, multiplies and skips disabled modifiers, how `evaluate_scaled`
scales each by its amount, the shown clock, blending,
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
and 200 random timelines whose cues fire the same under random ticks as in one window. For shows (`show/tests.rs`, with a simulated world that applies every effect): the
plan's checks and `spanning` on both edges; cuts with each clip's transition, pausing
before a pausing bookmark's point and Continue firing what sits there, non-pausing
bookmarks, `go_to`, Back, Skip, early Continue, an armed spot, loops (passes, clips
spanning a wrap staying up, a sequence clip ending at the loop's end stopped and played again on each wrap, a bookmark at a loop's start pausing once and one at its
end never pausing a pass, both ways of leaving), every end of show, the pause limit, a pause by hand holding show time and its sequences and playing on from there, a re-plan carrying what is up over to an edited plan,
off script, `stop`, one-shots never replayed and hand-set values left alone, exact
step times on a late tick, `next` and `next_due`, an audio clip up for its span and down
at its end with no fade, a jump cutting one off over the short fade and seeking one that
stays up, and an audio clip's level through its fades; and property tests that a jump to a
random bookmark or point leaves the same program, keyframe, overlays, audio clips and
their places, sequences and state values as playing there, with and without loops, and that 40 random operations
never panic and keep steps in time order.
