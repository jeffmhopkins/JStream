<!-- @doc
id = "ADR-0152"
title = "A PTZ controller drives scene cameras: a live aim, its actions, and VISCA endpoints"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0152: A PTZ controller drives scene cameras: a live aim, its actions, and VISCA endpoints

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
#1181 asks for the other direction of VISCA from #1182: an ordinary PTZ controller (a
joystick keyboard, or a PTZ control app) drives JStream's scene cameras the way it drives
a head. It pans and tilts with the joystick, zooms with the rocker, recalls presets and
goes home, and it reads the position back.

[ADR-0123](0123-lan-inputs-jtunnel-and-visca.md) (maintainer tier, accepted) set the
posture for this in §5. **Accept a PTZ controller** is off by default and set per
machine. The listener binds to one interface the person picks, never to all of them. It
answers only allowlisted controller addresses, and it drops and counts everything else.
Each endpoint drives only the camera actions of the scene camera it is bound to: #1181's
runtime drive and aim, and preset recall. [ADR-0146](0146-ptz-cameras-shots-and-actions.md)
§9 fixed the wire shape this side reuses: a drive with signed speeds in VISCA steps, and
an aim with an absolute pose.

What those leave open is the design inside that posture:
- what the drive and aim move, given that a scene camera
  ([ADR-0094](0094-scene-cameras.md)) is collection state, and joystick moves must never
  flood the undo history;
- how the moves reach the render thread, which already moves cameras with motion and
  follow ([ADR-0113](0113-camera-motion-and-follow-pointer.md));
- the units a controller reads and sends, and where presets live;
- what stops a drive whose controller went away.

None of it changes the posture, the product's scope or a compatibility promise, so it is
agent tier ([ADR-0043](0043-narrow-maintainer-tier.md)).

<a id="decision"></a>
## Decision
1. **A live aim per scene family.** Each scene family's camera has a live aim: a pan and
   a tilt in degrees from the stored camera's view, and a zoom factor. It is runtime
   state, like `scene.zoom_camera`'s steps. Nothing is stored or undone, and it goes
   back to the stored camera when the engine restarts or the collection is replaced. The
   aim (0, 0, 1) is the stored camera itself, which is the aim's home. A keyframe scene
   aims its family's camera, so a Move between keyframes keeps the operator's framing.
   The limits are ±170° of pan, ±89° of tilt (the view itself held within 89° of level),
   and zoom 1 to 8. A scene that draws through no camera (a 2D scene with no follow
   camera) has no aim, and the actions fail on it with `action_failed`.
2. **Drawn last.** The render and snapshot threads turn a pass's camera by its family's
   aim after the camera's motion and follow camera, and in both ends of a transition.
   The target turns about the camera's position (pan about the vertical, tilt toward up
   or down) and the field of view narrows by the zoom. A follow camera keeps following,
   and the joystick adds to it. This answers #1181's open question 5: the joystick
   offsets the follow camera and doesn't pause it.
3. **No integration on the render thread.** Each axis of an aim is a value at a time and
   how it moves from there: still, a drive at a fixed rate up to its limit, or an eased
   glide to a value by a time. Every thread computes the same aim for any time from
   that, so `scene.camera_aim` answers without asking the render thread (#1181's open
   question 8). A preview redraws while an aim's last motion was a drive or a glide.
4. **Actions (protocol 0.118).**
   - `scene.drive_camera { scene_id, pan?, tilt?, zoom? }` takes signed VISCA steps as
     `ptz.drive` does (pan ±24, tilt ±23, zoom ±8). A step of 0 stops that axis, and an
     axis left out keeps moving as it was, so the joystick and the zoom rocker drive
     apart.
   - `scene.aim_camera { scene_id, pan_deg?, tilt_deg?, zoom?, speed? }` glides the axes
     it sets, all arriving together, at `speed` (1 to 24, the fastest by default).
   - The query `scene.camera_aim { scene_id }` returns the aim now and whether it moves.
   - Speeds follow fixed curves. Pan and tilt run from 1°/s at step 1 to 80°/s at the
     top step, growing with the square of the step so the low steps are fine. Zoom runs
     from 0.1 to 1.0 in natural-log zoom a second. That answers #1181's open question
     6 with one fixed curve; a setting can come later.
   - A binding fires only on press, so `scene.drive_camera` can't be bound, as
     `ptz.drive` can't (ADR-0146 §3). `scene.aim_camera` can. Plugins may call both
     under `live`, and the query under `read`.
5. **A drive always ends.** A drive belongs to the session whose drive last set an axis
   moving. When that session ends, the aim stops where it is, as a head does
   (ADR-0146 §3). Without a stop, a drive ends at its axis's limit.
6. **Endpoints are protocol clients.** The host lists endpoints and one allowlist of
   controller addresses (`PtzControllerSetup`). Each endpoint has:
   - its bind address and port, on the LAN and never every interface;
   - its transport: Sony-style VISCA over IP on UDP, raw VISCA on UDP, or raw VISCA on
     TCP (up to four connections), as ADR-0146 §2 has for heads;
   - the VISCA address it answers as (1 to 7, and broadcast);
   - its target: a scene, or whatever is on program or on preview when each command
     arrives (#1181's open question 3).

   Each endpoint opens its own session on the engine and translates each VISCA message
   into `scene.drive_camera` or `scene.aim_camera` for its target. It answers each
   inquiry from `scene.camera_aim`, and it reads `studio.get` to find program or
   preview. It runs no other action. A command gets an ACK and a completion, or "not
   executable" when the target has no camera or the call fails. A message JStream
   doesn't know gets "not executable", and one that isn't well-formed gets a syntax
   error. The daemon takes `--ptz-controller` and `--ptz-controller-allow`. The desktop
   app's setting follows with the PTZ panel.
7. **The VISCA mapping.**
   - Pan-tilt drive becomes a drive on pan and tilt, and zoom drive a drive on zoom. A
     standard speed is step 4, and variable speeds 0 to 7 are steps 1 to 8.
   - Absolute and relative pan-tilt, and zoom direct, become an aim. A relative move
     adds to the aim the endpoint reads first. Home and pan-tilt reset aim pan and tilt
     back to 0 and keep the zoom, as a head does.
   - Power, focus, focus mode and one-push focus are taken and do nothing, since a scene
     camera has neither power nor a focus motor. A controller that sends them as it
     connects carries on.
   - Image settings, menus and the rest aren't camera moves (#1181's out-of-scope list),
     and get "not executable".
8. **Units a controller sees (#1181's open question 7).** Pan and tilt positions are 16
   counts a degree, so pan ±170° is ±2720 and tilt ±89° is ±1424. These fit both of
   Sony's widths, and answers use the width the controller last sent. The zoom position
   runs from 0 (zoom 1) to 16384 (zoom 8), even in the logarithm between. These are
   JStream's own units, not any one camera's.
9. **Presets live in the endpoint, in memory.** "Preset set" stores the target camera's
   aim now under its number. "Preset recall" aims back there at the fastest speed, and
   one never set is "not executable". "Preset reset" forgets it. Nothing a controller
   sends is stored in the collection, so ADR-0123 §5's bound (camera actions only) holds
   by construction. That answers #1181's open question 4 for now: a keyframe scene's
   Move remains the stored framing, and a preset is a live recall.
10. **Not answered here.** Discovery broadcasts (#1181's open question 10): it is manual
    IP and port only. Tally, which program and preview already show. The `profile.get`
    counters for the endpoints' answered and dropped messages, which ADR-0123's
    consequences ask for. The PTZ cameras' counters in #1423 wait for the same.

<a id="consequences"></a>
## Consequences
- **Easier:** standard PTZ controllers and control apps frame JStream's scene cameras
  with no bridge, and the same actions serve MCP, timelines, aim bindings and the UI.
- **Easier:** an endpoint is an ordinary session, so its calls are logged and audited
  like any client's, and its drives end with it.
- **Harder:** a controller's presets are lost when the host restarts. Stored presets
  would be an edit that a controller can't run (ADR-0123 §5), so they need their own
  design if wanted.
- **Harder:** the speed curve and the position units are JStream's own, so a controller
  that draws a position map shows them as a generic head's.
- **Must do:** the desktop app's **Accept a PTZ controller** setting, with its endpoint
  list and allowlist; the endpoints' counters in `profile.get`.
