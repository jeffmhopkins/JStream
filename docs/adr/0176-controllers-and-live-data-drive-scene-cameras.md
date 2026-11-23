<!-- @doc
id = "ADR-0176"
title = "Controllers and live data drive scene cameras, which pan, tilt and autofocus"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0176: Controllers and live data drive scene cameras, which pan, tilt and autofocus

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Replaces decision 5 of [ADR-0168](0168-midi-controllers-drive-motion.md) ("Not on
cameras") and the matching refusal of `data` modifiers on cameras from
[ADR-0171](0171-live-data-from-a-daw-drives-scenes.md). Every other decision of both
stands. Decision 6 raises the cap of 8 modifiers that
[ADR-0099](0099-native-motion-modifiers-and-timelines.md) set.

<a id="context"></a>
## Context
A scene camera moves through `camera_motion`
([ADR-0113](0113-camera-motion-and-follow-pointer.md)): up to 8 modifiers, each adding to
its position, target, field of view, roll, focus or blur. ADR-0168 gave items `midi`
modifiers, a fader or knob from a local port or a JTunnel sender mapped onto a range and
smoothed once a tick by the render thread's follower, and kept them off cameras only
because no follower ran for cameras. ADR-0171 did the same for `data` modifiers.

The maintainer wants one system that a controller can be assigned through to anything
that takes a number, the camera included: its position, its rotation and its field of
view (a zoom), as it already is for an item's pose, effects and source settings. A
camera's rotation today is only where it looks (its target) and its roll; turning the
view by an angle, as a camera operator does, takes moving the target by hand.

The maintainer also wants the lens to behave like one: an aperture that goes from
everything sharp to a shallow depth of field, and, when it isn't everything sharp, a
focus that by default follows whatever is at the centre of the frame, with a switch,
which a controller can also work, to focus by hand instead. A camera's depth of field
([ADR-0108](0108-camera-depth-of-field.md)) already has the aperture in all but name,
`blur_px`, the blur far behind the focus plane: 0 keeps everything sharp. And binding
a controller to every property of a camera, or of an item with effects, needs more
than the 8 modifiers a list may hold.

<a id="decision"></a>
## Decision
1. **A camera's motion takes `midi` and `data`.** `check_camera_motion` no longer refuses
   them, a minor protocol version (0.167, [R-API-001](../RULES.md#R-API-001)). Their
   parameters, ranges and defaults are an item's, and `scene.set_camera_motion` sets them
   with its undo. `audio` stays refused: a camera hears nothing.
2. **The same follower.** Planning records a camera's MIDI and data modifiers in the shot
   beside its items' (`Bindings::record_camera`), keyed by the scene family and modifier
   ID instead of an item. The follower smooths them once a tick exactly as it smooths an
   item's (ADR-0168 decision 3, and ADR-0173 for held clocks and previews), so a camera
   and an item driven by one fader move together. Camera motion belongs to the family
   (ADR-0113), so one smoothing carries across the family's keyframes and through a Move
   between them, and the control offsets every view of the family alike. A shot no
   follower follows reads the control as it is now, as an item's does.
3. **Pan and tilt.** `CameraProperty` gains `pan` (to the right) and `tilt` (up), in
   degrees. After the position and target have moved, they turn the view about the
   camera's position, the target kept at its distance and the view held within 89° of
   level: the turn a live aim makes ([ADR-0152](0152-a-ptz-controller-drives-scene-cameras.md)),
   computed by the same function. A modifier of any kind may move them.
4. **Resting value.** As on an item, a controller reads 0 before it first sends anything,
   so a `midi` modifier rests at its `from`. A binding meant to rest at the stored view
   either starts at `from` 0 or is sent its centre once.
5. **Aperture and autofocus.** `blur_px` is the camera's aperture, as the Properties
   dialog now names it. `SceneCamera` gains `autofocus` (default `false`, so stored
   cameras keep their focus). With it on and a depth of field, each frame the engine
   measures how far along the viewing axis the nearest item of the scene family's pass
   meets it, through each item's box at its motion (an item placed through a 3D group is
   passed over), once per frame time, and glides `focus_px` toward that in log distance
   with a 0.12 s time constant; the first frame lands on it, and while nothing is met the
   focus holds (at first `focus_px`). The glide is kept per render context and family, as
   a follow camera's springs are, and a Move holds the last focus. `CameraProperty`
   gains `autofocus`: a modifier's value is added to 1 when the switch is on and 0 when
   off, and autofocus is on while the sum is at least one half, so a CC button or a
   fader past its middle switches it. The Properties dialog turns autofocus on with
   Depth of field.
6. **32 modifiers.** `MAX_MOTION_MODIFIERS`, for an item's list and a camera's, is 32,
   so each of a camera's 13 properties, or an item's placement and effect settings, can
   have a controller and still have motion. Evaluating a list costs a step per modifier,
   with no allocation, so the larger cap costs only what a scene uses.

<a id="consequences"></a>
## Consequences
- **Easier:** a fader moves, turns or zooms a scene camera from Bitwig over JTunnel, or
  from any local port, with the call an agent already uses for camera motion; a DAW's
  live data can drive it too.
- **Easier:** pan and tilt turn a camera by an angle without computing a target.
- **Easier:** the camera focuses like a camera operator's: on what it looks at, unless
  told otherwise, and a button switches that.
- **Harder:** autofocus measures item boxes, so a model is met at its box rather than
  its surface, and an item inside a 3D group isn't met. A depth readback could replace
  the measure later.
- **Not decided here:** an audio modifier on a camera, which would need the program
  mix's scope planned for it.
- **Must do:** the maintainer's hardware run checks a real controller moving a camera
  end to end over JTunnel; CI only feeds messages in tests.
