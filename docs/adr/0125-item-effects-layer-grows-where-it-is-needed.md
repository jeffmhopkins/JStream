<!-- @doc
id = "ADR-0125"
title = "An item's effects layer grows at the first effect that draws past the box"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0125: An item's effects layer grows at the first effect that draws past the box

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0065](0065-scene-chains-and-item-effects.md) §5 runs an item's effects "on a layer
the size of the box in canvas pixels, plus an outset", the sum of the chain's
`max_outset_px`. Until #1226 no built-in kind declared one, so the layer was always the
box, and a glow or drop shadow on an opaque item was cut at its edge: the glow, drawn
under the picture, didn't show at all. Once drop shadow, outline, glow and bloom
declare their outsets, the layer as ADR-0065 has it is padded from the start. Every
effect then sees the box inside a transparent margin, and the effects that frame
their input frame the margin instead: `rounded_corners` rounds corners that are
transparent, a `vignette` darkens the margin, an area blur's rectangle and an
`image_mask` stretch over the larger frame. Rounded corners followed by a drop shadow
is the recipe for a framed card, so that would break it.

<a id="decision"></a>
## Decision
1. **The item is drawn into a layer the size of its box** (scaled down to fit the
   largest texture side with the outset, as before).
2. **The layer grows where it is needed.** At the first enabled effect whose kind
   declares an outset, the chain runtime pads that effect's input by the outset of it
   and of every effect after it, on every side, in layer pixels
   (`VideoFilter::set_pad`): the input is drawn centred in a transparent frame that
   much larger, then the effect runs over it. The effects before it see the box
   itself; it and the ones after it see the grown frame.
3. **The result is placed as ADR-0065 §5 places it:** over the box grown by the whole
   outset in proportion, turned and projected as the box is. The final layer's size is
   the same as before, up to rounding.
4. **Order is the person's.** A framing effect after one that draws past the box
   frames the grown frame, as any effect frames what it is given. Put rounded corners,
   a vignette or a mask before a shadow, a glow or an outline.
5. **Canvas-space effects** ([ADR-0106](0106-item-effects-in-canvas-space.md)) have no
   outset, so nothing changes for them. Source and scene chains are not padded.

<a id="consequences"></a>
## Consequences
- **Easier:** a glow, a shadow or an outline on an opaque item shows past its box, and
  the effects before it keep framing the box, so a rounded card keeps its corners and
  its shadow follows them.
- **Harder:** the padding costs one copy of the input into a scratch layer the size of
  the grown frame, once per frame for the padded effect. The layers after it are as
  large as ADR-0065's.
- **Amends** ADR-0065 §5's render order for box-space item effects.
- **Docs to change:** core › Snapshots, render › Filters, ARCHITECTURE's data model.
