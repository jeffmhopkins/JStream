<!-- @doc
id = "ADR-0094"
title = "Scene cameras: items in one 3D space"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0094: Scene cameras: items in one 3D space

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0022](0022-item-transform-and-canvas.md) gives every item a 3D placement of its own:
`tilt_deg`, `turn_deg`, `depth_px` and a per-item `perspective_px`, each item seen by its
own viewer centred on it. [ADR-0050](0050-3d-transforms-rendering.md) draws that: a
homogeneous polygon per item, a near plane at `w = 1/256`, mips with anisotropic sampling,
analytic edge coverage, and items in stacking order with no depth sorting. Its decision 4
names what it leaves out: "a shared 3D space for a group … would be a new scene-level field
and a new ADR".

The maintainer asked for that: sources that have a real place in a 3D scene, seen through
a camera the user controls (position, target, field of view), which agents can drive over
MCP and keyframe scenes can animate. Today a 3D look is faked per item: two items can't
share a vanishing point unless their centres coincide, moving the "camera" means editing
every item, and `depth_px` of two items isn't comparable, so nothing can be drawn
nearest-last.

What exists to build on:
- **Placement math** in `jstream-protocol` (`geometry/drawn.rs`): an item's plane is an
  affine map from its local `(x, y)` to a viewing frame, then a divide by `w`. The near
  clip, the inverse map (`unplace`), the pushed-out edge vertices and the compositor's `w`
  attribute all work from that one map.
- **Scenes** carry per-scene behaviour in `SceneOptions` ([ADR-0089](0089-scene-options-and-nested-audio.md))
  and switch through transitions ([ADR-0029](0029-transitions.md)).
- **Keyframe scenes** ([ADR-0049](0049-keyframe-scene-model-and-evaluation.md)) are
  variants that override parts of their parent and animate by a Move switch.
- **Live values** ([ADR-0032](0032-live-edits-and-undo.md)) carry a drag on the wire
  without an undo step per frame.
- **MCP tools** are generated from the protocol's edits, so a new edit reaches agents with
  its schema.

The forces:
- Turning 3D on must not move anything that is laid out flat. A scene of items with
  neutral 3D should look the same the moment it gets a camera, so a user can switch it on
  and then start moving the camera.
- Items are mostly transparent (graphics, keyed video, text). A depth buffer can't
  blend them; drawing far to near can.
- Nested scenes and groups are 2D layers today (ADR-0050 decision 4), and the
  compositor has one pass per scene.
- Anything new on the wire is a minor addition; an older client must keep working.

This ADR adds a scene-level camera. It changes nothing for a scene without one, so it
extends ADR-0050 rather than contradicting it. In a 3D scene only, decision 4 replaces
ADR-0022's painter's order (items in stacking order whatever their `z`) and decision 2
makes `depth_px` a position in the scene's space rather than only a change of projected
size; ADR-0022 still holds as written for every scene without a camera. It is agent tier
([R-GOV-005](../RULES.md#R-GOV-005)): the maintainer asked for the feature, and it changes
no licence, security or privacy posture, compatibility promise or spending; the protocol
gains an optional field and an edit.

<a id="decision"></a>
## Decision
**1. A scene may have a camera.** `SceneInfo` gains an optional `camera`. Absent, the
scene is drawn exactly as today: per-item perspective, stacking order. Present, the scene
is a **3D scene** and every item in it is placed in one 3D space and seen through the
camera:

| Field | Meaning | Default |
|---|---|---|
| `position` | Where the camera is, a `Vec3` in scene pixels | — (required) |
| `target` | The point it looks at, a `Vec3` | — (required) |
| `fov_deg` | Vertical field of view, in degrees, `1 ≤ fov ≤ 170` | 45 |
| `roll_deg` | Rotation of the picture about the viewing axis, clockwise | 0 |
| `depth_sort` | Draw items far to near (decision 4); `false` keeps stacking order | `true` |

Every number is `Finite32`. `position` equal to `target` (closer than 0.001 px), a field of
view out of range, and a coordinate past `MAX_DRAWN_PX` are `invalid_argument`.

**2. The scene's space is the canvas, extended toward the viewer.** World coordinates are
canvas pixels: x right, y down, z toward the viewer, the same frame as ADR-0022's local
frame, with the canvas at `z = 0`. An item's existing geometry is its pose in that space,
so no item field is added:
- its position is `(center.x, center.y, depth_px)`;
- its orientation is `rotation_deg`, `turn_deg` and `tilt_deg`, applied in ADR-0022's
  order about its centre;
- `size`, `fit_mode`, crop and flips mean what they mean today;
- `perspective_px` is ignored in a 3D scene, since the camera supplies the perspective.
  It is kept, so removing the camera brings the item's own perspective back.

**3. The camera's projection.** The camera looks from `position` toward `target`. The
picture's up is world `−y` (up on the canvas). Within 0.01° of straight down it is world
`−z`, and within 0.01° of straight up world `+z`: the up a camera facing `−z` has as it
tilts to either pole, so an orbit to the pole doesn't turn the picture over (#1047).
`roll_deg` then turns the picture clockwise. A point at distance `D` along the viewing axis lands at the canvas centre plus `f · (X, Y) / D`, where
`(X, Y)` are its right and down offsets from the axis and `f = (canvas height / 2) /
tan(fov / 2)` is the focal length in pixels.
- In ADR-0050's terms this is the same homogeneous map with `w = D / f`. So the near plane
  (`w ≥ 1/256`, a point closer than `f / 256`), the drawn polygon, `unplace`, edge
  coverage, mips and anisotropic sampling apply unchanged, through one shared function in
  `jstream-protocol`'s placement module that both the engine and the UI use.
- **The framing camera** is the one a user starts from: target at the canvas centre on
  `z = 0`, position straight in front of it at distance `f` for 45°. Through it, every item
  with neutral 3D lands exactly on its 2D box, so adding it moves nothing (`w = 1` at
  `z = 0`). `SceneCamera::framing(canvas)` builds it.
- Items in a 3D scene always take the 3D drawing path (edge coverage, mips), whatever
  their own angles.

**4. Far to near, stacking order breaking ties.** With `depth_sort` on, the items of a 3D
scene draw in order of the distance along the viewing axis of their box centre, farthest
first. Equal distances keep stacking order. Items aren't split where they pass through
each other, and there is still no depth buffer: as in ADR-0050, the later one covers the
earlier wherever they overlap. With `depth_sort` off, stacking order alone decides, as in
a 2D scene. A blend mode composites with whatever was drawn before it in that order.

**5. Nested scenes and groups stay 2D layers.** A scene source or a group placed in a 3D
scene is a flat card placed in the 3D space like any item. Its own items are drawn into its
layer first, through its own camera if it has one. A user who wants a fixed overlay over a
3D set nests the 3D scene in a 2D one and puts the overlay above it.

**6. Editing the camera.** One edit, `scene.set_camera { scene_id, camera? }`, sets or
removes the camera. Its inverse is `scene.set_camera` with the previous value. It is a
state edit, batchable, one undo step, and bumps `rev`.
- **Live.** `live.set` accepts it ([ADR-0032](0032-live-edits-and-undo.md)), so an orbit
  drag in the UI, or an agent stepping the camera, moves the picture every frame and
  commits once.
- **Keyframe scenes.** A variant's camera follows its parent's unless the variant
  overrides it: `scene.set_camera` on a variant writes the override, and `camera: null`
  on a variant removes it. Whether a scene is 3D at all belongs to the family: a camera
  override on a variant whose parent has none is `invalid_argument`, and removing the
  parent's camera drops its variants' overrides, which the inverse restores, as ADR-0040
  §1 does for item overrides. The override is reported as the variant's `camera`, and the
  effective camera is the override or else the parent's.
- **Collection files** store `camera` on the scene, omitted when absent. The schema version
  stays the same: a file with no camera is unchanged.

**7. Transitions.** A switch between two 3D scenes with a `Move` preset moves the camera as
well: its position, target and field of view linearly, its roll the shortest way, with the
same easing and progress as the items, and items paired as ADR-0029 and ADR-0049 pair
them are interpolated in that moving space. Between keyframes of one family this is how a
camera move is animated. A Move between a 3D scene and a 2D one has no single space to
move in, so it runs as a `Fade` of the same duration. `Fade` and `Cut` are unchanged.

**8. Agents.** The MCP `scene` tool gains `scene.set_camera` from the protocol, with its
schema, and the design guidance agents read (`jstream-mcp`'s design notes) explains the
space, the framing camera and how to animate a camera with keyframe scenes.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - a set, a floor and a wall share one vanishing point; a camera move is one value, not
    an edit per item;
  - 3D costs no new pass, target or depth buffer: it is ADR-0050's polygon with a
    different affine map, and a sort per scene;
  - camera moves animate through keyframe scenes and transitions that already exist, and
    agents get the edit with no hand-written tool.
- **Harder:**
  - sorting by centre is wrong for some arrangements (a long floor under a nearer small
    item can sort in front of it at grazing angles); `depth_sort: false` or splitting the
    item are the workarounds, and a per-pixel depth test would need a new ADR;
  - every item in a 3D scene pays for 3D sampling and edge coverage;
  - an item that already has 3D (`depth_px`, angles, or its own `perspective_px`) looks
    different once its scene gets a camera: `depth_px` moves it toward the camera, its
    angles gain the camera's perspective, and its own perspective is ignored. The UI
    says so when it turns a scene's camera on;
  - the UI's canvas has no handles in a 3D scene: dragging an item moves it on the plane
    of constant depth through it (`z = depth_px`, `CameraView::on_plane`), not along
    screen axes and not in a tilted item's own plane.
- **Not decided here:**
  - groups that share their parent's 3D space (CSS `preserve-3d`), which would flatten
    the group into the parent's sort;
  - timeline animation, keyframes that advance by themselves (ADR-0049 leaves it to a
    maintainer-tier ADR);
  - lights, shadows and materials.
- **Follow-up implementation PRs:**
  - `jstream-protocol`: `Vec3`, `SceneCamera`, `SceneInfo.camera`, `scene.set_camera`, the
    shared camera projection beside the drawn polygon, with unit tests that the framing
    camera reproduces 2D boxes; `jstream-graph`, the mock engine and collection files;
  - `jstream-render` and `jstream-core`: a 3D scene's items drawn through the camera and
    sorted, golden tests, live values and the Move camera of decision 7;
  - `jstream-ui`: the camera in scene Properties and an orbit drag on the canvas;
  - ARCH › Data model and GPU pipeline gain the camera.
