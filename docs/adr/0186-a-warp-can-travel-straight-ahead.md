<!-- @doc
id = "ADR-0186"
title = "A backdrop's warp can travel straight ahead of the scene, wherever its sky is turned"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0186: A backdrop's warp can travel straight ahead of the scene, wherever its sky is turned

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A backdrop's warp streams the stars away from the sky's own centre ahead
([ADR-0167](0167-a-sky-backdrop-for-3d-scenes.md) decision 5), which its item's turn,
tilt and rotation orient ([ADR-0182](0182-a-sky-follows-its-items-tilt-and-roll.md)).
The stars are laid out around that direction, so it is fixed in the sky.

A set that is a vehicle turns by turning its sky, from the light's item or the 3D groups
it sits in ([ADR-0164](0164-3d-groups-share-their-scenes-space.md)). Its warp then
travels toward wherever the sky's centre has been turned, not where the vehicle points.
A vehicle should warp the way it points: turn it, then jump, and the stars should burst
out of the point ahead of it. While at warp it can only look a little way off its
course, and when it drops out the sky it arrives in is where the next turn starts from.

The options were:
- **A channel feature:** a value held when a trigger fires, limited to a range around it
  and re-based when the trigger ends, on the channels that drive the groups' angles.
  That holds the heading but still leaves the warp travelling toward the sky's centre;
  the sky would still have to be laid out anew around the direction ahead. It would also
  be a new channel semantic whose only user is this.
- **A setting of the sky (chosen):** the backdrop itself takes the direction ahead when
  a warp starts, holds the travel near it while at warp, and keeps the sky it ends in.
  It needs nothing of the channels or groups that turn the sky, so any way of turning a
  sky works with it.

It is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)): it adds two settings that
`kind.describe` lists and no protocol field, and changes no licence, security or privacy
posture, compatibility promise or spending. It extends ADR-0167 and ADR-0182, whose sky
is unchanged with the default setting.

<a id="decision"></a>
## Decision
**1. Two settings.** The `light` kind's `environment` type gains:

| Setting | Meaning | Default |
|---|---|---|
| `warp_aim` | `sky`: travel toward the sky's centre, as before. `ahead`: travel along the scene's straight ahead (`-z`) | `sky` |
| `warp_look_deg` | With `ahead`, how far the travel may swing from straight ahead while at warp, 0 to 180 degrees | 10 |

Settings saved before these read as their defaults, so every sky is as it was.

**2. A warp aimed ahead lays the sky out around straight ahead as it starts.** When the
warp a backdrop streams at leaves 0 (it was still and its setting rose), the render
thread's per-scene sky state takes a rotation that undoes the item's orientation at that
moment. The sky is then drawn turned by that rotation and then by its item's, so at that
moment its centre ahead, and the warp's travel, is the scene's straight ahead.
A first frame, or a jump in time that lands at warp, lays it out so at once.

**3. It cross-fades.** Laying the sky out anew moves every star, nebula and the
gradient, so over the first half second of media time the backdrop cross-fades from the
sky as it was drawn before, still, drawn under the new one, to the new one. A first
frame or a jump has nothing to fade from.

**4. At warp, the travel turns only so far.** While the warp is above 0, a turn of the
item still turns the sky, so the point the stars stream from moves with the vehicle's
turn. When it would take the travel more than `warp_look_deg` from straight ahead, the
sky is turned back the least that brings the travel to that limit, and the state's
rotation takes up the slack, so turning back moves it at once rather than after undoing
the excess. A roll about the travel is free.

**5. The sky it ends in stays.** When the warp falls back to 0 the state keeps its
rotation, so the sky does not move, and later turns of the item turn it from there with
no limit. The next warp aimed ahead lays it out anew (decision 2). Setting `warp_aim`
back to `sky` drops the rotation, and the item orients the sky as before.

**6. Everything that reads the sky reads it so.** The backdrop, a scene-lit model's
light and reflections, and a reflection probe read the same orientation, from the state
as the backdrop last stepped it. Other lights in the vehicle's groups are not turned by
it; after a warp they keep their own orientation relative to the vehicle.

<a id="consequences"></a>
## Consequences
- **Easier:** a vehicle turned by its sky warps the way it points, looks only a little
  off course at warp and arrives with its sky where the warp left it, from one setting,
  whatever turns the sky (keyframes, motion modifiers, channels or groups).
- **Harder:**
  - **State.** The rotation is render-thread state, like the warp's travel: a snapshot or
    a new render context starts with none, so it shows the sky as its item turns it, or
    laid out ahead if it is at warp. Two render contexts that draw one scene each keep
    their own.
  - **Cost.** For the half second of a cross-fade, the backdrop is drawn twice, into two
    canvas layers.
  - A sun or other light in the vehicle's groups keeps turning with the groups, so after
    a warp aimed ahead its direction against the stars differs from before.
  - The cross-fade is half a second whatever the warp's ramp.
- **Must now do:** `jstream-sources` parses the settings into `ShownEnvironment` as a
  `WarpAim` and `warp_look_deg`; `jstream-core` keeps the rotation and the cross-fade in
  its per-scene sky state and draws the leaving sky into its own layer; a unit test
  checks the layout, the limit and the slack, one the rotation's convention against
  `Environment::sky_axes`, and a compositor test the cross-fade; the MCP design guide and
  the crates' READMEs describe the settings.
