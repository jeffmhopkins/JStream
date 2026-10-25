<!-- @doc
id = "CRATE-jstream-render"
title = "jstream-render"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-render

The one-scene GPU compositor. It owns a wgpu device, or uses the platform's, and draws
a single canvas.

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-render/src/**", "crates/jstream-render/Cargo.toml"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-render", "docs/ARCHITECTURE.md#gpu-pipeline", "docs/adr/0004-wgpu-with-native-interop.md", "docs/adr/0022-item-transform-and-canvas.md", "docs/adr/0073-sources-drawn-at-their-shown-size.md", "crates/jstream-platform/README.md#gpu-frames"]
-->

One scene, composited on the GPU with wgpu
([ADR-0004](../../docs/adr/0004-wgpu-with-native-interop.md)). This is the M1a
compositor in [ROADMAP › M1](../../ROADMAP.md#m1): a single canvas and a stack of
items, not the full mix graph. Layer 2, no `unsafe`
([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)).

**Why its own crate.** The crate map already places the device, the compositor and
snapshot readback here. `jstream-graph` is the same layer and stays free of GPU code,
so the two cannot depend on each other. `jstream-platform` holds the `unsafe` interop
and must not contain WGSL. `jstream-core` (layer 5) is what builds a render snapshot
and calls this crate.

**What one scene means.**

- A canvas size (`jstream-media`'s `FrameSize`: both dimensions non-zero) and a
  straight-alpha sRGB clear colour.
- Items in stacking order, bottom first, matching `Scene::items`.
- Each item places one image with one of
  [ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md)'s two placement forms:
  - a **geometry** (decisions 2 and 3): a box on the canvas. The image is cropped in
    source pixels, fitted into the box by its fit mode (`stretch`, `fit` or `fill`,
    which is clipped to the box), mirrored inside the box by `flip_h` and `flip_v`, and
    rotated clockwise about the box's centre;
  - a **legacy 0.3 placement** (decision 5): crop in source pixels, then scale the
    remaining rectangle by `scale` relative to the source's current size, put its
    top-left at `position` in canvas pixels, and rotate clockwise about that corner.

  Both depend on the image's size, which is runtime state, so the compositor evaluates
  the placement on every `render`. That size is the image's **extent**
  (`Image::extent`): its pixel size, unless the caller set another with
  `Image::set_extent` or `ResourceCache::set_extent`, for an image drawn with more
  pixels than the size it stands for, such as a text source drawn sharp for an
  enlarged item
  ([ADR-0073](../../docs/adr/0073-sources-drawn-at-their-shown-size.md)). Crops, a
  legacy scale and a fit mode's aspect ratio all count in the extent, and the texture
  is sampled across it.
- `visible == false`, a crop that leaves no pixels, or a scale of zero draws nothing.
- Opacity is a straight-alpha multiplier in `[0, 1]`, default 1: `jstream-core` passes
  an item's stored opacity, or during a transition its track's sampled one
  ([ARCH › Data model](../../docs/ARCHITECTURE.md#data-model)). The only blend is
  premultiplied src-over. Other modes stay rejected by the engine until a later change
  draws them.
- Nested scenes are not walked here. The caller composites the inner scene into a
  layer, an `Image` on this compositor, and places that image in the outer scene:
  `render_image` makes a new layer, and `render_into` redraws one the caller keeps
  (from `layer`). The depth bound and cycle handling are the caller's
  ([jstream-core › Snapshots](../jstream-core/README.md#design)).
- `ResourceCache` keeps what a caller draws every frame (uploads, imports of GPU
  frames, and layers), keyed by the caller's IDs, so a frame that shows what the last
  one showed allocates no texture. An import keeps its frame's `FrameSync` hold only
  for the frame that draws it, so the platform can reuse the texture afterwards. An image no frame references is freed only after the GPU has finished the
  last frame that used it, and the cache hands it back as `Reclaimed` so the caller
  picks the thread that drops it
  ([ARCH › Threads](../../docs/ARCHITECTURE.md#threading)).

- **Transitions** (`transition.rs`, [ADR-0029](../../docs/adr/0029-transitions.md)):
  a `Track` is one item's part in a transition: a `Pair` that interpolates between two
  `Endpoint`s (a geometry, a crop and an opacity), an `Appear` or a `Disappear` with
  its effect (`Fade`, `Scale` to 65 %, `Slide` to the nearest canvas edge, or `None`),
  each with its `Timing` (delay, duration and easing); `None` waits for the delay,
  then shows or hides the item at once. `Track::sample` gives the
  endpoint at a time since the start, or nothing once the item is invisible; it is a
  pure function, so the render thread and the control thread that retargets a running
  transition get the same answer. Angles take the shortest way round, discrete fields
  (fit mode, the flips, a perspective present at one end) switch halfway, and after easing the
  opacity is clamped to `[0, 1]`, the crop to the source, the size to `≥ 0` and the
  perspective to `≥ 1`. Pairing items and drawing the sampled endpoints are
  `jstream-core`'s.

**Where it sits on the existing APIs.**

- It does **not** open the media adapter. When platform interop works, the engine
  constructs the compositor on `GpuContext`'s device, queue and adapter
  ([jstream-platform › GPU frames](../jstream-platform/README.md#gpu-frames)), so
  capture, composite and encode share one device. `Compositor::open` is the other
  path: no interop (the fakes, CI, a headless machine). It opens a device itself,
  a hardware adapter first by default (`OpenOptions::hardware`), with the software
  adapter as the fallback, so ARCH principle 7's "runs with a software adapter" still
  holds on a machine with no GPU. `OpenOptions::headless` asks for software first.
- A source image is either straight-alpha sRGB RGBA8 bytes the compositor uploads, or
  a `GpuFrame` / `wgpu::Texture` already on this device. An upload is premultiplied in
  linear light and stored as `Rgba16Float`, so filtering cannot darken transparent
  edges ([Passes](#design)). An imported texture (`import`) must be `Rgba8UnormSrgb` or
  `Bgra8UnormSrgb`, so the sampler decodes it to linear, and is read as straight
  alpha: exact for an opaque capture, a fringe at partly transparent edges. A browser
  page's texture holds sRGB code values already multiplied by alpha (CEF's layout), so
  `import_premultiplied` takes `Rgba8Unorm` or `Bgra8Unorm` and reads it that way, with
  no fringe. `import_opaque` takes what `import` takes and reads every texel's alpha as
  1: for a captured frame, whose alpha means nothing, as the CPU capture path treats it
  (#451). A compositor opened `on_context` reports that context's `ContextKey`
  (`Compositor::context`), so a caller can tell a texture from that context from one
  it can't sample. A
  `GpuFrame` keeps its `FrameSync`; `render` calls `stage` once per distinct hold
  before the submit that samples it, holding the context's `SubmitLock` from the stage
  through that submit, and ends that submit by moving each staged texture to
  `PRESENT`, as `stage` requires. Staging stays the safe method on
  `jstream-platform`. This crate never calls wgpu-hal.
- An `Image` draws only in the `Compositor` that made it. It carries that
  compositor's identity, and `render` refuses an item whose image came from another
  one with `RenderError::Invalid` before any GPU work, instead of reaching wgpu's panic
  on a foreign bind group.
- The public scene types are `Placement` and `Crop`. `Placement::Geometry` holds the
  protocol's own `ItemGeometry`, and `Placement::Legacy` a `LegacyTransform` with the
  same fields as the protocol's `ItemTransform` (top-left `position`, `scale`,
  clockwise `rotation_deg`); `Crop` has the protocol's insets in source pixels. So the
  engine's mapping is field by field, and `place` evaluates both with the protocol's
  shared functions ([Placement math](#design)). A geometry with non-neutral 3D draws
  [ADR-0050](../../docs/adr/0050-3d-transforms-rendering.md)'s drawn polygon,
  perspective-correct, with antialiased edges, and sampled from a mip chain of its
  cropped source with 16× anisotropic filtering. The engine accepts any 3D it can draw
  from protocol 0.34.
- Readback is an owned tightly packed RGBA8 buffer, straight alpha, sRGB transfer,
  BT.709 primaries. `jstream-core` encodes it as the `video.snapshot` PNG. It
  is not the encoder's NV12 texture.
- `Nv12Converter` turns that sRGB output into NV12 for the encoders
  ([ARCH › GPU pipeline](../../docs/ARCHITECTURE.md#gpu-pipeline) step 4). The target
  is either the plane views of a native `NV12` texture (`Nv12Target::nv12_texture`,
  for the platform's `encode_target`) or `Nv12Planes`, an `R8Unorm` Y texture and an
  `Rg8Unorm` CbCr texture at half size, which every adapter can render to and which
  `Nv12Planes::read` copies back as packed NV12 for the CPU fallback. The colour is
  `jstream-media`'s `ColorInfo`: BT.709 only, limited range (the default) or full.
  `Nv12Converter::submit` runs a pass into a platform `GpuFrame` in a submit of its
  own on a compositor's queue, which is how `jstream-core`'s render thread feeds an
  encoder that takes GPU frames.
- **A lost device** (a driver reset after a hang, Windows' TDR, a removed GPU; #349).
  Every compositor sets wgpu's device-lost callback and uncaptured-error handler on its
  device, its own or the context's (wgpu keeps one of each per device, and the program
  stage opens one compositor on the context). `Compositor::lost` says why the device is
  gone, polling it once because wgpu reports the loss on a poll. Nothing on a lost
  device comes back: the caller opens a new compositor
  ([jstream-core › Design](../jstream-core/README.md#design) does). wgpu's default
  handler panics on every uncaptured error, and a lost device's invalid resources raise
  them on each later call, sometimes before wgpu reports the loss. So the handler keeps
  the first error instead (`Compositor::take_error`, logged as
  `uncaptured wgpu error` before a loss), and `render` fails with
  `RenderError::Uncaptured` when one is pending, before any readback.
  `Nv12Converter::bind` returns `RenderError::Uncaptured` too when its parameter
  buffer can't be mapped, as on a lost device, where wgpu's `create_buffer_init`
  would panic. `render` empties
  the slot, so an error that recurs every frame is logged once per frame until the caller
  gives up. The handler is per device: on the shared context an error that a browser or
  capture thread raised is taken by the program's next `render` and counts against the
  program's frames.

**Not in this cut.** The render thread and its snapshots are `jstream-core`'s
(`runtime/`), not this crate's, and so is the choice of encode target each tick. Also
not here: P010 conversion, BT.601 and BT.2020 matrices, encode-target allocation,
loading WGSL effect packs (`jstream-effects`; this crate only draws them), the video
filter chain runtime (`jstream-core`'s; the built-in filters' passes are here, see
[Filters](#filters)), the master chain and the privacy stage. The GPU pipeline's encode
steps stay in
[ARCH › GPU pipeline](../../docs/ARCHITECTURE.md#gpu-pipeline).

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-render/src/**"]
depends_on = ["docs/RULES.md#R-RT-002", "docs/adr/0004-wgpu-with-native-interop.md"]
-->

`render` does not wait on the control plane and does no file or network I/O
([R-RT-002](../../docs/RULES.md#R-RT-002)). This crate has no render thread: calling
`render` is the caller's tick, on `jstream-core`'s render thread. `ResourceCache`
returns what it frees as `Reclaimed`, so that thread can hand it to a collector
([R-RT-003](../../docs/RULES.md#R-RT-003)). The images this crate drops itself are a
multi-pass filter's scratch layers that a call replaces at a new size or no longer asks
for, and a video delay's held frames when its delay goes to 0, its input size changes or
its memory cap shrinks (**Filters**), so only when a filter's size or settings change; the
same goes for a filter draw's two small uniform buffers, which hold no texture. GPU
calls may allocate inside the driver; this crate does not allocate per item on the
steady path.

### Passes

Working space is linear light in an `Rgba16Float` colour target, as ARCH's data model
requires. The output the caller can sample or read back is `Rgba8UnormSrgb`.

1. **Upload** (when the image is new, not per frame). Tightly packed straight RGBA8
   sRGB bytes are converted on the CPU: a 256-entry table decodes each RGB byte with
   the sRGB transfer, RGB is multiplied by alpha, and each channel is rounded to
   binary16. The result goes into an `Rgba16Float` texture (`TEXTURE_BINDING |
   COPY_DST`) with `queue.write_texture`, which needs no row padding. The bind group
   (texture + linear sampler) is created once and stored on `Image`.

   **Why premultiply first.** The linear filter averages neighbouring texels. Averaging
   straight alpha mixes a transparent texel's RGB into its opaque neighbour, so a
   scaled, rotated or off-pixel edge darkens: half-way between opaque red and
   transparent black resolves to red about 188 at alpha 128 instead of 255. Premultiplied
   texels average correctly. `Rgba16Float` keeps the linear, premultiplied values
   without the banding 8 bits would give in the darks; it costs 8 bytes per texel
   instead of 4, and the conversion is CPU work at upload time only.

   **Imported textures** (`import`, `import_frame`, `import_premultiplied`,
   `import_opaque`) are not
   converted: a capture changes every frame while its `Image` lives, so a conversion
   would be a per-frame pass. A straight import is sampled as straight alpha and
   premultiplied after the filter. For an opaque texture, such as a captured frame,
   that is exact and costs nothing extra; a partly transparent one fringes at its
   edges. A premultiplied import (a browser page) is sampled raw, with no sRGB decode,
   so the filter averages premultiplied code values and transparent edges stay clean;
   the shader then divides by alpha, decodes sRGB and multiplies by alpha again.
   Averaging code values rather than linear light differs from an upload only where
   the filter mixes texels of different colour. An opaque import (a capture) is decoded
   by the sampler and its alpha replaced by 1, in the composite shader and in a
   filter's input alike.
2. **Mips** (ADR-0050 decision 5). Before the composite pass, each source a visible 3D
   item draws gets its mip chain rebuilt if its image changed: an `Rgba16Float` texture
   with every level down to 1×1, premultiplied linear. A chain is of the item's crop,
   not the whole source: level 0 copies the cropped texels one for one (a `Region`, the
   crop's whole texels carried as integers from the item's crop, never recovered from
   its texture coordinates; `vs_region` maps the pass onto it), converting an import as
   the composite shader would. So no coarser level averages a texel past the crop into
   the item, and clamping to the chain's edge keeps the anisotropic footprint inside the
   crop too (#641). The crop counts in the image's extent, so for an image drawn with
   more pixels than its extent (ADR-0073) it is scaled to texels and widened outwards to
   whole ones (`Region::in_texels`). The item's UVs and UV bounds are mapped into the
   chain's. An image
   keeps a chain per crop it is drawn with, the four most recently used. Each further
   level is one pass that samples the level above at its own texel centres, so the
   linear filter averages 2×2 texels. Each `Image` counts its changes (its clones share
   the count and the chains, as they share the texture), and a chain is
   rebuilt when the count moved since its last build: `rewrite_rgba8` marks an upload,
   `render_into` the layer it drew into, and `ResourceCache::import` an import it hands
   a new frame (one whose hold or info, its presentation time included, differs from
   the last one's, since a platform may reuse a hold for every frame it writes into a
   texture), not the same frame shown again. `Image::mark_changed` marks an import the
   caller keeps outside the cache, whose texture the platform rewrites. So a source
   drawn in 3D by several items or calls (two mixes, a nested scene) is rebuilt once per
   change, and not at all while it is unchanged. A 3D item samples its chain, which is
   always premultiplied linear (mode 0), with trilinear filtering and `anisotropy_clamp`
   16; wgpu clamps that to 1 on an adapter without anisotropic filtering. A source with
   no 3D placement gets no chain.
3. **Composite.** One render pass clears the FP16 target to the background converted
   to premultiplied linear (sRGB transfer on RGB, alpha unchanged, then RGB multiplied
   by alpha). Each visible item is a convex polygon drawn as a triangle fan: a 2D item
   is its quad (two triangles), a 3D item three to five vertices (up to three
   triangles). The vertex buffer holds clip-space positions, UVs, opacity, the image's
   alpha mode, ADR-0050's `w`, the item's five edge lines and its UV bounds (26
   `f32`s, 104 bytes).
   The vertex shader writes `(pos · w, 0, w)`, so the GPU interpolates UVs
   perspective-correctly; a 2D item has `w = 1`, the plain affine path. The fragment
   shader clamps its UV to the item's bounds (a 3D item's content before the one-pixel
   push, so a fringe pixel never samples past the crop or what `fill` clips; a 2D item's
   bounds never clamp) and samples at the clamped UV (a 3D item from its
   source's mip chain, step 2); for a straight import it multiplies RGB by the sampled
   alpha, for a premultiplied import it divides RGB by alpha (zero where alpha is
   0), decodes sRGB and multiplies by alpha, and for an opaque import (a capture) it
   reads alpha as 1; then it multiplies all four channels by
   opacity and by its edge coverage, `clamp(d + 0.5, 0, 1)` for the distance `d` in
   pixels to the nearest edge line (ADR-0050 decision 6), and outputs premultiplied
   linear. A 2D item's edge lines are never crossed, so its coverage is 1 and its edges
   are drawn as before. Blend is `One, OneMinusSrcAlpha` on colour and alpha. No depth buffer:
   stacking order is draw order. No cull, so a negative scale still draws (it flips
   the quad). The viewport clips to the canvas.
4. **Resolve.** A second pass draws one fullscreen triangle into the sRGB target with
   blending off. It `textureLoad`s the FP16 texel, un-premultiplies (zero RGB where
   alpha is 0) and writes straight linear RGB. The sRGB format encodes RGB on store;
   alpha is stored as-is. The output usage is `RENDER_ATTACHMENT | COPY_SRC |
   TEXTURE_BINDING`, so a snapshot or a later consumer can use it.

   **`render_image`** (a nested scene) stops after step 3. It composites into a new
   `Rgba16Float` texture (`RENDER_ATTACHMENT | TEXTURE_BINDING`) of the canvas size
   and returns it as an `Image` read as premultiplied, the same as an upload. So
   placing it filters exactly at partly transparent edges, and nothing is quantised
   to 8 bits between nesting levels. It is one submit, ordered before the submit of
   the outer scene that samples it. The texture belongs to the `Image`, not the
   compositor, so a later `render` cannot overwrite it. **`render_into`** is the same
   pass into a layer the caller already has (`layer` makes one; the `Image` keeps its
   render-target view, so a redraw creates no view either). The layer must be this
   compositor's, the canvas size, and not placed by the scene being drawn into it,
   or the call is `RenderError::Invalid` before any GPU work. Submits that placed the
   layer earlier are ordered before the redraw, so they keep the old contents.
   **`rewrite_rgba8`** refills an upload in place the same way (pass 1 into the
   existing texture).
5. **Readback**, only when asked. `copy_texture_to_buffer` into a map-read buffer,
   rows padded to 256 bytes, then a map and a pack into tightly packed RGBA8. This
   waits on the GPU. The tick that feeds an encoder does not call it. Snapshots and
   tests do.

6. **NV12** (`convert/`), when the caller records it. `Nv12Converter::bind` ties a
   source (`Rgba8UnormSrgb` or `Bgra8UnormSrgb`, the compositor's output) to a target
   once: it makes the bind group and a 16-byte uniform with the range constants, so
   `Nv12Pass::encode` allocates nothing per frame. Bind again when either texture is
   replaced. `encode` records two fullscreen passes. The Y pass `textureLoad`s each
   texel, which the sRGB view decodes to linear; multiplies RGB by alpha, because NV12
   has no alpha and a transparent canvas should encode as black; applies the sRGB
   transfer to get R'G'B'; and writes `Y' = 0.2126 R' + 0.7152 G' + 0.0722 B'`. The
   CbCr pass writes one texel per 2x2 block, `Cb = (B' − Y') / 1.8556` and
   `Cr = (R' − Y') / 1.5748`. Its chroma is **left-sited**, the H.264 default and the
   same as Windows capture's CPU path: a `[1 2 1]` tap across the block's left column
   and its neighbours on each side, averaged over the block's two rows, with the
   border texel repeated at the edges. Values are quantised as in ITU-T H.273, in
   8-bit codes: limited `16 + 219 Y'` and `128 + 224 C`, full `255 Y'` and
   `128 + 255 C`. The shader rounds to the code and writes `code / 255`, so the unorm
   store is exact everywhere. Odd sizes are `RenderError::Invalid`: NV12 needs even
   dimensions, and dropping a row or column is the caller's choice to make. The caller
   records the pass into its own encoder, after the composite, and stages a shared
   target's `FrameSync` before that submit. `Nv12Converter::submit` does that for a
   platform `GpuFrame`: it stages the frame, records the pass, transitions the frame's
   texture to `PRESENT` as the composite does with the frames it samples, and submits,
   holding the compositor's `SubmitLock` from the stage through the submit as `render`
   does.

The composite and resolve passes are one command buffer and one `queue.submit`. `FrameSync::stage` runs
before that submit, for every distinct hold the scene samples, and the command buffer
ends by transitioning each of those textures to `PRESENT` (on D3D12 the `COMMON` state
the platform's other APIs read a shared texture in), which `stage` requires of the
submit. `render_into` does the same. Staging comes after every other step that can
fail (targets, vertex buffer, readback buffer), so a refused canvas or readback stages
nothing. From the first stage through the submit, the compositor holds the queue's
`SubmitLock`. On a platform context that is the context's lock, which the platform's
other submitters on that queue take too, so none of their submits takes the staged
wait and signal. The lock is let go before the readback's wait. The compositor lets go of the holds after the submit, so it keeps no platform
texture busy between frames. The output texture is the
compositor's own, not a shared encode target, so nothing transitions it; an encode
target the NV12 pass writes is transitioned by `Nv12Converter::submit`.

### Resources kept across frames

| Resource | Lifetime |
|---|---|
| Device, queue, pipelines, sampler, layouts | Compositor |
| FP16 target, sRGB target, resolve bind group | Recreated when the canvas size changes |
| Vertex buffer | Grown when the scene needs more vertices, never shrunk; `queue.write_buffer` otherwise |
| Scratch vertex `Vec<u8>` | Cleared and refilled each frame; capacity kept |
| Staged-hold scratch | Refilled for each submit and cleared after it; capacity kept |
| Per-image texture and bind group | `Image`, shared by its clones and dropped with the last |
| Mip chains (one per crop, at most four), their level views, region uniform and bind groups | Made the first time the `Image` is drawn in 3D with that crop; shared by its clones and dropped with the last, or when four newer crops pushed it out. So a crop that changes every frame, or five or more crops of one source, makes and builds a chain per draw |
| `render_image` texture | The returned `Image`, allocated per call |
| `layer` texture and its render-target view | The returned `Image`; `render_into` reuses it |
| `Nv12Converter` pipelines and layout | The converter |
| `Nv12Pass` bind group, source view, uniform and plane views | The pass; made again by `bind` when the source or target is replaced |
| `Nv12Planes` textures | The planes value |
| A video delay filter's held frames | Its `VideoFilter`: reused as the ring turns; dropped when the input size changes or the delay is set to 0 ([Filters](#filters)) |
| A multi-pass filter's scratch layers | Its `VideoFilter`: reused by the next call at the same size; replaced for a new size, and dropped when a call asks for fewer ([Filters](#filters)) |
| A filter draw's uniform buffers | Its `VideoFilter`: rewritten by the next call's same draw; made again for another compositor or parameter size, and dropped when a call draws fewer times. They hold no texture ([Filters](#filters)) |
| `ResourceCache` entries | While a frame uses them; then retired until the GPU finishes the last frame that used them, and handed back as `Reclaimed` |

**`ResourceCache`.** A frame runs `begin_frame`, `upload`, `import` or `layer` for each
image it draws, its passes, then `end_frame`. `upload` keyed by an ID reuses the entry when the
caller's version and the size are unchanged, rewrites the same texture for a new
version, and makes a new texture (retiring the old one) for a new size; `layer` reuses
a layer of the same size. `end_frame` retires every entry the frame did not use,
registers `Queue::on_submitted_work_done` for the frame (the frame fence: it records
the newest finished frame in an atomic), polls the device once without blocking, and
returns the retired images whose last frame has finished. A steady frame allocates
only that callback; a frame that fails before `end_frame` leaves its retirees to the
next frame's fence, which covers it. `frame` names the frame being built, so work that
several draws of one frame share (a source's video chain) can run once in it.
`set_extent` places a key's image as if it were another size (ADR-0073); a reused entry
keeps the extent it had, so a caller that sets one sets it on every frame, and a new
texture starts at its own size.
`retire_filter` hands the cache a `VideoFilter` no frame runs any more: it may hold
textures of its own (a video delay's ring), so it waits for the same fence and comes
back in `Reclaimed` (`Reclaimed::filters` counts them) to be dropped with the images.

`import` keyed by an ID samples a `GpuFrame` with no copy (`import_premultiplied`
does the same for a premultiplied texture, and `import_opaque` for a capture, and each
replaces an entry the key imported another way, as a new import does). The entry's image carries
the frame's `FrameSync` hold, so each `render` that draws it stages it, until
`end_frame`, which drops every live entry's hold (as retiring does): a cached image
never keeps a platform texture from being reused. The import itself, texture view and
bind group, is kept: a frame in the texture the key showed recently swaps that
texture's import back in, so a platform ring of N textures costs N imports once, not
one per frame. At most eight such kept imports per key; a frame of a new size retires
the kept imports of the old size, which the platform has released. An upload or a
layer under the key replaces the import, which retires as any entry does.

A returned output texture is a clone of the cached sRGB target. The next `render`
overwrites those texels, including when the canvas size is unchanged. Readback bytes
are a separate owned copy.

### Placement math

Canvas coordinates: x right, y down, origin at the top left. Every quad comes from the
protocol's shared formula ([`geometry.rs`](../jstream-protocol/src/geometry.rs)), so
what the compositor draws and what a UI overlay computes cannot disagree.

**Geometry.** `ItemGeometry::content` gives the cropped image's rectangle in the box's
local frame (origin at the box centre, before rotation) and its UVs: the whole box for
`stretch`, the centred largest rectangle of the crop's aspect ratio for `fit`, the
centred smallest covering one for `fill`. `flip_h` and `flip_v` swap that rectangle's
`u` or `v` pair, which mirrors the cropped, fitted image about the box centre without
moving the box. `place` intersects that rectangle with the
box, which only changes `fill`, and cuts the UVs in proportion, so the clip needs no
scissor or shader change, and a flipped `fill` keeps the mirror of the same middle. Clipping in the local frame is exact for any in-plane
rotation. `ItemGeometry::place` then maps the four clipped corners: rotation about the
centre, then `center` added. With 3D neutral that mapping is affine, so linear UVs
across the quad are correct.

**3D.** With non-neutral 3D, `place` takes `ItemGeometry::drawn_polygon`: the content
clipped to the box and then by the near plane, three to five vertices with canvas
point, UV and `w`. Its edge lines (inward normal and offset, in canvas pixels, for
either winding) go to the shader for coverage. Each vertex is then pushed out one
pixel, to where its two edges moved out a pixel cross (capped at 4 pixels from the
corner for a sharp one), and that point is mapped back into the plane with
`unplace` and `drawn_vertex`, so its `w` and UV stay exact. A point that can't be
mapped back, past the near plane, keeps the unpushed vertex. An item seen edge-on or
wholly behind the viewer draws nothing.

**Legacy.** `place` asks [`legacy_quad`](../jstream-protocol/src/geometry.rs) for the
quad. For a source of size `(sw, sh)`, the image's extent, and crop `(left, top, right, bottom)` that
function:

- takes `cw = sw - left - right`, `ch = sh - top - bottom`, and skips when either is
  `<= 0` or the source size is zero;
- sets the destination size to `(cw * scale.x, ch * scale.y)`, and skips when either
  component is 0 (a negative component reflects that axis);
- covers the cropped texels with UVs `u0 = left/sw`, `u1 = (sw-right)/sw`, and the
  same for `v` with y down in the texture;
- puts the destination rectangle's top-left at `position`, then rotates clockwise
  about that corner by `rotation_deg`. In y-down coordinates that is
  `x' = px + dx·cos θ − dy·sin θ`, `y' = py + dx·sin θ + dy·cos θ`.

This crate then maps those canvas pixels into clip space:
`x_ndc = x/canvas_w·2 − 1`, `y_ndc = 1 − y/canvas_h·2`.

Opacity outside `[0, 1]` and any non-finite placement, colour or opacity is
`RenderError::Invalid`, not a clamped draw. So is an item whose image another
compositor made, visible or not.

### Device selection

`Compositor::open` builds an instance with
`InstanceDescriptor::new_without_display_handle` and the backends this crate enables:
Vulkan, DX12 and Metal (one `wgpu` pin, ADR-0004; features are additive,
[R-RS-010](../../docs/RULES.md#R-RS-010)). No GLES backend: lavapipe covers Linux
without an EGL stack. Instance flags start empty and `with_env()`, so a debug build
does not require the Khronos validation layer; `WGPU_VALIDATION` still opts in.

Adapter order when `OpenOptions` asks for software (`OpenOptions::headless`):

1. `force_fallback_adapter: true`, which is a CPU adapter when the backend has one
   (lavapipe, WARP).
2. Any adapter, so a machine with a GPU and no software rasterizer still runs.
3. `RenderError::NoAdapter` when the instance has none.

When it asks for hardware (`OpenOptions::hardware`, the default, which the engine's
render, snapshot and preview threads, the tests and the benchmarks use; on WARP a 720p
program ran at about 10 fps, #433): on Windows, a D3D12 hardware adapter first, since
the platform's capture, interop and encoders are D3D11/D3D12 and wgpu's own pick on a
Radeon 780M was Vulkan (a D3D12 device that won't open is logged and skipped, and WARP,
which D3D12 offers when it has no GPU, is skipped too); then, everywhere, the
high-performance adapter without `force_fallback_adapter`, then the software adapter,
then `RenderError::NoAdapter`. A hardware adapter whose device refuses to open (limits
below the defaults, a removed or hung GPU) falls back to the software adapter too, and
the error names both when that fails as well. When the pick already was the software
adapter (no GPU), its device error is returned without a second try. A machine with no
GPU still runs, on lavapipe or WARP.

`Compositor::on_context` does not search. It uses the `GpuContext` it is given.

Logging goes through `tracing` (adapter name at open). No `println!`.

<a id="filters"></a>
## Filters
<!-- @section
covers = ["crates/jstream-render/src/filter/**"]
depends_on = ["docs/adr/0025-typed-settings-kind-descriptors.md", "docs/adr/0026-source-filter-chains.md"]
-->

The built-in video filter kinds ([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)
descriptors for [ADR-0026](../../docs/adr/0026-source-filter-chains.md)'s video chain),
and the GPU passes that draw them. The kinds are grouped by roadmap item, one module
each under `src/filter/` (`pack`, `aspect`, `multipass`, `assets`). Each kind has a
`video_filter` descriptor, the mapping from its settings (complete, as the graph stores
them) to shader values, and its WGSL. A test checks that every kind is unique, that its
defaults pass its descriptor, and that they build its pass.

**Where they run.** `jstream-core`'s chain runtime (roadmap V0,
[core › Design](../jstream-core/README.md#design)) keeps a `VideoFilter` per enabled
filter of a source's video chain and runs them in order before placement, and of a
scene's over its composite
([ADR-0065](../../docs/adr/0065-scene-chains-and-item-effects.md)), each into a
cached layer with `apply_filter_into`. `video_filter_descriptors()` is how its
`kind.list` gets these kinds.

**The harness.**

- `VideoFilter::new(kind_id, settings)` builds a kind's pass; `set_settings` updates it
  in place, keeping its state; `output_size` is the size it draws for a given input,
  so a filter can change the source's reported size. Only aspect ratio does (its crop,
  pad and stretch reshape the frame); every other kind keeps its input's size
  (**Bounds** below).
- `Compositor::apply_filter(filter, input, clock)` draws the filter over `input` into a
  new layer of that size, in one submit. `apply_filter_into(filter, input, output,
  clock)` draws into a layer the caller keeps (a `ResourceCache` layer), so a chain
  that runs every frame allocates no output; it refuses an output of another size than
  the filter reports, the input itself, or an image that isn't a layer. `clock` is the media-clock time, for filters
  that move or hold frames. An imported input's `FrameSync`, and that of any image the
  filter reads besides it (`Ctx::hold`), is staged once the pass has recorded, right
  before the submit, and the submit hands those textures back in `PRESENT`, as `render`
  does. The prelude's `frame.time` is that time wrapped
  to a day (better than 10 ms in an `f32`) and jumps back once a day, so a kind that
  moves with time computes its phase on the CPU in `f64` and passes it in its `Params`.
- A pass records fullscreen draws through `Ctx::draw`. Every filter shader is appended
  to one prelude (`prelude.wgsl`) that defines the fullscreen triangle, the bindings
  (input, a linear clamp sampler, a `Frame` uniform with the input, output and aux
  sizes and the time, an optional aux image, and the filter's `Params` uniform) and
  colour helpers. `src`, `texel` and `aux` always return premultiplied linear colour:
  a straight import is premultiplied after sampling, and a premultiplied import (a
  browser page) is divided by alpha, decoded from sRGB and multiplied again, and an
  opaque import (a capture) has its alpha read as 1, as the compositor's own draw does
  (`Frame`'s `alpha` and `aux_alpha` say which). A pipeline is built on first use and
  kept for the compositor's life, keyed by the shader's label and source (up to four
  sources per label, so a reloaded pack's old and new filters don't evict each other).
  A shader wgpu refuses, by its own validation or a backend compiler's, is a
  `RenderError`, not a panic: error scopes for both catch the build, and the refusal is
  cached so it isn't recompiled every frame. An external filter's uniform bytes that
  aren't a multiple of 16 (at least 16) are refused the same way.
- A multi-pass filter draws into `Ctx::scratch` layers first. A draw either replaces
  its target or blends premultiplied src-over it.
- Each `apply_filter` makes its output layer (`apply_filter_into` reuses the caller's).
  A multi-pass filter's `Ctx::scratch` layers (up to three, each at most the filter's
  output size, about 16.6 MB at 1080p) come from a pool its `VideoFilter` keeps (V0): a
  call's `n`th scratch layer is the call before's `n`th when the size is the same, so a
  filter at a steady size makes them once. A layer of another size replaces it, and one
  a call doesn't ask for is dropped at its end. A pass never keeps a scratch layer, or
  relies on its contents, past the call that takes it. A filter's draws keep their two
  small uniform buffers in that pool too (#369): a call's `n`th draw reuses the call
  before's `n`th, rewriting them with `queue.write_buffer` before the submit. They are
  made again for another compositor or when the parameter block changes size. Each draw
  still makes its bind group and two views: they hold the textures it reads, and a kept
  one could outlive the cache entry that owns those textures and free them on the
  drawing thread (R-RT-003). Besides its scratch layers and these buffers, a filter keeps
  between frames only the video delay's ring of held frames (its own layers,
  `Ctx::layer`, never scratch ones) and the reference image an asset or scene kind is
  given (`VideoFilter::set_reference`, see Assets and references below).

**External kinds** (effect packs, ADR-0035). `ExternalFilter` is the seam for a kind
whose WGSL arrives at run time: a label (the kind ID), WGSL appended to the prelude
(exported as `FILTER_PRELUDE`, which packs validate against), and the `Params` bytes for
a settings map. `VideoFilter::external(kind_id, filter, settings)` draws it as one
replacing pass. [`jstream-effects`](../jstream-effects/README.md#purpose) implements it
for each pack effect.

**The kinds.** Each group documents its kinds here as it lands.

The single-pass pack (V1, `src/filter/pack/`): one draw each, keeping the input's
size. The colour kinds work on straight sRGB-encoded colour, as OBS does, so OBS's
numbers carry over; sharpen works on premultiplied encoded colour. Each decodes back to
premultiplied linear light on the way out.

- `color_correction`: `gamma` (−3..3; above 0 brightens, OBS's exponent mapping),
  `contrast` (−4..4), `brightness` (−1..1), `saturation` (−1..5; −1 is greyscale,
  BT.709 weights), `hue_shift` (°, a rotation about the grey axis), `multiply` (an RGB
  colour) and `opacity` (%). Gamma first, then one affine matrix built on the CPU, then
  a clamp to 0..1 and opacity. Every default is neutral.
- `chroma_key`: `key_color_type` (green, blue, magenta or custom `key_color`),
  `similarity`, `smoothness` and `spill` on OBS's 1–1000 scale. The distance is in
  BT.709 limited-range CbCr, averaged over the 3×3 texels around each pixel as OBS's v2
  does; spill desaturates towards luma.
- `color_key`: `key_color`, `similarity`, `smoothness` (1–1000), by RGB distance.
- `luma_key`: `luma_min`, `luma_max` and a smoothing width for each (0..1). A zero
  width is a hard edge that keeps the limit itself, so the defaults keep black and white.
  Each key multiplies the input's alpha, so stacked keys only remove more.
- `sharpen`: an unsharp mask, `amount` (0..4) away from the mean of the
  `(2 radius + 1)²` box, `radius` 1..3 px. It works on premultiplied encoded colour and
  clamps colour to alpha, so a transparent edge stays clean.
- `rounded_corners`: `radius` (px, clamped to half the shorter side), `border_width`
  (px) and `border_color` (with alpha). A signed distance antialiases over one pixel;
  outside is transparent; the border sits inside the frame, drawn over the picture.
- `scroll`: `speed_x`, `speed_y` (px/s; positive moves right and down), `loop_x`,
  `loop_y`. The offset is clock seconds times speed, worked out in `f64` on the CPU and
  wrapped to the image, so it stays exact however long the clock runs (the shader's
  `f32` `frame.time` would drift). Sampling is bilinear by hand: a looping axis wraps
  across its seam; off, the area the picture leaves is transparent. A new speed jumps
  to where that speed puts the picture at the current time.
- `vignette`: `strength` (0..1, default 0.5), `radius` (0..1, default 0.5), `softness`
  (0..1, default 0.5) and `color` (with alpha, default opaque black). The distance from
  the centre is measured in the frame's own UV, so the shape is an ellipse that follows
  the frame's aspect: 0 at the centre, 1 at the corners. Inside `radius` the picture is
  untouched. Over the next `softness` it fades towards `color`, to `strength` times the
  colour's alpha (a smoothstep; 0 is a hard edge). Coverage is kept, so a transparent
  pixel stays transparent. It frames whatever its chain runs on: the source's whole
  frame here, and an item's cropped box or a scene's composite once those owners are
  built (ADR-0065, #529).

Aspect ratio (V1a): `aspect_ratio` gives the frame a target shape before it is placed.
Settings: `ratio` (`16:9`, `9:16`, `4:3`, `1:1`, `21:9` or `custom`, with
`custom_width` and `custom_height` from 1 to 10000 shown only for `custom`), `mode`
(`crop`, the default, `pad` or `stretch`), `anchor` (`center`, an edge or a corner;
crop and pad only), `pad_color` (with alpha, default opaque black; pad only) and
`resampling` (`bilinear`, `bicubic`, the default, or `lanczos`; stretch only). Its
`output_size` is the rule the source's reported size follows: crop keeps one input
dimension and shrinks the other, pad keeps one and grows the other, and stretch keeps
the input's larger dimension as the output's larger one (1920x1080 stretched to 9:16 is
1080x1920). A derived dimension is rounded to the nearest pixel and never 0, and the
math saturates instead of overflowing, so a size past the adapter's limit fails as
`TooLarge` when the layer is made. Crop and pad copy input pixels 1:1 with an offset
(no scaling); only stretch resamples, as bilinear, Catmull-Rom bicubic (4x4 texel loads)
or Lanczos-3 (6x6 texel loads), normalised and clamped to a valid premultiplied colour
(an SDR assumption: values above 1 are clipped).
The kernels are not widened for a downscale, so a large shrink along one axis can alias.

The multi-pass filters (V2), in `src/filter/multipass/`:

- `blur`: `radius` (0 to 100 px, default 8) and an area: `area` (`whole`, `inside` or
  `outside` a rectangle) with `area_x`, `area_y`, `area_width`, `area_height` (fractions
  of the frame). A separable gaussian (sigma radius / 3). Its cost is bounded for every
  radius: it runs at the smallest working scale of 1, 2, 4 or 8 that keeps radius /
  scale at 16 or less, so a box downsample reads at most 64 texels a texel and each
  blur pass at most 33. It uses three scratch layers. The frame's edges clamp, so they
  don't darken. The area's edge is hard (no feather), and the blur inside it still
  reads pixels just outside it.
- `pixelate`: `block_size` (1 to 256 px, default 16) and the same area. Blocks are
  aligned to the top-left and are the mean of their pixels: exact up to 8 px, partial
  edge blocks included, an 8 x 8 grid of samples above that.
- `drop_shadow`: `color` (with alpha, default `#000000b3`), `offset_x`, `offset_y`
  (-200 to 200 px, default 8) and `blur` (0 to 100 px, default 8). The input's alpha is
  blurred, tinted, moved and drawn under the input. There is no spread.
- `outline`: `width` (0 to 64 px, default 4), `color` and `position` (`outside` or
  `inside`). It uses a jump flood: a seed pass, then steps of S, S/2, ..., 1 and one
  more step of 1, with S the power of two that reaches width + 1. That is at most 8
  steps of 9 loads each, in two scratch layers. Coverage is antialiased over one pixel.
  An inside stroke treats the area past the frame as outside the shape.
- `glow`: `color`, `radius` (0 to 100 px, default 16) and `strength` (0 to 4, default
  1, which multiplies the blurred alpha before it is clamped). It is a drop shadow with
  no offset.
- `video_delay`: `delay_ms` (0 to 5000, default 0, which passes the input through) and
  `max_memory_mb` (64 to 16384, default 1024). The pass keeps a ring of frame copies
  stamped with the clock and shows the newest one at or before now minus the delay, or
  the oldest one while the ring fills. The ring holds `min(300, memory / (w x h x 8))`
  frames, and never fewer than 2. To fit the delay into that many frames, it stores a
  frame only once delay / (capacity - 1) has passed since the last one. Past the cap
  the delay holds and the delayed picture's frame rate drops. Frames older than the one
  shown are reused. A new input size empties the ring, and so does a clock that goes
  backwards.

**Bounds.** Drop shadow, outline (outside) and glow draw within their input's frame,
as [ADR-0065](../../docs/adr/0065-scene-chains-and-item-effects.md) §5 has it for a
source or scene chain: the output is the input's size, the input is drawn 1:1, and
the part of the effect past the frame's edge is cut. So they don't change the
source's reported size, and around an opaque frame a shadow, an outside stroke or a
glow is cut away whole; on a shape with transparent margins (a keyed camera, a logo)
they show inside them. A drop shadow and a glow still blur the shape in a scratch
frame grown by the blur radius, so the blur fades out instead of stopping at the
frame's edge. An effect that hangs outside a placement is an item effect, whose layer
ADR-0065 pads by the chain's `max_outset_px` (not built yet).

Assets and references (V3), in `filter/assets/`: `apply_lut`, `image_mask` and
`scene_mask`. Each reads one external image that the chain runtime hands over with
`VideoFilter::set_reference(Option<Arc<Image>>)` (a defaulted `Pass::set_reference`
underneath): render opens no files (R-RT-002), so `jstream-core`'s chain runtime
resolves the reference and decodes it, or for `scene_mask` hands over the scene's layer
as drawn this frame (`Image` is `Clone`, sharing its texture). A kind passes its input through unchanged until both its
reference setting is non-`null` and an image is set, and refuses an image from another
compositor. A reference's `FrameSync` is staged and handed back with the input's, once
per submit even when the reference is the input itself; a mask test passes one
imported frame as both and counts one stage.

- `apply_lut` (`lut`: asset, `cube` or `png`; `amount` 0 to 1, default 1). The table
  maps sRGB-encoded straight colour, the `.cube` convention: the shader unpremultiplies,
  encodes, looks up, mixes with the input by `amount` in encoded values, then decodes
  and premultiplies; alpha is kept. `Lut::parse_cube` is a pure-Rust `.cube` parser
  (`LUT_3D_SIZE` 2 to 64, `DOMAIN_MIN`/`DOMAIN_MAX` or `LUT_3D_INPUT_RANGE`, comments,
  `TITLE` and other keywords ignored; 1D tables, bad sizes, wrong row counts and bad
  numbers are a `LutError`). A domain other than 0 to 1 is resampled onto it, so input
  outside the domain clamps. `Lut::from_tiled_rgba8` reads the common PNG layout (a
  square of `k * k` tiles of `k² x k²`, 512x512 for 64 points, blue choosing the tile)
  from decoded RGBA8. `lut_image(compositor, lut)` makes the reference image: the blue
  slices as near-square 2D tiles (512x512 for 64 points, so no adapter limit is near),
  stored `Rgba16Float` through a one-function data upload in `image.rs`, values clamped to
  0 to 1. It is an upload made straight through the compositor's image maker, with no
  draw, layer or submit (#369). The shader recovers the size from the image's dimensions and does the
  trilinear lookup as two bilinear taps in adjacent slices at texel centres; an image
  that is not such a layout passes the input through. `Lut` (with `parse_cube`,
  `from_tiled_rgba8` and `size`), `LutError` and `lut_image` are public, for the chain
  runtime to load a table and make its image.
- `image_mask` (`image`: asset, png/jpg/jpeg/webp/gif, what `jstream-sources` decodes; `mode` alpha or luma;
  `invert`) and `scene_mask` (`scene`: scene reference, its composited layer; `mode`
  alpha, luma or clip; `invert`). The mask is stretched over the frame and scales the
  premultiplied input: by its alpha, by BT.709 luma of its sRGB-encoded colour times its
  alpha (`#808080` keeps about half), or for `clip` by 1 where the scene's alpha is at
  least one half and 0 elsewhere (a hard clip that ignores translucency). `invert` uses
  one minus the coverage. OBS's colour blend modes for image masks are not offered.

<a id="testing"></a>
## Testing
<!-- @section
covers = ["crates/jstream-render/src/tests.rs"]
depends_on = ["docs/RULES.md#R-TEST-003", "docs/RULES.md#R-TEST-004", "docs/RULES.md#testing"]
-->

CI has no GPU ([R-TEST-004](../../docs/RULES.md#R-TEST-004)). The
[testing table](../../docs/RULES.md#testing) already names the two software adapters:
Linux lavapipe for a render smoke, Windows WARP as the golden-frame reference.
`cargo xtask golden` does not exist, and
[R-TEST-003](../../docs/RULES.md#R-TEST-003) says files under `tests/golden/` change
only through that command on WARP. This cut therefore does **not** add golden PNGs.
It renders offscreen and checks pixels in the crate tests, with a tolerance, so the
same tests run on lavapipe and on WARP. They open the compositor hardware first, so a
machine with a GPU runs them there (what the Windows hardware runs check); CI has no
GPU and gets the software adapter.

**Filters.** `filter::testing` runs one filter over a small uploaded image and reads
the result back through a 1:1 scene on a transparent canvas, so each kind's golden
test is a pixel or grid check like the ones below. A two-pass copy covers the harness
itself, including a half-transparent texel round-tripping, and copies a premultiplied
browser-page import through unchanged (read as premultiplied linear, it would brighten).
`tests/kinds.rs` snapshots every built-in video filter kind's descriptor
(`schema/kinds.snap`, an `insta` snapshot, as `jstream-sources` does for source kinds),
so a change to a filter's settings is reviewed like an API diff (ADR-0025, #369).
`INSTA_UPDATE=always cargo test -p jstream-render --test kinds` rewrites it.

**Why tolerance instead of a smoke-only Linux run.** Flat, opaque, channel-pure sRGB
colours (0 and 1) round-trip through the linear FP16 target exactly, and a 1:1
axis-aligned quad samples texel centres under a linear filter, so interior pixels
match across rasterizers. A small tolerance (a few codes) covers the sRGB encode of a
partial-alpha blend, where implementations round differently. Sampling stays off the
quad edge. That is a pixel check on both CI operating systems, which is stronger than
a Linux run that only proves the frame compiled, and it does not freeze a WARP image
the golden tooling cannot update.

**Geometry goldens.** The ADR-0022 placements are checked as small text grids rather
than images: the scene is sampled every 4 pixels, each sample classified as black,
red, green, blue, white, or `?`, and the grid compared with one written in the test.
Every box edge and every band edge in the test images sits more than a texel away
from a sample, so the grid is the same on lavapipe and on WARP, and a reviewer reads
the expected layout in the diff. These are not R-TEST-003's golden PNGs under
`tests/golden/`, which wait for `cargo xtask golden`. The grids cover `fit`
(letterboxed), `fill` (clipped to the box, the middle of the image showing), `stretch`
rotated 90° about the box centre, crop before fitting, a legacy item beside them, and
a non-axis-aligned box turning about its centre. A flip grid draws a four-quadrant
image unflipped, with `flip_h`, `flip_v` and both, flipped then turned 90° (the mirror
is in the box's own frame), and a flipped `fill`; another checks that a legacy
negative scale and the geometry `from_legacy` makes of it draw the same grid.

**3D goldens** (`tests/three_d.rs`). Spike S5's scenarios at a quarter of its size, on a
240×136 canvas from a 64² checkerboard with a texel grid (512² for C), against a CPU
reference that averages 8×8 (16×16 for C and D) point samples per pixel, each mapped
back with `unplace` and sampled bilinearly from level 0, clamped to the crop as the
chain is. The tests assert a mean and a
p99 error per channel in sRGB 8-bit steps. On lavapipe:

| Scenario | Pixels | Limit (mean) | Measured | Without the feature |
|---|---|---|---|---|
| A, an item turned 50° with perspective | interior | 3 | 1.1 | 67 drawn affinely (`w = 1`) |
| B, tilted toward the viewer, a corner behind it | all | 2 | 0.24 | 78 affinely |
| C, a floor tilted 75°, minified | interior | 8 | 6.2 | 18 with trilinear mips alone, 29 with one level |
| D, a small rotated and turned item | edge | 6 | 3.2 | 53 with a hard edge, 13 affinely |
| D cropped, a smaller D cropped just past a texel ring | edge | 5 | 2.8 | 6.6 with the chain built from the whole source, 8.2 without the UV clamp either |
| E, two items turned through each other | all | 2 | 0.53 | 15 affinely |

The p99 limit is 40 throughout. C's pattern is much finer than the spike's (one-texel
rings every 16 texels), which is why its mean is higher than the spike's 1.16. Further
tests check:
- that an upload's chain is rebuilt after `rewrite_rgba8`;
- that a layer's is rebuilt after `render_into` draws into it and not on a second draw
  while it is unchanged, and built once for two items and two calls;
- that a cached import drawn by two calls in a frame is rebuilt once per new frame;
- that an import's is rebuilt for a new frame from the cache, one under the same hold
  with a later time included, but not for the same frame again;
- that a 3D item's region is its crop's texels exactly at 1920×1080, where a crop edge
  doesn't survive a round trip through its `f32` texture coordinate;
- that two crops of one source get a chain each;
- that a cropped floor minified onto coarse levels shows nothing of a red border just
  past its crop (red 160 with the chain built from the whole source);
- that a slightly tilted item of a 128² image placed as 64² and cropped in those pixels
  samples its crop's texels: each half of the crop where it belongs and nothing past it
  (`Region::in_texels` at its call site; fails with the crop read as texels, the extent
  and texels swapped, or the UV remap divided by the extent).

Accepted means drawable (protocol 0.34's `check_drawable`): over 3D geometries from a
fixed seed, every input spread from 0.1 to f32's limit, each one `check_drawable` accepts
gives a `place::shape` for 64×36, 8192×1, 1×8192, 16384×1 and 1×16384 sources, with
and without a crop (`every_drawable_geometry_gives_a_shape`), and a frame of the most
extreme of them renders on 8192×1, 1×8192 and 64×36 sources
(`a_frame_of_the_most_extreme_accepted_3d_items_renders`). Both fail with the input
bound removed.

**What is checked.** Background only; an opaque rect over a background (position and
stacking); crop; an image placed at an extent of half its pixels (its size and its crop
count in the extent, ADR-0073); integer scale; 90° rotation about the top-left; opacity over an
opaque background (the linear blend, then the sRGB encode); a fully cropped item and
a hidden item draw nothing; readback size and row packing; a `FrameSync` hold is
staged once per submit, and not staged when the item is hidden or when a readback
too large for a buffer refuses the render; on a platform context it is staged with the
context's `SubmitLock` held, which is free again after the submit. Nested scenes
(`render_image`), three pixel tests and an ownership check: an image placed in
another scene lands where its scene put the item and is transparent elsewhere, and
survives a later `render`; a half-opaque item stays exact in linear light; images
nest twice with scaling at each level; another compositor refuses the image. Layers
and the cache: a layer redrawn with `render_into` keeps its texture and shows only
the newest composite; `render_into` refuses a layer the scene places, a wrong size
and a non-layer; `rewrite_rgba8` keeps the texture and changes the pixels and refuses
a layer; the cache reuses a texture per key, rewrites it for a new version and
replaces it for a new size; an unused entry is handed back only after the frame fence
(not while its frame is still being built). Alpha edges: a 2x1
opaque-red-then-transparent upload drawn half a pixel off and at scale 2 keeps red
at 255 where it is partly covered (straight filtering gave 187 on lavapipe); an
imported sRGB texture is read as straight alpha, and a premultiplied `Unorm` import
of half-transparent red over white keeps full red and lets half the white through in
linear light (each refused in the other's format). An opaque import of
half-transparent red draws full red over black, faded only by the item's opacity, and
an identity filter over it keeps it opaque; the cache reuses an opaque import and
replaces it with a straight one of the same texture. An image from another compositor on
the same device is refused. GPU frames through the cache: a frame drawn from an
import is staged once, and once the frame ends nothing but the frame holds its
`FrameSync`; a second texture under the key is a second import, the first texture
coming round again reuses its import and stages again, a frame of a new size retires
both old imports after the fence, an upload under the key replaces the import, and a
straight import under a key that held a premultiplied one replaces it
(the staged submit's `PRESENT` transition runs on the software adapter here). A lost
device: after `destroy` (what wgpu reports a driver reset through too) `lost` says
`destroyed`, a render fails, an NV12 bind returns `Uncaptured`
(`convert/tests.rs`), and an upload and a layer return without a panic (wgpu hands back invalid handles, which only a later `render` reports); an uncaptured
validation error before any loss fails the next render as `Uncaptured`, and the
render after it draws. Without
an adapter, unit tests still cover the binary16 encode
(every value an upload can produce, within half an ulp), the premultiply order and
the compositor identity.

**One device at a time.** Each pixel test opens its own compositor and keeps the
process lock in that same value until the value is dropped. The device field is
declared first, so it drops while the lock is held. WARP (Microsoft Basic Render
Driver) access-violates when several Dx12 devices are created and destroyed together.
The tests still all run; they do not share one device.

**Skipping.** `Compositor::open` returns `NoAdapter` when nothing can be created. The
test helper writes one line to stderr and returns, so a laptop without lavapipe or
WARP skips cleanly. When `CI` is set, the same miss panics: the workflow is
responsible for the software adapter, and a silent skip would make `test` green
without rendering.

**CI install (Linux `test` job only).** `mesa-vulkan-drivers` and `libvulkan1`, and
`VK_DRIVER_FILES` pointed at lavapipe's ICD so the job cannot pick another driver.
wgpu loads `libvulkan` at runtime; no new native crate and no `lint-deps` allowlist
change ([R-DEP-002](../../docs/RULES.md#R-DEP-002)). Windows already has WARP through
DX12; that job needs no package. macOS has no software adapter, so the pixel tests
use Metal if the runner has it. If it does not, they do not skip: GitHub sets `CI`,
so the missing adapter panics and the job fails. The `macos` job is
`continue-on-error` and not required, so that failure does not block a merge.
Editing the workflow is a governance change because `.github/**` is protected; it
adds a package so tests can render, and it does not weaken a check.

**NV12** (`convert/tests.rs`) is checked against a CPU reference
(`convert/reference.rs`): the same formulas in `f64`, written separately from the
shader. Every Y, Cb and Cr byte is within one code of it, for a pattern of primaries,
greys, gradients and partial alpha at a width that is not a multiple of four, in both
ranges, and for a compositor output with an item at 0.75 opacity. Black, white and
50% grey land on their codes in both ranges with neutral chroma, transparent white
encodes as black, and a single red column shows up in the two blocks whose `[1 2 1]`
taps reach it, which centre-sited chroma would not do. Odd sizes, a size mismatch,
BT.601 and a non-sRGB source are refused. `submit` stages its frame once, with the
compositor's `SubmitLock` held and free again after the submit, and leaves the
conversion in the planes, with a plane texture standing in for the platform's
target. The native path (plane views of an `NV12`
texture, compared with the planes path) runs where the adapter has
`TEXTURE_FORMAT_NV12`. It skips on lavapipe, which offers the feature but writes its
plane-1 render into plane 0's memory (the Y plane's first rows came back holding the
CbCr values), so the native path is checked on WARP if WARP has the feature, and on
real hardware.

**Benchmarks** (`benches/composite.rs`, criterion, [R-RT-005](../../docs/RULES.md#R-RT-005)).
A 1920x1080 frame of 1, 4 and 16 rotated 1280x720 items at 0.8 opacity, with readback
(`composite_1080p_readback`) and without, waiting for the queue (`composite_1080p_submit`),
and refilling a 1080p upload in place (`rewrite_rgba8_1080p`). Run them with
`cargo bench -p jstream-render --bench composite`, which takes the GPU when there is
one; the `bench` workflow records them on lavapipe on every push to `main` ([R-GIT-003](../../docs/RULES.md#R-GIT-003)). A
software adapter's numbers are CPU rasterisation: they track regressions in what the
compositor submits, and say nothing about GPU frame time. **Not measured:** the NV12
pass, and frame time on
a hardware GPU.

<a id="open-questions"></a>
## Open questions
<!-- @section
-->

- **3D.** The compositor draws ADR-0050 decisions 1, 2, 4, 5 and 6. Open: whether
  rotated 2D items should get edge coverage too, which changes their goldens; whether
  strongly minified 2D items should get mips.
- **Encode target.** ARCH step 4 writes NV12/P010 straight into a shared texture and
  ends the submit in `PRESENT`. The NV12 pass samples this RGBA output into either the
  platform `encode_target`'s plane views or `Nv12Planes`, which is the CI path, and
  `submit` ends its submit in `PRESENT`. Open: P010, and whether an adapter that rejects planar render
  targets copies `Nv12Planes` into the shared texture through hal or uses a D3D11
  video processor (ARCH's fallback).
- **Capture formats.** Windows capture's GPU frames (`WgcCapture::on_gpu`) are
  `Bgra8UnormSrgb`, which `import_opaque` takes as is, so they need no conversion pass;
  their alpha, which a driver or HDR mode can leave below 1, is read as 1. A
  browser page's frames are premultiplied `Bgra8Unorm`, which `import_premultiplied`
  takes as is.
- **Opacity and blend ownership** was left open by ADR-0022 (decision-register D2).
  The snapshot puts opacity on the item because that is what ARCH's data model and
  the compositor consequences of
  [ADR-0023](../../docs/adr/0023-placements-references-and-clones.md) describe. Blend modes other than src-over
  are not represented.
- **Background.** The clear colour is a field of the snapshot. `Scene.background` is
  not in the graph.
- **Golden files.** When `cargo xtask golden` exists, WARP renders under
  `tests/golden/` should freeze the cases these tolerance checks and geometry grids
  cover. Until then they are the pixel oracle on both software adapters.
- **Render thread.** Per-frame work is bounded for a caller that reuses `Image`s, but
  the thread, the snapshot swap and the panic boundary
  ([R-RT-006](../../docs/RULES.md#R-RT-006)) belong to `jstream-core`.
- **Output stability.** Callers that need the sRGB texture after the next `render`
  must copy it. Whether `render` should copy into a caller-supplied texture (the
  encode target, a preview) is open; the first engine wiring can decide.
