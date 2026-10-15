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
verified = { hash = "b3:e3f4d43a894fb9b8", by = "agent:claude@session_01JxutE3HYspWxJqSNBBmcVu", note = "New crate (C1a): re-read against source.rs, registry.rs, color.rs, image/ and describe.rs after the WebP decoder-limit change; seams, limits, kinds, timing rules, asset resolution, deps and tests match the code" }
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
  sides and 1 GiB of decoded frames by default, checked before the decoder allocates,
  so a small file can't decode into a huge image.

**Kinds.**
- `color`: one frame of the `color` setting (`#rrggbb` or `#rrggbbaa`, straight
  alpha), `width` by `height` pixels. The colour is live; a size change needs a new
  source.
- `image`: the `file` asset, decoded whole when the source is created. The format is
  told from the file's first bytes, not its extension. PNG (first image only; an APNG
  plays as its still) through `png`, JPEG through `zune-jpeg`, WebP (still or
  animated, lossy or lossless) through `image-webp`, and GIF (still or animated)
  through `gif`. GIF frames are composited onto the logical screen with their disposal
  methods; "restore to background" clears to transparent, as browsers do. A frame
  shorter than 20 ms shows for 100 ms, as in browsers. A GIF plays once plus its
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
