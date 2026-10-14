<!-- @doc
id = "ADR-0022"
title = "Item transform and canvas coordinates"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0022: Item transform and canvas coordinates

<a id="context"></a>
## Context
M1b builds "place, move, resize, rotate and crop scene items, with golden tests"
([ROADMAP](../../ROADMAP.md#m1)). Golden tests freeze whatever the transform means, so the
meaning has to be settled first.

**Today.** Protocol 0.3 has `ItemTransform { position, scale, rotation_deg }`:
- `position` is the item's top-left corner, in pixels of "the scene's canvas";
- `scale` is relative to the source's native size;
- rotation turns clockwise about the top-left corner.

`Crop` cuts whole **source** pixels off each edge. The doc comment promises the full 3D
transform "as added fields with defaults". However, scenes have no size, the collection
has no canvas, and `VideoMix` has a `resolution` but no space to scale from
([ARCH › Data model](../ARCHITECTURE.md#data-model)). No query or event reports a
source's current size, and window captures change size at runtime (`CaptureStream::latest`:
"Its size can change between frames").

**What the UI needs.** The chosen UI prototype (`design/ui-prototype/`, direction D) and
its decision register (entries D1, D12 and D37) assume the following:
- item boxes are `x, y, w, h` in canvas pixels on a fixed 1920×1080 canvas;
- rotation turns about the item's **centre**;
- resizing keeps the aspect ratio, and crop handles cut the source;
- the Inspector shows width and height in pixels, and as a percentage of the source's
  own size;
- each item has tilt, turn, depth and its **own perspective**, and items always draw
  in stacking order.

The canvas overlay (handles, snapping, marquee hits) is drawn from item boxes, every
frame while dragging.

**The forces:**
- With a top-left pivot and scale-based size, every UI rotation must also rewrite
  `position`, and that needs the source's current size. A window capture that changes
  size then grows on the canvas, and if it is rotated it also appears to swing around
  its corner.
- A centre pivot can't be expressed exactly with a top-left pivot unless the native
  size is known.
- 3D rotation needs a defined pivot too.
- Changing what an existing field means is a **major** change under
  [R-API-001](../RULES.md#R-API-001). The protocol README applies that rule at 0.x
  exactly as after 1.0. A major change needs an ADR
  ([R-ARCH-004](../RULES.md#R-ARCH-004)) and one minor release of deprecation first.

This decision stays within accepted ADRs. It builds on [ADR-0013](0013-command-model.md)
(edits and their inverses, runtime state carries no `rev`) and
[ADR-0001](0001-library-first-engine.md) (protocol first). It leaves these questions
open:
- whether opacity and blend belong to the item or the source (register D2, for the
  maintainer);
- whether groups are scenes (D4, for the maintainer);
- how Move transitions interpolate (D25).

<a id="decision"></a>
## Decision

**1. Canvas.**
- The collection has one `canvas { width, height }`, in whole pixels, with a default of
  1920×1080. It is every scene's coordinate space: x points right, y points down, and
  (0, 0) is the top-left corner of the canvas.
- A new edit, `canvas.set_size { width, height }`, changes it. Its inverse is
  `canvas.set_size` with the previous size. Zero, or a size above the engine's maximum,
  is rejected with `invalid_argument`. The maximum is at least 7680×4320 and is set by
  the implementation.
- Changing the canvas size moves and resizes nothing: items keep their canvas-pixel
  geometry. A client that wants to rescale sends a batch.
- `state.get` reports the canvas.
- A `scene` source's native size is the canvas size.
- A `VideoMix` renders the canvas scaled to its `resolution`. If the two aspect ratios
  differ, the canvas is fitted inside the mix resolution with black bars, never
  stretched.

**2. Item geometry.** The item's 2D-plus-3D placement is a new type, `ItemGeometry`, with
these wire fields (all numbers are `Finite32`):

| Field | Meaning | Default |
|---|---|---|
| `center` | Centre of the item's box, in canvas pixels | — (required) |
| `size` | `{ width, height }` of the box, in canvas pixels, each > 0 | — (required) |
| `fit_mode` | How the cropped source fills the box: `stretch`, `fit` or `fill` | `fit` |
| `rotation_deg` | Clockwise rotation in the canvas plane, about `center` | 0 |
| `tilt_deg` | Rotation about the box's horizontal axis through `center`; positive moves the top edge away from the viewer | 0 |
| `turn_deg` | Rotation about the box's vertical axis through `center`; positive moves the right edge away from the viewer | 0 |
| `depth_px` | Offset along the viewing axis; positive is toward the viewer | 0 |
| `perspective_px` | Distance from the viewer to the canvas plane, for this item only; absent means no perspective (orthographic); > 0 when present | absent |

- **The box is the item.** `center`, `size` and `rotation_deg` alone define the box, so
  selection handles, snapping and hit tests need no source size. A box always has the
  aspect ratio the user gave it, and a source that changes size stays inside it (like
  OBS's bounding box).
- **Fit modes** map the cropped source rectangle into the box:
  - `stretch` fills the box exactly, ignoring aspect ratio;
  - `fit` uses the largest rectangle of the source's aspect ratio that fits inside the
    box, centred, and leaves the rest transparent;
  - `fill` uses the smallest such rectangle that covers the box, centred, and clips it
    to the box.

  The three values are the complete set. A new value is a string-enum addition, which
  needs protocol open question 8 settled first.
- **Crop is unchanged:** `Crop { left, top, right, bottom }` in whole pixels of the
  source's own current frame, cut before fitting. A crop that leaves nothing draws
  nothing. Crop stays in source pixels, so it doesn't change when the box is resized,
  and the UI converts for its handles using the source's size (decision 4).
- **No flip** for now. Flips can come later as additive boolean fields defaulting to
  `false`. A negative `size` is rejected, not read as a flip.
- **Default placement:** `item.add` without a geometry places the item centred on the
  canvas, with `size` equal to the canvas and `fit_mode = fit`. Clients that want the
  source's native size send it explicitly. The graph never needs runtime sizes to apply
  an edit.

**3. The formula, defined once.** A point (u, v) in the box's local frame (origin at the
box centre, y down, z toward the viewer) is placed as follows:
1. Rotate about z by `rotation_deg`, then about y by `turn_deg`, then about x by
   `tilt_deg`. The signs follow CSS's `rotateZ`, `rotateY` and `rotateX`, which the
   prototype draws with.
2. Add `depth_px` to z.
3. If `perspective_px` = p is present, scale x and y by p / (p − z). A point with
   z ≥ p is behind the viewer and is clipped.
4. Add `center`.

So perspective is per item and centred on the item, with no shared scene camera.

- **Painter's order:** items draw in stacking order (bottom first), whatever their z.
  There is **no depth buffer**, so `depth_px` only changes the projected size and
  position.
- **One shared implementation:** the formula is a pure function in `jstream-protocol`,
  which is layer 1 and WASM-safe. It gives the box's projected corner quad (needing no
  source size) and the content rectangle within the box for a given source size and
  crop. The engine's compositor and the web UI both use it, so the overlay and the
  output can't disagree. Golden tests and unit tests lock it.
- **3D is defined now and built in M3.** Until the compositor draws them, the engine
  rejects non-neutral `tilt_deg`, `turn_deg`, `depth_px` and `perspective_px` with
  `invalid_argument`. Stored state never claims something that isn't drawn, and
  accepting those values later is not a breaking change.

**4. Source sizes are runtime state.**
- A source's current frame size is not collection state, so it carries no `rev` (like
  `output.status`, per ADR-0013).
- New query `source.sizes`: every source's current `size { width, height }`. The size
  is absent while the source has no video frame (not started, audio-only, or failed).
- New event `source.size_changed { source_id, size? }`: sent when a source's size
  changes, including when it becomes known or unknown.
- From these and decision 3, a client draws the overlay locally while dragging, with no
  round trip per frame. It also derives "% of native size" and crop handle positions
  from them.

**5. Compatibility and migration.** The change of meaning follows R-API-001 and the
protocol README's versioning rules:
- **0.4, a minor release that adds and deprecates.** It adds:
  - the canvas, in `state.get`, and `canvas.set_size`;
  - `ItemGeometry`, with an optional `geometry` on `item.add` and a new edit
    `item.set_geometry` (its inverse: `item.set_geometry` with the previous value);
  - `ItemInfo.geometry`;
  - `source.sizes` and `source.size_changed`.

  It deprecates `ItemTransform`, `item.set_transform`, `item.add`'s `transform` and
  `ItemInfo.transform`. Their old meaning keeps working through a shim in the engine,
  which knows source sizes:
  - a deprecated write is converted to the new geometry using the source's current
    size, and is rejected with `invalid_argument` while that size is unknown;
  - `ItemInfo.transform` is computed from the geometry and the current size. While the
    size is unknown, it is computed as if the cropped source were exactly the box's
    size (scale 1);
  - sending both `transform` and `geometry` is `invalid_argument`.
- **The next major release (1.0) removes them.** That follows `can_serve`: a 0.4 server
  still serves 0.3 clients, and 1.0 doesn't. Other breaking changes from the decision
  register may join the same major release, so the protocol breaks once rather than
  several times.
- **Snapshot and schema review:** both releases change `schema/protocol.snap` and
  `schema/examples.snap`, and those diffs are the API review
  ([R-TEST-002](../RULES.md#R-TEST-002)). The parity fixture's expected answers change
  with them.
- **Collections:** nothing is persisted yet, so no collection migration is needed if
  this lands first. If persistence lands first, the change ships a `schema_version`
  bump, a migration and a fixture test ([R-API-003](../RULES.md#R-API-003)).

<a id="consequences"></a>
## Consequences
- **Easier:**
  - a rotated or resized window capture stays in its box;
  - the UI's interaction model maps one to one onto the stored fields;
  - agents reason in canvas pixels and boxes, not in scale factors of a size they can't
    see;
  - the overlay works before a source produces its first frame;
  - groups and mask scenes get a defined coordinate space, should the maintainer decide
    for them.
- **Harder:**
  - the protocol takes one deprecation cycle and then a major bump;
  - the engine carries a conversion shim for one minor release;
  - `fill` needs clipping to the box in the compositor;
  - every client depends on the shared formula, so changing it is a protocol change.
- **Not decided here:**
  - opacity and blend on the item or the source (D2);
  - whether groups are scenes (D4);
  - how Move interpolates geometry (D25);
  - an explicit anchor field. It can come later as an optional field defaulting to the
    centre.
- **Follow-up implementation PR** (this ADR changes no code):
  - `jstream-protocol` 0.4: the new types, edits, query and event, the shared formula
    function with unit tests, the deprecation notes on the old forms (including fixing
    the `ItemTransform` doc comment's "added fields with defaults" promise), and the
    snapshot updates;
  - `jstream-graph`: the canvas, the geometry, their edits and inverses, the
    validation, and the property test extended to cover them;
  - `jstream-core`: the shim, runtime source sizes and the size event;
  - the compositor in M1b: 2D geometry, fit modes and crop, with golden tests;
  - 3D rendering in M3.

  Each changes its crate README, and ARCH where it says what is built.
