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
verified = { hash = "b3:ac15a26828d08135", by = "agent:claude@session_01K4N6DrL8C1KSQcuHTvpBcs", note = "Assets paragraph now states hard links inside the root are part of the trusted-root assumption (issue 220); checked against AssetRoots::read's dev/ino check in source.rs, which can't distinguish a hard link from its target" }
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
  `create(id, settings, ctx)`. The engine creates its sources through it on its own
  `jstream-sources` thread, never under its collection lock or on the render thread,
  and its program mix asks each source for `frame` and `next_change` (see
  `jstream-core`'s *Live sources*).
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
- `audio_input` (a `device` ID; empty follows the system's default input),
  `desktop_audio` (what the default output plays) and `app_audio` (an `exe` file name,
  found again when the application restarts) are audio-only: their descriptors have
  audio and no video, `create` fails with `SourceError::AudioOnly`, and the engine
  records them through the platform's audio I/O from the `AudioTarget` that
  `AudioTarget::of(kind, settings)` reads from their settings (see `jstream-core`'s
  *Audio sources*). Both settings are free text rather than a list of what is present,
  so a collection naming an unplugged device still loads.
- `text`: the `text` string drawn with the `font` asset (TrueType or OpenType, or the
  first font of a collection), or with the bundled font when it is `null`. Fonts are
  read with `skrifa` and glyph outlines rasterised with `ab_glyph_rasterizer`, unhinted.
  `size` is the em size in pixels, as CSS's `font-size`. Lines break at `\n`; with a
  `wrap_width` above 0 they also wrap at spaces, and a word wider than the wrap breaks
  inside the word. The line box is as wide as the widest line, or `wrap_width` when it
  wraps, and `align` places each line within it. `outline_width` grows the glyphs by
  that many pixels in `outline_color`, drawn under the fill; `background` fills the
  whole frame, which is the line box plus `padding` on every side, grown to fit any
  glyph ink or outline that reaches past it. Every setting but `font` is live: an update
  redraws the frame, and a new font needs a new source. There is no shaping and no
  kerning: each character is its font's glyph for that code point, placed by its
  advance, so kerning, ligatures, complex scripts and right-to-left text need a shaper
  later. A character the font lacks draws its `.notdef` box; tabs draw as one space and
  other control characters are dropped. The outline comes from an exact Euclidean
  distance transform of the fill, so its cost doesn't grow with its width. Fill, outline
  and background are blended on sRGB values, not in linear light. The frame must fit
  `Limits`: its sides within `max_side`, and its working memory (8 bytes a pixel, 12
  with an outline) within `max_decoded_bytes`; otherwise `create` or `update` fails
  naming `text`.
- `display`, `media`, `webcam` and `window` are described only, with the settings B4
  gave them: sources of them can be created and configured, and `create` fails with
  `SourceError::NotBuilt`.

Every frame is packed `Rgba8` with straight alpha, sRGB, full range.

**Bundled font.** `assets/fonts/Roboto-Regular-subset.ttf` is the text source's
default font and the one its tests draw with: Roboto Regular by Google, under the
Apache License 2.0 (`assets/fonts/LICENSE-Roboto.txt`). It was cut from
`src/hinted/Roboto-Regular.ttf` in [googlefonts/roboto](https://github.com/googlefonts/roboto)
(Roboto 2.001047, SHA-256
`56a45233d29f11b4dfb86d248e921939d115778f87325e7ae8cc108383d6664d`) with fontTools 4.67:

```sh
fonttools subset Roboto-Regular.ttf \
  --unicodes="U+0020-007E,U+00A0-017F,U+2010-2027,U+2030-203A,U+20AC,U+2122,U+FFFD" \
  --no-hinting --layout-features='' --drop-tables+=GPOS,GSUB,GDEF,gasp \
  --name-IDs='*' --notdef-outline --output-file=Roboto-Regular-subset.ttf
```

The subset keeps Basic Latin, Latin-1, Latin Extended-A, common punctuation, the euro
and trademark signs and the replacement character, about 22 KB. Hinting and the
OpenType layout tables are dropped, because the text source neither hints nor shapes.
The name table, which carries the copyright and licence notice, is kept whole. Text in
other scripts needs a `font` asset that covers it.

**Assets.** `AssetRoots::resolve` resolves a `collection` or `app` path through
symlinks and rejects one that leaves its root; an `absolute` path is used as it is.
`AssetRoots::read` checks the file it opens, not only the path it resolved, so swapping
a folder inside the root for a symlink meanwhile can't make it read a file outside. On
Unix the opened file must be, by device and inode, the file its path resolves to inside
the root afterwards. Stable Rust has no file IDs on Windows, so there the resolved path
is walked from the root and each folder on it is held open without delete sharing,
which stops it being renamed or replaced until the file is open; one that has become a
link (a symlink, junction or other name-surrogate reparse point) is refused, while a
OneDrive placeholder or dedup file, also a reparse point, is read. The root folder
itself is the host's configuration and is trusted, and so is what is in it: a hard link
inside the root to a file outside is read, because the link and its target are the same
file and neither check can tell them apart. Making one needs write access inside the
root, and refusing every file with more than one link would also refuse deduplicated
files.
Errors name an asset by the root and path its setting holds, never by the host path
([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)). A missing or
undecodable file is an error from `create`, never from the edit that set it.

**Dependencies.** `jstream-media` (frames and the media clock), `jstream-protocol`
(descriptors, settings and references), `serde_json` (setting values), `thiserror`
(`SourceError`), and the pure-Rust decoders `png`, `zune-jpeg` with `zune-core`,
`image-webp` and `gif`, and for fonts `skrifa` (with `read-fonts`, `font-types`,
`bytemuck`, `yoke` and `once_cell`) and `ab_glyph_rasterizer`, each with default
features off. `ttf-parser`, which `ab_glyph` and `fontdue` build on, is unmaintained
(RUSTSEC-2026-0192), so the font crates come from Google's fontations instead
([ADR-0031](../../docs/adr/0031-source-types-and-decoding-scope.md)).

**Tests.** Unit tests cover colour parsing, animation timing, the text outline's
distance transform and reading an audio target from settings. `tests/sources.rs` checks
the registry and runs the colour and image kinds through the public API on files in a
temporary folder: PNG, GIF and lossless WebP files are encoded by the test, and
`tests/data` holds a JPEG, a lossy WebP and an animated WebP made once with Pillow, as
the test's header describes. `tests/text.rs` draws with the bundled font: wrapping,
alignment, padding, outline, live updates, font assets and limits, plus golden PNGs in
`tests/data/text` (`JSTREAM_BLESS=1` rewrites them).
