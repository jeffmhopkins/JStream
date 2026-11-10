<!-- @doc
id = "ADR-0154"
title = "The PTZ cameras' counters in profile.get"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0154: The PTZ cameras' counters in profile.get

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0153](0153-ptz-controller-idle-stop-counters-and-setting.md) put the PTZ controller
endpoints' counters in `profile.get` and left the PTZ cameras' own for later (#1423).
`ptz.list` already gives each camera's sent, reply and dropped counts, but an operator
reading `profile.get` for why a shot was late sees nothing of the heads. It also can't
tell a head that needed its command sent twice (ADR-0153 §4), one that never answered,
one that refused a command, and one whose command buffers were full.

It adds a report and changes no posture, scope or compatibility promise, so it is agent
tier ([ADR-0043](0043-narrow-maintainer-tier.md)). It closes ADR-0153's last open
consequence.

<a id="decision"></a>
## Decision
1. **`profile.get` lists the PTZ cameras (protocol 0.122).** `ptz_cameras` gives each
   camera on the machine's list, in the list's order, as a `PtzCameraProfile`: its name,
   address and port, transport, and whether it answered in the last three seconds.
   It is omitted when the list is empty.
2. **What it counts.** From when the camera's thread started, as `network` counts:
   - the messages sent, the replies read and the datagrams dropped, as `ptz.list` gives;
   - the commands sent again because nothing answered in time (`resent`);
   - the commands nothing answered even then (`unanswered`);
   - the commands the head refused with an error (`refused`);
   - the times the head's command buffers were full (`busy`), each try that found them
     full.

   The pose and last error stay in `ptz.list`: `profile.get` reports how the link is
   doing, not where the head points.

<a id="consequences"></a>
## Consequences
- **Easier:** one `profile.get` shows both ends of a PTZ setup, the controller endpoints
  and the heads they drive, and a lossy LAN shows as `resent` before it shows as
  `unanswered`.
- **Harder:** nothing here; the counts are four more fields on a status the camera's
  thread already keeps.
