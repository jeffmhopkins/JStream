<!-- @doc
id = "CRATE-jstream-render"
title = "jstream-render"
kind = "normative"
status = "draft"
review_every = "90d"
-->

# jstream-render

Design for the one-scene GPU compositor, written before the code. The crate does not
exist yet; this note is the contract the implementation follows. It becomes the crate
README when the code lands.

<a id="purpose"></a>
## Purpose
<!-- @section
-->

One scene, composited on the GPU with wgpu
([ADR-0004](../adr/0004-wgpu-with-native-interop.md)). This is the M1a
compositor in [ROADMAP › M1](../../ROADMAP.md#m1): a single canvas and a stack of
items, not the full mix graph. Layer 2, no `unsafe`
([ARCH › Crate map](../ARCHITECTURE.md#crate-map)).

**Why its own crate.** The crate map already places the device, the compositor and
snapshot readback here. `jstream-graph` is the same layer and stays free of GPU code,
so the two cannot depend on each other. `jstream-platform` holds the `unsafe` interop
and must not contain WGSL. `jstream-core` (layer 5) is what will build a render
snapshot and call this crate.

**What one scene means.**

- A canvas size (`jstream-media`'s `FrameSize`: both dimensions non-zero) and a
  straight-alpha sRGB clear colour.
- Items in stacking order, bottom first, matching `Scene::items`.
- Each item places one image with the **legacy 0.3 placement**
  ([ADR-0022](../adr/0022-item-transform-and-canvas.md) decision 5): crop in
  source pixels, then scale the remaining rectangle by `scale` relative to the
  source's current size, put its top-left at `position` in canvas pixels, and rotate
  clockwise about that corner. The source size is the image's size, which is runtime
  state, so the compositor evaluates it on every `render`.
- `visible == false`, a crop that leaves no pixels, or a scale of zero draws nothing.
- Opacity is a straight-alpha multiplier in `[0, 1]`, default 1. The graph does not
  store it yet; the snapshot carries it so `jstream-core` can pass it when it does
  ([ARCH › Data model](../ARCHITECTURE.md#data-model)). The only blend is
  premultiplied src-over. Other modes stay rejected by the engine until a later change
  draws them.
- Nested scenes are not walked here. The caller renders the inner scene and places
  that output image in the outer one.

**Where it sits on the existing APIs.**

- It does **not** open the media adapter. When platform interop works, the engine
  constructs the compositor on `GpuContext`'s device, queue and adapter
  ([jstream-platform › GPU frames](../../crates/jstream-platform/README.md#gpu-frames)), so
  capture, composite and encode share one device. `Compositor::open` is the other
  path: no interop (the fakes, CI, a headless machine). It opens a device itself,
  software adapter first, as ARCH principle 7 and that section's "when there is no
  interop" paragraph say.
- A source image is either RGBA8 bytes the compositor uploads, or a `GpuFrame` /
  `wgpu::Texture` already on this device. Sampled formats are `Rgba8UnormSrgb` and
  `Bgra8UnormSrgb` only, so the shader can treat every sample as linear sRGB. A
  `GpuFrame` keeps its `FrameSync`; `render` calls `stage` once per distinct hold
  before the submit that samples it. Staging stays the safe method on
  `jstream-platform`. This crate never calls wgpu-hal.
- The graph's `ItemTransform` and `Crop` are not named here. `Placement` and `Crop`
  are the same fields (top-left `position`, `scale`, clockwise `rotation_deg`; crop
  insets in source pixels), so the engine's mapping is field by field. The shared
  geometry formula ADR-0022 puts in `jstream-protocol` is not in the tree yet, and
  this crate will not invent a second one.
- Readback is an owned tightly packed RGBA8 buffer, straight alpha, sRGB transfer,
  BT.709 primaries. It is the snapshot path (`video.snapshot` later encodes it). It
  is not the encoder's NV12 texture.

**Not in this cut.** The render thread (`frame_loop.rs`), arc-swap snapshots, NV12 or
P010 conversion, encode-target export and the D3D12 `PRESENT` transition, WGSL effect
packs, fit modes, 3D tilt/turn/depth/perspective, transitions, the master chain and
the privacy stage. ADR-0022 keeps 3D and fit modes out until the protocol formula
exists; the GPU pipeline's encode steps stay in
[ARCH › GPU pipeline](../ARCHITECTURE.md#gpu-pipeline).

<a id="design"></a>
## Design
<!-- @section
-->

`render` does not wait on the control plane and does no file or network I/O
([R-RT-002](../RULES.md#R-RT-002)). There is no render thread in this crate
yet, so there is no lock-free hand-off to build ([R-RT-003](../RULES.md#R-RT-003)).
Calling `render` is the caller's tick. GPU calls may allocate inside the driver; this
crate does not allocate per item on the steady path.

### Passes

Working space is linear light in an `Rgba16Float` colour target, as ARCH's data model
requires. The output the caller can sample or read back is `Rgba8UnormSrgb`.

1. **Upload** (when the image is new, not per frame). Tightly packed straight RGBA8
   sRGB bytes go into an `Rgba8UnormSrgb` texture (`TEXTURE_BINDING | COPY_DST`). The
   bind group (texture + linear sampler) is created once and stored on `Image`.
2. **Composite.** One render pass clears the FP16 target to the background converted
   to premultiplied linear (sRGB transfer on RGB, alpha unchanged, then RGB multiplied
   by alpha). Each visible item is two triangles. The vertex buffer holds clip-space
   positions, UVs and opacity; the fragment shader samples, multiplies alpha by
   opacity and outputs premultiplied linear. Blend is `One, OneMinusSrcAlpha` on
   colour and alpha. No depth buffer: stacking order is draw order. No cull, so a
   negative scale still draws (it flips the quad). The viewport clips to the canvas.
3. **Resolve.** A second pass draws one fullscreen triangle into the sRGB target with
   blending off. It `textureLoad`s the FP16 texel, un-premultiplies (zero RGB where
   alpha is 0) and writes straight linear RGB. The sRGB format encodes RGB on store;
   alpha is stored as-is. The output usage is `RENDER_ATTACHMENT | COPY_SRC |
   TEXTURE_BINDING`, so a later nested-scene image or a snapshot can use it.
4. **Readback**, only when asked. `copy_texture_to_buffer` into a map-read buffer,
   rows padded to 256 bytes, then a map and a pack into tightly packed RGBA8. This
   waits on the GPU. The tick that feeds an encoder does not call it. Snapshots and
   tests do.

Both passes are one command buffer and one `queue.submit`. `FrameSync::stage` runs
before that submit, for every distinct hold the scene samples. The output texture is
the compositor's own, not a shared encode target, so nothing transitions it to
`PRESENT`.

### Resources kept across frames

| Resource | Lifetime |
|---|---|
| Device, queue, pipelines, sampler, layouts | Compositor |
| FP16 target, sRGB target, resolve bind group | Recreated when the canvas size changes |
| Vertex buffer | Grown when the scene needs more vertices, never shrunk; `queue.write_buffer` otherwise |
| Scratch vertex `Vec<u8>` | Cleared and refilled each frame; capacity kept |
| Staged-hold scratch | Cleared and refilled each frame; capacity kept |
| Per-image texture and bind group | `Image`, dropped with it |

A returned output texture is a clone of the cached sRGB target. The next `render`
overwrites those texels, including when the canvas size is unchanged. Readback bytes
are a separate owned copy.

### Placement math

Canvas coordinates: x right, y down, origin at the top left. For a source of size
`(sw, sh)` and crop `(left, top, right, bottom)`:

- `cw = sw - left - right`, `ch = sh - top - bottom`. If either is `<= 0`, skip.
- Destination size is `(cw * scale.x, ch * scale.y)`. If either component is 0, skip.
- UVs cover the cropped texels: `u0 = left/sw`, `u1 = (sw-right)/sw`, and the same for
  `v` with y down in the texture.
- The four corners start at the destination rectangle with that top-left at
  `position`, then rotate clockwise about that top-left by `rotation_deg`. In y-down
  coordinates that is `x' = px + dx·cos θ − dy·sin θ`, `y' = py + dx·sin θ + dy·cos θ`.
- Clip space is `x_ndc = x/canvas_w·2 − 1`, `y_ndc = 1 − y/canvas_h·2`.

Opacity outside `[0, 1]` and any non-finite placement, colour or opacity is
`RenderError::Invalid`, not a clamped draw.

### Device selection

`Compositor::open` builds an instance with
`InstanceDescriptor::new_without_display_handle` and the backends this crate enables:
Vulkan, DX12 and Metal (one `wgpu` pin, ADR-0004; features are additive,
[R-RS-010](../RULES.md#R-RS-010)). No GLES backend: lavapipe covers Linux
without an EGL stack.

Adapter order when `OpenOptions` asks for software (the test and headless default):

1. `force_fallback_adapter: true`, which is a CPU adapter when the backend has one
   (lavapipe, WARP).
2. Any adapter, so a machine with a GPU and no software rasterizer still runs.
3. `RenderError::NoAdapter` when the instance has none.

`Compositor::on_context` does not search. It uses the `GpuContext` it is given.

Logging goes through `tracing` (adapter name at open). No `println!`.

<a id="testing"></a>
## Testing
<!-- @section
-->

CI has no GPU ([R-TEST-004](../RULES.md#R-TEST-004)). The
[testing table](../RULES.md#testing) already names the two software adapters:
Linux lavapipe for a render smoke, Windows WARP as the golden-frame reference.
`cargo xtask golden` does not exist yet, and
[R-TEST-003](../RULES.md#R-TEST-003) says files under `tests/golden/` change
only through that command on WARP. This cut therefore does **not** add golden PNGs.
It renders offscreen and checks pixels in the crate tests, with a tolerance, so the
same tests run on lavapipe and on WARP.

**Why tolerance instead of a smoke-only Linux run.** Flat, opaque, channel-pure sRGB
colours (0 and 1) round-trip through the linear FP16 target exactly, and a 1:1
axis-aligned quad samples texel centres under a linear filter, so interior pixels
match across rasterizers. A small tolerance (a few codes) covers the sRGB encode of a
partial-alpha blend, where implementations round differently. Sampling stays off the
quad edge. That is a pixel check on both CI operating systems, which is stronger than
a Linux run that only proves the frame compiled, and it does not freeze a WARP image
the golden tooling cannot update.

**What is checked.** Background only; an opaque rect over a background (position and
stacking); crop; integer scale; 90° rotation about the top-left; opacity over an
opaque background (the linear blend, then the sRGB encode); a fully cropped item and
a hidden item draw nothing; readback size and row packing; a `FrameSync` hold is
staged once per submit and not staged when the item is hidden.

**Skipping.** `Compositor::open` returns `NoAdapter` when nothing can be created. The
test helper writes one line to stderr and returns, so a laptop without lavapipe or
WARP skips cleanly. When `CI` is set, the same miss panics: the workflow is
responsible for the software adapter, and a silent skip would make `test` green
without rendering.

**CI install (Linux `test` job only).** `mesa-vulkan-drivers` and `libvulkan1`, and
`VK_DRIVER_FILES` pointed at lavapipe's ICD so the job cannot pick another driver.
wgpu loads `libvulkan` at runtime; no new native crate and no `lint-deps` allowlist
change ([R-DEP-002](../RULES.md#R-DEP-002)). Windows already has WARP through
DX12; that job needs no package. macOS has no software adapter: Metal if the runner
has one, otherwise the skip. The `macos` job is not required. Editing the workflow is
a governance change because `.github/**` is protected; it adds a package so tests can
render, and it does not weaken a check.

**Not measured** ([R-RT-005](../RULES.md#R-RT-005)). No frame-time numbers.

<a id="open-questions"></a>
## Open questions
<!-- @section
-->

- **Geometry and fit modes.** ADR-0022's `ItemGeometry` (centre, size, `stretch` /
  `fit` / `fill`, rotation about the centre) is specified but not in
  `jstream-protocol` yet, and the ADR says that formula lives there so the UI and the
  compositor share it. This crate implements only the legacy evaluation. When the
  function exists, the snapshot should carry its projected quad (or the geometry plus
  the source size) instead of a second copy of the math. `fill` also needs a clip to
  the box, which this pipeline does not have.
- **3D.** Tilt, turn, depth and perspective are defined and rejected by the engine
  until M3. No fields for them here.
- **Encode target.** ARCH step 4 writes NV12/P010 straight into a shared texture and
  ends the submit in `PRESENT`. Software adapters often lack `TEXTURE_FORMAT_NV12`,
  so that pass cannot be the CI path. Proposed default: keep this RGBA output, and
  let a later pass sample it into the platform `encode_target` on the real device.
- **Capture formats.** WGC frames are not sRGB RGBA. A conversion into the sampled
  format has to land before `import` can take a capture `GpuFrame`. Who owns that
  pass, this crate or the platform crate, is open.
- **Opacity and blend ownership** was left open by ADR-0022 (decision-register D2).
  The snapshot puts opacity on the item because that is what ARCH's data model and
  the compositor consequences of
  [ADR-0023](../adr/0023-placements-references-and-clones.md) describe. Blend modes other than src-over
  are not represented.
- **Background.** The clear colour is a field of the snapshot. `Scene.background` is
  not in the graph yet.
- **Golden files.** When `cargo xtask golden` exists, WARP renders under
  `tests/golden/` should freeze the cases these tolerance checks cover. Until then
  the tolerance checks are the pixel oracle on both software adapters.
- **Render thread.** Per-frame work is bounded for a caller that reuses `Image`s, but
  the thread, the snapshot swap and the panic boundary
  ([R-RT-006](../RULES.md#R-RT-006)) belong to `jstream-core`.
- **Output stability.** Callers that need the sRGB texture after the next `render`
  must copy it. Whether `render` should copy into a caller-supplied texture (the
  encode target, a preview) is open; the first engine wiring can decide.
