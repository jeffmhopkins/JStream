<!-- @doc
id = "ADR-0151"
title = "Shadows, reflections, occlusion, volumetric fog, depth-aware bloom, a scene environment, lit cards, billboards and model animation"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0151: Shadows, reflections, occlusion, volumetric fog, depth-aware bloom, a scene environment, lit cards, billboards and model animation

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0143](0143-scene-lights-and-depth-aware-scene-filters.md) gave 3D scenes lights
placed by items, a per-pixel view depth, and depth-aware scene filters, starting with
`depth_fog`. It left shadows, volumetric fog, ambient occlusion and planar reflections to
decisions of their own, along with depth-aware bloom, an environment for a whole scene,
and lighting flat items. Issue #1231 also asks for items that always face the camera, a
material override and double-sided drawing for flat items, and glTF animations played on
the animation clock. The maintainer asked for all of it in one release.

What exists to build on:
- **The view depth** (ADR-0143 decision 4): a half-float canvas layer holding `1 / w`
  of the nearest card or model fragment, drawn only for a 3D scene whose chain holds a
  filter that reads it, and handed to those filters with the camera's focal length.
- **Scene lights** (ADR-0143 decisions 1 to 3): up to 8 shading lights and any ambient
  lights, taken into the camera's viewing frame in `f64` on the CPU each frame, lighting
  `scene`-lit models only. Cards are not lit.
- **Models** (ADR-0107): one or more meshes in model space, posed by an item's box
  through `ItemGeometry::solid`, drawn into a layer with their own depth test, and
  shaded with glTF's metallic-roughness model. The glTF loader flattens the node tree
  into model space at load.
- **Item geometry** (ADR-0094): `turn_deg`, `tilt_deg` and `rotation_deg` orient a box
  about its centre, and every placement goes through the protocol's geometry, so the
  compositor, depth of field, sorting and lights share one pose.
- **The animation clock**: every filter and source draws a frame for a media time, the
  same on every machine.

The forces:
- A scene that uses none of these draws and costs exactly as today, and every existing
  golden image stays as it is.
- Each effect is a setting or a filter kind that `kind.describe` describes, so the UI's
  properties and the MCP tools take it with no hand-written code.
- Each stays inside the frame budget at 1080p60 on a mid-range GPU with its default
  quality, measured in the render benchmarks ([R-RT-005](../RULES.md#R-RT-005)).
- JStream stays general: nothing here knows what a scene depicts.

It is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)): the maintainer asked for the
features, and they change no licence, security or privacy posture, compatibility promise
or spending. On the wire they add two optional item fields (one minor protocol version)
and settings and filter kinds that `kind.describe` lists. It extends ADR-0094,
ADR-0107 and ADR-0143 and contradicts none of their decisions.

<a id="decision"></a>
## Decision
**1. The scene view a depth-aware filter reads.** ADR-0143's view depth grows into the
**scene view**, handed to a depth-aware filter on a 3D scene's chain:
- the view depth, as ADR-0143 decision 4 draws it;
- the camera as seen on the canvas: its position, its right, down and forward axes, its
  focal length and its principal point, so a filter turns a pixel and its `1 / w` back
  into a point in the scene's space;
- the scene's lights, as a `scene`-lit model receives them (ADR-0143 decision 3);
- the shadow maps of the lights that cast shadows (decision 3).

The engine draws only the parts some filter of the chain reads: the depth for every
depth-aware kind, the shadow maps only when a filter or a model needs them, and the
reflection (decision 4) only for `floor_reflection`. Anywhere else (a 2D scene, a source,
item, master or output chain) a depth-aware filter has no scene view and passes its input
through, as ADR-0143 decision 5 says.

**2. Billboards.** `ItemGeometry` gains `billboard`, a boolean that defaults to `false`
and is omitted on the wire when false. In a 3D scene, a billboard item's box turns about
its centre so that its front faces the camera's position: its `turn_deg` and `tilt_deg`
are replaced by the turn and tilt that point its front normal at the camera, and
`rotation_deg` still rolls it in its own plane. It applies to anything a box places: a
card, a model's pose and a light's aim (a billboard spot light shines away from the
camera). Sorting, depth of field, the view depth and lights all use the turned pose,
because it is computed in the protocol's geometry, where every placement is. In a 2D
scene it changes nothing. It is a minor protocol version.

**3. Shadows from scene lights.** The `light` kind gains:

| Setting | Meaning | Default |
|---|---|---|
| `shadows` | Whether a `spot` or `directional` light casts shadows | `false` |
| `shadow_softness` | How soft a shadow's edge is, 0 (hard) to 1 | 0.3 |

- **Which lights.** The first 2 lights in the scene's stacking order with `shadows` set
  and of type `spot` or `directional` cast shadows. Point and ambient lights never do,
  and more than 2 shadowed lights are logged once, as more than 8 lights are.
- **The map.** Each shadowed light has a 2048 × 2048 depth map drawn from the light: a
  perspective view along its aim with its cone for a spot light, and an orthographic view
  along its aim fitted to the bounds of the scene's casters for a directional light. It is
  drawn once a frame, only while a `scene`-lit model or a filter reads it.
- **Casters** are the scene's models and its cards seen through the camera: a model's
  triangles, and a card's box where its alpha is at least one half, as the view depth
  draws them. A model's `casts_shadows` setting (default `true`) leaves it out.
- **Receivers** are `scene`-lit models, with `receives_shadows` (default `true`). A
  shadowed light's contribution to a fragment is scaled by its visibility, filtered over
  a disc whose size `shadow_softness` sets. Cards are not lit (ADR-0143 decision 3), so
  they don't receive shadows; a floor that shows shadows is a `scene`-lit plane model.
- A light's shadows follow everything that moves it, as its light does.

**4. Floor reflections.** `floor_reflection` is a depth-aware filter kind that mirrors the
scene in a horizontal floor:

| Setting | Meaning | Default |
|---|---|---|
| `floor_y_px` | The floor's height: the plane `y = floor_y_px` in the scene's space, in canvas pixels | the canvas height |
| `strength` | How much of the reflection shows, 0 to 1 | 0.5 |
| `fade_px` | Height above the floor over which a reflected thing fades out; 0 never fades | 0 |
| `blur_px` | Blur of the reflection, for a rough floor | 0 |

- For a chain holding it, the engine draws the scene's items a second time, each mirrored
  in the plane, through the same camera, into a canvas layer, with each fragment's height
  above the floor in its alpha-weighted second layer for the fade.
- The filter lays that reflection over the pixels whose view depth puts them on the
  plane (within a pixel's footprint at their distance), at `strength`, faded by the
  reflected thing's height, source-over in premultiplied linear light. Elsewhere,
  including where nothing is drawn, the picture is unchanged, so the floor is whatever
  the scene draws there: a plane model or a card lying flat.
- A thing below the floor is not reflected.

**5. Ambient occlusion.** `ambient_occlusion` is a depth-aware filter kind that darkens
creases and contact points:

| Setting | Meaning | Default |
|---|---|---|
| `radius_px` | How far from a point occluders count, in scene pixels | 48 |
| `strength` | How dark full occlusion is, 0 to 2 | 1 |
| `color` | What occlusion tints toward, sRGB | black |
| `quality` | `low` (8 samples) or `high` (16 samples) | `low` |

It reconstructs each pixel's point and normal from the view depth, samples a hemisphere
of `radius_px` around it in a fixed per-pixel rotated pattern, counts the samples the
depth puts behind something nearer, and multiplies the pixel toward `color` by the
occluded share times `strength`, with a depth-aware 4 × 4 blur of the occlusion so the
pattern doesn't show. Where nothing is drawn it changes nothing.

**6. Volumetric fog.** `volumetric_fog` is a depth-aware filter kind for fog that has
height, moves, and is lit by the scene's lights:

| Setting | Meaning | Default |
|---|---|---|
| `color` | The fog's own colour (light it scatters from everywhere), sRGB with alpha | light grey |
| `density` | Fog per 1000 px at the base height | 0.5 |
| `base_y_px` | The height where fog is densest, in canvas pixels (a kind's default is fixed, so the bottom of a 1080p canvas) | 1080 |
| `falloff_px` | Height above the base over which density halves; 0 makes it even | 400 |
| `start_px` | Distance from the camera where fog begins | 0 |
| `max_px` | How far a ray marches where nothing is drawn | 8000 |
| `noise` | How much moving noise varies the density, 0 to 1 | 0.3 |
| `noise_scale_px` | The size of the noise's features | 300 |
| `wind_x_px`, `wind_y_px`, `wind_z_px` | How far the noise drifts per second, right, down and toward the viewer (three numbers, as settings have no vector type) | 20, 0, 0 |
| `scattering` | How strongly scene lights light the fog, 0 to 8 | 1 |
| `quality` | `low` (24 steps), `medium` (48) or `high` (96) | `medium` |

- Each pixel marches its viewing ray from `start_px` to its depth (or `max_px`), in steps
  jittered per pixel and per frame, accumulating density from the height falloff and the
  noise, and transmittance.
- At each step the fog takes its colour plus the light of each scene light reaching that
  point (with the light's falloff and cone, and, for a light that casts shadows, its
  shadow map's visibility), with a forward-scattering phase. So a shadowed spot light
  through the fog draws a light shaft.
- The result is laid over the picture as depth fog is: the in-scattered light plus the
  picture times the transmittance.
- The noise moves with the animation clock, the same on every machine; a wind
  of zero makes it still.

**7. Model animation.** The `model` kind plays a glTF file's animations:

| Setting | Meaning | Default |
|---|---|---|
| `animation` | The animation to play, by name; empty plays none | empty |
| `animation_speed` | Playback speed, 0 to 8 | 1 |
| `animation_loop` | Whether it loops; if not, it holds its last frame | `true` |

- **What plays.** Node animation (translation, rotation and scale channels, with step,
  linear and cubic-spline interpolation) and skinning (up to 4 joint weights a vertex,
  and up to 256 joints a skin, skinned on the GPU). Morph targets are not played; a file
  with them loads, and they are logged once.
- **When.** The pose at media time `t` is the animation at `t × animation_speed` from
  when the source was created, looped over its duration or held at its end, so every
  machine shows the same pose at the same time, and a timeline or a restart that resets
  the source restarts it.
- **The box.** A model is fitted into its item's box by its rest pose's bounds, so an
  animation moves within the box and never rescales the model frame to frame.
- The loader keeps the node tree, the skins and the clips, with their limits counted
  toward the model's limits (ADR-0107).
- `kind.describe` lists the file's animation names in the setting's description once
  the file loads, so a person or an agent can pick one.

**8. Depth-aware bloom.** `depth_bloom` is a depth-aware filter kind: bloom whose glow
stays with what glows.

| Setting | Meaning | Default |
|---|---|---|
| `threshold` | Linear brightness above which a pixel glows | 0.8 |
| `strength` | How bright the glow is, 0 to 4 | 1 |
| `radius_px` | How far the glow spreads, in canvas pixels | 24 |
| `depth_px` | How far in front of a glowing thing a nearer one may be before the glow stops at it | 200 |
| `far_px` | Distance past which glow fades to nothing; 0 never fades | 0 |

It takes the bright parts as `bloom` does, and blurs them in a separable pass whose
weights also fall off with the difference in distance (from the view depth) between a
sample and the pixel it glows onto, so a glow doesn't bleed across something more than
`depth_px` nearer. Where nothing is drawn, a glow spreads as plain bloom's does.

**9. A scene environment.** The `light` kind's `type` gains `environment`: the light of
the whole sky around a 3D scene, for `scene`-lit models' ambient light and reflections.

| Setting | Meaning | Default |
|---|---|---|
| `sky_color` | The sky straight up, sRGB | a pale blue |
| `horizon_color` | The sky at the horizon | white |
| `ground_color` | Below the horizon | a dark grey |
| `image` | A source whose picture is the sky instead, as an equirectangular panorama (an HDRI still or any live source) | none |

- An environment light's item turns the sky (its `turn_deg` spins it about the vertical
  axis), and its intensity times its opacity scales it, as with any light.
- A `scene`-lit model's environment light (its ambient and reflections, ADR-0143
  decision 3) comes from the scene's first environment light, sampled at the reflected
  direction with a blur its roughness sets, in place of the studio gradient or its own
  `reflection`. Ambient lights still scale it, and with no ambient light the environment
  lights the model on its own.
- One environment counts; any after the first are logged once.
- It is not drawn behind the scene. A scene's background is its own setting
  ([ADR-0089](0089-scene-options-and-nested-audio.md)); a sky behind the set is a card or a model that
  shows the same source.

**10. Lit cards, and their backs.** An item gains `surface`, an optional object, omitted
on the wire when absent, which today's cards behave as:

| Field | Meaning | Default |
|---|---|---|
| `lit` | Whether scene lights light the card in a 3D scene | `false` |
| `roughness` | As a model material's, 0 to 1 | 1 |
| `metalness` | As a model material's, 0 to 1 | 0 |
| `emissive` | How much of the card's own picture shows unlit, 0 to 4 | 0 |
| `back` | `show` the card's mirrored picture from behind, or `hide` it | `show` |
| `shadows` | Whether a lit card receives shadows | `true` |

- A `lit` card in a 3D scene is shaded as a flat glTF quad would be: its picture is the
  base colour, with the scene's lights, environment and shadows, and `emissive` adds its
  picture back unlit, so a screen still glows in a dark set. In a 2D scene, or with no
  lights, an unlit card is drawn as today.
- `back = hide` draws nothing where the camera sees the back of the card, in any scene.
- A keyframe scene keys `surface` with the item's other values; Moves glide its numbers.
- Both fields are in the same minor protocol version as `billboard`.

**11. Cost and quality.** Each new pass draws only when read (decisions 1 and 3). The
render benchmarks gain a shadowed lit model, a reflected floor, occlusion, volumetric fog
at each quality and an animated skinned model, each at 1080p, and their defaults stay
inside the frame budget on a mid-range GPU. Depth-aware bloom, a lit card and a model lit
by an environment join them.

**12. Agents and people.** The new settings and kinds reach the UI's properties and the
MCP tools through `kind.describe`. The design notes for building a 3D set gain lighting
with shadows, fog and reflections, which filters go first in a scene's chain (the
depth-aware ones), and what each costs.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - a 3D set gets grounded with shadows, occlusion and a reflective floor, gets
    atmosphere with lit, moving fog, and gets animated models, all as settings and
    filters that key, glide and bind like everything else;
  - a camera-facing sprite or label in a 3D scene is one checkbox;
  - a model reflects a real sky, a glow stays on what glows, and a screen in a set is lit
    by the set's lights and still glows.
- **Harder:**
  - **Cost.** Shadows add up to two 2048² depth passes a frame; a reflection adds a
    second draw of the scene; occlusion and volumetric fog are full-screen passes of up
    to 16 and 96 samples a pixel. Each is drawn only when used, and measured.
  - **Approximations.** Screen-space occlusion misses what is off screen or hidden.
    Reflections are of a horizontal floor only, and a floor tilted by the camera reflects
    correctly only because the plane is in the scene's space, not the screen's. Only `lit`
    cards are lit by lights and receive shadows. Point lights cast no shadows. The
    environment lights models and lit cards but isn't drawn behind the scene.
  - The scene view a depth-aware filter reads is larger than ADR-0143's view depth, and
    every new kind must say which parts it reads.
- **Not decided here:** point light shadows; morph targets; drawing the environment as
  the scene's background.
- **Follow-up implementation PRs**, each with golden tests against a CPU reference:
  - billboards in the protocol's geometry (decision 2);
  - ambient occlusion with the scene view's camera (decisions 1 and 5);
  - volumetric fog with the scene's lights (decisions 1 and 6);
  - shadow maps for scene lights, read by models and by volumetric fog (decision 3);
  - floor reflections (decision 4);
  - model animation, node and then skinned (decision 7);
  - depth-aware bloom (decision 8);
  - the environment light (decision 9);
  - lit cards and hidden backs (decision 10);
  - the design notes and a light marker in the canvas (decision 12, and ADR-0143
    decision 7).
