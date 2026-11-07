<!-- @doc
id = "ADR-0127"
title = "A Move blends the settings of effects that differ only in their settings"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0127: A Move blends the settings of effects that differ only in their settings

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A Move pairs an item across two scenes only when its effects look the same on both
sides ([ADR-0081](0081-effects-pair-by-their-look.md)), and between two keyframes of one
family that rule holds for overridden effects too
([ADR-0088](0088-keyframe-scenes-override-item-effects.md) §4). Both say effect settings
are never interpolated. So a keyframe that changes one number of an effect (a blur
radius, a glow's strength, a colour) makes the item disappear on one side and appear on
the other instead of gliding, and the setting jumps. Scenes that change a light or a
blur between two states end up doing it in a browser source instead (#1065).

<a id="decision"></a>
## Decision
This amends ADR-0081 and the last bullet but one of ADR-0088 §4, for Moves only. A Fade
and a stinger pair nothing and are unchanged, and so is the rest of both ADRs.

**1. Which items pair.** Two items pair in a Move when their effects (the enabled
effects of kinds the engine draws, as ADR-0081 counts them) are the same kinds in the
same order, and each parameter of each effect either holds the same value on both sides
or holds a plain value on each: a number, a colour, a string or a boolean. A list of
other kinds, another order, or another length still makes each item take its unmatched
appear or disappear effect. So does a reference (a scene, a source, a bus or a file) or
a `null` that differs, since each side's effect reads only its own scene or file and the
picture between them can't be drawn.

**2. How the settings move.** A paired item whose effects' settings differ draws its
arriving side's effects with settings blended from the leaving side's, with the item's
own progress and easing (its delay, its stagger and its curve, as its placement moves):
- every number linearly, a whole number on both sides staying whole (rounded);
- every colour (`#rrggbb` or `#rrggbbaa`) per channel, in the stored sRGB values;
- every other value that differs (an enum, a string, a boolean) switches from the
  leaving side's to the arriving side's when the item's eased progress reaches one half.

The progress a blend uses is clamped to `[0, 1]`, so a `spring` easing never takes a
setting past its ends and out of the range its kind allows, although the placement
overshoots. A parameter only the arriving side has (a newer kind version) keeps its
value throughout. Angles blend as numbers: a hue of 350 moving to 10 passes through 180.

**3. Where it is drawn.** The engine plans the blend with the transition and the render
thread gives the item's chain each frame's settings before it runs, in place, so a
stateful effect keeps its state. A `video.snapshot` taken at the same moment draws the
same settings. When the transition ends, the chain draws the arriving side's settings,
which the blend has reached. A switch that cuts the Move short starts the next one from
the settings on screen then.

**4. Compatibility.** No protocol change: what a Move draws changes, and nothing a client
sends or reads does. A collection whose items pair now where they cross-faded before
glides instead.

<a id="consequences"></a>
## Consequences
- A keyframe can animate an effect's numbers (a blur coming in, a colour grade, a
  vignette tightening) with a native Move, without a browser source.
- Effects keep no live values, so this is still the only way an effect's setting moves
  over time; motion modifiers on filter settings would be a separate decision.
- A blended frame applies new settings to each blended filter once per frame, which is
  the same work a settings edit does, for the length of the transition.
