<!-- @doc
id = "ADR-0250"
title = "A model shows a live texture on each part it names"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0250: A model shows a live texture on each part it names

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0107](0107-3d-models.md) decision 5 gives a `model` source one live
texture: the `texture` setting names a source whose picture replaces the base-colour
texture of the material `texture_material` names, or of every material. A set with
several screens (a cockpit, a control room, a wall of monitors) can then show one
picture on one model. Showing a picture on each screen takes one model item per
screen, and each model item is drawn into a layer of its own, the canvas's size,
however small it is seen. On an integrated GPU (Radeon 780M) at 1080p30, nine small
screen quads drawn that way cost 1.7 to 2.5 ms each, about 20 ms a frame of 33.

Screens in a file often share one material, so naming a material is not enough to tell
them apart: a file's nodes and meshes have names of their own, which its author gives
each screen. Their texture coordinates may share one texture too, an atlas, each screen
covering its own part of the square.

[ADR-0249](0249-a-nested-scene-is-drawn-at-the-size-it-is-shown.md) already sizes a
model's live texture by the texels its textured triangles show.

<a id="decision"></a>
## Decision
1. **Texture slots.** A `model` source has sixteen texture slots. The first is the
   existing `texture` and `texture_material`. The others are `texture_2` to
   `texture_16`, each a live source like `texture`, with `texture_material_2` to
   `texture_material_16`, each a name like `texture_material`. Each slot also has a
   region (`texture_region`, `texture_region_2` to `texture_region_16`). They are
   ordinary live settings of the kind, in the group `Textures`. No protocol type,
   message or setting changes for a source that sets only the first, and settings
   saved before the slots existed have none. A slot whose source is unset is left out.
2. **What a slot is on.** A slot's name matches a part (a glTF primitive) by its
   material's name, the name of the node that draws it, or its mesh's name, as the file
   has them. A part takes the first slot whose name matches it, else the first slot
   with no name, else none and keeps its own texture. With only the first slot set,
   that is ADR-0107's rule.
3. **A slot's region.** A region is `x y width height`, each from 0 to 1 of the parts'
   texture coordinates, inside the square: the whole picture covers that part of
   them, so a screen laid out in one corner of an atlas shows the whole picture with no
   edit to the file. Empty, the default, covers all of them. The picture repeats past
   its region, as a texture repeats past the square.
4. **One model, one pass.** The model is drawn once into its layer, each part bound to
   its slot's picture through its own material group, so a set of screens on one
   model costs one model pass, however many pictures it shows.
5. **Each picture sized as it is seen.** Each slot's picture records the resolution its
   own parts need, as ADR-0249 decision 2 measures it, against the bounds of those
   parts rather than the model's (the model's for a rigged model, whose pose moves its
   parts), its picture spread over its region. A screen seen close is drawn at full
   resolution, one far off at less.

<a id="consequences"></a>
## Consequences
- **Many screens cost one model.** A file with nine screens shows nine pictures in one
  model pass, in place of nine model items and nine canvas-sized layers.
- **No model edits.** Screens that share a material are told apart by their node or
  mesh names, which a file usually has, and screens that share an atlas by their
  regions.
- **Sixteen at most.** A model with more screens than slots shows the rest on a second
  model item, or one picture across several parts by naming their shared material.
- **The panel grows.** The kind lists forty-six more settings, grouped under
  `Textures`.
- **What stays.** A model with one texture draws as before.
