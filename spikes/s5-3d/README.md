<!-- @doc
id = "SPIKE-S5"
title = "Spike S5: 3D transforms"
kind = "guide"
status = "draft"
review_every = "180d"
-->

# Spike S5: 3D transforms

Throwaway code for item 3D1 (full 3D transforms, [ROADMAP M3](../../ROADMAP.md#m3)). It
checks that the one-scene compositor can draw the 3D fields
[ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md) already defines (tilt, turn,
depth and per-item perspective) with one textured polygon per item, and measures the
choices [ADR-0050](../../docs/adr/0050-3d-transforms-rendering.md) makes.

The crate is not a workspace member. It has its own `[workspace]` table and its own
`Cargo.lock`. It depends on `jstream-protocol` by path, so its placement is checked
against the shared formula, and it changes nothing in the workspace.

Results don't go in this file. The run behind ADR-0050 is recorded in that ADR.

<a id="questions"></a>
## What it measures
<!-- @section
-->

Every scenario is drawn twice: by the GPU path under test, and by a CPU reference that
averages an N×N grid of point samples per pixel (N = 8 or 16), each mapped back into the
item's plane and sampled bilinearly from the full-resolution texture. The errors are per
channel, in sRGB 8-bit steps.

- **A, perspective-correct texturing.** A turned item drawn with `w = (p − z) / p` per
  vertex (the GPU interpolates perspective-correctly), against the same projected corners
  with `w = 1` (what the 2D compositor would do with ADR-0022's projected quad).
- **B, the near plane.** An item that reaches behind the viewer. ADR-0022's four-corner
  projection has corners behind the viewer and can't draw it; the spike clips the content
  rectangle in the box's local frame at `w ≥ 1/256` and draws the polygon left.
- **C, minification.** A floor tilted 75°, about four texels per pixel one way and one the
  other. No mips, trilinear mips, and trilinear mips with 16× anisotropic filtering.
- **D, edges.** A small rotated and turned item. Aliased, 4× MSAA, and analytic coverage
  (the fragment's signed distance to each projected edge, with the polygon drawn pushed out
  by one pixel and its new vertices mapped back into the plane).
- **E, painter's order.** Two items through each other, drawn in stacking order.
- **CPU cost** of building one item's polygon, near clip, expansion and edge lines.

<a id="running"></a>
## Running it
<!-- @section
-->

```text
cd spikes/s5-3d
cargo test --release          # placement matches jstream-protocol; clip and inverse map
cargo run --release           # software adapter (lavapipe, WARP); writes out/
cargo run --release -- --hardware --out out-hw
```

`out/` gets each render, the references, amplified difference images and `results.md`.
The texture is `Rgba8UnormSrgb` with mips built on the CPU, and the target is
`Rgba8UnormSrgb`; the compositor's FP16 working target would change the numbers by a
fraction of a step.
