<!-- @doc
id = "ADR-0120"
title = "Follow pointer is a filter on a capture, with a safe margin"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0120: Follow pointer is a filter on a capture, with a safe margin

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Amends [ADR-0113](0113-camera-motion-and-follow-pointer.md): where a follow camera lives
(decision 2), how it is set and saved (decision 5), what happens when its item goes, and
a safe margin for the pointer. Its algorithm, its state on the render thread (decision 3)
and its zoom (decision 4) are unchanged.

<a id="context"></a>
## Context
ADR-0113 put the follow camera on the scene: `camera_follow`, set with
`scene.set_camera_follow` and edited in Scene Properties under 3D camera. To use it,
someone has to turn on the scene's 3D camera first, then find Follow pointer in that
section. The maintainer's first hardware run found it hidden there (#1225): it is an
effect on a capture, so people look for it where they add effects.

The same run found the pointer leaving the frame (#1216). The camera glides after the
pointer through springs with a fixed time constant. On a quick move, more so when zoomed
in, the springs fall behind and the pointer leaves the view. Nothing in ADR-0113
guarantees it stays in.

<a id="decision"></a>
## Decision
1. **A filter kind.** `follow_pointer` is a `video_filter` kind (its descriptor in
   `jstream-protocol`, beside `CameraFollow`). Its parameters are a `CameraFollow`'s
   numbers: `zoom`, `yaw_deg`, `pitch_deg`, `lean_deg`, `smooth_ms` and the new
   `safe_margin_pct`, with the same ranges and defaults. It goes only on an item's own
   effects, on an item that shows a display or window capture; on any other owner,
   `filter.add` is `invalid_argument` on `owner`. It draws nothing: the chain runtime skips
   it, so it costs no pass.
2. **The filter drives its scene's camera.** A scene's follow camera is the first enabled
   `follow_pointer` filter on its items' effects, in stacking order, read as a
   `CameraFollow` of that item. A keyframe scene draws its parent's, as before. Its
   behaviour is ADR-0113's.
3. **It supplies the camera.** A scene with a follow camera and no camera of its own (or,
   for a keyframe scene, no camera from its parent) draws through the framing camera
   (ADR-0094 decision 3), which shows the scene as it looks in 2D until the pointer moves
   it. Nothing is stored: turning the filter off or removing it returns the scene to its
   own camera, or to 2D. Undo is the filter edits' undo.
4. **A safe margin.** `safe_margin_pct`, from 0 to 40 and 10 by default, is how far in
   from each edge of the frame the pointer stays, in percent of the frame's width and
   height. After each step of the springs, if the pointer as the camera sees it (through
   its angle, lean and zoom) is past the margin, the look point slides toward the pointer
   just far enough to put it back on the margin, and the springs carry on from there. The
   pull-in the follow camera does for a corner aims for the same margin. ADR-0113's rule
   that the frame never shows past the item's box still comes first, so within the margin
   of the box's own edges, at an angle, the pointer can sit between the margin and the
   frame's edge.
5. **The wire.** One minor protocol version, from the alpha.5 queue:
   - `scene.set_camera_follow` stays, as a shortcut for the filter edits: with `follow`,
     it sets the followed item's `follow_pointer` filter to its numbers and turns it on,
     adding one last in the item's effects when it has none, and removes every other
     `follow_pointer` filter of the scene's items; without, it removes them all. Its
     inverse is those filter edits' inverse.
   - `SceneInfo.camera_follow` stays, read from the filter as decision 2 says.
   - `CameraFollow` gains `safe_margin_pct`, omitted as 10.
6. **Collection files.** Schema version 7 moves each scene's `camera_follow` onto the item
   it follows, as a `follow_pointer` effect last in its `effects`. A follow whose item had
   left the scene was inert and is dropped. A version 7 file has no `camera_follow`.
7. **The UI.** Add filter offers Follow pointer only on a display or window capture
   item's effects. Its settings and its Zoom in and Zoom out keys are in the Filters
   window. Scene Properties says which capture's filter moves the camera and opens it, or
   opens Add filter on the scene's first capture.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - Follow pointer is found where effects are, on the capture it follows, and works on
    a 2D scene with no camera set up.
  - the follow goes with its item: deleting, moving, copying or duplicating the item takes
    it along, and there is no inert follow left on a scene.
  - the pointer stays in view on fast moves whatever the smoothing.
- **Harder:**
  - a filter now changes something outside its owner's picture: its scene's camera. The
    chain runtime has to skip it, and the scene planner reads it.
  - with two enabled `follow_pointer` filters in one scene, only the first in stacking
    order drives the camera.
  - an older build loading a version 7 file keeps the filter as a kind it doesn't know,
    and follows nothing.
- **We must now:**
  - get the protocol version from the alpha.5 queue;
  - check the safe margin on Windows hardware with the follow camera (#1159);
  - lay out Scene Properties' camera section around the note (#1214).
