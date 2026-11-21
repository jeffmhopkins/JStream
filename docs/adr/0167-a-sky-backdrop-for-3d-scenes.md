<!-- @doc
id = "ADR-0167"
title = "A 3D scene can draw its environment's sky behind it: stars, nebulae and warp"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0167: A 3D scene can draw its environment's sky behind it: stars, nebulae and warp

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A 3D set often needs a world around it: open sky past a window, a skyline behind a
desk, space outside a vehicle. What is out there is far enough away that it should turn
when the camera looks around and never shift when the camera moves. Today the only way
to show it is a card or a model ([ADR-0107](0107-3d-models.md)) placed far behind the
set, and both are wrong in the same way: they sit at a finite distance, so moving the
camera slides them against the set (parallax), and they end at their edges, so turning
the camera far enough shows the scene's background past them. A sphere model with the
picture on its inside is closer, but it still has a centre the camera must stay at, its
own depth sorts with the set, and its picture is stretched over the model's texture
coordinates at one fixed resolution, so a starfield on it shows its texels. The same
set often travels: stars that stream past and stretch into streaks, as at very high
speed, have to stream away from the direction of travel whichever way the camera
looks, which a flat card showing a streak animation can't do once the camera turns.

What exists to build on:
- **The environment light** ([ADR-0151](0151-the-rest-of-richer-3d-scenes.md) decision
  9): a `light` of `type` `environment` holds a sky (a gradient of sky, horizon and
  ground colours, or a source's picture read as an equirectangular panorama), spun about
  the vertical by its item's `turn_deg`, at its intensity times its item's opacity. It
  lights `scene`-lit models and lit cards. Its decision left "drawing the environment as
  the scene's background" undecided.
- **The scene camera** ([ADR-0094](0094-scene-cameras.md)): a 3D scene's items are seen
  through one camera, whose view gives each canvas pixel a ray: right, down and forward
  axes, a principal point and a focal length.
- **Layers placed as items**: a model draws into a canvas-sized layer that the scene
  then places (ADR-0107 decision 6), and an item seen through no camera draws in
  stacking order, writes no view depth (ADR-0143 decision 4) and casts no shadow.

The forces:
- A scene that doesn't use it draws and costs exactly as today, and every golden image
  stays as it is.
- It is a setting that `kind.describe` describes, so the UI's properties and the MCP
  tools take it with no hand-written code.
- JStream stays general: a sky is any gradient, picture or live source, with nothing
  that knows what it depicts.

It is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)): the feature was asked for, and it
changes no licence, security or privacy posture, compatibility promise or spending. It
adds settings that `kind.describe` lists and no protocol field. It extends ADR-0151
decision 9, decides what that left open, and contradicts no accepted decision.

<a id="decision"></a>
## Decision
**1. The environment light can be the scene's backdrop.** The `light` kind's
`environment` type gains:

| Setting | Meaning | Default |
|---|---|---|
| `backdrop` | Draw the sky behind the scene's items | `false` |
| `stars` | How many stars to scatter over the backdrop, 0 (none) to 1 | 0 |
| `star_brightness` | How bright the stars are, 0 to 8, before the intensity | 1 |
| `nebula` | How much nebula to draw, 0 (none) to 1 | 0 |
| `nebula_color`, `nebula_color_2` | The nebulae's two colours | a violet, a blue |
| `galaxy` | How bright a galaxy band is, 0 (none) to 1 | 0 |
| `sky_seed` | Which sky the stars, nebulae and band lay out, 0 to 9999 | 0 |
| `warp` | How fast the sky streams past, 0 (still) to 1 | 0 |
| `warp_ramp_s` | Seconds the warp takes to go from still to full, or back, 0 to 10 | 1.5 |

Settings saved before these read as their defaults, so an existing environment light
draws nothing new.

**2. What it draws.** In a 3D scene whose first environment light (the one that counts,
ADR-0151 decision 9) has `backdrop` on, each canvas pixel shows that sky along the
pixel's ray from the camera, as if infinitely far away:
- the ray is the camera's forward axis plus its right and down axes times the pixel's
  offset from the principal point over the focal length; the camera's position plays no
  part, so the backdrop turns with where the camera looks and never moves with where it
  is;
- the item's `turn_deg` spins the sky, and the gradient and the panorama are read as the
  environment light reads them, so what a model reflects is what is behind it;
- its brightness is the light's intensity, and its item's opacity is its opacity, so
  fading or hiding the light's item fades the backdrop over the scene's background
  ([ADR-0089](0089-scene-options-and-nested-audio.md)), and dims the light as before.

A scene without a camera draws no backdrop: a 2D scene has no ray to look along.

**3. Stars.** `stars` above 0 adds points of light that are fixed in the sky. They are
laid out around the direction of travel, the sky's straight ahead (`-z`, which the
item's `turn_deg` spins): a direction is its angle about that axis and `v`, the log of
the tangent of half its angle from it. A star moving away from the travel at any angle
then moves at one speed in `v`, and a cell of a fixed number of columns around the axis
and as tall in `v` is square on the sky wherever it is. A cell holds a star with a
chance `stars` sets times the cell's share of the sky (the square of the sine of its
angle from the axis), so stars are as dense everywhere, at a point hashed from the cell.
Two layers of different cell sizes give faint and bright stars, and each star's colour
runs from warm to cool by its hash. A star is a Gaussian about 0.6 of a canvas pixel
wide in its angle, so it is a sharp point at any resolution and field of view, rather
than a texel of a picture.

**4. Nebulae and a galaxy band.** `nebula` adds glowing gas clouds over the sky: fractal
value noise on the direction, warped once by more noise so the clouds stream and fold,
shading from `nebula_color` to `nebula_color_2`, with brighter filaments where they
fold. `galaxy` adds a band of light along one great circle of the sky, brighter toward
one side, broken by dark dust lanes, with up to four times the faint stars along it.
`sky_seed` lays all of it out anew. They are added to the gradient or the panorama, so
a panorama can carry them too.

**5. Warp.** `warp` above 0 moves the sky past: the stars move away from the direction of
travel in `v` and leave a streak behind each one, so looking ahead they burst out of
one point, looking to the side they slide past in parallel lines, and looking back they
converge on the point being left, each as the camera sees it. A streak's length in `v`
rises with the square of `warp`, to 1.2 at 1 (about 70 degrees at right angles to the
travel), and it fades from its head to its tail; a streak's light is spread along it,
so a long one is dimmer per pixel. Faint stars dim out as the warp grows. The nebulae and
the band blur along the same lines, and light ahead is shifted bluer and behind redder.
- **Ramp and travel.** A source's settings aren't keyed by keyframe scenes or driven by
  motion, so the sky ramps itself: the render thread keeps, per scene, the warp it
  streams at now and how far the stars have moved. Each frame the warp moves toward the
  setting by one over `warp_ramp_s` seconds, and the travel advances by the mean of the
  warp's speed before and after (6 in `v` a second at 1, rising with its square) times
  the media time since the last frame. So one edit of `warp` to 0.8 accelerates into
  warp and one edit to 0 slows out of it, with no jump. A step back in time or of more
  than a second, and a new render context or a snapshot, starts at the setting with no
  travel, so the stars' places then are the same on every machine.
- The whole travel is passed as whole cells, wrapped to 32 bits, and a fraction, so a
  long journey keeps every star exact.

Stars, nebulae and the band are part of the backdrop only: the sky a model reflects or
is lit by has none of them.

**6. How it is drawn.** For each colour pass of such a scene, the engine draws the sky
through the camera the pass's items are seen through, live and follow-camera moves
included, into a canvas-sized layer: one full-canvas triangle, every pixel written, at
the start of the pass. The layer is then placed under every item, flat over the canvas
and seen through no camera, so it sorts with no item, writes no view depth (a
depth-aware filter sees it as infinitely far, as it saw the background before) and is
left out of shadow maps and floor reflections. The pipeline is built the first time any
backdrop draws. A scene without a backdrop has no new pass, layer or pipeline.

**7. Transitions.** While a transition runs, each end's backdrop is drawn through the
camera that end's tracks are seen through: a Move's one moving camera, or a Fade's own
camera of each side. The backdrop of the scene being shown fades in on top of the one
being left, by the transition's eased progress as its camera moves, and the one left
stays whole when the shown scene
has a backdrop too and fades out when it doesn't. So a Move between two keyframes of one
set shows its sky turning with the camera throughout.

**8. Agents and people.** The settings reach the UI's properties and the MCP tools
through `kind.describe`, and the design notes agents read say how to build a sky around
a 3D set, with stars, nebulae, a galaxy band or a panorama, and how to ramp a warp.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - a 3D set gets a world around it that behaves as distant scenery does: it turns as
    the camera looks around, never slides as the camera moves, and never ends;
  - a night sky or deep space, with nebulae and a galaxy, is a few settings, sharp at
    any resolution, with no asset;
  - a set can travel at speed, with streaks that are right from any angle the camera
    looks, and ramp in and out with one edit of its `warp`;
  - a model reflects the sky that is drawn behind it, since both read one environment.
- **Harder:**
  - **Cost.** A backdrop is one full-canvas pass a frame per scene that shows it, and one
    more canvas layer. A still sky costs up to one texture read, two star lookups and,
    with nebulae and the band, about 30 octaves of noise per pixel; at full warp a pixel looks for
    streaks over up to 64 cells and blurs the nebulae over 5 taps, so warp with nebulae
    is the costliest. On a software rasterizer a 1280x720 frame takes about 0.1 s still
    and 0.34 s at warp; it is not yet in the render benchmarks or measured on a GPU.
  - **State.** The travel is render-thread state, like a follow camera's: two render
    contexts that draw one scene each keep their own.
  - **Approximations.** A panorama is read at its own resolution with no mips, so a very
    large panorama seen with a wide view can shimmer. A flat picture read as a panorama
    is stretched toward the poles. A star is a point sampled once per pixel, so a faint
    one can flicker slightly as the camera turns.
  - A floor reflection shows the set but not the sky behind it.
  - **One sky layer a scene a frame.** A scene's backdrop is drawn once a frame, so a
    scene shown both as a transition's end and nested in the other end through a scene
    source shows the transition's camera and fade in both places while it runs.
- **Not decided here:** a sky that isn't an environment light (a scene setting of its
  own); clouds, planets or other procedural skies; a direction of travel other than
  the sky's straight ahead; streaks for a panorama's own stars; stars, nebulae or
  the band in what the environment lights; mips for a panorama.
