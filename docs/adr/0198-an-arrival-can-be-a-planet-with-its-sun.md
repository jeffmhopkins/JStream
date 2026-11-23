<!-- @doc
id = "ADR-0198"
title = "An arrival can be several pictures pinned around one another, the last lighting the scene"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0198: An arrival can be several pictures pinned around one another, the last lighting the scene

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0188](0188-a-backdrop-arrives-at-pictures-pinned-in-its-sky.md) has a backdrop
arrive at one picture after each warp, pinned near straight ahead, and light the scene
from it. A planet shown that way is lit by its own colour from in front of the viewer,
whichever side of it the picture shows lit. A set that arrives at a planet wants its sun
too: somewhere else in the sky, on the side the planet is lit from, found by turning the
vehicle, and lighting the set from there.

The options were:
- **A setting per light for one companion picture.** It would fit only a planet and one
  sun, and every arrival of the scene would share its placement, though each planet is
  lit from its own side.
- **An arrival made of a nested scene's pictures (chosen):** the `sky_pictures` scene
  already names each arrival by an item, so an item showing a scene of pictures is one
  arrival of all of them, each placed by its item. Nothing new is named in the settings.

It is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)): it adds no setting and no protocol
field, and changes no licence, security or privacy posture, compatibility promise or
spending. It extends ADR-0188, whose single-picture arrivals are unchanged, and amends
ADR-0188 decision 2: an item of the `sky_pictures` scene showing a nested scene, which
that decision leaves out, is now one arrival of that scene's pictures (decision 1 below).

<a id="decision"></a>
## Decision
**1. Which pictures.** An item of the `sky_pictures` scene showing a nested scene is one
arrival of the frames of up to four visible items of that scene, bottom first, each
sized by its item's width as ADR-0188 sizes a picture. An item there showing a light, a
model, a probe, a scene or a capture is left out.

**2. Where.** The first is pinned where ADR-0188 decision 4 pins an arrival, near
straight ahead; its item's turn and tilt are ignored. Each other is pinned by its item's
`turn_deg` (to the right) and `tilt_deg` (up), in degrees, from the first: turned about
the sky's up through the first's centre, then tilted toward it. A sun can so sit behind
a planet's shoulder, where the vehicle turns to find it.

**3. How it is drawn.** The backdrop draws up to four pinned pictures, bottom first,
each over those before it where they meet, each as ADR-0188 decision 5 draws one.

**4. How it lights.** The arrival lights the scene as ADR-0188 decision 6 says, from its
last picture: where the backdrop shows it, in its colour. For a planet and its sun, the
sun on top lights the scene and the planet doesn't.

<a id="consequences"></a>
## Consequences
- **Easier:** a planet arrives with its sun, placed per planet so its light matches the
  planet's lit side, from a scene of pictures and no new setting.
- **Harder:**
  - The backdrop's pass binds four picture textures, a blank one where a slot is empty,
    and its uniform grows by twelve vec4s; an arrival shows at most four pictures.
  - Only an arrival's last picture lights; a planet lit by its own glow needs to be on
    top, or alone.
  - A deeper nesting (a scene item inside the arrival's scene) is left out.
- **Must now do:** `jstream-render`'s `SkyDraw` takes up to `MAX_SKY_PICTURES` pictures
  and `sky.wgsl` draws them; `jstream-core` plans each arrival's pictures as `SkyPart`s,
  places them and lights from the last; tests cover the draw, the plan, the placement
  and the light; the MCP design guide and the crates' READMEs describe it.
