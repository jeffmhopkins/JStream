<!-- @doc
id = "CRATE-jstream-sources"
title = "jstream-sources"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-sources

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-sources/src/**", "crates/jstream-sources/tests/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-sources", "docs/adr/0031-source-types-and-decoding-scope.md", "docs/adr/0025-typed-settings-kind-descriptors.md"]
verified = { hash = "b3:fe857959912f77a1", by = "agent:claude@session_01JkxnrLGfCUpQuDuUjrUHMR", note = "png 0.18: PNG decode maps a usize-overflowing frame size to a decode error; purpose and format list unchanged" }
-->

Layer-4 source kinds ([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)). Every
kind lives here ([ADR-0031](../../docs/adr/0031-source-types-and-decoding-scope.md)): its
descriptor, which the engine reports through `kind.list` and `kind.describe` and
validates settings against
([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)), and the code that
produces its frames. `jstream-core` takes its kinds from `Registry::builtin()`.

**Seams.**
- `SourceKind`: a kind's descriptor and `create(settings, ctx)`, which builds a
  `Source` from a complete settings map, as the graph stores it. Creating may block on
  reading and decoding a file, so it never runs on a real-time thread.
- `Source`: `frame(at)` returns the frame to show at `at`, the time since the source
  started on the media clock ([ADR-0014](../../docs/adr/0014-media-clock.md)), as an
  `Arc<VideoFrame>`. The built-in kinds make their frames when created or updated, so
  `frame` neither blocks nor allocates, and a frame handed out stays valid after an
  update. `next_change(at)` says when that frame is next replaced (`None`: never), so
  a renderer can skip re-uploading a still. `update(settings)` applies a change to
  `live` parameters only; `needs_restart(descriptor, old, new)` says when a change
  needs a new source instead.
- `Registry`: every kind by ID. `descriptors()` (ordered by ID), `descriptor(id)` and
  `create(id, settings, ctx)`. Rendering sources from the registry on the engine's
  render thread comes later; that work calls `create` off the render thread and hands
  the frames over.
- `Context`: the folders behind the `collection` and `app` asset roots
  (`AssetRoots`), and `Limits` on what a file may decode to: 256 MiB files, 16384 px
  sides and 1 GiB of decoded memory by default. The image's size is checked before
  anything is allocated for it, and the decoded-memory cap counts the frames kept plus
  the working buffers held while decoding: a GIF's canvas and its "restore to
  previous" copy, the PNG and WebP output buffers, and the pixel buffers `image-webp`
  allocates itself (its own memory limit covers only metadata chunks), counted at
  their size in 0.2: one RGBA canvas for a still, two for an animation. Each new
  buffer is checked before it is allocated, so a small file can't decode into a huge
  image. Not counted: the decoders' row buffers and tables; the GIF decoder's
  per-frame buffer, which `gif`'s own limit caps at the same figure; and
  `zune-jpeg`'s internal planes, which for a progressive JPEG hold the whole image's
  coefficients (2 bytes per sample of each colour component, so 6 bytes a pixel for
  full-resolution YCbCr) while it decodes, bounded only by the side
  limit.

**Kinds.**
- `color`: one frame of the `color` setting (`#rrggbb` or `#rrggbbaa`, straight
  alpha), `width` by `height` pixels. The colour is live; a size change needs a new
  source. The frame must also fit the decoded-memory cap: 16384x16384 RGBA is exactly
  1 GiB, so the default cap allows every size the descriptor does, and a lower cap
  makes `create` fail naming the larger side.
- `image`: the `file` asset, decoded whole when the source is created. The format is
  told from the file's first bytes, not its extension. PNG (first image only; an APNG
  plays as its still) through `png`, JPEG through `zune-jpeg`, WebP (still or
  animated, lossy or lossless) through `image-webp`, and GIF (still or animated)
  through `gif`. GIF frames are composited onto the logical screen with their disposal
  methods; "restore to background" clears to transparent, as browsers do. A frame of
  10 ms or less shows for 100 ms, as in browsers. A GIF plays once plus its
  NETSCAPE loop count, or forever; a WebP plays its loop count, or forever; after the
  last play the last frame stays. With no file, the source shows one transparent pixel.
  Colour profiles, gamma and JPEG EXIF orientation are not applied yet.
- `display`, `media`, `text`, `webcam` and `window` are described only, with the
  settings B4 gave them: sources of them can be created and configured, and `create`
  fails with `SourceError::NotBuilt`.

Every frame is packed `Rgba8` with straight alpha, sRGB, full range.

**Assets.** `AssetRoots::resolve` resolves a `collection` or `app` path through
symlinks and rejects one that leaves its root; an `absolute` path is used as it is.
Errors name an asset by the root and path its setting holds, never by the host path
([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)). A missing or
undecodable file is an error from `create`, never from the edit that set it.

**Dependencies.** `jstream-media` (frames and the media clock), `jstream-protocol`
(descriptors, settings and references), `serde_json` (setting values), `thiserror`
(`SourceError`), and the pure-Rust decoders `png`, `zune-jpeg` with `zune-core`,
`image-webp` and `gif`, each with default features off
([ADR-0031](../../docs/adr/0031-source-types-and-decoding-scope.md)).

**Tests.** Unit tests cover colour parsing and animation timing. `tests/sources.rs`
runs every kind through the public API on files in a temporary folder: PNG, GIF and
lossless WebP files are encoded by the test, and `tests/data` holds a JPEG, a lossy
WebP and an animated WebP made once with Pillow, as the test's header describes.
