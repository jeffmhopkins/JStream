<!-- @doc
id = "ADR-0241"
title = "The engine's choice of model size has a floor, and a fixed size is any whole percentage"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0241: The engine's choice of model size has a floor, and a fixed size is any whole percentage

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0197](0197-3d-models-draw-with-fewer-pixels-when-the-gpu-is-short-of-time.md) has
the render thread draw 3D models with fewer pixels while the GPU can't keep up, stepping
through 100, 85, 75, 67 and 50, and
[ADR-0200](0200-the-size-3d-models-are-drawn-at-is-a-rendering-switch.md) decision 1
makes the size a rendering switch, `model_resolution`: 0 for the engine to choose, or
one of those steps, with any other value refused.

During a recording the engine's choice went to 50%, which looked too soft for the
show. The only way to stop that was to fix the size, which gives up the choice's
protection of the outputs' frames altogether. A person who would accept a little
softness under load, but not much, had no way to say so, and the steps themselves
(85, then 75) are coarser than such a limit needs: 90% was the size asked for, and no
step is 90.

<a id="decision"></a>
## Decision
1. **A floor for the engine's choice.** The rendering switches gain
   `min_model_resolution`: the smallest size the render thread may lower models to
   while it chooses (`model_resolution` 0), any whole percentage from 50 to 100. 50, the
   default and what a file or client that leaves it out reads as, is every step it has,
   so nothing changes until it is set. 100 keeps models whole.
2. **A floor between two steps is the last step down.** While it chooses, the render
   thread steps through the steps above the floor and then the floor itself: at 90 it
   goes from 100 to 90 and no further; at 80, through 85 to 80. A floor raised past the
   size it draws at lifts the size to the floor at once. What it learnt of the old
   sizes' savings is forgotten when the sizes change.
3. **A fixed size is any whole percentage.** ADR-0200 decision 1 changes: a fixed
   `model_resolution` may be any whole percentage from 50 to 100
   (`MODEL_RESOLUTION_RANGE`), not only one of the steps. The steps stay what the
   render thread chooses among (`MODEL_RESOLUTIONS`). A fixed size is drawn as set,
   whatever the floor. When the choice is handed back from a size between two steps,
   it goes on from the next step up.
4. **The rest stands.** ADR-0197's marks, ADR-0200's other decisions and ADR-0236 are
   unchanged. `studio.set_rendering` refuses a value outside the range on either
   setting, and the protocol's minor version rises (0.194).

<a id="consequences"></a>
## Consequences
- The Settings dialog's 3D rendering page sets both in steps of 1%: the fixed size
  beside Fixed, and Lowest automatic resolution beside it, disabled while the size is
  fixed.
- A floor of 100 is the same as fixing the size at 100, by another route: the two
  settings still never disagree, since the floor applies only while the engine
  chooses, and ADR-0200's one-value rule holds for who chooses.
- A high floor can let the GPU fall behind for good, and the outputs' frames give way
  as ADR-0117 says; that is the trade the person chose.
- An older engine ignores `min_model_resolution` and refuses a fixed size between the
  steps; an older client keeps working and leaves both as they are.

Tests: `resolution::tests::a_floor_between_two_steps_is_the_last_step_down`,
`a_floor_of_100_keeps_models_whole`,
`a_floor_raised_below_the_size_drawn_lifts_it_to_the_floor`,
`a_fixed_size_between_two_steps_is_drawn_and_the_choice_goes_on_above_it` and
`a_fixed_size_below_the_floor_is_drawn_as_set`.
