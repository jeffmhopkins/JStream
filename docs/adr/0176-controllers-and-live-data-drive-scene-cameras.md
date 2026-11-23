<!-- @doc
id = "ADR-0176"
title = "Controllers and live data drive scene cameras, which also pan and tilt"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0176: Controllers and live data drive scene cameras, which also pan and tilt

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Replaces decision 5 of [ADR-0168](0168-midi-controllers-drive-motion.md) ("Not on
cameras") and the matching refusal of `data` modifiers on cameras from
[ADR-0171](0171-live-data-from-a-daw-drives-scenes.md). Every other decision of both
stands.

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

<a id="decision"></a>
## Decision
1. **A camera's motion takes `midi` and `data`.** `check_camera_motion` no longer refuses
   them, a minor protocol version (0.164, [R-API-001](../RULES.md#R-API-001)). Their
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

<a id="consequences"></a>
## Consequences
- **Easier:** a fader moves, turns or zooms a scene camera from Bitwig over JTunnel, or
  from any local port, with the call an agent already uses for camera motion; a DAW's
  live data can drive it too.
- **Easier:** pan and tilt turn a camera by an angle without computing a target.
- **Harder:** a camera's 8 modifiers are shared between motion and bindings, as an
  item's are.
- **Not decided here:** an audio modifier on a camera, which would need the program
  mix's scope planned for it.
- **Must do:** the maintainer's hardware run checks a real controller moving a camera
  end to end over JTunnel; CI only feeds messages in tests.
