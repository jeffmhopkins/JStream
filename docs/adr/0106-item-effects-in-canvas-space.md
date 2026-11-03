<!-- @doc
id = "ADR-0106"
title = "An item's effects can run over the canvas, after it is placed"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0106: An item's effects can run over the canvas, after it is placed

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0065](0065-scene-chains-and-item-effects.md) §5 runs an item's effects over
its cropped, fitted box, in the box's own frame, before the item is placed, so an effect
turns, scales and flips with its item. That is right for rounded corners or a vignette
on a shot. It is wrong for an effect that must line up with the canvas: an `image_mask`
or `scene_mask` drawn at canvas size, or a gradient across the screen. A reflection is
the common case. It is the item flipped upside down, squashed, blurred and faded by a
canvas-sized gloss mask. The mask flips and squashes with the item, so today each
reflection needs one more nested scene, with the mask on the nested scene's item
(#1075).

<a id="decision"></a>
## Decision
1. **An item has an effects space**, `box` (the default, ADR-0065 §5 unchanged) or
   `canvas`. `item.set_effects_space { item_id, space }` sets it, with the previous
   space as its inverse. Like a lock ([ADR-0096](0096-item-lock-and-png-preview-frames.md)),
   it belongs to the item: it takes no `scene_id`, and a keyframe doesn't override it.
   A keyframe's overridden effects ([ADR-0088](0088-keyframe-scenes-override-item-effects.md))
   run in the item's space. `ItemInfo.effects_space` reports it, omitted while `box`.
   `item.remove`'s inverse sets it back. The collection file saves it only when it is
   `canvas`, so no schema bump is needed.
2. **Canvas space.** The item is drawn, with its crop, fit, place, rotation, flips and
   per-item 3D, into a transparent canvas-sized layer. The effects run over that layer,
   and the layer is then drawn over the whole canvas, flat, with the item's opacity and
   blend. Effect sizes are in canvas pixels. The layer has no outset, since nothing
   outside the canvas is seen. It is scaled down uniformly to fit the largest texture
   side, as a box layer is. The result is what the nested-scene workaround draws,
   without the extra scene.
3. **A scene camera** ([ADR-0094](0094-scene-cameras.md)) sees the layer as a flat
   picture covering the canvas, as it sees a nested scene placed there. The item is
   drawn into the layer without the camera.
4. **Transitions.** A canvas-space item's layer covers the canvas in both scenes, so it
   can't glide: in a Move it never pairs, and it disappears and appears with its
   effects. A space with no enabled effect changes nothing: the item draws as if it had
   no effects.
5. **Cost.** One canvas-sized `Rgba16Float` layer and its scratch texture per item, in
   place of the box-sized ones, and a pass per enabled effect over the whole canvas. It
   is bounded by the canvas, so it costs no more than the nested scene it replaces.
6. **The protocol** goes to 0.77 for the new edit and field. The CLI, the MCP tool's
   schema and the Filters panel (an "Over the canvas" check under an item's effects)
   expose it.

<a id="consequences"></a>
## Consequences
- **Easier:** a reflection is one item, a flipped `scene` source with a blur, a
  canvas-space mask and an additive blend. Any canvas-aligned mask or gradient on a
  moved item works the same way.
- **Harder:** a canvas-space item costs canvas-sized passes even when the item is small.
  It doesn't glide in a Move.
- **Not decided here:** a per-effect space (some effects over the box, then others over
  the canvas). That needs two layers per item. The nested-scene workaround still covers
  it.
- **Amends** ADR-0065 §5, which gives only the box-space render order.
- **Docs to change:** ARCHITECTURE's data model, protocol and GPU pipeline, the
  protocol, graph, core, control, CLI and UI READMEs, and UI-FEATURES' filters rows.
