<!-- @doc
id = "ADR-0082"
title = "Blend modes: how each is drawn, and in which colour space"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0082: Blend modes: how each is drawn, and in which colour space

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0023](0023-placements-references-and-clones.md) §1 gives every placement a `blend`,
a string enum defaulting to `normal`, and leaves the set of modes and their maths to the
render pipeline (decision register D13,
[design/ui-prototype](../../design/ui-prototype/decision-register.md)). Until the
compositor drew a mode the engine refused it with `invalid_argument`, so the protocol
listed nine modes (`BlendMode::ALL`, the prototype's list) and the UI's blend picker
offered only `normal`.

The compositor works in premultiplied linear light in an `Rgba16Float` target
([ARCH › GPU pipeline](../ARCHITECTURE.md#gpu-pipeline)). For a source `s` and a
backdrop `b`, both premultiplied, with alphas `αs` and `αb`, the W3C compositing spec's
source-over composite of a separable mode with mixing function `B` is
`s·(1 − αb) + b·(1 − αs) + αs·αb·B(cs, cb)` (`cs`, `cb` the straight colours), with alpha
`αs + αb − αs·αb`. A GPU's fixed-function blend can compute some of these exactly from
the fragment and the target; the rest need the target's texels in the shader.

D13 recommended fixed-function states for additive, multiply, darken and lighten, a
destination copy for screen, overlay, difference and exclusion, and blending in linear
light except overlay, difference and exclusion, which users from OBS or Photoshop expect
in sRGB-encoded space. Working the formulas through moves two modes across that line:

- **Screen** is `s + b − s·b` premultiplied, which is `One, OneMinusSrc`: exact in one
  fixed-function draw at any alpha.
- **Darken and lighten** with `Min` or `Max` compare `s` with `b` directly. That is right
  only where both are opaque: a half-transparent black item over white darkens to black
  rather than to grey, and a transparent texel of the item blackens what is below it.
  The exact form, `min(s + b·(1 − αs), b + s·(1 − αb))`, needs both source-over orders
  at once, which one target can't hold.
- **Multiply** is exact in two fixed-function draws of the same triangles: the first
  writes `s·b + b·(1 − αs)` (`Dst, OneMinusSrcAlpha`) and keeps `αb` (`Zero, One`); the
  second adds `s·(1 − αb)` with the `αb` the first left (`OneMinusDstAlpha, One` on
  colour and alpha).

<a id="decision"></a>
## Decision
1. **The compositor draws all nine modes**, and the engine refuses none. A mode added to
   `BlendMode` later is `RenderError::Invalid` until the compositor draws it, and the
   engine keeps refusing it until then, as ADR-0023 §1 says.
2. **Colour space.** Every mode blends in linear light, except **overlay, difference and
   exclusion**, which mix sRGB-encoded values. Their `cs` and `cb` are clamped to
   [0, 1], encoded, mixed, and the mix is decoded; the composite around it stays linear
   and premultiplied. Brighter-than-white values are clamped for these three only. The
   other modes keep them.
3. **Fixed-function modes** (one pipeline per draw, no extra pass):
   - `normal`: `One, OneMinusSrcAlpha` on colour and alpha, as before;
   - `additive`: `One, One` on colour, source-over alpha (so alpha stays in [0, 1]);
   - `screen`: `One, OneMinusSrc` on colour, source-over alpha;
   - `multiply`: the two draws above.
4. **Backdrop modes**: `darken`, `lighten`, `overlay`, `difference` and `exclusion`.
   Before such an item draws, the render pass ends, the target's texels under it (its
   drawn polygon's bounding box, a texel of margin, clipped to the target) are copied
   into a backdrop texture, and a new pass draws it: its fragment loads the backdrop at
   its own pixel, computes the W3C composite above, and writes the result with blending
   off. Items after it draw in the same new pass until the next backdrop item. The
   backdrop is one `Rgba16Float` texture per compositor that only grows, shared by the
   canvas and every layer, each copying into and reading from the same texels.
5. **Layers are isolated.** A nested scene's items blend with its own layer (cleared to
   its transparent background), not with the scene it is placed in; the layer then
   blends as one picture by its own item's mode. So a transition flattens a nested scene
   into its items (ADR-0029 §4) only when its item and all of its items are `normal`.
   An item with effects (ADR-0065) blends its effects' output by its mode.
6. **Transitions.** A track draws with its endpoint's mode; a pair whose ends differ
   switches halfway, as flips do.
7. **The protocol** goes to 0.47: no wire change, but a client can tell from the version
   that the engine draws every mode, as 0.34 told it the engine draws 3D.

<a id="consequences"></a>
## Consequences
- **Easier:** the Inspector's blend picker offers every mode, and every mode is exact at
  any alpha, on translucent layers as on the opaque canvas. The pixel tests check each
  mode against a CPU reference of the formulas.
- **Harder:** each backdrop item ends a render pass and copies its bounding box. It
  costs a pass break and a copy of the texels it covers, bounded by the target, so it is
  bounded work under [R-RT-002](../RULES.md#R-RT-002), but a scene of many full-canvas
  backdrop items copies the canvas once each. Not measured on hardware yet.
- **Not decided here:** non-separable modes (hue, saturation, colour, luminosity), and
  pass-through groups that blend a nested scene's items with the parent.
- **Docs to change:** ARCHITECTURE's data model and GPU pipeline, the protocol, graph,
  core, render and UI READMEs, and UI-FEATURES' CV-15 row.
