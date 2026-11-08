<!-- @doc
id = "ADR-0146"
title = "PTZ cameras: per-machine heads, ptz actions, and shots that scenes and keyframes fire"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0146: PTZ cameras: per-machine heads, ptz actions, and shots that scenes and keyframes fire

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
#1182 asks JStream to drive physical PTZ heads over VISCA: pan, tilt, zoom and focus,
home, and above all presets, with scene switches and keyframe scenes framing the camera.
[ADR-0123](0123-lan-inputs-jtunnel-and-visca.md) (maintainer tier, accepted) settled the
security posture: **Control PTZ cameras** is off by default, set per machine, sends only
to LAN addresses the person adds, and reads replies only from them, which can only update
the pose and status JStream shows. [ADR-0122](0122-controller-input-midi-and-one-binding-system.md)
§8 added `jstream-visca`, the format, with no sockets.

What those leave open is the design inside that posture:
- how a collection names a head whose address belongs to the machine;
- the shape of the control actions, which the UI, hotkeys and controller bindings
  ([ADR-0112](0112-assignable-camera-zoom-bindings.md), ADR-0122), MCP and paired
  devices all use, and which #1181's virtual-camera actions should match;
- where a scene's framing of a head lives, so that "switching to a keyframe recalls its
  preset, or moves to its stored pan, tilt and zoom, as part of the transition" (the
  maintainer's addition on #1182) works with keyframe scenes
  ([ADR-0049](0049-keyframe-scene-model-and-evaluation.md));
- what stops a preview from moving a camera that is live.

None of it changes the posture, the product's scope or a compatibility promise, so it is
agent tier ([ADR-0043](0043-narrow-maintainer-tier.md)).

<a id="decision"></a>
## Decision
1. **Heads are per machine and named.** The host gives the engine its list of PTZ
   cameras: a name (1 to 64 bytes, no control characters), an IP address and port, a
   transport, and the VISCA address on that link (1 to 7, default 1). The daemon takes
   `--ptz-camera`, repeatable; the desktop app's setting comes with its PTZ panel. With
   no camera listed no socket is open, which is ADR-0123's "off". The protocol can't add,
   change or remove a camera, so neither an agent nor a paired device can point JStream
   at a new address. A collection names a head only by its name, so a show carries its
   framing to another machine, which maps the same names to its own heads.
2. **Transports.** Sony-style VISCA over IP on UDP (the 8-byte header, sequence numbers
   and the reset control message; port 52381 by default), raw VISCA over UDP, and raw
   VISCA over TCP (which also reaches a serial head through an IP-to-serial bridge).
   Discovery is out of scope (ADR-0123 §4).
3. **Control is runtime actions, never edits.** `ptz.drive` (signed pan, tilt and zoom
   speeds, 0 stops), `ptz.aim` (an absolute pose), `ptz.home`, `ptz.recall` and
   `ptz.store` (a preset in the head), and `ptz.focus` (auto, manual, one push, near, far,
   stop). Nothing is stored in the collection or undone; `ptz.store` writes the head's
   own memory. An action is refused for a camera this machine doesn't list, and when it
   lists none. Otherwise it returns once the command is queued for the head, which may
   be unreachable: `ptz.list` says so. A queued drive replaces an unsent one, so a
   joystick never builds a backlog. Every `ptz.*` action can be bound to a trigger, since
   each one only moves a head. They name no collection object, so deleting nothing
   removes such a binding.
4. **`ptz.list` reports each head.** Its name, address, transport, whether it answered
   lately, its last pose and its last error. While a head is idle the engine asks its
   pan-tilt, zoom and focus positions about twice a second, so the pose shown follows
   moves JStream didn't make (another controller, the head's own remote).
5. **Units are the head's.** Positions are the signed pan and tilt counts, and the zoom
   and focus positions, that the head reports; speeds are VISCA's steps (pan 1 to 24, tilt
   1 to 23, zoom and focus 1 to 8). A per-camera calibration to degrees is a later step.
6. **A scene holds shots.** `scene.set_ptz_shots` (an edit, undoable, saved with the
   collection) gives a scene up to 16 shots, at most one per camera. A shot names a
   camera and either a preset (0 to 127) or a pose (pan and tilt together, zoom, focus,
   any of them). Shots sit on the scene, not on a source: one head can feed several
   sources or none, and a keyframe scene is a scene. A keyframe scene's shot for a camera
   replaces its parent's for that camera, and it follows the parent's shots for every
   other camera, as an override does (ADR-0049 §1). "Set from current" is a client
   reading the pose from `ptz.list` and writing it into a shot.
7. **When shots fire.**
   - A scene going on program sends its effective shots at the start of the switch,
     with or without a transition. Recalling a preset is immediate. A pose takes speeds
     matched to the duration of the transition the switch runs, as `studio.changed`
     reports it (a switch with nothing on screen to move from is a cut): the fastest for a cut or a transition of half
     a second or less, and a proportionally slower one for a longer transition, never
     below the slowest step. The curve is fixed until calibration (5) lets the engine
     measure the distance.
   - A scene going on preview sends only the shots for cameras that the program scene's
     effective shots don't name, so a preview never moves a camera that frames what is
     live (a tally guard).
   - An edit to a scene's shots doesn't move a head, even on program: framing changes
     only when a scene is put on program or preview again, or by a `ptz.*` action.
8. **Network, per ADR-0123.** Each camera's address is checked against the LAN ranges
   (loopback, 10/8, 172.16/12, 192.168/16, 169.254/16, and their IPv6 counterparts:
   `::1`, `fc00::/7` and `fe80::/10`) when the host starts PTZ control, and refused
   otherwise. A datagram from any other address or port, or one that isn't a VISCA reply,
   is dropped and counted.
9. **The shape #1181 reuses.** The virtual-camera side keeps these names and units on
   the wire: a drive with signed speeds and an aim with an absolute pose. #1181 adds them
   for scene cameras as `scene.drive_camera` and `scene.aim_camera`, and VISCA endpoints
   that translate a controller's commands into them.

<a id="consequences"></a>
## Consequences
- **Easier:** a scene or a keyframe frames its cameras with no extra wiring, and every
  trigger JStream has (hotkeys, MIDI, timelines, MCP) can move a head.
- **Easier:** a collection never holds an address, so ADR-0123's per-machine rule holds
  by construction.
- **Harder:** a pose is in the head's own counts, so a collection's poses suit one model
  of head. Calibration (5) is the fix, later.
- **Harder:** a matched-speed move is approximate until calibration, so a long Move may
  end before or after the head arrives.
- **Must do:** the desktop app's camera list and PTZ panel, and a shot editor in scene
  properties with "set from current"; the `ptz.list` counters in `profile.get`
  (ADR-0123 consequences).
