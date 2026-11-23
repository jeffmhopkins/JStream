<!-- @doc
id = "ADR-0188"
title = "A backdrop arrives at a picture pinned in its sky after each warp, which lights the scene"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0188: A backdrop arrives at a picture pinned in its sky after each warp, which lights the scene

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A backdrop's warp ([ADR-0167](0167-a-sky-backdrop-for-3d-scenes.md) decision 5) streams
the stars past and drops out into the same kind of sky it left. A set that travels wants
somewhere to arrive: a planet, a nebula, a station, seen out of the window as the warp
ends, a different one each time, and lighting the set as something that bright would.

Modelling each in 3D costs a model's draw and assets for something no camera ever
reaches. A flat picture infinitely far away is enough: it never moves as the camera
moves, only turns with the sky, as the sky itself does.

The options were:
- **Picture cards placed far away in the set.** They show, but they are not pinned to
  the sky's layout ([ADR-0187](0187-a-warp-can-travel-straight-ahead.md) lays the sky out
  anew at each warp aimed ahead), they are drawn as models of the scene with depth, and
  picking one at random after each warp would need a new channel or sequence feature.
- **Pictures the backdrop itself shows (chosen):** the sky draws one picture as part of
  the backdrop, in its own layout, and picks another as each warp ends. A scene holds the
  candidates, so any number of them, of any source, are named by one setting.

It is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)): it adds settings that
`kind.describe` lists and no protocol field, and changes no licence, security or privacy
posture, compatibility promise or spending. It extends ADR-0167 and ADR-0187, whose sky
is unchanged without the new setting.

<a id="decision"></a>
## Decision
**1. Three settings.** The `light` kind's `environment` type gains:

| Setting | Meaning | Default |
|---|---|---|
| `sky_pictures` | A scene whose visible items are the pictures to arrive at | none |
| `sky_picture_deg` | How wide the picture shown is across the sky, 1 to 120 degrees | 30 |
| `sky_picture_light` | How much it lights the scene, 0 to 8, before the light's intensity | 1 |

Settings saved before these read as their defaults: no pictures.

**2. Which pictures.** Planning puts the frame of each visible item of the
`sky_pictures` scene in the shot, bottom first, whatever the item's place in that
scene, so it is uploaded though no item shows it. An item showing a light, a model, a
probe, a nested scene or a capture is left out.

**3. One at a time, picked at random as a warp ends.** The render thread's per-scene sky
state keeps the picture shown and where it is pinned. It shows one from the first frame.
While the deep sky has gone at warp (ADR-0167 decision 5), it shows none. On the first
step that the deep sky shows again with the warp still or dropping, it picks another, once,
by a hash of the sky's seed and how many it has picked, not the one it showed last when
there is another, and keeps it. A warp aimed ahead (ADR-0187) leaves the picture with the sky it
cross-fades from, so the new layout shows none until the warp drops out.

**4. Where.** It is pinned near the sky's straight ahead, where the warp ends: up to 9
degrees across and 6 up or down from it, by the same hash, so a warp aimed ahead arrives
with it in view, a little off centre. Its top is toward the sky's up, and it keeps its own
shape.

**5. How it is drawn.** The backdrop's pass reads it through the tangent plane at its
centre, so it is a flat picture wherever it is. It covers the sky, the nebulae and the
stars behind it where it is opaque, at the light's intensity, and fades as the deep sky
does at warp, so it fades out going into warp and back in as the warp drops. It is in the
backdrop only: what the sky lights or a model reflects doesn't show it.

**6. How it lights.** While it shows, the scene gains one directional light shining from
where the backdrop shows it, in the average colour of its opaque pixels, at the
environment light's intensity times its item's opacity times `sky_picture_light`, faded
as the picture is. The colour is taken on the CPU once per frame version, from at most
about 4096 of its pixels, for the picture shown only, from the frame after it is picked,
while it is still faint; until then it adds no light. A picture whose pixels aren't at
hand (a GPU texture, or a still whose pixels the program stage has let go) shines white
until they are. It counts toward the lights a model is lit by (ADR-0143) after the
scene's own, in a reflection probe's capture too.

<a id="consequences"></a>
## Consequences
- **Easier:** a travelling set arrives somewhere new after each warp, from a scene of
  pictures and one setting, with no 3D cost and no scripting.
- **Harder:**
  - **Memory.** Every candidate's frame is uploaded, as a panorama `image` is, though
    only one shows; a dozen large pictures hold a dozen textures.
  - **State.** The pick is render-thread state, like the warp's travel: a snapshot or a
    new render context starts at its first pick, and two contexts pick on their own.
  - The picture is sampled with no mips, so a large picture shown small can shimmer.
  - The light is one direction and one colour, not the picture's shape.
- **Must now do:** `jstream-sources` parses the settings; `jstream-render`'s `SkyDraw`
  takes a `SkyPicture` and `sky.wgsl` draws it; `jstream-core` plans the candidates'
  frames, keeps the pick in `SkyState`, takes the colour and adds the light; tests cover
  the pick, the draw, the plan and the light; the MCP design guide and the crates'
  READMEs describe it.
