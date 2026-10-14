<!-- @doc
id = "ADR-0022"
title = "Item transform and canvas coordinates"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0022: Item transform and canvas coordinates

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

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
(edits and their inverses, runtime state carries no `rev`, collection reads change only
with `rev`), [ADR-0032](0032-live-edits-and-undo.md) (live values for drags) and
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
- **Default placement (from 1.0):** `item.add` without a geometry places the item
  centred on the canvas, with `size` equal to the canvas and `fit_mode = fit`. Clients
  that want the source's native size send it explicitly. The graph never needs runtime
  sizes to apply an edit. Until 1.0 an `item.add` with neither `geometry` nor
  `transform` keeps its 0.3 meaning instead (native size, top-left at (0, 0)): it
  stores a legacy placement with the identity transform (decision 5), because a minor
  release can't change what an omitted field means ([R-API-001](../RULES.md#R-API-001)).

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
  output can't disagree. Golden tests and unit tests lock it. The same module holds the
  legacy evaluation of decision 5, which turns a 0.3 transform, a crop and a source size
  into the box it draws.
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
  round trip per frame. It also derives "% of native size", crop handle positions and
  the box of a legacy placement (decision 5) from them. Protocol 0.3 reports no source
  size at all, so the query and the event are needed for this and come with 0.4.

**5. Compatibility and migration.** The change of meaning follows R-API-001 and the
protocol README's versioning rules:
- **0.4, a minor release that adds and deprecates.** It adds:
  - the canvas, in `state.get`, and `canvas.set_size`;
  - `ItemGeometry`, with an optional `geometry` on `item.add` and a new edit
    `item.set_geometry`;
  - `ItemInfo.geometry` and `ItemInfo.placement`;
  - `source.sizes` and `source.size_changed`.

  It deprecates `ItemTransform`, `item.set_transform`, `item.add`'s `transform` and
  `ItemInfo.transform`. Their 0.3 meaning keeps working, unchanged, until 1.0.
- **Two placement forms.** Until 1.0 an item's stored placement is one of two forms,
  and only an edit changes it:
  - a **geometry** (decision 2), written by `item.add` with `geometry` or by
    `item.set_geometry`;
  - a **legacy placement**: the 0.3 `ItemTransform` exactly as written, by `item.add`
    with `transform`, by `item.add` with neither field (the identity transform), or by
    `item.set_transform`. A deprecated write is never refused for want of a size, so
    every batch that 0.3 accepts is still accepted. Sending both `transform` and
    `geometry` is `invalid_argument`.
- **A legacy placement keeps its 0.3 meaning.** The compositor evaluates it every frame
  against the source's *current* size, as 0.3 defines it: the cropped source, scaled by
  `scale`, with its top-left corner at `position`, rotated clockwise about that corner.
  A window capture that is resized therefore grows on the canvas, as in 0.3. Nothing
  converts it implicitly: not the first frame, not a size change. A source with no
  frame draws nothing, as in 0.3. Whatever consumes item boxes at runtime (the
  overlay, a Move transition) uses that frame's evaluated box.
- **Converting is an explicit edit.** A client turns a legacy placement into a
  geometry by sending `item.set_geometry`. It may build the geometry from the transform
  and the source's current size with the shared legacy evaluation (decision 3), which
  gives the box drawn now with `fit_mode = stretch`, so non-uniform scales stay exact.
  It is an ordinary edit: it bumps `rev`, sends `state.changed` and is one undo step.
  From then on the box is fixed, as for any geometry.
- **Inverses** (ADR-0013): `item.set_geometry` and `item.set_transform` each restore the
  previous placement in whichever form it had, so the inverse of a conversion is
  `item.set_transform` with the legacy transform. `item.remove`'s inverse is `item.add`
  with the field matching the removed item's form. The edit set stays closed under
  inversion.
- **Reads** (`state.get`, `item.list`):
  - `ItemInfo.placement` is `legacy` or `geometry`, so a 0.4 client knows which form is
    in use. It is a new field, which 0.3 clients ignore. Its two values are the
    complete set, and `legacy` goes in 1.0;
  - for a legacy item, `ItemInfo.transform` is the stored transform exactly as written,
    and `ItemInfo.geometry` is **absent**, because the box depends on the source's
    runtime size. A 0.4 UI draws the box itself with the legacy evaluation, from the
    transform and the size it gets from `source.sizes` and `source.size_changed`
    (decision 4);
  - for a geometry item, `ItemInfo.geometry` is the stored geometry, and the deprecated
    `ItemInfo.transform` is a compatibility view computed from the stored geometry
    alone: `position` is the box's top-left corner after rotation about `center`,
    `rotation_deg` is the box's rotation, and `scale` is (1, 1), as if the source were
    exactly the box's size. It is exact for the box's position and rotation, and states
    neither its size relative to the source, its fit mode nor its 3D fields. It depends
    on no runtime size, so it changes only with `rev`. A 0.3 client never sees a
    geometry item unless a 0.4 client made it. If it writes the view back with
    `item.set_transform`, the item becomes a legacy placement with exactly the 0.3
    meaning of what it wrote, which is native size at scale (1, 1).

  Every collection read is therefore a function of stored state: it changes only with
  an edit, which bumps `rev` and sends `state.changed`, as the protocol README and
  ADR-0013's `expected_rev` require.
- **Live values** ([ADR-0032](0032-live-edits-and-undo.md)): `item.set_geometry` is
  live-adjustable. It is a parameter-channel value that needs no new resources, so it
  meets ADR-0032's criterion and adds to its list rather than changing it. An item's
  placement is one live parameter, whichever form a value has: a live
  `item.set_geometry` or `item.set_transform` replaces the gesture's previous value for
  that item's placement, and for the gesture's duration the item draws the live value
  in its own form, a legacy one evaluated every frame. The committing batch, its
  `rev` bump and its undo step follow ADR-0032 unchanged, and the stored form changes
  only when that batch applies.
- **The next major release (1.0) removes the deprecated forms.** That follows `can_serve`: a 0.4 server
  still serves 0.3 clients, and 1.0 doesn't. Legacy placements go with them: a
  collection that still holds one is converted by 1.0's collection migration, which may
  change meaning because it is a major release, and which settles how to size a source
  that has no frame at load. Other breaking changes from the decision register may join
  the same major release, so the protocol breaks once rather than several times.
- **Snapshot and schema review:** both releases change `schema/protocol.snap` and
  `schema/examples.snap`, and those diffs are the API review
  ([R-TEST-002](../RULES.md#R-TEST-002)). The parity fixture's expected answers change
  with them.
- **Collections:** nothing is persisted yet, so no collection migration is needed if
  this lands first. If persistence lands first, the change ships a `schema_version`
  bump, a migration and a fixture test ([R-API-003](../RULES.md#R-API-003)); that
  migration stores every existing item as a legacy placement, which keeps its meaning.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - a rotated or resized window capture placed with a geometry stays in its box;
  - the UI's interaction model maps one to one onto the stored fields;
  - agents reason in canvas pixels and boxes, not in scale factors of a size they can't
    see;
  - a geometry item's overlay works before its source produces a first frame;
  - groups and mask scenes get a defined coordinate space, should the maintainer decide
    for them.
- **Harder:**
  - the protocol takes one deprecation cycle and then a major bump;
  - until 1.0 items have two placement forms, and the compositor evaluates legacy ones
    against the source's current size every frame;
  - a 0.3 client sees a geometry item only through a lossy compatibility transform;
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
    function and the legacy evaluation with unit tests, `ItemInfo.placement`, the
    deprecation notes on the old forms (including fixing the `ItemTransform` doc
    comment's "added fields with defaults" promise), and the snapshot updates;
  - `jstream-graph`: the canvas, both placement forms, their edits and inverses, the
    validation, and the property test extended to cover them;
  - `jstream-core`: runtime source sizes and the size event, live geometry values, and
    the per-frame evaluation of legacy placements for the render plan;
  - the compositor in M1b: 2D geometry, fit modes and crop, with golden tests;
  - 3D rendering in M3.

  Each changes its crate README, and ARCH where it says what is built.
