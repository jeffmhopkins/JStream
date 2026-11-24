<!-- @doc
id = "ADR-0219"
title = "A big item's effects layer is placed at its box's size"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0219: A big item's effects layer is placed at its box's size

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Amends [ADR-0211](0211-a-filters-pixel-settings-count-in-its-inputs-extent.md) decision 3.
It changes how built-in effects draw on one size of item and promises nothing new to clients
or packs, so it is agent tier ([ADR-0043](0043-narrow-maintainer-tier.md)).

<a id="context"></a>
## Context
An item with effects is drawn into a layer the size of its box, and a box longer with its
outset than the largest layer side (`MAX_LAYER_SIDE`, 8192) is drawn into a layer scaled
down to fit ([ADR-0065](0065-scene-chains-and-item-effects.md) §5). Core turns the effects'
outset, in canvas pixels, into the layer's texels for the pad
([ADR-0125](0125-item-effects-layer-grows-where-it-is-needed.md),
[ADR-0136](0136-an-effects-outset-follows-its-settings.md)).

ADR-0211 decision 3 takes an item's layer to be placed at its own size, so its texels are
its extent pixels. A scaled-down layer broke that: its extent was its own size, so its
built-in effects counted their pixel settings in layer pixels while the pad counted canvas
pixels. A drop shadow or blur reached up to twice as far across the canvas as on a smaller
item, and the pad cut it (#1720). The box's side and the pad were also each rounded up, so
the padded layer could be one pixel past the largest side, and then the layer couldn't be
made and the item drew nothing.

<a id="decision"></a>
## Decision
1. **A scaled-down layer is placed at the box's size.** An item effects layer scaled down to
   fit the largest side has the box's size in canvas pixels, rounded, as its extent
   ([ADR-0073](0073-sources-drawn-at-their-shown-size.md)), set on every frame. Its texels
   per extent pixel are then the scale core pads by, so ADR-0211's scaling and the pad agree.
   A layer at its own size keeps its size as its extent, and a model's layer keeps the
   extent ADR-0197 gives it.
2. **A pad grows the extent in extent pixels.** A pad counts in texels; the extent of what
   the padded filter draws grows by as many extent pixels as those texels stand for, so the
   filters after it keep the same scale.
3. **The scale leaves room for rounding.** A box with an outset is scaled to fit 2 pixels
   short of the largest side, so its side and its pad, each rounded up, still fit it.

<a id="consequences"></a>
## Consequences
- A shadow, a glow or a blur on an item larger than the largest layer side reaches as far
  across the canvas as on a smaller item, and is not cut at the layer's edge.
- Such an item no longer draws nothing when its padded layer would have been a pixel too
  large.
- A pack effect on such a layer still counts its parameters in texels (ADR-0211 decision 2),
  so it reaches further than its pad allows and can be cut there, as before.
- The cost is one extent stored per boxed item per frame; the filters' scaled settings are
  worked out again only when the scale changes (ADR-0211 decision 4).
