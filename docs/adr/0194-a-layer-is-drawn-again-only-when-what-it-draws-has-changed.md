<!-- @doc
id = "ADR-0194"
title = "A layer is drawn again only when what it draws has changed"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0194: A layer is drawn again only when what it draws has changed

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
The engine draws every nested scene into a layer it keeps, on every tick, whether or
not anything in the scene moved. A scene of still pictures, text and shapes costs the
same GPU time each frame as one that animates: its clear, each item's draw, and in the
browser-style space ([ADR-0126](0126-one-set-of-blend-modes-and-a-blending-space.md))
a copy and a decode pass. On an integrated GPU a canvas-sized 2D scene of about ten
items was measured at close to 2 ms of a 33 ms frame, every frame, for pixels that
were already in its layer.

The compositor already knows when an image's texels change. Every image counts its
writes ([ADR-0050](0050-3d-transforms-rendering.md) decision 5, `Image::mark_changed`): an
upload rewritten, a layer drawn into, an import handed a new frame. Mip chains are
rebuilt from that count, a layer's known content is tied to it, and a sky backdrop is
already kept while the counts of what it shows stand still.

ADR-0126 §3 also describes the decode of the browser-style space as one pass over
every composite target. Since then a frame's own target is decoded by its resolve
instead, and a layer's decode covers only what its items reached. What that section
promises is unchanged: a layer always leaves the compositor as premultiplied linear
light.

<a id="decision"></a>
## Decision
1. **`Compositor::render_into` leaves a layer as it is when the layer still holds the
   very draw it is asked for.** That is when all of these are as they were at the
   layer's last `render_into`:
   - the vertices the scene's items make: each one's place, texture coordinates,
     opacity, alpha mode, edges and blur, compared byte for byte;
   - each drawn item's image, and that image's count of writes;
   - each one's blend mode, the mip chain it samples and the texels a backdrop mode
     copies;
   - the background, and the blending space;
   - the layer's own count of writes, so nothing else drew into it since.
2. **The comparison is exact.** What was drawn is kept on the layer and compared as it
   is, not hashed, so two different draws never compare equal.
3. **A scene with a model's depth is always drawn.** Its pass also writes the depth
   target the items test against, which the comparison does not cover.
4. **A kept layer is not marked as changed.** So a scene that places it compares equal
   too, and a still scene costs its parents nothing either, however deep it is nested.
5. **Every writer of an image's texels marks the change.** This was already required
   for mip chains; it now decides what is redrawn, so a new kind of write into a layer
   or an upload must call `Image::mark_changed`, and an import must be handed its
   frames through the resource cache, which marks each new one.

This is the first step. A scene in which a few items change still redraws whole;
redrawing only the boxes that changed is left for a later decision.

No setting, switch, protocol message or file changes. The pixels are those a redraw
would give.

<a id="consequences"></a>
## Consequences
- **A still scene costs no GPU time.** Its pass records nothing and submits nothing.
  The CPU work of laying out its items stays, since that is what is compared.
- **The profile shows it.** A pass that was kept reports almost no time, so a scene's
  cost in `profile.get` now depends on whether it changed in that frame.
- **Memory.** Each layer keeps a copy of its last vertices and a few words per item,
  refilled in place, so a steady frame allocates nothing.
- **Risk moved to the write count.** A write into an image that does not mark it would
  now leave a stale layer on screen, where before it left only a stale mip chain. The
  compositor's own writers all mark; decision 5 is the rule for new ones.
- **What does not benefit.** A scene that shows a live source (a capture, a video, a
  browser page that repaints) changes every frame and is drawn every frame, as are the
  scenes that place it. Filter chains, models and the final composite are not covered.
