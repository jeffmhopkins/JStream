<!-- @doc
id = "ADR-0164"
title = "3D groups share their scene's space, and a camera may ride with one"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0164: 3D groups share their scene's space, and a camera may ride with one

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0094](0094-scene-cameras.md) decision 5 draws a group placed in
a 3D scene as one flat card: its children are drawn into the group's layer first, and the
layer is placed in the scene's space like any item. Its "Not decided here" names the other
choice: groups that share their parent's 3D space, as CSS `preserve-3d` does, which would
flatten the group into the parent's sort.

That other choice is what a set built from parts needs. A rig of a few models, cards and
lights that should move, sway or be keyframed as one can be grouped today, but the group's
card loses what makes the parts 3D:
- parallax between the parts, since they are one picture at one depth;
- their own place in the scene's far-to-near sort, so a part can't pass in front of an item
  outside the group and behind another;
- depth of field per part ([ADR-0108](0108-camera-depth-of-field.md)), since the card blurs
  by its centre;
- scene lighting and shadows ([ADR-0143](0143-scene-lights-and-depth-aware-scene-filters.md),
  [ADR-0151](0151-the-rest-of-richer-3d-scenes.md)): a light inside the group lights only
  the group's layer, and a model inside it writes no depth into the scene.

The alternative of moving each part on its own means an edit per part for every move, and
a Move, a motion modifier or a live drag can't move them as one.

A related need is a camera that moves with such a rig: a seat the view is taken from, on a
vehicle or a crane that moves, with the operator still turning the view inside it
([ADR-0152](0152-a-ptz-controller-drives-scene-cameras.md)). Today the camera's numbers are
in the scene's space, so a moving rig and a moving camera are keyframed separately and
drift apart.

What exists to build on:
- An item's pose is its `ItemGeometry` (ADR-0094 decision 2): centre and `depth_px` for its
  position, `rotation_deg`, `turn_deg` and `tilt_deg` for its orientation in ADR-0022's
  order, `size` for its scale. A model takes its pose from the same fields
  ([ADR-0107](0107-3d-models.md) decision 3), and so does a light (ADR-0143 decision 2).
- Every consumer of a 3D item (the drawn polygon, the sort, lights, shadow maps, lit
  cards, models, depth of field, billboards) reads that geometry and the scene's
  `SceneCamera`. So a group whose children resolve to ordinary geometries and whose camera
  resolves to an ordinary camera each frame needs no change in any of them.
- Scene options ([ADR-0089](0089-scene-options-and-nested-audio.md)) hold per-scene
  behaviour, set by `scene.set_options`, and a group is a scene with the `group` role
  ([ADR-0040](0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md)).

This extends ADR-0094 decision 5 for groups that ask for it. A group that doesn't is drawn
exactly as today, so it contradicts nothing accepted. It is agent tier
([R-GOV-005](../RULES.md#R-GOV-005)): it changes no licence, security or privacy posture,
compatibility promise or spending, and the protocol gains two optional fields.

<a id="decision"></a>
## Decision
**1. A group may share its scene's space.** `SceneOptions` gains `share_space`, default
`false` and omitted when `false`. On a group placed by an item in a 3D scene, it makes the
group a **3D group**: the group has no layer, and each of its children is placed in the
scene's space through the group item's pose and drawn as an item of that scene. Everywhere
else nothing changes:
- a group placed in a 2D scene, or inside a group that doesn't share space, is a flat layer
  as today, since there is no space to share there;
- on a scene with the `scene` role the option is kept and ignored, as a group keeps and
  ignores a background (ADR-0089 §4), so `scene.set_role` back to `group` brings it back;
- a group item with a legacy transform is drawn flat.

**2. How a child is placed.** A child's geometry is in the group's own canvas pixels, as
it is today. The group item maps that space into the scene by a rotation, a uniform scale
and a move:
- **Scale.** `s` is the scale the item's fit mode gives the group's canvas in the item's
  box: the smaller of the box's two scales for `fit`, the larger for `fill`. `stretch`
  takes the smaller, as a model takes it for depth (ADR-0107 decision 3), so a 3D group
  always scales uniformly and its children keep their shapes at any angle.
- **Position.** The group canvas's centre, on its `z = 0`, lands on the item's centre and
  `depth_px`. A point of the group's space is moved by `s`, turned by the item's
  orientation (ADR-0022's order, about that centre), then moved to it.
- **The child.** Its centre and `depth_px` are placed as that point. Its orientation is the
  group's orientation followed by its own, written back as one `rotation_deg`, `turn_deg`
  and `tilt_deg` in ADR-0022's order. Its size is `s` times its own. A model scales its
  depth by `s` too, and its fit mode and flips apply in its own frame as before.
- **Nesting.** A 3D group among the children of a 3D group is placed the same way, inside
  its parent's placed space, so rigs nest. Nesting stops at the depth any nesting stops at
  (`MAX_NESTING`); a deeper group draws flat.
- The result is an ordinary geometry in the scene's space, so ADR-0094's sort, the drawn
  polygon, edge coverage, depth of field, billboards, lit cards and models take it as it
  is. A billboard child turns to the camera from where it is placed.

**3. What a 3D group keeps and drops.** Each child keeps what it has as an item: its own
place in the scene's far-to-near sort (decision 4 of ADR-0094, so children interleave with
items outside the group), its blend mode, effects, crop, depth of field, lighting, shadows
and depth. Lights among the children light the whole scene, and models among them cast
shadows into it. The group item and the group scene have no layer, so:
- the item's `opacity` multiplies each child's, nested groups multiplying in turn, and a
  hidden item hides them all;
- the item's crop, flips, `billboard`, `surface`, blend mode and effects, the `scene`
  source's filters, and the group scene's own video chain don't apply. The group's
  Properties say so while `share_space` is on, and the options are kept for when it is off.

**4. Moving a 3D group.** The group item's pose is read the way any item's is, each frame,
and its children are placed through it then: its live values ([ADR-0032](0032-live-edits-and-undo.md)),
its motion modifiers and audio bindings ([ADR-0099](0099-native-motion-modifiers-and-timelines.md)),
and a Move that glides it. So one live drag, one modifier or one keyframe moves the whole
rig, with each child's own live values, modifiers and keyframes on top.
- In a Move within a family ([ADR-0049](0049-keyframe-scene-model-and-evaluation.md) §3),
  the group item and each child glide on their own tracks, and each frame places the
  child's gliding pose through the group's. A rig that turns sweeps its parts round on
  arcs rather than cutting across on straight lines.
- A child pairs, between the two sides of a transition, with the same child placed through
  the same group item on the other side. One on only one side appears or disappears as an
  unpaired item does.
- A group whose `share_space` differs between the two sides of a Move draws each side as
  that side has it, and the Move runs as a `Fade` of the same duration.

**5. A camera may ride with a 3D group.** `SceneCamera` gains `anchor`, an optional item ID.
With one that names a 3D group item of the camera's scene (for a keyframe scene, an item of
its family), the camera's `position` and `target` are points of that group's space, in its
canvas pixels as its children's are, and the camera is placed through the group item's pose
each frame as a child is: both points move with the group, the picture's up is the group's
up (its `−y` placed), and `roll_deg` rolls the picture from there. `fov_deg` doesn't change,
and `focus_px` is a distance in the group's space, so it scales by `s` with it.
- **Inside the rig.** Camera motion and the follow camera
  ([ADR-0113](0113-camera-motion-and-follow-pointer.md)) and the live aim (ADR-0152) apply
  in the group's space, before the camera is placed: an operator turns the view about the
  camera's place in the rig while the rig moves.
- **Resolved each frame.** The placed camera is an ordinary camera in the scene's space,
  with its roll worked out from the group's up, so every consumer of a scene's camera
  takes it unchanged. When the anchor is within 0.01° of the viewing axis (looking along
  the group's up), the up falls back as ADR-0094 decision 3's does.
- **Editing.** `scene.set_camera` writes `anchor` with the rest of the camera.
  An anchor that names no item of the scene (or family) whose source is a group is
  `invalid_argument`. One that later names no 3D group (the item removed, its group turned
  flat or to a scene) is kept and ignored, the camera drawn in the scene's space by its own
  numbers, and the scene's Properties say so.
- **Moves.** Between two cameras with the same anchor, a Move glides the camera's numbers
  in the group's space and places them through the gliding group each frame. Between
  different anchors, or an anchor and none, it glides between the two placed cameras in
  the scene's space.

**6. Agents and people.** `share_space` and `anchor` reach agents through
`scene.set_options` and `scene.set_camera` with their schemas, and `jstream-mcp`'s design
notes explain 3D groups, what they drop, and riding a camera with one. The group's
Properties gain a "Share the 3D space" switch and the scene camera's Properties a "Moves
with" group picker. In a 3D scene the canvas selects and drags a 3D group as one item, by
the outline of its children, and opening it edits its children in its own space, as for
any group.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - a rig of models, cards and lights moves, sways and keyframes as one, while each part
    keeps its parallax, sort, focus, light and shadow;
  - a camera can sit in a moving rig and still be aimed live from inside it;
  - no new pass, target or shader: a 3D group and an anchored camera resolve to the
    geometries and the camera every 3D consumer already takes.
- **Harder:**
  - a 3D group drops its own filters, effects, crop and blend; a rig that needs one of them
    is a flat group, or the effect goes on each child;
  - `stretch` scales a 3D group uniformly, so a group stretched to another aspect looks
    different once `share_space` is on; the Properties say so;
  - a child's place costs a composition per frame, and a 3D group with many children adds
    them all to the scene's sort;
  - ADR-0094's sort by centre is per child, so its grazing-angle mistakes now happen between
    parts of a rig too.
- **Not decided here:**
  - non-uniform scale and mirroring of a 3D group (shear can't be one geometry);
  - a nested *scene*, rather than a group, sharing its parent's space: it has its own
    camera and options, so it stays a card;
  - anchoring a light, or any item other than a camera, to an item that isn't its group.
- **Follow-up implementation PRs:**
  - `jstream-protocol`: `SceneOptions::share_space`, `SceneCamera::anchor`, and the
    composition of a child's geometry and of a camera through a group's pose beside the
    placement math, with unit tests that a neutral group places its children where their
    own geometry would; the graph's validation, the mock engine and collection files;
  - `jstream-core`: a 3D scene's 3D groups planned into its own pass, children placed each
    frame through the group's live, moved and gliding pose, their lights, models and boxes
    as the scene's; anchored cameras resolved in the pass and in transitions; tests;
  - `jstream-ui`: the two Properties controls, the outline and drag of a 3D group;
  - `jstream-mcp` design notes, and ARCH › Data model and GPU pipeline.
