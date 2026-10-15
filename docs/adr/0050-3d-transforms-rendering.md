<!-- @doc
id = "ADR-0050"
title = "Drawing 3D item transforms"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0050: Drawing 3D item transforms

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0022](0022-item-transform-and-canvas.md) defines what an item's 3D fields mean:
`tilt_deg`, `turn_deg`, `depth_px` and a per-item `perspective_px`, one placement formula
shared by the engine and the UI, items drawn in stacking order with no depth buffer, and
"a point with z ≥ p is behind the viewer and is clipped". It defers drawing them to M3
([ROADMAP](../../ROADMAP.md#m3)), and until then the engine rejects non-neutral values.

Building the compositor side leaves questions the formula doesn't answer:
- **Texture interpolation.** The 2D compositor draws a quad with linear texture
  coordinates. Under perspective that is wrong: the two triangles of a turned quad bend
  the picture along their shared diagonal.
- **The near plane.** ADR-0022's `project()` returns four corners, any of which may be
  behind the viewer. An item tilted toward the viewer, partly behind it, has a visible part
  that four corners can't describe.
- **Sampling.** A tilted item is minified much more in one direction than the other. The
  compositor samples one mip level with bilinear filtering today.
- **Edges.** Slanted edges alias without antialiasing. The compositor has no MSAA.
- **Depth sorting.** Whether items that pass through each other should be sorted or split.
- **Filters.** Where source filters, the reserved per-placement `effects[]`, opacity and
  blend sit relative to the 3D placement ([ARCH › Data model](../ARCHITECTURE.md#data-model)).

Spike S5 ([`spikes/s5-3d`](../../spikes/s5-3d/README.md)) drew each case on the GPU and
compared it with a CPU reference that averages 8×8 or 16×16 point samples per pixel, each
mapped back into the item's plane. Its run on lavapipe (llvmpipe, LLVM 20.1.2, Vulkan), a
960×540 canvas and a 1024×1024 source, in sRGB 8-bit steps per channel:

| Scenario | Variant | Pixels compared | Mean | p99 |
|---|---|---|---|---|
| A, item turned 50°, p = 700 | `w` per vertex | interior | 2.28 | 17 |
| | same corners, `w = 1` (affine) | interior | 55.52 | 179 |
| B, top edge behind the viewer | clipped in the local frame | all | 0.43 | 9 |
| C, floor tilted 75° | no mips | interior | 4.80 | 69 |
| | trilinear mips | interior | 3.22 | 49 |
| | trilinear mips, 16× anisotropic | interior | 1.16 | 17 |
| D, small rotated and turned item | aliased | edge pixels | 36.13 | 135 |
| | 4× MSAA | edge pixels | 13.31 | 67 |
| | analytic coverage | edge pixels | 1.94 | 16 |
| E, two items through each other | stacking order | all | 0.48 | 11 |

ADR-0022's four corners can't draw B at all (two are behind the viewer); the clipped
polygon has four vertices. Building one item's polygon, near clip, one-pixel expansion and
edge lines costs about 0.3 µs on the CPU. A software adapter says nothing about GPU time;
the per-frame cost belongs to the render benchmarks against the [R-RT-005](../RULES.md#R-RT-005) budgets.

<a id="decision"></a>
## Decision
This extends ADR-0022 and changes none of its meaning: the point formula, painter's order
and per-item perspective stay as written.

**1. Homogeneous placement.** The shared placement module in `jstream-protocol` gains a
function that returns the **drawn polygon** of an item: its content rectangle, clipped to
the box (as for `fill` today), then clipped by the near plane (decision 2), as up to five
vertices. Each vertex carries its canvas point, its local point, its texture coordinate and
`w = (p − z) / p` (1 without perspective).
- The compositor emits clip position `(ndc · w, 0, w)`, so the GPU interpolates texture
  coordinates perspective-correctly. With `w = 1` this is exactly today's 2D path.
- Both clips happen in the box's local frame, where texture coordinates are linear, so
  they are exact. The engine and the UI draw from the same polygon.

**2. The near plane is `w = 1/256`.** ADR-0022's "behind the viewer is clipped" means: the
part of an item with `w < 1/256` (closer to the viewer than `p / 256`, so magnified more
than 256 times) is not drawn, and the rest is. An item with nothing left draws nothing. So
does an item seen edge-on (zero projected area). `project()` keeps its meaning for corner
handles; an overlay outline uses the box's polygon clipped the same way.

**3. The inverse map.** The same module gains `unplace`: a canvas point to the local point
it shows, or none when the item is edge-on or the point is behind the viewer. Hit tests,
the overlay and the UI's handles use it, so they agree with what is drawn.

**4. No depth sorting.** Items draw in stacking order whatever their `z`, as ADR-0022
says. Items that pass through each other are neither split nor sorted: the upper one
covers the lower one wherever they overlap.
- Each item has its own perspective and its own viewer, so `z` values of two items aren't
  comparable, and a sort would have no single camera to sort for.
- A shared 3D space for a group (CSS's `preserve-3d`) would be a new scene-level field and
  a new ADR. It is not part of M3.
- **Nested scenes and groups flatten.** A scene source is a 2D layer: its items' 3D is drawn
  into it, and the layer is then placed like any source, in 3D or not.

**5. Sampling.** An item whose tilt or turn is non-neutral samples with trilinear mips and
16× anisotropic filtering, clamped to what the adapter offers.
- Mips of a live source are rebuilt on the GPU each frame it changes, once per source and
  frame, however many 3D placements use it. A source with no 3D placement gets no mips.
- Mips for strongly minified 2D items are a separate compositor choice and need no ADR.

**6. Edges use analytic coverage, not MSAA.** The fragment multiplies its colour by its
coverage, `clamp(d + 0.5, 0, 1)`, where `d` is the signed distance in pixels to the nearest
edge of the projected polygon. The polygon is drawn pushed out by one pixel, and each new
vertex is mapped back into the plane (decision 3) so its `w` and texture coordinate stay
exact.
- It beat 4× MSAA by a factor of seven on edge pixels in the spike, and costs no
  multisampled FP16 target (four times the working target's memory).
- Every item with non-neutral 3D uses it. Using it for rotated 2D items too is a
  compositor choice made with its golden-image changes, and needs no ADR.

**7. Filters, effects, opacity and blend.** In order, for one item:
1. the source's frame, through the source's video filters, in source pixels (a source's
   filters apply wherever it is placed, so they know nothing of any one placement);
2. crop, then fit into the box, then clip to the box, in the box's local frame;
3. the per-placement `effects[]`, reserved and empty in v1, in the box's local frame,
   before projection, so an effect turns with its item;
4. the 3D placement and the near clip (decisions 1 and 2), and sampling (decision 5);
5. coverage (decision 6) times `opacity`, then `blend`, in stacking order;
6. the master and output chains, on the finished mix.

No filter runs in screen space per item.

**8. Lifting the rejection.** The compositor PR that draws decisions 1 to 7 also makes the
engine accept non-neutral `tilt_deg`, `turn_deg`, `depth_px` and `perspective_px`, which
ADR-0022 says is not a breaking change. Its golden tests cover the spike's scenarios A to E.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - 3D is one more vertex attribute and a fragment term on the existing compositor: no
    depth buffer, no new pass, no new render target;
  - the UI's overlay, hit tests and the output all come from one shared polygon and one
    inverse map, so they can't disagree;
  - items partly behind the viewer draw their visible part instead of vanishing.
- **Harder:**
  - a source placed in 3D costs a mip rebuild per changed frame;
  - analytic coverage can leave a faint seam where two items' edges exactly meet, as
    any per-item antialiasing does; MSAA would not, at four times the target memory;
  - items that pass through each other look wrong in a way a depth buffer would fix, by
    design.
- **Not decided here:**
  - how a Move transition interpolates between an absent and a present `perspective_px`
    ([ADR-0029](0029-transitions.md) interpolates perspective linearly and doesn't cover
    absence);
  - a shared 3D space for groups;
  - GPU time per 3D item, which the render benchmarks measure.
- **Follow-up implementation PR** (this ADR changes no workspace code):
  - `jstream-protocol`: the drawn polygon and `unplace` beside the existing placement
    functions, with unit tests against the spike's checks;
  - `jstream-render`: the `w` vertex attribute, per-source mips and anisotropic sampling,
    analytic coverage, and golden tests for scenarios A to E;
  - `jstream-graph` or `jstream-core`, wherever the rejection lives: accept the 3D fields;
  - ARCH › GPU pipeline drops "3D" from what is still outside the crate.
