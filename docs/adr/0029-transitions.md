<!-- @doc
id = "ADR-0029"
title = "Transitions"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0029: Transitions

<a id="context"></a>
## Context
[ROADMAP › M2](../../ROADMAP.md#m2) lists "Studio mode (Preview/Program) and
transitions". The data model has a single `studio.transition`
([ARCH › Data model](../ARCHITECTURE.md#data-model)), and the protocol lists
`studio.transition` as an action. Nothing says what a transition is.

The chosen UI prototype (`design/ui-prototype/`, direction D) defines transitions in
detail, and its README asks for an ADR. The UI decision register
([D. Transitions and studio](../../design/ui-prototype/decision-register.md#d-transitions-and-studio),
entries D24 and D25) records it:

- **Presets** `{ name, kind: Cut | Fade | Move, duration, easing, unmatched, stagger }`,
  with one default. The prototype ships "Smooth move" (Move, 600 ms) as the default,
  plus two fades and a cut.
- **Per-scene overrides:** a scene can name the preset used when switching *into* it and
  the one used when switching *out of* it. If both apply, the destination's "into" wins.
- **Move** pairs items in the outgoing and incoming scenes **by source**, and interpolates
  each pair's geometry. Items without a partner fade, scale or slide toward the nearest
  canvas edge.
- **Retargeting:** "switching mid-transition retargets from what is on screen".
- **Per item, per scene:** a delay, an easing (or inherit), "Glide to other scenes"
  (Auto or Never), and appear and disappear effects.

The register also
[notices](../../design/ui-prototype/decision-register.md#things-not-decided-here-but-noticed)
that the Spring easing overshoots, so interpolated values can
leave their range (crop below zero, opacity above one).

Scene switching itself is an action, not an edit, and program and preview are runtime
state. That is register entry D26, a technical decision within
[ADR-0013](0013-command-model.md), recorded in its own ADR. This ADR only uses it.

The maintainer decided D24 and D25 in the builder session. The maintainer's choice was
**"Adopt as prototyped"**.

<a id="decision"></a>
## Decision

**1. Presets and the default.**
- The collection holds `transitions { presets, default }`. A preset is
  `{ id, name, kind, duration_ms, easing, unmatched, stagger_ms }`:
  - `kind`: `Cut`, `Fade` or `Move`;
  - `easing`: `EaseInOut`, `EaseOut`, `Linear` or `Spring`;
  - `unmatched`: the default appear and disappear effect, `Fade`, `Scale` or `Slide`;
  - `stagger_ms`: an extra delay per item, by stacking order.
- Exactly one preset is the default. The default can't be deleted.
- Presets are edited with ordinary undoable edits (create, update, delete, set default).
- **Deleting a preset that overrides use** clears those overrides in the same edit, and the
  inverse restores them, as the prototype does.

**2. Which preset a switch uses.** The engine resolves it, so every UI and agent agrees:
1. a preset named in the switch action itself;
2. else the destination scene's `transition_in`;
3. else the current scene's `transition_out`;
4. else the default.

`Scene.transition_in` and `Scene.transition_out` are optional preset IDs, set by an edit.

**3. The kinds.**
- **Cut** replaces the picture on the next tick. Duration and easing are ignored.
- **Move** pairs items by source and interpolates each pair (below). Unpaired items
  disappear or appear with their effect.
- **Fade** pairs nothing: every outgoing item disappears and every incoming item appears,
  each with its own effect, delay and easing. With default settings that is a fade of
  every item, as in the prototype.

**4. Pairing (Move).**
- Items pair when they place the **same source**. When a source is placed more than once,
  the k-th placement in stacking order (bottom to top) in the outgoing scene pairs with the
  k-th in the incoming one, and the rest are unpaired. The rule is deterministic, so the
  same switch always pairs the same way.
- An item whose "Glide to other scenes" is **Never**, on either side, never pairs.
- **Nested scenes (groups):** a nested scene is one item, paired by its scene source. When
  it has no filters, normal blend and full opacity, it is flattened for pairing, and its
  items pair individually as in the prototype. Otherwise it stays one item, because it
  renders to a texture with its own filters, which flattening would drop.
- Pairing is computed **on the control thread** when the switch starts, into a plan the
  render thread receives. The render thread only interpolates
  ([R-RT-002](../RULES.md#R-RT-002)).

**5. Interpolation.**
- Each pair interpolates position, size, crop, opacity, rotation, tilt and turn, depth
  and perspective, eased by the item's progress.
- Rotation, tilt and turn take **the shortest way** round. The rest are linear.
- **Unmatched effects:** `Fade` (opacity only), `Scale` (toward 65% about the item's
  centre, while fading), `Slide` (toward the nearest canvas edge until off-canvas, while
  fading), or `None` (instant).
- **Easing curves** are the prototype's: cubic ease-in-out, cubic ease-out, linear, and
  the spring `1 − e^(−6p)·cos(11p)`. They are specified once, as a shared function with
  golden tests, so UIs that preview a transition draw what the engine renders.
- **Clamping:** a spring overshoots by design. Position, size, rotation and depth may
  overshoot, since that is the spring's look. Values with a hard range are clamped after
  easing: opacity to `[0, 1]`, crop to `[0, source size]`, size to `≥ 0`, perspective to
  `> 0`. The compositor clamps, not the UI.

**6. Per-item settings.** Each scene item carries
`transition { delay_ms, easing, glide, appear, disappear }`:
- `easing` and the effects default to `Inherit` (from the preset);
- `glide` is `Auto` or `Never`;
- `appear` and `disappear` are `Inherit`, `Fade`, `Scale`, `Slide` or `None`.

The settings belong to the placement, so they are keyed by item ID and edited like other
item properties. An item's progress starts after its delay plus `stagger_ms` × its index
in stacking order. The transition ends when its last item ends.

**7. Retargeting.** A switch that arrives mid-transition starts from **what is on screen**:
- the render thread owns the live interpolated state;
- the control thread sends the new target, pairing and settings;
- the render thread takes the current sampled state of every visible item, including items
  still fading out, as the new starting point.

There is no jump and no wait for the running transition to finish.

**8. Audio.** Audio that follows scenes, if any (decided separately, register D18), fades
over the transition's duration.

**9. Timing.**
- The model fields are **reserved now**, with neutral defaults, while M1b builds the model:
  - `transitions` with a single default preset;
  - `Scene.transition_in` and `Scene.transition_out` set to none;
  - `SceneItem.transition` with zero delay and everything inherited.
- The behaviour is **built before or during M2**.

<a id="consequences"></a>
## Consequences
- Transitions behave the same whether a human, a hotkey or an agent switches, because the
  engine resolves the preset, pairs and interpolates.
- **Move is not a texture blend.** During a Move, the program mix is a synthetic item
  list whose transforms and opacity come from the transition state. The render plan needs
  a transition node that reads both scenes' source textures. Its per-tick cost is bounded
  by the two scenes' item counts, and gets measured when built
  ([R-RT-005](../RULES.md#R-RT-005)).
- **Fade is per item**, as prototyped. Overlapping opaque items show what lies beneath
  them mid-fade, which a whole-picture crossfade would not. A whole-picture crossfade can
  be added later as another kind.
- **Group flattening is conditional.** A group with filters or a non-normal blend moves
  as one item, unlike the prototype, which always flattens. It keeps the group's look
  intact during the switch.
- Per-item settings are keyed by item ID, which is one more reason for edits that keep
  item identity.
- **Follow-on edits when this ADR is accepted** (not made while it is proposed):
  - [ARCH › Data model](../ARCHITECTURE.md#data-model): replace `studio.transition` with
    `transitions { presets, default }`, and add `Scene.transition_in`,
    `Scene.transition_out` and `SceneItem.transition`.
  - [ARCH › GPU pipeline](../ARCHITECTURE.md#gpu-pipeline): the transition node, and the
    clamping.
  - [ARCH › Control protocol](../ARCHITECTURE.md#protocol): the preset and override edits,
    and the optional preset on the scene-switch action.
  - [ROADMAP › M2](../../ROADMAP.md#m2): spell out presets, overrides, Move and per-item
    settings under "Studio mode (Preview/Program) and transitions".
