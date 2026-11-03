<!-- @doc
id = "ADR-0108"
title = "Depth of field for scene cameras"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0108: Depth of field for scene cameras

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0094](0094-scene-cameras.md) gives a scene a camera: items share one 3D space and are
seen through `position`, `target`, `fov_deg` and `roll_deg`, and a Move between keyframe
scenes glides the camera. Everything is in focus. The maintainer asked for a focus depth
as well, settable like the field of view and animatable like the rest of the camera, so
that a camera can pull focus from a far item to a near one, or soften a backdrop behind a
presenter.

What exists to build on:
- **The camera's map** (ADR-0094 decision 3): each fragment of an item already knows its
  homogeneous `w = D / f`, where `D` is its distance along the viewing axis and `f` the
  focal length in pixels. `D` is therefore known per pixel, including across a tilted item.
- **Mips.** Every item in a 3D scene takes the 3D drawing path
  ([ADR-0050](0050-3d-transforms-rendering.md) decisions 5 and 6): a full mip chain of its
  cropped texels, trilinear and anisotropic sampling, and analytic edge coverage with a
  polygon pushed out past its edges.
- **The blur filter** (`filter/multipass`) is a separable Gaussian over a whole layer. It
  blurs a source by one radius, not each pixel by its own.
- **Moves and live values.** `render::transition::lerp_camera` interpolates the camera
  through a Move, and `live.set` of `scene.set_camera` carries a drag.

The forces:
- A scene without depth of field must cost and look exactly as today.
- The blur varies within one item (a tilted floor runs from sharp to soft), so one blur
  radius per item is wrong. A per-item Gaussian pass would also need an intermediate
  target per item, which a 3D scene doesn't have.
- A defocused item's silhouette must soften too, or it reads as a sharp cut-out with
  a blurry inside.
- Anything new on the wire is a minor addition, and an older client must keep working.

It is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)): the maintainer asked for the
feature, and it changes no licence, security or privacy posture, compatibility promise
or spending. The protocol gains two optional fields. It extends ADR-0094 and contradicts
none of its decisions.

<a id="decision"></a>
## Decision
**1. Two camera fields.** `SceneCamera` gains:

| Field | Meaning | Default |
|---|---|---|
| `focus_px` | Distance along the viewing axis from the camera to the plane in focus, in scene pixels; absent, everything is in focus | absent |
| `blur_px` | How soft focus gets: the blur radius, in canvas pixels, of a point infinitely far behind the focus plane | 0 |

`focus_px` is above 0 and at most `MAX_DRAWN_PX`; `blur_px` is `0 ≤ blur ≤ 64`. Both are
`Finite32`, and anything else is `invalid_argument`. The camera has **depth of field**
when `focus_px` is present and `blur_px` is above 0. Otherwise it draws exactly as
ADR-0094 does.

**2. The blur at each pixel.** A point at distance `D` along the viewing axis is blurred
over a disc of radius `r = blur_px · |D − focus_px| / D`, capped at 64 canvas pixels. This
is the thin-lens circle of confusion with the aperture scaled so that `blur_px` is its
limit far away. At the focus plane it is 0. At half the focus distance it is `blur_px`.
Nearer than that it grows until the cap. The compositor works it out per fragment from
the fragment's `w`, so a tilted item blurs gradually along itself.

**3. Drawn with the item's mips.** The fragment shader samples the item's mip chain over a
disc of radius `r`: a fixed pattern of taps, rotated per pixel, read at the level whose
texel matches the disc's tap spacing. It adds no pass, target or per-item intermediate.
Where `r` is under half a pixel, the shader takes today's single sample.

**4. The silhouette softens.** The edge coverage of ADR-0050 decision 6 widens from one
pixel to the disc: `clamp(d / (2r) + 0.5, 0, 1)` where `r ≥ 0.5`. The drawn polygon is
pushed out by the larger of one pixel and the vertex's own `r`, so the soft edge
rasterises. Coverage and colour are blurred separately, which is the usual
approximation: an item doesn't blur into what is behind it beyond its own soft edge.

**5. Layers, sort and blend.** A nested scene or group in a 3D scene is a card like any
item (ADR-0094 decision 5), and it is blurred as a card. The scene inside it is drawn
through its own camera, with its own depth of field if it has one. Sorting and blend
modes are unchanged.

**6. Editing and animating.** Both fields belong to the camera, so everything ADR-0094
decision 6 gives the camera applies to them. `scene.set_camera` sets them, `live.set`
carries them, a keyframe scene's camera override holds them, and collection files
store them, omitted when absent, with the schema version unchanged.
- **A Move** interpolates `blur_px` linearly, and `focus_px` linearly when both ends
  have one. When only one end has a focus, the other is treated as focused there with
  `blur_px` 0, so the blur fades in or out without jumping.
- **Native motion** (ADR-0099, in review) of the camera, when it lands,
  lists `focus_px` and `blur_px` among its targets.

**7. Interfaces.**
- Scene Properties gains a **Depth of field** check with **Focus distance** and **Blur**
  fields. The check turns it on at the distance to the camera's target. **Focus on
  selection** sets the focus distance to the selected item's distance along the view.
- The MCP `scene` tool's `scene.set_camera` gains the fields from the protocol schema.
  The design notes say how to pull focus with two keyframe scenes.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - focus pulls and a soft backdrop are one camera value, which animates through Moves,
    live values and agents like the rest of the camera;
  - it costs nothing in a scene without depth of field, and adds no pass or target in
    one that has it.
- **Harder:**
  - **Quality.** A mip-based disc is an approximation. Large radii look smoother but
    blockier than a true Gaussian. A thin bright line can shimmer as the blur animates.
    Golden tests bound it against a reference blur.
  - **Edges.** An item never bleeds colour past its own softened edge. A sharp item in
    front of a blurred one keeps a sharp edge, which is right. A blurred item in front
    of a sharp one doesn't show the sharp one through its blur, beyond its coverage ramp.
  - **Cost.** The extra taps cost fill rate in proportion to the blurred area.
- **Not decided here:**
  - **Bokeh shapes, highlights and a per-pixel depth buffer.** A true scatter-based
    depth of field would need the depth buffer that ADR-0094 leaves out.
  - **2D scenes.** They have no camera, so they have no focus.
- **Follow-up implementation PRs:**
  - **`jstream-protocol`:** the fields, their validation, and the shared
    circle-of-confusion function, with unit tests. Also `jstream-graph`, the mock engine
    and collection files (a minor protocol version).
  - **`jstream-render`:** the per-fragment blur and the soft edge, with golden tests of a
    tilted plane against a reference blur. Also `lerp_camera`'s rule.
  - **`jstream-ui`:** the dialog's fields and Focus on selection. Also the MCP design notes.
