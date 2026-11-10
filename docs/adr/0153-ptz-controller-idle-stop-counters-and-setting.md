<!-- @doc
id = "ADR-0153"
title = "A PTZ controller's idle stop, its counters, the desktop app's setting, and a resent command"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0153: A PTZ controller's idle stop, its counters, the desktop app's setting, and a resent command

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0152](0152-a-ptz-controller-drives-scene-cameras.md) let a VISCA controller drive
scene cameras through endpoints the daemon opens. It left three things for later:
- **A drive whose controller went away.** Decision 5 stops a drive when its session
  ends, but an endpoint's session lasts as long as the endpoint. A UDP controller that
  goes away mid-drive leaves it running to the axis's limit (#1486 review N1).
- **The counters.** ADR-0123's consequences ask for the endpoints' answered and dropped
  messages in `profile.get`.
- **The desktop app's setting.** ADR-0123 §5 asks for "Accept a PTZ controller", off by
  default and set per machine.

Separately, #1494 found that a PTZ camera's command that no reply answered in time was
simply lost ([ADR-0146](0146-ptz-cameras-shots-and-actions.md)): a loaded machine, or a
datagram lost on the LAN, left the head where it was.

None of it changes ADR-0123's posture, the product's scope or a compatibility promise, so
it is agent tier ([ADR-0043](0043-narrow-maintainer-tier.md)). It amends ADR-0152
decisions 5, 6 and 10, and ADR-0146's command exchange.

<a id="decision"></a>
## Decision
1. **A quiet controller's drive stops.** An endpoint notes which axes its own drives
   left moving, and on which scene's camera.
   - **Quiet:** when it has heard nothing from an allowed controller for 5 seconds, it
     stops those axes, as a head stops when its controller cable is pulled. Any
     message keeps a drive going, an inquiry included.
   - **TCP:** an endpoint stops them at once when its last connection closes.
   - **What clears an axis:** a drive at 0, or an aim that sets it, clears it from the
     endpoint's list. A drive on another scene's camera leaves the first camera's to its
     limits.

   This replaces ADR-0152 decision 5's "without a stop, a drive ends at its axis's
   limit" for drives from an endpoint. 5 seconds is longer than a controller usually
   waits between messages while its joystick is held, and short enough that a lost
   controller doesn't swing the shot to its end stop. A controller that sends nothing
   while held steady sees the drive stop, and moving the stick again restarts it.
2. **Counters in `profile.get` (protocol 0.120).** `ptz_controllers` lists each running
   endpoint in the host's order. Each entry gives its address and port, its transport,
   its target, and these counts, from when the endpoint opened:
   - the messages it answered;
   - the datagrams and connections from addresses it doesn't allow;
   - the datagrams that aren't VISCA over IP;
   - the connections refused past the four it holds;
   - the drives it stopped for quiet or a closed connection.

   The engine holds the endpoints weakly, so they leave the report when their handle
   drops. The list is omitted when no endpoint runs.
3. **The desktop app's "Accept a PTZ controller".** This is
   `ptz_controllers.json` in the data folder, user-only, as "Receive from JTunnel
   senders" is (ADR-0123 §1 and §3):
   - **What it holds:** a switch, the endpoints (address and port, transport, VISCA
     address, and target: Program, Preview or one scene), and the controller addresses
     allowed.
   - **Who changes it:** only the person, from the app's own window, through a Tauri
     command no engine client reaches. Per machine, never in the collection, off until
     turned on, and it applies when the app next starts.
   - **Checks:** the app checks it as the engine would before saving, and refuses an
     endpoint off the LAN or on every interface, or endpoints with no allowed address.
     The app keeps the endpoints open until it exits.
   - **Where:** Settings › Devices, after the JTunnel setting.
4. **A PTZ camera's command with no reply goes once more.** When nothing answers a
   command within the reply wait, the camera's thread sends it again, once. Every
   command JStream sends lands the head in the same place however often it arrives
   (absolute moves, drives at a speed, stops, home, presets), so a copy that did arrive
   does no harm. A head that never answers still shows "no reply", after twice the
   wait.

<a id="consequences"></a>
## Consequences
- **Easier:** a controller that crashes, loses its network or is switched off
  mid-drive no longer leaves a scene camera panning to its limit.
- **Easier:** an operator sees from `profile.get` whether their controller reaches the
  endpoint, and why its messages are dropped.
- **Harder:** a controller that sends nothing for 5 seconds while its joystick is held
  steady sees the drive stop. The value is a constant; it becomes a setting only if a
  real controller needs it.
- **Still open:** the PTZ cameras' own counters (#1423) aren't in `profile.get` yet.
