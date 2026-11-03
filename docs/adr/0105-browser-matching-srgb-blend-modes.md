<!-- @doc
id = "ADR-0105"
title = "Blend modes that mix in sRGB-encoded values, to match a browser"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0105: Blend modes that mix in sRGB-encoded values, to match a browser

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0082](0082-blend-modes-drawn.md) §2 blends `normal` and `additive` in linear light.
That is physically right, but a browser, CSS and most design tools composite in
sRGB-encoded values. A soft overlay made in HTML (a vignette, a faint white veil, a
glow, a drop shadow) therefore looks different in JStream than in the page it was
designed in. A white veil at 10% over an sRGB 30 grey reads as about 53 in a browser and
about 95 in linear light, almost twice as bright (#1096). A layout reproduced natively
from a browser design can't look identical with only the linear modes, and changing
`normal` itself would change every existing scene.

<a id="decision"></a>
## Decision
1. **Two new modes**, `normal_srgb` and `additive_srgb`, appended to `BlendMode::ALL`.
   They are `normal` and `additive` with the colours mixed in sRGB-encoded values, as a
   browser does. The existing modes, and the default `normal`, are unchanged.
2. **The maths.** For a source `s` and a backdrop `b`, premultiplied, with alphas `αs`
   and `αb`: the straight colours are clamped to at least 0 and encoded (`cs`, `cb`), the
   result alpha is source-over, `α = αs + αb − αs·αb`, and the encoded colour is
   `(cs·αs + cb·αb·(1 − αs)) / α` for `normal_srgb` and `(cs·αs + cb·αb) / α` for
   `additive_srgb`. That colour is decoded and premultiplied by `α`. Where `α` is 0 the
   result is transparent. Brighter-than-white values are kept, as the linear modes keep
   them. The encoding is the sRGB curve extended above 1.
3. **Drawn as backdrop modes** (ADR-0082 §4): the mix needs the backdrop's straight
   colour, which fixed-function blending can't encode. They break the pass and copy the
   texels under the item like `darken`, with the same bounded cost.
4. **Everything else is ADR-0082's.** Layers stay isolated (§5), so a nested scene
   blends with these modes as one picture, and a transition never flattens it. Tracks
   and flips follow §6.
5. **The protocol** goes to 0.76 for the two new enum values. The CLI, the MCP tool's
   schema and the Inspector's blend picker list them; the picker names them "Normal
   (browser)" and "Additive (browser)".

<a id="consequences"></a>
## Consequences
- **Easier:** an HTML overlay rebuilt natively, or a page and a native item stacked
  together, can match the browser's look by choosing the `_srgb` mode. The pixel tests
  check both modes against a CPU reference, and that a faint veil keeps its browser
  strength.
- **Harder:** each `_srgb` item costs a pass break and a copy, where `normal` costs
  neither. A scene of many full-canvas `normal_srgb` items copies the canvas once each.
- **Not decided here:** an sRGB variant of the other linear modes (`multiply`, `screen`,
  `darken`, `lighten`), and a per-scene setting that makes every item blend in sRGB.
  Either can be added the same way if a design needs it.
- **Amends** ADR-0082 §2, which lists the modes that mix sRGB-encoded values.
- **Docs to change:** ARCHITECTURE's data model and GPU pipeline, the protocol, graph,
  core, render and UI READMEs, the CLI's usage, and UI-FEATURES' CV-15 row.
