<!-- @doc
id = "ADR-0100"
title = "Audio-reactive bindings are a motion modifier kind that follows a bus or a source"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0100: Audio-reactive bindings are a motion modifier kind that follows a bus or a source

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Scenes often want items that react to the music: lights that pulse to the program mix,
meters, a logo that pumps with the bass. A page can't hear the mix, so today such a scene
simulates its audio in JavaScript (#1060, found while building a complex native scene for
tracking issue #1056). The engine already hears the mix in two places:
- the `waveform` and `spectrum` sources draw a bus or a source from an `AudioScope`,
  a lock-free ring the mixer thread pushes each block into (#362, #459);
- a shader source with `audio = true`
  ([ADR-0093](0093-shader-sources-from-effect-packs.md)) gets `level`, `peak`, `onset`
  and 64 log-spaced bands each frame from an `AudioMeter`, which measures a scope on the
  render thread and allocates nothing while it does.

What is missing is a way for an ordinary item to follow those numbers: a light's
opacity rising on each kick, a logo's scale pumping with the bass, a glow's strength
following the master level. [ADR-0099](0099-native-motion-modifiers-and-timelines.md)
proposes motion modifiers, an ordered `motion` list on an item whose values add to (or
multiply) its properties each frame, and leaves "audio-driven modifiers (#1060)" to this
ADR "as a modifier source". This ADR decides part 1 of #1060. Part 2, pages hearing the
mix through a `window.jstream` API (#1063), touches the threat model's browser section
and is a separate, maintainer-tier ADR.

<a id="decision"></a>
## Decision

**1. A binding is a modifier.** ADR-0099's modifier kinds gain `audio`. It sits in an
item's `motion` list beside `wiggle`, `oscillate` and `drift`, and inherits everything
ADR-0099 says of a modifier: its `id`, `property` and `enabled`, the properties it can
drive, how its value is applied (added, or multiplied for `scale` and `opacity`), groups
moving their whole layer, the limit of 8 an item, `motion` being a keyable override, and
`item.set_motion` with its live values and one undo step. When modifiers come to live
filter parameters (ADR-0099's last step), the same kind drives them, with no decision of
its own.

**2. What it hears.** `audio { audio, audio_source, measure, ... }`:
- `audio` (a bus, or none for the program mix) and `audio_source` (a source, which
  overrides `audio`) have exactly the meaning the `spectrum` kind's settings of those
  names have: the master after its filters, a bus after its fader, or a source after
  its own filters, fader, mute and follow. A binding hears what the audience hears.
- `measure` is one of `level` (RMS of the latest window), `peak` (its peak absolute
  sample), `onset` (`AudioMeter`'s 0 to 1 jump above the recent average), or
  `band { low_hz, high_hz }`: the power of the spectrum between the two frequencies,
  20 Hz to 20 kHz with `low_hz < high_hz`. A band narrower than one of the meter's
  bands reads the band it falls in.

**3. How a measure becomes a value.** In order, each step a pure function in
`jstream-anim` so a client previews what the engine draws:
1. `level`, `peak` and `band` are taken in dBFS and mapped from `floor_db` (default
   -60) to `ceiling_db` (default 0) onto 0 to 1, clamped. `onset` is already 0 to 1.
   `-90 <= floor_db < ceiling_db <= 0`.
2. An envelope follower with `attack_ms` (default 10) and `release_ms` (default 250),
   each 0 to 10000: one-pole smoothing toward a rising input with the attack time, and
   toward a falling one with the release time. It reads the meter's unsmoothed window
   values, so its own times are the only smoothing applied.
3. `curve`, one of `linear` (the default), `ease_in` (cubic, so quiet passages stay
   low and only loud ones lift), `ease_out` (cubic, so quiet passages already show) and
   `ease_in_out`, bends the 0 to 1 envelope. These are curve shapes of their own, never
   the transition `Easing`, whose `spring` would overshoot the range.
4. The result is mapped onto `from` to `to` (defaults 0 and 1, any finite numbers
   within ADR-0099's largest amplitude, `from > to` allowed to invert). That number is
   the modifier's value, applied as every modifier's is: added, or for `scale` and
   `opacity` multiplying by one plus the value. So for `opacity`, `from: -0.6, to: 0` is
   a light at 40% between beats and full on a kick, and for `x`, `from: 0, to: 6` is a
   6 px push.

**4. Where it runs.** Where the other modifiers are evaluated, with no allocation
and no wait on the control plane ([R-RT-002](../RULES.md#R-RT-002)):
- The engine's audio sync keeps a scope for every bus and source a binding names, as it
  does for the scope sources, and drops it when no binding or scope source names it.
- The engine keeps one `AudioMeter` per heard scope, made when a plan first names
  that scope (never while a steady frame draws) and shared by every binding that hears
  it. It measures each scope once per animation-clock frame, running the FFT only when a
  `band` binding hears that scope. Cost scales with distinct inputs, not bindings.
- A binding keeps its envelope's state, keyed by item and modifier `id`. The envelope
  advances once per animation-clock frame, and every mix that draws that frame reads the
  same value, so program, preview, a scene source and a reflection agree, as ADR-0099's
  one clock requires. This state is the one way an `audio` modifier differs from the
  others: it is a function of the audio heard so far, not of the clock alone. It is
  never stored in the collection.
- Across a keyframe Move, ADR-0099's output blending applies. A binding with the same
  `id` on both sides keeps one envelope, so a light that pulses in both keyframes never
  restarts its envelope mid-move.

**5. Silence.** A binding hears silence whenever the scope sources would draw it: when the
named bus or source is gone or not playing into the mixer, and while no output runs and
the program mix stands still. Its envelope then releases toward the value for silence.
It never errors and never reads stale audio.

**6. Nothing leaves the engine.** A binding changes only how an item is drawn. No level
reaches a page, a plugin or a client through it beyond the frames every mix already
shows, and `meters.tick` is unchanged. That is why it is agent tier, and why part 2 is
not decided here.

**7. Compatibility.** Additive: a new variant in ADR-0099's modifier kind union, in the
same minor protocol release as the `motion` field or a later one, with the schema
snapshot as the API review. Every parameter is checked on each edit and refused with
`invalid_argument` at its path. An Inspector section and the MCP motion tools gain the
kind, and the MCP design guidance gets one recipe for light that pulses to the mix.

<a id="consequences"></a>
## Consequences
- **Easier:** lights, meters and pulsing logos follow the real program mix
  with a few modifiers and no page. Agents add them over MCP with the same tool that adds
  a wiggle.
- **Harder:** a frame now depends on audio as well as on time, so a render test that
  compares frames pins both: it feeds the scope known samples and fixes the animation
  time. The envelope's per-binding state is a small table the engine keeps in step with
  the items, beside what it keeps of when each item was shown.
- **Limits that remain:** a binding hears nothing while no output runs, because the
  program mix stands still then, as it does for the scope sources. A scene author
  previewing reactive lights starts a recording or a stream, or a monitor once the mixer
  runs for one. Making the mixer run with no output is its own decision.
- **Must now do, after ADR-0099's `motion` field lands:** the `jstream-anim` functions
  (dB map, envelope, curve, range); the protocol variant and its checks; the scope
  bookkeeping and shared meters in the engine; the Inspector and MCP surface; a
  render test with a pinned scope; the design recipe. One PR for the model and one for
  the protocol and engine, the second taking a protocol-queue slot.
- **Not decided here:** pages hearing levels (#1060 part 2, #1063), beat detection or
  tempo, side-chaining one binding from another, and bindings on audio parameters (a
  fader following a band).
