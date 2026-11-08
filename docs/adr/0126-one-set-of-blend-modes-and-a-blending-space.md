<!-- @doc
id = "ADR-0126"
title = "One set of blend modes, and a project setting for the space they mix in"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0126: One set of blend modes, and a project setting for the space they mix in

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0082](0082-blend-modes-drawn.md) §2 mixes most blend modes in linear light, and
overlay, difference and exclusion in sRGB-encoded values. A browser, CSS and most
design tools mix everything in sRGB-encoded values, so
[ADR-0105](0105-browser-matching-srgb-blend-modes.md) added `normal_srgb` and
`additive_srgb`. A design rebuilt natively from HTML could then keep its look: a 10% white
veil over an sRGB 30 grey reads about 53 in a browser and about 94 in linear light
(#1096).

That gave the Inspector two modes with the same name, "Normal" and "Normal
(browser)". Which one to use is a property of the whole project, not of one item: a
design made in a browser expects every layer to mix as a browser mixes. Picking it per
item was confusing. Each `_srgb` item also cost a pass break and a backdrop copy. The
maintainer's decision after the alpha.4 run (#1230): keep **one** list of modes, and add
a project-wide setting for the space they mix in, defaulting to the browser's.

<a id="decision"></a>
## Decision
1. **One list of modes.** `BlendMode` is ADR-0082's nine modes: normal, additive,
   multiply, screen, overlay, darken, lighten, difference and exclusion. `normal_srgb` and
   `additive_srgb` are gone from `BlendMode::ALL`, from the schema and from the UI. The
   wire still reads them, as aliases of `normal` and `additive`, so a client that sends
   them keeps working. The engine never sends them.
2. **A project setting, `blending`**, with two values:
   - `srgb`, "Browser-style (sRGB)", is the default.
   - `linear`, "Linear light", is ADR-0082 §2's.

   It is collection state, like the base frame rate
   ([ADR-0119](0119-base-frame-rate-option.md)). It is reported in `state.get`'s canvas
   as `blending`, and `canvas.set_blending { blending }` sets it. That is a `rev`, an
   undo step, and the collection file's `blending` field, which is left out at `srgb`.
   The UI's File › Video settings… offers it beside the resolution and the frame rate.
   Every scene of the project mixes in that space: the canvas, every nested scene's
   layer, and a transition's frames.
3. **Browser-style is drawn in an encoded working space.** While the setting is `srgb`,
   a composite target holds premultiplied sRGB-encoded colour instead of premultiplied
   linear light:
   - Each item's fragment converts its premultiplied linear colour `s` (alpha `αs`) to
     `E(s / αs)·αs`, where `E` is the sRGB encode, with values below 0 clamped and the
     curve extended above 1, so an opacity gain or a bright texel still brightens. A
     fragment with no alpha but some colour, pure added light, encodes its colour as it
     is.
   - The background is encoded the same way when the target is cleared.
   - **The fixed-function modes stay fixed-function.** `normal`, `additive`, `screen`
     and `multiply` use ADR-0082 §3's blend states unchanged, and those states, applied
     to encoded values, are exactly the browser's composite of each mode. So
     Browser-style adds no pass break and no copy for them.
   - **The backdrop modes** (ADR-0082 §4) read encoded colours and mix them directly,
     with no encode or decode around the mix. Overlay, difference and exclusion still
     clamp to [0, 1].
   - **After the last item,** one pass decodes the target in place: the target is
     copied into the backdrop texture, and a full-target draw writes `D(t / αt)·αt` back,
     where `D` is the sRGB decode. So a canvas, a layer or a snapshot always leaves the
     compositor as premultiplied linear light, and filters, outputs, mips, the resolve
     and every reader after them are unchanged.

   The cost is that one copy and one pass per composite target, not per item.
4. **Linear light** is ADR-0082 §2 unchanged: one pass, linear mixing, overlay,
   difference and exclusion mixed encoded.
5. **Migration (collection `schema_version` 12).** The old file has no `blending`, and
   its placements may name `normal_srgb` or `additive_srgb`:
   - Those names load as `normal` and `additive`, in placements and in any keyframe
     track that holds a blend.
   - The project is set to `srgb` when no placement names any mode other than those two
     (including a project with no placements at all).
   - Otherwise it is set to `linear`, so every scene that used ADR-0082's linear modes,
     which is every scene that left the default `normal`, keeps its look.
   - A project that mixed both kinds can't keep both looks. It keeps the linear items'
     look, and its former `_srgb` items now draw in linear light.

   A new collection is `srgb`.
6. **The protocol** goes to the next minor for `canvas.set_blending`, the canvas's
   `blending` field, and the `BlendMode` change. The MCP tool's schema gains the action,
   and its guidance says when to choose each space: Browser-style to match a design made
   in a browser or a design tool, and Linear light for physically even light, such as
   glows and fades of camera footage.
7. **Supersedes** ADR-0105, and **amends** ADR-0082 §2: the colour space is the
   project's, and ADR-0082 §2 describes `linear`.

<a id="consequences"></a>
## Consequences
- **Easier:** One "Normal" in the blend picker. A design rebuilt from HTML matches the
  browser in every mode, not only normal and additive, with no per-item choice. The pixel
  tests check every mode in both spaces against a CPU reference.
- **Cheaper than ADR-0105:** a browser-style `normal` item no longer breaks the pass or
  copies its backdrop. The decode pass is a fixed cost per scene, and the render bench
  measures it.
- **Harder:** precision. Encoded values are stored in the `Rgba16Float` target, whose
  10-bit mantissa covers [0, 1] in steps finer than 8-bit output, so the round trip adds
  no visible banding. A nested scene's layer is decoded and then re-encoded when placed,
  which is lossless within the same precision.
- **Look changes:**
  - A project that used both `_srgb` and linear modes changes look on its `_srgb` items
    (decision 5).
  - A new project's scenes look as they do in a browser. A soft veil is weaker, and a
    crossfade passes through darker mid-tones than it did in linear light.
  - Snapshot-based expectations made against the old default in agent guidance need the
    new default's numbers.
- **Older clients:** they send `normal_srgb` and `additive_srgb` and get `normal` and
  `additive`, whose look then depends on the project's space. A client older than this
  minor ignores the canvas's `blending` field, so it can't show it.
- **Docs to change:**
  - ARCHITECTURE's data model and GPU pipeline;
  - the protocol, graph, core, render, control and UI READMEs;
  - the CLI's usage and the MCP guidance;
  - UI-FEATURES' CV-15 and HD-11 rows;
  - the getting-started guide.
