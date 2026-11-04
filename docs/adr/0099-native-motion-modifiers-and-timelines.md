<!-- @doc
id = "ADR-0099"
title = "Native motion: one animation clock, motion modifiers and timelines"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0099: Native motion: one animation clock, motion modifiers and timelines

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Today nothing in a scene moves on its own. An item moves only during a transition
([ADR-0029](0029-transitions.md)), and a keyframe scene is a second state of its parent
that a switch glides to ([ADR-0049](0049-keyframe-scene-model-and-evaluation.md)). ADR-0049
left out "timeline animation (keyframes that advance on their own, or per-property curves
over time)" as a product-scope call needing its own maintainer-tier ADR. This is that ADR.

Building the starship cockpit scene (tracking issue #1056) showed what that costs. About
45 HTML pages did the engine's job:
- **continuous motion**: the cockpit's sway (about 3 px and 0.1°), the sky's drift, pulsing
  lights and a scanning radar were page JavaScript (#1058);
- **one clock**: the pages agreed only because each read `Date.now()`, and every mask and
  reflection repeated the sway's maths;
- **sequences**: the weapons run (lock, four volleys at 1500 to 2850 ms, an explosion at
  3300 ms) was one page, because nothing native can say "at t, do x" (#1059).

A scene author, a person or an agent over MCP, should get all three from native items and
keyframes, and keep HTML for what a browser draws best: rich type and vector UIs.

<a id="decision"></a>
## Decision

**1. Scope: scenes may animate on their own.** Two features come in: motion modifiers
(continuous, looping or drifting changes to an item's properties) and timelines (timed cues
inside a scene). Keyframe scenes stay the way to name states; a timeline sequences them.
This is the product-scope decision ADR-0049 deferred. Everything below it is design, and
later changes to it are agent tier.

**2. One animation clock.** Every modifier and timeline reads one **animation time**: the
engine's media clock ([ADR-0014](0014-media-clock.md)) in seconds since the engine started,
taken at the frame being drawn, not at the tick that sends a plan. All mixes draw the same
time for the same frame, so an item, its mask, a reflection of it and a scene source showing
it agree on every frame. Pages (#1063) and shader sources read the same value. It is never
the wall clock, so it doesn't jump when the system time changes.

**3. Motion modifiers.**
- An item, a group item included, holds an ordered list `motion` of at most 8 modifiers.
  Each has an `id` unique in the list, a `property`, a `kind` with its parameters, and
  `enabled`.
- **Properties:** `x`, `y` (canvas pixels), `scale` (a factor about the item's centre),
  `rotation`, `tilt` and `turn` (degrees, ADR-0094's 3D angles), `opacity` (a factor), and
  `crop_left`, `crop_top`, `crop_right`, `crop_bottom` (source pixels).
- **Kinds:**
  - `wiggle { amplitude, frequency_hz, seed, octaves }`: smooth seeded noise in
    `[-amplitude, amplitude]`, the same for the same seed on every machine;
  - `oscillate { shape, amplitude, period_ms, phase }` with `shape` one of `sine`,
    `triangle`, `square`, `saw`;
  - `drift { rate_per_s, wrap }`: a constant rate, wrapping into `[0, wrap)` when `wrap` is
    set, so a star plate can loop.
- **Clock origin:** `clock: "engine"` (the default) evaluates at the animation time, so
  every item that shares a modifier's parameters moves in step. `clock: "shown"` evaluates
  from when the item last became visible on program, for effects that should start fresh
  each time they are shown.
- **How a value is applied:** a modifier's value is added to the property (multiplied for
  `scale` and `opacity`, and `opacity` clamped to `[0, 1]` after), on top of the item's
  effective value after keyframes and any running transition. Modifiers on one property add
  in list order. A modifier never writes the collection: it is evaluated, never stored.
- **Groups:** a modifier on a group item moves the group's whole layer, so its children,
  their masks and their effects move together. That is how a cockpit plate and its gloss
  masks sway as one.
- **Where it runs:** the control thread puts each item's modifiers in the plan it already
  sends the render thread. The render thread evaluates them per frame as pure functions of
  the animation time, with no allocation and no wait on the control plane
  ([R-RT-002](../RULES.md#R-RT-002)).
- **Keyframes:** `motion` is a keyable override field, overridden whole like `effects`
  ([ADR-0088](0088-keyframe-scenes-override-item-effects.md)). During a Move between two
  members of a family the item evaluates both sides' modifiers and blends their outputs
  with the item's eased progress. Blending outputs instead of parameters keeps every
  oscillator's phase continuous, so a sway can calm down or speed up across a move without
  a jump. Modifiers never stop an item from pairing.
- **Edits:** `item.set_motion { scene_id?, item_id, motion }` writes the whole list (in a
  keyframe, as its override). Its inverse writes the old list, or clears the override.
  Modifier parameters are live values ([ADR-0032](0032-live-edits-and-undo.md)), so a slider
  previews as it drags and undo takes one step.

**4. Timelines.**
- A scene holds named **timelines**. Each has an `id`, a `duration_ms`, `loop` (none, loop,
  or loop a range), and an ordered list of **cues**: `{ at_ms, action }`.
- A cue's action is one of a closed set, each an existing protocol action or a new one in
  the same family: switch the scene, or a group (#1068), to a keyframe with an optional
  preset; show, hide or toggle an item (#1057's actions); set a live value; restart a
  browser source; start or stop another timeline. Only actions, never edits, so playing a
  timeline adds nothing to undo history.
- `timeline.play`, `timeline.stop` and `timeline.seek` are actions, bindable to hotkeys
  ([ADR-0040](0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md)) and
  callable over MCP. What is playing is studio state, not collection state, like the
  program scene.
- **Frame-exact timing:** a cue's time is fixed when the timeline starts, as `start + at_ms`
  on the animation clock. The control thread runs a cue on the first tick at or after that
  time, and gives any transition it starts that exact start time, so a late tick never
  delays what is seen. A cue whose time has passed when the timeline is sought fires only if
  it is a state change (a keyframe or visibility) and only the last one per target.
- Events report a timeline starting, stopping, looping and each cue it fires, never each
  frame.

**5. Size limits.** At most 8 modifiers an item, 32 timelines a scene and 256 cues a
timeline, and a `duration_ms` of at most one hour, checked on every edit.

**6. Compatibility.** Additive: an optional `motion` on items and on keyframe overrides, a
new override field, `timelines` on scenes, new edits, actions and events. Each lands as a
minor protocol release with the schema snapshot as the API review, and the collection gains
`schema_version` bumps with migrations and fixtures ([R-API-003](../RULES.md#R-API-003)).
The pure model (modifier evaluation, output blending, the timeline schedule) lives in
`jstream-anim`, which stays dependency-free, so a client previews motion exactly as the
engine draws it.

<a id="consequences"></a>
## Consequences
- **Easier:** sway, drift, pulses, parallax, scanning and repeating looks are a few
  modifiers. A sequence like the cockpit's weapons run is a timeline of keyframes and item
  toggles. Agents build both over MCP with no page and no shared JavaScript clock.
- **Harder:** a frame's look now depends on time even when nothing is edited, so tests that
  compare frames must pin the animation time; render tests take it as an input. The plan
  carries modifiers per item, a little more for the render thread to sample each frame.
- **Must now do, in order:** the `jstream-anim` model (modifiers, blending, schedule); the
  item and keyframe `motion` field and its render evaluation; timelines and their actions;
  modifiers on live filter parameters, which needs filter settings to become live values,
  so a modifier, a cue or an audio binding (#1060) can drive a glow or a blur. Each is its
  own PR, and the protocol ones take protocol-queue slots.
- **Not decided here:** curves edited per property in a graph editor, expressions, and
  audio-driven modifiers (#1060 decides those as a modifier source). Keyframe moves
  interpolating effect settings, mixing 2D and 3D keyframes and per-keyframe timing are
  decided in their own agent-tier ADRs (#1065, #1066, #1067).
