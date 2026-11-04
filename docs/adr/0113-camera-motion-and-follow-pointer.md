<!-- @doc
id = "ADR-0113"
title = "Camera motion, and a scene camera that follows the pointer over a capture"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0113: Camera motion, and a scene camera that follows the pointer over a capture

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Extends [ADR-0094](0094-scene-cameras.md) (scene cameras) with
[ADR-0099](0099-native-motion-modifiers-and-timelines.md)'s motion modifiers, and adds the
follow mode whose zoom input ADR-0112 (assignable camera zoom bindings, on `main`) decides.

<a id="context"></a>
## Context
A scene camera is one value. It moves when an edit or a live value changes it, and through
keyframe scenes and `Move` transitions (ADR-0094 decisions 6 and 7). It can't move on its
own, so a handheld sway or a slow drift has to be keyed.

The maintainer also wants the effect from the follow-cursor mock render. A camera sits
close to a captured screen, at a slight angle, and glides after the pointer, with depth of
field focused where it looks, and a zoom the person drives live. That camera depends on
where the pointer is, frame by frame. `CaptureStream::pointer` (#1152) now reads where the
pointer is over a capture. A camera that follows it has state of its own: springs that
carry it from frame to frame, and a zoom. That state isn't a function of time, as motion
modifiers are, or of the scene, as a camera value is.

The mock's algorithm is described in its notes (`follow_reel.rs` in the project files):
- It aims with a dead zone measured in the view.
- It pulls back with the pointer's speed.
- It keeps the screen filling the frame by sliding the aim, and by moving nearer when that
  isn't enough.
- It smooths through two chained critically damped springs.
- It leans with the pointer's velocity.
- It focuses at the aim's distance.

It ran 27 s with no frame showing past the screen's edge.

<a id="decision"></a>
## Decision
1. **Camera motion.** A scene gains `camera_motion`, beside its camera: at most 8 camera
   motion modifiers.
   - **Kinds, clocks and checks:** the same as an item's (`MotionKind`, `MotionClock`, and
     the checks in `check_motion`).
   - **Properties:** the camera's own. These are the position's and the target's x, y and
     z (in scene pixels), `fov` and `roll` (in degrees), and `focus` and `blur` (in
     pixels).
   - **How they apply:** each value is added to the camera as it is drawn, never stored.
     The field of view is then clamped to 1 to 170°, focus to above 0, and blur to 0 to 64.
     A camera whose position would reach its target keeps the unmoved position.
   - **Clocks:** the engine clock runs everywhere, and the shown clock starts when the
     scene starts being drawn on program.
   - **Edits:** `scene.set_camera_motion { scene_id, motion }` writes the whole list. Its
     inverse is the previous list, and it is one undo step. The list stays out of
     `SceneCamera`, which stays a small `Copy` value, so `scene.set_camera`, its live value
     and every camera override are unchanged.
   - **Keyframes:** the list belongs to the family. A variant draws its parent's list, and
     setting it on a variant is `invalid_argument`. A `Move` between two scenes blends the
     two sides' offsets by its eased progress, as it does an item's.
   - **Scenes without a camera:** a scene with no camera keeps its list, which draws
     nothing until it has a camera again.
2. **Follow mode.** A scene gains `camera_follow`, which is absent by default. It is set
   with `scene.set_camera_follow { scene_id, follow? }`, whose inverse is the previous
   value. It belongs to the family, as `camera_motion` does, and is kept, inert, while the
   scene has no camera.
   - **Fields:**
     - `item_id`: an item of the scene whose source is a display or window capture.
     - `zoom`: the resting zoom, from 1 to 4, default 1. At zoom 1 the frame shows two
       thirds of the item's height, and zoom `z` shows `1/z` of that.
     - `yaw_deg` and `pitch_deg`: the resting angle off the item's normal, to its right
       and below it, defaults 20° and −10° (a little to the right, looking down), each
       within ±60°.
     - `lean_deg`: how far the camera leans with the pointer's velocity, default 9°, from
       0 (off) to 30.
     - `smooth_ms`: the time constant of the two springs, default 450, from 50 to 5000.
   - **What follow sets:** while `follow` is set, the engine computes the camera's position
     and target each frame. The camera's own `position` and `target` are where it starts,
     and where it rests when no pointer is known. `fov_deg`, `roll_deg`, `depth_sort` and
     `blur_px` apply as always. When `blur_px` is above 0, focus is at the aim's distance
     every frame, and `focus_px` is ignored. Camera motion applies after follow, so a
     wiggle adds a handheld sway to it.
   - **The algorithm:** the mock's, on the item's box at its depth. The pointer is mapped
     from capture pixels through the item's crop to its box. The frame never shows past
     the box's edges where the zoom and the angle allow it. At an angle the frame's
     footprint on the box is a skewed quad, so near an edge the frame stays on the box
     first and the pointer can sit just outside the frame. A rotated, tilted or turned
     item is followed as its unrotated box at its committed geometry, so a live drag of
     the item moves the camera once it is committed: that is approximate, and
     documented.
   - **Without a pointer:** where the capture gives no position (Linux for now, a macOS
     window, before the first frame, or a source that isn't a capture), the camera holds
     its last aim, or rests at the box's centre.
   - **Checks:** an `item_id` that isn't in the scene,
     or whose source isn't a display or window capture, is `invalid_argument`. Deleting
     the item, or moving it out of the scene, leaves the follow in place and inert: the
     camera rests at its own position and target until the follow is set again, or an
     undo brings the item back.
3. **Follow state lives on the render thread.** The springs, the smoothed speed and the
   live zoom are kept per scene. Every pass that draws the scene in a frame shares them,
   and they advance once per frame by the frame's time. A scene that stops being drawn
   keeps its state for 10 s, then drops it, so it starts again at rest. The state is never
   stored or reported. A `Move` into or out of a follow scene uses that scene's camera as
   computed for the frame.
4. **Zoom is an action.** `scene.zoom_camera { scene_id, steps }` multiplies the scene's
   live zoom by 1.25 per step. A negative step zooms out, and the result is held to 1 to 4.
   The zoom is smoothed in log space over 350 ms. It isn't an edit: it isn't undone or
   stored, and it starts again at `follow.zoom` when the camera's `follow` changes or the
   engine restarts. It takes one step by default, so a hotkey can bind it, and a wheel
   chord once ADR-0112's wheel keys land (decision 3).
   It is `not_found` for a scene that doesn't exist and `action_failed` for one
   without a follow camera. The engine counts the steps asked for each family and hands
   the count to the render thread on the live-value channel; a follow camera takes the
   steps asked for since it started.
5. **Wire and files.** These are additive fields and one new action, shipped as one minor
   protocol version from the alpha.4 release queue. In collection files, `motion` and
   `follow` are omitted when empty, with the schema-version bump, migration and fixture
   R-API-003 asks for. The UI's Scene Properties camera section gets a Follow pointer group
   and a Camera motion list, and the MCP design notes describe both.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - a camera sways, drifts or breathes with no keys, on the same modifiers items use;
  - the follow-cursor shot is one setting on a 3D scene with a display capture in it, and
    its zoom is a hotkey, a wheel chord or an agent's action.
- **Harder:**
  - for the first time a scene's picture depends on state the render thread carries from
    frame to frame. Two engines given the same edits and the same pointer path draw the
    same frames only if they draw the same frame times. Tests drive the springs with
    fixed steps.
  - Linux follows nothing until the portal's pointer metadata is drawn by the engine;
    until then a follow camera there rests at the box's centre.
  - following an item that is rotated or turned in 3D is approximate.
- **We must now:**
  - get the protocol version from the release queue;
  - carry #1152's review notes N1 and N2 (the published frame's size, and `None` when the
    DPI context can't be set) into the render-thread wiring;
  - check the pointer on real hardware (Windows, and a macOS display);
  - with ADR-0112's PR: the wheel keys, so a wheel chord can zoom (refusing one bound
    with more than one step), and decision 4's default-binding offer;
  - keep in mind that an inert follow (its item gone) isn't saved: a project file holds
    only a follow whose item is in the scene, since the load's `scene.set_camera_follow`
    would refuse any other.
