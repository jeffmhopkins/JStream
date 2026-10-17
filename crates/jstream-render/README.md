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
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-render", "docs/ARCHITECTURE.md#gpu-pipeline", "docs/adr/0004-wgpu-with-native-interop.md", "docs/adr/0022-item-transform-and-canvas.md", "crates/jstream-platform/README.md#gpu-frames"]
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
    which is clipped to the box), and rotated clockwise about the box's centre;
  - a **legacy 0.3 placement** (decision 5): crop in source pixels, then scale the
    remaining rectangle by `scale` relative to the source's current size, put its
    top-left at `position` in canvas pixels, and rotate clockwise about that corner.

  Both depend on the image's size, which is runtime state, so the compositor evaluates
  the placement on every `render`.
- `visible == false`, a crop that leaves no pixels, or a scale of zero draws nothing.
- Opacity is a straight-alpha multiplier in `[0, 1]`, default 1. The graph does not
  store it; the snapshot carries it so `jstream-core` can pass it when it does
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

**Where it sits on the existing APIs.**

- It does **not** open the media adapter. When platform interop works, the engine
  constructs the compositor on `GpuContext`'s device, queue and adapter
  ([jstream-platform › GPU frames](../jstream-platform/README.md#gpu-frames)), so
  capture, composite and encode share one device. `Compositor::open` is the other
  path: no interop (the fakes, CI, a headless machine). It opens a device itself,
  software adapter first, as ARCH principle 7 and that section's "when there is no
  interop" paragraph say.
- A source image is either straight-alpha sRGB RGBA8 bytes the compositor uploads, or
  a `GpuFrame` / `wgpu::Texture` already on this device. An upload is premultiplied in
  linear light and stored as `Rgba16Float`, so filtering cannot darken transparent
  edges ([Passes](#design)). An imported texture must be `Rgba8UnormSrgb` or
  `Bgra8UnormSrgb`, so the sampler decodes it to linear, and is read as straight
  alpha: exact for an opaque capture, a fringe at partly transparent edges. A
  `GpuFrame` keeps its `FrameSync`; `render` calls `stage` once per distinct hold
  before the submit that samples it, and ends that submit by moving each staged
  texture to `PRESENT`, as `stage` requires. Staging stays the safe method on
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
  shared functions ([Placement math](#design)). 3D is not drawn: a geometry with a
  non-neutral 3D field is `RenderError::Invalid`.
- Readback is an owned tightly packed RGBA8 buffer, straight alpha, sRGB transfer,
  BT.709 primaries. `jstream-core` encodes it as the `video.snapshot` PNG. It
  is not the encoder's NV12 texture.

**Not in this cut.** The render thread and its snapshots are `jstream-core`'s
(`runtime/`), not this crate's. Also not here: NV12 or
P010 conversion, encode-target export, loading WGSL effect packs
(`jstream-effects`; this crate only draws them), the video filter chain runtime (the built-in filters' passes are here, see
[Filters](#filters)), 3D tilt/turn/depth/perspective, transitions, the master chain and the privacy
stage. The protocol formula for 3D exists; this crate does not draw it, and 3D
fields stay neutral until M3. The GPU pipeline's encode
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
`render` is the caller's tick, on `jstream-core`'s render thread. Nothing here drops
an image on its own: `ResourceCache` returns what it frees as `Reclaimed`, so that
thread can hand it to a collector ([R-RT-003](../../docs/RULES.md#R-RT-003)). GPU
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

   **Imported textures** (`import`, `import_frame`) are not converted: a capture
   changes every frame while its `Image` lives, so a conversion would be a per-frame
   pass. They are sampled as straight alpha and premultiplied after the filter. For an
   opaque texture, such as a captured frame, that is exact and costs nothing extra.
   A partly transparent import still fringes at its edges.
2. **Composite.** One render pass clears the FP16 target to the background converted
   to premultiplied linear (sRGB transfer on RGB, alpha unchanged, then RGB multiplied
   by alpha). Each visible item is two triangles. The vertex buffer holds clip-space
   positions, UVs, opacity and the image's alpha mode (six `f32`s, 24 bytes). The
   fragment shader samples; for a straight import it multiplies RGB by the sampled
   alpha; then it multiplies all four channels by opacity and outputs premultiplied
   linear. Blend is `One, OneMinusSrcAlpha` on colour and alpha. No depth buffer:
   stacking order is draw order. No cull, so a negative scale still draws (it flips
   the quad). The viewport clips to the canvas.
3. **Resolve.** A second pass draws one fullscreen triangle into the sRGB target with
   blending off. It `textureLoad`s the FP16 texel, un-premultiplies (zero RGB where
   alpha is 0) and writes straight linear RGB. The sRGB format encodes RGB on store;
   alpha is stored as-is. The output usage is `RENDER_ATTACHMENT | COPY_SRC |
   TEXTURE_BINDING`, so a snapshot or a later consumer can use it.

   **`render_image`** (a nested scene) stops after pass 2. It composites into a new
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
4. **Readback**, only when asked. `copy_texture_to_buffer` into a map-read buffer,
   rows padded to 256 bytes, then a map and a pack into tightly packed RGBA8. This
   waits on the GPU. The tick that feeds an encoder does not call it. Snapshots and
   tests do.

Both passes are one command buffer and one `queue.submit`. `FrameSync::stage` runs
before that submit, for every distinct hold the scene samples, and the command buffer
ends by transitioning each of those textures to `PRESENT` (on D3D12 the `COMMON` state
the platform's other APIs read a shared texture in), which `stage` requires of the
submit. `render_into` does the same. The compositor lets go of the holds after the
submit, so it keeps no platform texture busy between frames. The output texture is the
compositor's own, not a shared encode target, so nothing transitions it.

### Resources kept across frames

| Resource | Lifetime |
|---|---|
| Device, queue, pipelines, sampler, layouts | Compositor |
| FP16 target, sRGB target, resolve bind group | Recreated when the canvas size changes |
| Vertex buffer | Grown when the scene needs more vertices, never shrunk; `queue.write_buffer` otherwise |
| Scratch vertex `Vec<u8>` | Cleared and refilled each frame; capacity kept |
| Staged-hold scratch | Refilled for each submit and cleared after it; capacity kept |
| Per-image texture and bind group | `Image`, dropped with it |
| `render_image` texture | The returned `Image`, allocated per call |
| `layer` texture and its render-target view | The returned `Image`; `render_into` reuses it |
| A video delay filter's held frames | Its `VideoFilter`: reused as the ring turns; dropped when the input size changes or the delay is set to 0 ([Filters](#filters)) |
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
next frame's fence, which covers it.

`import` keyed by an ID samples a `GpuFrame` with no copy. The entry's image carries
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
centred smallest covering one for `fill`. `place` intersects that rectangle with the
box, which only changes `fill`, and cuts the UVs in proportion, so the clip needs no
scissor or shader change. Clipping in the local frame is exact for any in-plane
rotation. `ItemGeometry::place` then maps the four clipped corners: rotation about the
centre, then `center` added. With 3D neutral that mapping is affine, so linear UVs
across the quad are correct; a non-neutral 3D field would need perspective-correct
UVs, and is refused until M3.

**Legacy.** `place` asks [`legacy_quad`](../jstream-protocol/src/geometry.rs) for the
quad. For a source of size `(sw, sh)` and crop `(left, top, right, bottom)` that
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

Adapter order when `OpenOptions` asks for software (the test and headless default):

1. `force_fallback_adapter: true`, which is a CPU adapter when the backend has one
   (lavapipe, WARP).
2. Any adapter, so a machine with a GPU and no software rasterizer still runs.
3. `RenderError::NoAdapter` when the instance has none.

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

**Not wired yet.** The chain runtime (roadmap V0) is what keeps a `VideoFilter` per
enabled filter of a source's video chain and runs them in order before placement.
Until it lands nothing in the engine calls these passes, and the kinds are not in
`kind.list`: `video_filter_descriptors()` is what V0 adds to `jstream-core`'s built-in
kinds.

**The harness.**

- `VideoFilter::new(kind_id, settings)` builds a kind's pass; `set_settings` updates it
  in place, keeping its state; `output_size` is the size it draws for a given input,
  so a filter can change the source's reported size (for example aspect ratio, padding,
  a drop shadow, an outside outline or a glow).
- `Compositor::apply_filter(filter, input, clock)` draws the filter over `input` into a
  new layer of that size, in one submit. `clock` is the media-clock time, for filters
  that move or hold frames. An imported input's `FrameSync` is staged once the pass has
  recorded, right before the submit, and the submit hands its texture back in
  `PRESENT`, as `render` does. The prelude's `frame.time` is that time wrapped
  to a day (better than 10 ms in an `f32`) and jumps back once a day, so a kind that
  moves with time computes its phase on the CPU in `f64` and passes it in its `Params`.
- A pass records fullscreen draws through `Ctx::draw`. Every filter shader is appended
  to one prelude (`prelude.wgsl`) that defines the fullscreen triangle, the bindings
  (input, a linear clamp sampler, a `Frame` uniform with the input, output and aux
  sizes and the time, an optional aux image, and the filter's `Params` uniform) and
  colour helpers. `src` and `aux` always return premultiplied linear colour; an
  imported input is premultiplied after sampling. A pipeline is built on first use and
  kept for the compositor's life, keyed by the shader's label and source (up to four
  sources per label, so a reloaded pack's old and new filters don't evict each other).
  A shader wgpu refuses, by its own validation or a backend compiler's, is a
  `RenderError`, not a panic: error scopes for both catch the build, and the refusal is
  cached so it isn't recompiled every frame. An external filter's uniform bytes that
  aren't a multiple of 16 (at least 16) are refused the same way.
- A multi-pass filter draws into `Ctx::scratch` layers first. A draw either replaces
  its target or blends premultiplied src-over it.
- Each draw creates its two small uniform buffers, two views and a bind group, and each
  `apply_filter` its output layer. A multi-pass filter also creates its `Ctx::scratch`
  layers on every call: up to three, each at most the filter's output size (about
  16.6 MB at 1080p). Pooling all of these is the chain runtime's job (V0); until then a
  filter costs those allocations per frame. The video delay's ring of held frames is
  the one thing a filter keeps between frames.

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

**Bounds.** Drop shadow, outline (outside) and glow grow their output by the effect's
extent on each side, rounded up to whole pixels: the blur radius plus the offset
toward that side, or the stroke width. The input is drawn 1:1 at the matching inset, so
**these filters change the source's reported size**. Until the chain runtime (V0)
reports the inset, a placement fits the grown frame rather than the original content
box.

Assets and references (V3): not yet.

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
same tests run on lavapipe and on WARP.

**Filters.** `filter::testing` runs one filter over a small uploaded image and reads
the result back through a 1:1 scene on a transparent canvas, so each kind's golden
test is a pixel or grid check like the ones below. A two-pass copy covers the harness
itself, including a half-transparent texel round-tripping.

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
a non-axis-aligned box turning about its centre.

**What is checked.** Background only; an opaque rect over a background (position and
stacking); crop; integer scale; 90° rotation about the top-left; opacity over an
opaque background (the linear blend, then the sRGB encode); a fully cropped item and
a hidden item draw nothing; readback size and row packing; a `FrameSync` hold is
staged once per submit and not staged when the item is hidden. Nested scenes
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
imported sRGB texture is read as straight alpha. An image from another compositor on
the same device is refused. GPU frames through the cache: a frame drawn from an
import is staged once, and once the frame ends nothing but the frame holds its
`FrameSync`; a second texture under the key is a second import, the first texture
coming round again reuses its import and stages again, a frame of a new size retires
both old imports after the fence, and an upload under the key replaces the import
(the staged submit's `PRESENT` transition runs on the software adapter here). Without
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

**Benchmarks** (`benches/composite.rs`, criterion, [R-RT-005](../../docs/RULES.md#R-RT-005)).
A 1920x1080 frame of 1, 4 and 16 rotated 1280x720 items at 0.8 opacity, with readback
(`composite_1080p_readback`) and without, waiting for the queue (`composite_1080p_submit`),
and refilling a 1080p upload in place (`rewrite_rgba8_1080p`). Run them with
`cargo bench -p jstream-render --bench composite`; the `bench` workflow records them on
lavapipe on every push to `main` ([R-GIT-003](../../docs/RULES.md#R-GIT-003)). A
software adapter's numbers are CPU rasterisation: they track regressions in what the
compositor submits, and say nothing about GPU frame time. **Not measured:** frame time on
a hardware GPU.

<a id="open-questions"></a>
## Open questions
<!-- @section
-->

- **3D.** `tilt_deg`, `turn_deg`, `depth_px` and `perspective_px` are fields on
  `ItemGeometry` and default to neutral. `is_neutral_3d` is the check an engine uses
  to reject a non-neutral value until M3 draws it. No 3D uniforms here.
- **Encode target.** ARCH step 4 writes NV12/P010 straight into a shared texture and
  ends the submit in `PRESENT`. Software adapters often lack `TEXTURE_FORMAT_NV12`,
  so that pass cannot be the CI path. Proposed default: keep this RGBA output, and
  let a later pass sample it into the platform `encode_target` on the real device.
- **Capture formats.** Windows capture's GPU frames (`WgcCapture::on_gpu`) are opaque
  `Bgra8UnormSrgb`, which `import` takes as is, so they need no conversion pass. Open:
  whether a pass that writes premultiplied linear should exist anyway, to remove the
  straight-alpha fringe that a partly transparent import (a browser source, a nested
  scene's output) still has, and which crate owns it.
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
