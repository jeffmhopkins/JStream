<!-- @doc
id = "ADR-0137"
title = "Scene lights and depth-aware scene filters"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0137: Scene lights and depth-aware scene filters

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0094](0094-scene-cameras.md) puts a scene's items in one 3D space seen through a
camera, [ADR-0107](0107-3d-models.md) adds models lit by a fixed `studio` rig that moves
with the camera, and [ADR-0108](0108-camera-depth-of-field.md) blurs each item by its depth
as it draws. ADR-0107 decision 4 left "lights placed in a scene and shared by its models"
to a new ADR. The maintainer asked for richer 3D scenes: lights that can be placed and
animated, and scene effects that read the scene's depth, starting with fog (issue #1231).

What exists to build on:
- **Items carry everything that moves.** An item's geometry (position, `depth_px`,
  `turn_deg`, `tilt_deg`) is keyed by keyframe scenes, glided by Moves, and driven by
  motion modifiers ([ADR-0099](0099-native-motion-modifiers-and-timelines.md)) and audio
  bindings ([ADR-0100](0100-audio-reactive-motion-modifiers.md)), which also drive
  `opacity`. A source's settings change live through `source.update`.
- **The model shader** shades in the camera's viewing frame, with glTF's
  metallic-roughness model, from transforms composed on the CPU in `f64` (ADR-0107
  decision 3), so a far camera doesn't jitter.
- **Each fragment of a card knows its `w`** (ADR-0108 decision 2): `w = D / f`, where `D`
  is its distance along the viewing axis and `f` the focal length in pixels. A model's
  fragments know theirs from the clip position.
- **Scene filters** ([ADR-0065](0065-scene-chains-and-item-effects.md) decision 5) run over
  the scene's composite, in order, after its items are drawn. A filter pass can already
  take images besides its input (a LUT, a referenced scene, a shader source's inputs).
- There is **no depth buffer** across a scene: cards sort by their centres (ADR-0094
  decision 4), and a model's depth test is its own.

The forces:
- A scene without lights or a depth-reading filter must draw and cost exactly as today,
  and existing golden images stay as they are.
- A light should be placed, keyed, moved and bound like everything else in a scene, with
  nothing new to learn and no new animation path.
- Fog and the effects after it (volumetric fog, ambient occlusion, depth-aware bloom)
  need each pixel's distance. A per-pixel depth for the whole scene is the one new thing
  they share, so it is decided once, here.
- Anything on the wire is a minor addition, and an older client keeps working.
- JStream stays general: a light, a fog and a depth image know nothing of what a scene
  depicts.

It is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)): the maintainer asked for the
feature, and it changes no licence, security or privacy posture, compatibility promise or
spending. It adds a source kind and a filter kind, both described by `kind.describe`, a
`lighting` value, and no protocol field. It extends ADR-0065, ADR-0094, ADR-0107 and
ADR-0108, and contradicts none of their decisions.

<a id="decision"></a>
## Decision
**1. A light is a source kind, placed by an item.** `light` is a built-in kind in
`jstream-sources`. Its settings:

| Setting | Meaning | Default |
|---|---|---|
| `type` | `point`, `spot`, `directional` or `ambient` | `point` |
| `color` | The light's colour, sRGB | white |
| `intensity` | Irradiance on a surface facing the light, before falloff, 0 to 64 | 1 |
| `range_px` | Distance at which a point or spot light has faded to nothing, in scene pixels; 0 means it never fades | 0 |
| `cone_deg` | A spot light's full cone angle, 1 to 170 | 45 |
| `softness` | The share of a spot cone, from its edge inward, over which it fades, 0 to 1 | 0.2 |

`kind.describe` describes them, so source properties and the MCP tools take them with no
hand-written code. Every setting is live.

**2. Where a light is, and where it points.** The item that places a light gives it
everything that moves:
- **Position** is the centre of the item's box in the scene's 3D space (its x, y and
  `depth_px`), for `point` and `spot`.
- **Aim** is the item's back normal: out of the back of its box, into the scene. An
  unturned light shines away from the viewer, onto the fronts of what is behind it, and
  `turn_deg` and `tilt_deg` aim it. `spot` and `directional` use it.
- **Strength** is `intensity` times the item's opacity, so a fade, a Move, a motion
  modifier or an audio binding that drives opacity dims the light, and a hidden item's
  light is off.
- The item's size, crop, blend and effects don't change the light.
- In the output a light draws nothing: it reads as a transparent frame, as a model reads
  where something takes it as a flat picture (ADR-0107 decision 6). An editor may draw a
  marker for it in its own overlays.

So a light keys, glides, wiggles and pulses to music through what items already do.

**3. What a light lights.** A light lights the models of the scene it is placed in that
use the new `lighting` value `scene`:
- **`scene`** shades a model with the scene's lights in place of the studio rig. A point
  light at distance `d` arrives with `intensity · (1 − (d / range)²)²` for
  `d < range`, and 0 past it, or with `intensity` when `range_px` is 0. A spot light
  also fades over the outer `softness` of its cone. A directional light arrives from its
  aim, with no falloff. Each shades with the same metallic-roughness model as the studio
  key light.
- **Ambient** lights add up to one colour, which scales the model's environment (the
  studio gradient, or its `reflection` source, ADR-0107 decision 5) for its ambient and
  reflected light. A scene with no ambient light gives a `scene`-lit model no
  environment light, so only its lights and its emission show.
- `studio` and `unlit` are as ADR-0107 decides, and `studio` stays the default, so an
  existing model looks as it did. The kind's settings gain `scene` with no other change.
- A light lights only the scene it is placed in, not scenes nested in it nor scenes that
  nest it. A nested scene or group is drawn by its own lights, then placed as a card.
- **Only in a 3D scene.** Lights are placed in the camera's space. In a 2D scene each
  model is seen by its own viewer (ADR-0107 decision 3) and there is no shared space, so
  a `scene`-lit model there is lit by the studio rig.
- **Bounded.** A scene's first 8 point, spot and directional lights in stacking order
  light it; any after them are ignored and logged once. Ambient lights have no limit.
- **Precision.** Light positions and aims are taken into the camera's viewing frame on
  the CPU in `f64`, as model transforms are, and only the result is `f32`.
- **Flat items are not lit** by scene lights. A card shows its source as it is. Lighting
  a card would need a material on items (#1231 item 4), which is a later decision.
- **No shadows yet.** A light passes through everything. Shadow maps for scene lights
  are a later ADR, with their cost measured.

**4. A scene's view depth.** A scene with a camera can produce a **view depth**: a
canvas-sized image whose red channel holds `1 / w = f / D` for the nearest thing drawn
at each pixel, and 0 where nothing is, which reads as infinitely far.
- It is drawn by a **depth pass** over the scene's items, after the colour pass, with the
  same placement: each card writes its per-fragment `1 / w` where its coverage times its
  alpha is at least one half, and each model writes its own per-fragment `1 / w`. Writes
  keep the larger value, so the nearest thing wins whatever order items draw in, and
  `depth_sort` doesn't change it.
- A nested scene or group writes as a card, flat at its plane, as it sorts and blurs as
  one (ADR-0094 decision 5, ADR-0108 decision 5).
- Depth is not blurred by depth of field: it is each item's sharp shape.
- **Only when read.** The pass runs only for a scene with a camera whose chain has an
  enabled filter that reads depth (decision 5). Otherwise nothing is drawn or allocated,
  so a scene that doesn't use it costs exactly as today.
- Storing `1 / w` keeps its relative precision across the near and far parts of a
  scene in a half-float layer, and makes "cleared" mean "infinitely far".

**5. Depth-aware scene filters.** A built-in filter kind may read the view depth. Such a
kind is a `video_filter` like any other (ADR-0065 decision 2), with the view depth handed
to it beside its input and the camera's focal length in its parameters, so it can turn
`1 / w` back into a distance:
- **On a scene's chain, in a 3D scene,** it reads that scene's view depth.
- **Anywhere else** (a 2D scene, a source, item, master or output chain), there is no
  depth, and it passes its input through unchanged. Its description in `kind.describe`
  says so, so an agent or a person knows where it works.
- **Order.** Scene filters run in chain order over the composite (ADR-0065 decision 5),
  after depth of field has blurred each item as it drew. A depth-aware filter reads the
  sharp depth of decision 4, so at a blurred item's soft edge the depth is the item's or
  what is behind it, not a mix. A filter earlier in the chain that moves pixels (a
  transform, a blur) moves them away from their depth, so depth-aware filters belong
  first; the design notes say so.
- **Translucency.** A pixel where a card's alpha is under one half takes the depth of
  what is behind it, so fog or occlusion there is that of the far thing. This is the
  usual compromise for a single depth layer.
- Which built-in kinds read depth is the render crate's to know. No protocol field marks
  them; a later kind that reads depth needs only to be listed there.

**6. Depth fog.** `depth_fog` is the first depth-aware filter kind:

| Setting | Meaning | Default |
|---|---|---|
| `mode` | `linear` or `exponential` | `exponential` |
| `color` | The fog's colour, sRGB with alpha | light grey, opaque |
| `start_px` | Distance along the viewing axis where fog begins, in scene pixels | 0 |
| `end_px` | `linear`: where fog is at its most | 4000 |
| `density` | `exponential`: fog per 1000 px past `start_px` | 0.5 |
| `max` | The most fog anywhere, 0 to 1 | 1 |
| `background` | Whether fog fills where nothing was drawn (infinitely far) | `true` |

At distance `D`, the fog amount `a` is `(D − start) / (end − start)` for `linear`, or
`1 − exp(−density · (D − start) / 1000)` for `exponential`, clamped to 0 to 1 and then
to `max`, and 0 where `D < start`. The pixel becomes `mix(pixel, fog, a)`, both
premultiplied, in linear light: fog is a layer of opacity `a` over what is there. Where
nothing was drawn, `a` is `max` if `background` is set and 0 if not. Its settings
animate as any filter's do ([ADR-0127](0127-moves-interpolate-effect-settings.md)).

**7. Agents and people.** The `light` kind, the `scene` lighting value and the
`depth_fog` kind reach the UI's properties and the MCP tools through `kind.describe`. The
design notes agents read gain a section on lighting a 3D set: a key, a fill and an
ambient light, aiming by turning the item, dimming through opacity, and depth fog first
in a scene's chain, with what each costs.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - a light is placed, keyed, moved, wiggled and bound to audio with the tools that
    already move items, and its colour and strength change live;
  - fog that thickens with distance is one scene filter, and the view depth is the one
    shared input for volumetric fog, ambient occlusion and depth-aware bloom later;
  - nothing changes or costs more for a scene without lights or a depth-aware filter.
- **Harder:**
  - **Cost.** A `scene`-lit model shades up to 8 lights per pixel. A depth-aware filter
    adds one extra draw of every item into a half-float canvas-sized layer, and its own
    pass. The render benchmarks gain a lit model and a fogged 3D scene to keep both
    measured ([R-RT-005](../RULES.md#R-RT-005)).
  - **Approximations.** One depth layer: translucent cards under half alpha don't hold
    fog of their own, and a blurred edge's fog follows its sharp shape. No shadows, and
    cards are not lit.
  - A light item has a box that does nothing visible. Editors must show it some other
    way.
- **Not decided here:** shadows from scene lights; lighting flat items; an environment
  for the whole scene (beyond a model's own `reflection`); volumetric fog, ambient
  occlusion, depth-aware bloom and planar reflections, each of which builds on
  decision 4 as its own decision; lights in 2D scenes.
- **Follow-up implementation PRs**, each with golden tests against a CPU reference:
  - **`jstream-render`:** `scene` lighting in the model draw with a light list
    (decision 3), the view depth pass (decision 4), and the `depth_fog` kind with its
    depth input (decisions 5 and 6), with a lit model and a fogged scene in the render
    benchmarks;
  - **`jstream-sources`, `jstream-media` and `jstream-core`:** the `light` kind and the
    `scene` lighting value, gathering a scene's light items into the light list each tick,
    and producing the view depth for a scene chain that reads it;
  - **`jstream-ui` and the MCP design notes:** a marker for a light item in the canvas,
    and the lighting section (decision 7);
  - ARCH › GPU pipeline gains lights and the view depth.
