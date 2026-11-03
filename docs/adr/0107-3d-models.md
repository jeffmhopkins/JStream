<!-- @doc
id = "ADR-0107"
title = "3D models: meshes, materials and live textures in a scene"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0107: 3D models: meshes, materials and live textures in a scene

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0050](0050-3d-transforms-rendering.md) draws an item as a flat picture placed in 3D,
and [ADR-0094](0094-scene-cameras.md) puts a scene's items in one 3D space seen through a
camera. Every item is still a card: a source's flat frame on a plane. The maintainer asked
for the 3D system to take real assets: models made in a 3D tool, with their textures, lit
and moved like any other item, with JStream's sources, filters and effects applying to
them. ADR-0094 left "lights, shadows and materials" undecided.

An audit of the 3D system as built (the protocol's `geometry/drawn.rs` and
`geometry/camera.rs`, the compositor in `jstream-render`, the UI's `canvas/camera.rs`),
before anything is added to it:
- **Near-plane clipping holds.** A flat item is clipped at `w = 1/256` in its own frame, so
  the part in front of the camera draws exactly (spike S5's scenario B: mean 0.43, p99 9).
  A mesh has no single plane to clip in; the GPU's own clip against a near plane in clip
  space does the same job per triangle.
- **Precision holds for cards and would not for meshes.** Placement is composed in `f64`
  and only the final canvas point and `w` are `f32`, so a far camera doesn't jitter. A
  mesh drawn the usual way (a world matrix in `f32` on the GPU) would: at a camera
  10⁷ px from the origin an `f32` world position has a resolution of about 1 px.
- **Depth is per item.** There is no depth buffer. Items sort by the depth of their box
  centre (ADR-0094 decision 4), which is right for cards that don't pass through each
  other and wrong for a solid shape, whose own faces must hide each other per pixel.
- **Picking is by the box.** The UI hits and outlines an item by its box's four projected
  corners, so it can only pick what is flat. An item with a corner behind the camera can't
  be hit at all; the camera work tracks that as #1054, and the up-vector jump as #1047.
- **Cost is unmeasured.** The render benchmarks have no 3D scene. Every item in a 3D scene
  takes the 3D path, so each live source in one builds a mip chain on every new frame.

What exists to build on: the source registry (`kind.list`, `kind.describe`, settings that
name an asset file or another source, as `spectrum`'s `audio_source` does), assets read
under an asset root with a size cap, the per-source mip chains of ADR-0050 decision 5,
the shader sources of [ADR-0093](0093-shader-sources-from-effect-packs.md), item effects
([ADR-0065](0065-scene-chains-and-item-effects.md)) and the reflection
recipes in the MCP design notes, which build a reflection from layered cards.

The forces:
- A model must work in a 2D scene too, and must not change anything for a scene without
  one: existing golden images stay as they are.
- Most items are transparent cards; a model is mostly opaque. Drawing far to near (ADR-0094)
  keeps cards right; a model also needs depth within itself.
- Sources, filters and effects are JStream's vocabulary. A model should be a source, take
  them, and be able to show other sources (a webcam on a model's screen, a shader on a
  sphere).
- JStream stays general: a model is any asset, with no knowledge of what it depicts.

This extends ADR-0050 and ADR-0094 and contradicts neither: a scene with no model draws as
before. It is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)): the maintainer asked for the
feature; it adds a source kind and an optional protocol field, and one pure-Rust parsing
crate under the existing licence list ([R-DEP-001](../RULES.md#R-DEP-001)). 3D models are
neither audio nor video codecs, so ADR-0061's codec rule doesn't reach them; like images
and fonts they use a pure-Rust crate, the same on every platform.

<a id="decision"></a>
## Decision
**1. A model is a source kind.** `model` is a built-in kind in `jstream-sources`. Its
settings:

| Setting | Meaning | Default |
|---|---|---|
| `shape` | `file`, or a built-in shape: `cube`, `sphere`, `cylinder`, `cone`, `plane` | `file` |
| `file` | A glTF 2.0 asset (`.glb`, or `.gltf` with its buffers and images beside it in the same asset root), for `shape = file` | — |
| `color` | Base colour of a built-in shape; multiplies a file's base colour | white |
| `metallic`, `roughness` | A built-in shape's material; a file's materials keep their own | 0, 0.5 |
| `texture` | Another source whose picture is the base-colour texture (decision 5) | none |
| `texture_material` | Which of a file's materials `texture` replaces; empty means all | empty |
| `reflection` | Another source whose picture the model reflects (decision 5) | none |
| `lighting` | `studio` or `unlit` (decision 4) | `studio` |
| `double_sided` | Draw back faces of a built-in shape; a file's materials keep their own | `false` |

`kind.describe` describes them, so the UI's source properties and the MCP tools take them
with no hand-written code. A setting change that only changes the material (`color`,
`metallic`, `roughness`, `texture`, `reflection`, `lighting`) is live; a new `shape` or
`file` reloads the model.

**2. What a file may hold.** The loader reads a glTF file's default scene: its node
hierarchy (matrix or TRS), triangle meshes with positions, normals, tangents, one or two
texture-coordinate sets and vertex colours, and metallic-roughness materials (base colour,
metal and roughness, normal, occlusion and emissive textures and factors, alpha mode
`OPAQUE`, `MASK` or `BLEND`, `doubleSided`), with `KHR_materials_unlit` and
`KHR_materials_emissive_strength`. Missing normals are computed flat, missing tangents
from texture coordinates. Images are PNG or JPEG, decoded by the image source's decoders.
- Skins, morph targets, animations, cameras, lights and other extensions are ignored, and
  the model draws in its rest pose; each ignored feature is logged once. A file that
  *requires* an extension the loader doesn't have (Draco, meshopt, KTX2) is an error that
  names it.
- **Bounded.** The file and each buffer and image file count against the asset size cap
  the image source uses. A model may have at most 2,000,000 triangles and 64 textures,
  and take at most 512 MiB in all: its vertex and index data and its textures with their
  mips. A texture larger than 4096 px on a side is scaled down on load. A model over a cap
  is an error naming the cap, so a source can't take unbounded GPU memory.
- **Off the render thread.** Parsing, decoding and building the mesh happen when the
  source is created or updated, on the thread that does that for every source, and so
  does building each texture's mips. The render thread uploads a model once per version
  and then only draws it
  ([R-RT-002](../RULES.md#R-RT-002)).

**3. Placing a model.** An item places a model as it places any source; its geometry is
the model's pose:
- **Size.** The model's bounds (its default scene with node transforms applied) are
  centred on the item's centre. `fit` scales the model uniformly so its width and height
  fit the box, `fill` uniformly so they cover it, and `stretch` scales x to the box's
  width, y to its height and z by the smaller of the two scales. Depth is in the same
  pixels, so a model's depth is its scale times its own depth.
- **Axes.** glTF's +x right, +y up, +z toward the viewer map to the scene's x right,
  y down, z toward the viewer by negating y, which also flips winding, so front faces are
  counter-clockwise as authored. `flip_h` and `flip_v` mirror the model's x and y.
- **Pose.** `rotation_deg`, `turn_deg`, `tilt_deg` and `depth_px` orient and place it as
  ADR-0022 and ADR-0094 orient and place a box.
- **2D scenes.** Without a camera, a model is seen by the item's own viewer: through its
  `perspective_px` when it has one, orthographically when it doesn't (ADR-0022's
  `w = 1`). A model in a 2D scene looks straight on at its front by default.
- **Crop** doesn't apply to a model and is ignored.
- **Precision.** The full model-to-clip transform is composed on the CPU in `f64`,
  relative to the camera, and only the result is `f32`, so a far camera or a large scene
  doesn't jitter. Depth is a reversed `Depth32Float` with its near plane at ADR-0050's
  `w = 1/256` and no far plane.

**4. Lighting.** A model is shaded with glTF's metallic-roughness model (Lambert diffuse,
GGX specular) in linear light, into the compositor's linear working space.
- **`studio`** is a fixed rig relative to the camera: a key light above and to the left, a
  fill opposite it, and a soft environment (a vertical gradient) for ambient light and
  reflections. Because it moves with the camera, a model reads well from any angle with no
  setup. It is not a scene light: other models are lit by their own rig.
- **`unlit`** shows base colour (and emissive) as authored, which is how a flat-shaded or
  baked asset is meant to look. `KHR_materials_unlit` materials are always unlit.
- Emissive light above 1 is kept: the working space is floating point, and the output
  chain decides how it is shown.
- **Not decided here:** lights placed in a scene and shared by its models, and shadows.
  Both would be scene-level fields, and a new ADR.

**5. Sources as textures and reflections.** `texture` and `reflection` name another
source, so anything JStream can show can be on a model:
- **`texture`** replaces the base-colour texture of the chosen materials with the named
  source's picture, after that source's own filters, sampled through its mip chain
  (ADR-0050 decision 5) with the material's texture coordinates. A built-in shape maps it
  once around (its texture coordinates are the usual ones for each shape). A webcam, a
  browser page, a media file, a shader source or a scene (through a scene source) all
  work, live, every frame.
- **`reflection`** is an environment the model reflects: the named source's picture is
  read as an equirectangular panorama and replaces the studio environment, for both the
  specular reflection (by roughness, through its mips) and the ambient light. A sky
  plate, a shader source or the scene itself, through a scene source, makes the model
  reflect its surroundings.
- **Cycles.** A model whose `texture` or `reflection` reaches a scene that shows the model
  is a reference cycle, refused as the scene source refuses one.
- A named source that is missing or not yet drawing reads transparent (`texture`) or the
  studio environment (`reflection`), and the model still draws.

**6. Drawing.** For each model item, the compositor:
1. draws the model through the camera (or the item's viewer) with its own depth test,
   cleared per model, into a transparent layer the size of the canvas. Opaque and masked
   primitives draw first, then blended ones far to near by the depth of their centres,
   testing depth without writing it, as glTF viewers commonly do;
2. runs the source's video filters, then the item's effects, over that layer, in canvas
   pixels (a model has no flat box to run them in, so its effects always run over the
   canvas);
3. composites the layer like any item, with its opacity and blend, in its place in the
   scene's order: in a 3D scene, the depth of the item's centre along the viewing axis
   (ADR-0094 decision 4), so a model sorts with cards exactly as a card does.

A scene with no model draws exactly as before: no new pass, target or pipeline.

**7. Depth between models and cards.** In a 3D scene with a model, the scene's pass gets
one depth target. A model's layer writes its depth there where the model covered the pixel
(before filters, so a blur doesn't move depth), and every item drawn after it tests against
it without writing. So a card that sorts in front of a model but is partly behind it is
hidden where the model is nearer. A card drawn before a model doesn't hide it: cards still
have no depth of their own, as ADR-0094 says. A scene with no model has no depth target.

**8. Bounds and picking.** `source.sizes` and `source.size_changed` report a model's size
as the width and height of its bounds, at 512 px for the larger, and gain an optional
`depth` for a source with depth (omitted for every other source, so it is a minor
protocol addition). An item of a model is placed by that size as any new item is. The UI
outlines and hits a model's item by its 3D bounds (the item's box extruded by the model's
depth through decision 3), the nearest one under the pointer winning in a 3D scene, through
one shared function in `jstream-protocol`'s placement module, as for cards.

**9. Agents.** The kind and its settings reach the MCP tools through `kind.describe`. The
design notes agents read gain a section on models: built-in shapes for simple sets, a file
for an asset, `texture` to put a live source on a model, `reflection` with a scene source
for a model that reflects its scene, and `studio` against `unlit`.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - a real asset drops into a scene as one source, lit, from any angle, with no setup;
  - a model shows any JStream picture on its surfaces and in its reflections, so effect
    packs and live sources reach 3D with nothing new to learn;
  - a model moves, keys, animates and transitions as any item does, since its pose is
    the item's geometry.
- **Harder:**
  - a model costs a canvas-sized layer, its depth and a pass per model per frame, plus
    its filters over the whole layer; the render benchmarks gain a 3D scene and a model
    to keep that measured ([R-RT-005](../RULES.md#R-RT-005));
  - cards still sort by their centres among themselves, and a card drawn before a model
    can't hide part of it; per-pixel depth for cards would need a new ADR;
  - only static models: an asset's own animation, skins and morphs don't play;
  - `jstream-sources` gains a glTF parsing dependency, justified in its PR
    ([R-DEP-004](../RULES.md#R-DEP-004)).
- **Not decided here:** scene lights and shadows; playing a file's animations on the media
  clock; per-material settings beyond the base-colour texture; cards that share the depth
  buffer.
- **Follow-up implementation PRs**, each with golden tests against a CPU reference:
  - `jstream-render`: the model draw (decisions 3, 4 and 6) from CPU mesh data, with the
    built-in shapes, and a 3D scene in the render benchmarks;
  - `jstream-sources` and `jstream-core`: the `model` kind, the glTF loader and its
    limits, and routing a model item to the model draw;
  - `texture` and `reflection` (decision 5), with the cycle check;
  - the protocol's `depth` and the shared bounds function, the UI's picking and outline,
    and the MCP design notes (decisions 8 and 9);
  - the depth target between models and cards (decision 7);
  - ARCH › Data model and GPU pipeline gain models.
