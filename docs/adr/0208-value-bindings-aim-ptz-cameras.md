<!-- @doc
id = "ADR-0208"
title = "PTZ cameras in the desktop app: the camera list, the PTZ panel, and value bindings that aim a head"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0208: PTZ cameras in the desktop app: the camera list, the PTZ panel, and value bindings that aim a head

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0146](0146-ptz-cameras-shots-and-actions.md) gave JStream PTZ heads over VISCA: a
per-machine camera list the host hands the engine, `ptz.*` runtime actions, `ptz.list`,
and shots that scenes fire. Its consequences left "the desktop app's camera list and PTZ
panel" to do. Until now only the daemon's `--ptz-camera` could list a camera, and no GUI
could move one: the alpha.12 feature audit's item A18.

The maintainer asked for a dedicated PTZ panel: add and configure cameras in the desktop
app, and control them live (pan, tilt and zoom, presets with recall and save, speed), with
their parameters mappable like every other number. That leaves three questions:
- where the desktop app keeps the camera list, and when a change applies;
- what the panel offers, and how a key or a controller reaches a camera;
- what "mappable" means for a head, whose moves are runtime actions, not settings, and
  whose continuous drive ADR-0146 §3 keeps off bindings because a binding has no release.

None of it widens [ADR-0123](0123-lan-inputs-jtunnel-and-visca.md)'s posture (the list
stays per machine, off by default, LAN-only, and out of every engine client's reach),
changes the product's scope, or makes a compatibility promise, so it is agent tier
([ADR-0043](0043-narrow-maintainer-tier.md)).

<a id="decision"></a>
## Decision
1. **The desktop app's camera list is a machine setting that applies at once.** "Control
   PTZ cameras" on Settings › Devices holds a switch and the cameras (a name, an address
   and port, a transport and a VISCA address), saved as `ptz_cameras.json` in the data
   folder, user-only, through an app command that no engine client (an agent, a plugin
   or a paired device) can reach, as the JTunnel and PTZ controller settings are. The app
   checks a list as the engine would (ADR-0146 §1 and §8) and never saves one it would
   refuse. Saving gives the engine the new list (`Engine::start_ptz`), which replaces the
   running heads, each sending its stop first, so no restart is needed. Off keeps the
   list and opens no socket. A browser on a daemon keeps `--ptz-camera`: the page has no
   way to change the machine's list.
2. **The PTZ panel controls one camera at a time.** It is a dock panel ("PTZ cameras")
   with the cameras `ptz.list` reports and whether each answers; a pad of eight arrows
   around Home that drive (`ptz.drive`) while held and stop on release, the pointer
   leaving or the panel closing, and the arrow keys when focused; Wide and Tele held the
   same way; focus Auto, One push and Manual, with Near and Far held while manual; one
   Speed, the pan step, which tilt takes up to its own fastest and zoom and focus take
   scaled to 1 to 8; the pose the head last answered, each axis a field that aims it
   there; and presets 1 to 16 as buttons with a field for any other, a click recalling
   one and Save arming the next click to store instead. A preset's menu also names it,
   binds a key that recalls it (`hotkey.set`, as for any action) and makes it the
   selected scene's shot, and the panel shows and sets that shot (`scene.set_ptz_shots`),
   from the head's position too ("set from current", ADR-0146 §6). Preset names are a UI
   setting kept per machine (`ptz.preset_names`), never in the collection, since a
   preset's content lives in the head.
3. **A value binding can aim one axis of a head (protocol 0.180).** `ValueTarget` gains
   `ptz_axis {camera, axis}`, `axis` one of `pan`, `tilt`, `zoom` and `focus`, in the
   head's own counts (ADR-0146 §5). So a MIDI control, a live-data field or a channel can
   drive a camera's position through the same binding system, mapping range, smoothing,
   curve and amount as any number (ADR-0178), and the panel puts the control button on
   each axis. The range a binding maps onto is whole counts within the protocol's limits
   (pan ±524287, tilt ±32767, zoom and focus 0 to 65535); the panel starts a learned
   control on a typical head's range, which the mapping narrows.
   - **It aims, never edits.** The engine's value tick turns each moved binding into a
     count and, per camera, an aim (`ptz.aim`'s command) at the fastest speed, sent only
     when the bound pose changed. A head keeps only its latest unsent aim (ADR-0146 §3),
     so a fast control never builds a backlog. Nothing is stored or undone.
   - **Pan and tilt move together** in VISCA. A binding on one of them takes the other
     from the pose the bindings last aimed, else the one the head last answered; until
     the head has answered, it waits.
   - **The camera is named, not looked up.** As a shot does, the binding names the camera
     by name, so the collection carries it to another machine, and on a machine that
     lists no such camera it drives nothing. `binding.set_value` checks the name as
     `ptz.*` does and nothing else.
   - **A drive stays unbindable.** ADR-0146 §3 holds: a continuous drive needs a release,
     which a value has no clean equivalent of (a controller unplugged mid-push), so a
     binding aims positions, which end by themselves.

<a id="consequences"></a>
## Consequences
- **Easier:** a person sets up and runs PTZ heads entirely from the desktop app, and a
  fader, a JTunnel Mod or a channel can zoom or pan a head, through the mapping UI they
  already use for every other number.
- **Easier:** the Map control modal (the mapping design for alpha.12) covers the PTZ axes
  with no work of its own, since they are ordinary value targets.
- **Harder:** an axis mapping is in the head's counts, which differ between models, so a
  mapping's range suits one model until per-camera calibration (ADR-0146 §5) lands.
- **Harder:** a key recalls a preset through a hotkey, one action per chord. A MIDI note
  or a DAW event recalling one waits for the mapping work's trigger editors, though
  `binding.set` takes `ptz.recall` already.
- **Must do:** the protocol README, the MCP guidance on value bindings, the desktop and
  UI READMEs and UI-FEATURES describe the list, the panel and `ptz_axis`.
