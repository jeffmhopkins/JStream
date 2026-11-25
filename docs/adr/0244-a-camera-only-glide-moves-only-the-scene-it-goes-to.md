<!-- @doc
id = "ADR-0244"
title = "A camera-only glide moves only the scene the Move goes to"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0244: A camera-only glide moves only the scene the Move goes to

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0226](0226-a-camera-only-move-keeps-its-scenes-chain.md)
decision 1 draws a Move that moves only a 3D scene's camera as the program scene at rest
through a gliding camera, and says every camera of its family draws the Move's camera
for the Move's length. The engine kept the glide per family to do that.

A video mix can show another scene of the same family while program glides: the family's
own scene, or a sibling keyframe. Such a mix then drew the gliding camera instead of its
scene's own, so a mix meant to show one fixed view moved with program (#1801). Before
ADR-0226, a Move drew its tracks on program alone, and every mix kept its scene's camera.

This is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)). It changes which scenes a
camera-only Move draws through its camera, and nothing on the wire.

<a id="decision"></a>
## Decision
1. **Only the scene the Move goes to glides.** While a camera-only Move runs, the
   scene it goes to is drawn through the Move's camera, and every other scene of its
   family keeps its own camera, a mix of the family's scene or of a sibling keyframe
   included.
2. **A mix of that scene glides with program.** A mix that shows the scene the Move
   goes to shows it as program does, through the gliding camera, since it is the same
   scene drawn at the same time.

This amends ADR-0226 decision 1 on which cameras carry the glide. The rest of ADR-0226
holds.

<a id="consequences"></a>
## Consequences
- A mix of another view of a 3D scene holds still while program glides between two
  of its views.
- A mix of the program scene itself moves with the glide, where before ADR-0226 it
  stayed at the destination's camera during the Move.
