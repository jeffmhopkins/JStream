<!-- @doc
id = "ADR-0166"
title = "Moves curve through keyframes, turn as one rotation, and may orbit the camera"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0166: Moves curve through keyframes, turn as one rotation, and may orbit the camera

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Keyframe scenes ([ADR-0049](0049-keyframe-scene-model-and-evaluation.md)) already hold a
full 3D pose for every item (`center`, `depth_px`, `rotation_deg`, `turn_deg`, `tilt_deg`,
`size`) and for a scene's camera (`position`, `target`, `fov_deg`, `roll_deg`, focus and
blur, [ADR-0094](0094-scene-cameras.md) and [ADR-0108](0108-camera-depth-of-field.md)).
A Move between two members of a family glides all of it, and a placed scene switches its
own keyframe the same way ([ADR-0116](0116-keyframe-tracks.md)). What happens between the
two ends is where it falls short:

- **Two ends only.** ADR-0029 §5 and ADR-0094 decision 7 interpolate each value in a
  straight line from one end to the other. A camera that should go from one shot through
  a second to a third has to be three Moves, chained by a timeline or by hotkeys. Each
  Move eases to a stop at its end, or, when the next one cuts it short, turns a corner
  where it retargets (ADR-0029 §7). Either way the path stops or kinks at every middle
  keyframe.
- **Angles one at a time.** Each of an item's three angles takes the shortest way on its
  own. When more than one changes, the item doesn't turn about one axis: it wobbles
  through orientations neither end has, and a model or card turned in 3D visibly swings
  wide of the direct turn.
- **Straight camera moves.** The camera's position and target move in straight lines.
  A camera that should circle what it looks at instead cuts across the chord, toward its
  target, and then out again: its distance dips and the subject swells mid-move.

What exists to build on: the transition plan the control thread sends and the render
thread samples (ADR-0029 §4, `jstream-render`'s `transition` module); pairing by item
within a family (ADR-0049 §3); keyframe timing (ADR-0129); and the 2D side of a Move seen
through the framing camera (ADR-0128).

This is agent tier ([R-GOV-005](../RULES.md#R-GOV-005), [ADR-0043](0043-narrow-maintainer-tier.md)).
It changes how a switch the user starts moves, within the keyframe scenes and Moves that
are already in scope. It is not the timeline animation ADR-0049 leaves to a maintainer
decision: nothing advances on its own clock, and no property gets curves of its own. A
path is still one switch, started by a call, a hotkey or a cue, that runs for one
preset's duration. The protocol gains optional fields only.

<a id="decision"></a>
## Decision
This amends ADR-0029 §5 (angles the shortest way, everything else in a straight line)
and extends ADR-0094 decision 7, ADR-0049 §3 and ADR-0116 decision 4. The rest of each
stands.

**1. An item turns as one rotation.** When a pair's orientation changes in more than one
of `rotation_deg`, `turn_deg` and `tilt_deg`, a Move turns it as one rotation: each end's
three angles, in ADR-0022's order, as a unit quaternion, and the shortest arc between the
two, at the pair's eased progress. Each sample turns back into the three angles, which is
what the placement reads. The orientation is continuous; the triple is, of those that
give it, the one nearest the per-axis path, so it may switch to another triple for the
same orientation between frames, which draws the same. The first and last samples are
the ends' own angles. A spring's overshoot carries on along the same arc.
- A pair that changes only one angle moves as it does today, the shortest way on that
  angle, which is the same rotation.
- A billboard (ADR-0151 decision 2) keeps the per-axis way: it draws only its rotation
  and faces the camera with the rest, so one rotation split over all three would roll it.
- This holds wherever a pair interpolates: scene switches, keyframe Moves, placed
  keyframes, and the Moves of `item.show` and `item.hide`. It is the same in 2D scenes,
  where an item's own perspective draws the same angles.
- The camera has no angles of its own besides `roll_deg`, which still takes the shortest
  way. Where it points follows its position and target.

**2. A Move may pass through keyframes on its way: `via`.** `scene.switch` and
`scene.switch_and_watch` gain an optional `via`, a list of up to eight scenes the Move
passes through, in order, before it ends on the one it names.
- **What may be named.** Each entry is a member of the destination's family (ADR-0049
  §3), other than the destination itself. A missing scene is `not_found`. Any other
  entry, a repeat of the entry before it, or more than eight is `invalid_argument` on
  `via`.
- **When it applies.** Only a Move between members of one family follows `via`: the
  Move pairs by item there, so each item has a value at every stop. A Fade, a cut, a
  stinger, or a Move from a scene outside the family runs as it would without `via`.
- **The curve.** Each value a pair moves follows one curve through its stops: where it
  starts (on screen), its value in each `via` scene, and its value at the end. Positions,
  sizes, depths, crop, opacity, perspective, a card's surface numbers and every camera
  number follow a Catmull-Rom curve with its stops at equal shares of the path; its ends
  take the one-sided tangent, so a path with no `via` is exactly today's straight Move.
  Orientations (decision 1) follow the spherical form of the same curve (Shoemake's
  construction on quaternions), which is today's shortest arc with no `via`. The curve
  is continuous in value and in velocity at every middle stop, so the move doesn't stop
  or kink there. Settings that switch rather than glide (fit mode, flips, billboard,
  blend mode, depth sort) still switch at the path's midpoint.
- **Timing.** The path is one Move. The preset's duration and easing, or the item's own
  and the destination keyframe's (ADR-0129), apply to the whole path, so only its two
  ends ease. A `via` scene's own timing and transition overrides aren't used.
- **Items a stop doesn't show.** An item hidden in a `via` scene passes that stop by: its
  curve runs through the stops that show it. An item that appears or disappears in the
  Move (ADR-0049 §3) does so as it would without `via`.
- **Groups.** A group's children that a Move moves inside the group's layer (ADR-0049 §3)
  go straight from their ends, with the group item's progress; the group item itself
  follows the path.
- **Cutting it short.** A switch while a path runs starts from what is on screen, as
  ADR-0029 §7 retargets any transition.
- **Bindings.** A hotkey binding names its `via` scenes as well as its destination, so
  ADR-0040 §3 removes it when any of them is deleted. A timeline cue's path to a deleted
  stop fails when it fires, as ADR-0115 decision 2 says of any cue.
- **What a path reports.** The switch reports only its destination, as it would without
  `via` (`studio.changed`): the `via` scenes are never on program.

**3. A Move may orbit the camera around its target.** The `move` transition kind gains a
setting, `camera_path`: `line` (the default) or `orbit`.
- Under `line`, the camera's position moves along its curve as decision 2 and ADR-0094
  decision 7 say.
- Under `orbit`, the target moves as before, and the position moves about it: its
  direction from the target turns along the shortest arc between the ends' directions
  (or along the spherical curve through the stops of a path), and its distance from the
  target follows its own curve. The camera circles what it looks at instead of cutting
  across. When the two directions are opposite, it turns about the picture's up.
- It changes nothing at the ends. A Move between a 3D side and a 2D one orbits between
  the 3D camera and the framing camera (ADR-0128). A preset whose Move has no camera, or
  whose camera stays still, ignores it.

**4. Agents.** The MCP tools take `via` and `camera_path` from the protocol with their
schemas. The design guidance agents read (`jstream-mcp`'s design notes) explains a path
through keyframes and an orbiting Move, in general terms.

**5. Compatibility.** Every change to the wire is an addition, released as a minor
protocol version: the `via` fields, and the `move` kind's new setting, whose default
keeps today's behaviour. A hotkey binding or a timeline cue stores its action as JSON, so
one may carry `via` with no change to the collection's schema. Decision 1 changes how an
existing Move looks when it turns an item about more than one axis, and changes no value
at either end.

<a id="consequences"></a>
## Consequences
- **Easier.** A camera move through several shots is one switch with a smooth path, from
  a call, a hotkey or one cue, instead of a chain of Moves that stop at each shot. Items
  move along the same path. A turn in 3D looks like a turn, and a camera can circle a
  subject without its distance dipping.
- **Harder.**
  - A Catmull-Rom curve can overshoot a stop's value between two stops when they are
    uneven: a camera path may swing a little wide of a straight line on the way. That is
    the price of keeping its velocity.
  - Equal shares of the path mean a long leg and a short one take the same time, so the
    camera speeds up on the long one. Per-stop times are not decided here.
  - The planner plans each `via` scene's items as well as the destination's, which costs
    one more plan per stop when the switch starts, not per frame.
- **Not decided here:**
  - a chain of separate Moves, fired one after another, keeping its speed where one cuts
    the next short;
  - per-stop times along a path;
  - the children of a group drawn as one layer following the path inside it;
  - `via` on `scene.set_keyframe` (ADR-0116), which would plan the program through each
    stop's placed keyframe. Decisions 1 and 3 already apply to its Moves;
  - a curve that reaches back past the scene on screen (a path's first leg takes the
    one-sided tangent).
- **Follow-up implementation PRs:**
  - decision 1, in `jstream-render`'s transition sampler, with no protocol change;
  - decisions 2 and 3: the protocol fields and the `move` kind's setting, the planner's
    stops and the sampler's curves in `jstream-core` and `jstream-render`, the mock
    engine, and the MCP design notes, in one protocol slot.
