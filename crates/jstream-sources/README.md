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
  `Arc<VideoFrame>`. The built-in kinds make their frames when created or updated, or
  ahead of time on a thread of their own (`media`), so `frame` neither blocks on I/O
  nor allocates, and a frame handed out stays valid after an update.
  `next_change(at)` says when that frame is next replaced (`None`: never), so a
  renderer can skip re-uploading a still. `show(at)` says the program mix shows the
  source again after plans that didn't reach it; a kind may restart there, and the
  default does nothing. `audio()` gives a kind's own sound (`MediaAudio`, a `media`
  file's), for the engine to mix; the default is `None`. `update(settings)` applies a change to `live` parameters only;
  `needs_restart(descriptor, old, new)` says when a change needs a new source instead.
- `Registry`: every kind by ID. `descriptors()` (ordered by ID), `descriptor(id)` and
  `create(id, settings, ctx)`. The engine creates its sources through it on its own
  `jstream-sources` thread, never under its collection lock or on the render thread,
  and its program mix asks each source for `frame` and `next_change`, and calls `show`
  (see `jstream-core`'s *Live sources*).
- `MediaAudio`: a `media` file's sound as the engine sees it. `format()` is the file's
  rate and layout, interleaved `f32`. `open(handler, clock, start)` starts a
  `jstream-media-audio` thread that hands the audio to `handler` (a mixer input's
  producer, `jstream-platform`'s `AudioInputHandler`) as device packets, and returns
  the `AudioStream` whose drop stops it; `start` is the source's time zero on `clock`.
  `set_audible` says whether the program shows the source; until it does, nothing is
  handed on.
- `Context`: the folders behind the `collection` and `app` asset roots
  (`AssetRoots`), the platform's media files (`media`, an
  `Option<Arc<dyn MediaDecoders>>` from `jstream-platform`, `None` until the host
  injects its backends), and `Limits` on what a file may decode to: 256 MiB files, 16384 px
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
- `media`: the `file` asset, a local video or audio file demuxed and decoded by the platform's
  `MediaDecoders` (the Media Foundation Source Reader on Windows,
  [ADR-0031](../../docs/adr/0031-source-types-and-decoding-scope.md)), so it plays the
  containers and codecs the OS does. `create` opens the file through
  `AssetRoots::open` and decodes its first frame, so a file the platform can't play,
  one with neither video nor audio, or one without a media backend
  (`SourceError::NoBackend`) fails there. A decode thread (`jstream-media-file`) then
  reads frames ahead, up to 8, or
  fewer when they, the frame shown and two more held elsewhere would pass
  `max_decoded_bytes`; the frame's sides must be within `max_side`, and every later
  frame must have the first one's size. The source maps its
  clock time onto the file's timeline at `speed` (0.25 to 4), from where it last
  started, and shows the latest frame the timeline has reached; a speed change keeps
  the position. With `loop` the file plays again from the start after its end, the
  timeline running on; without it the last frame stays, and turning `loop` on
  afterwards plays on. With `restart_on_show` the file starts again each time the
  program mix shows it after not showing it (`Source::show`), the first time
  included. When the frame shown
  falls more than a second behind the timeline (it was hidden, or the decoder can't
  keep up), the source seeks instead of decoding every frame between; a seek decodes
  from the key frame before its target and skips frames up to it (a looping file
  sought past its end before its length is known plays from its start there). The
  decode thread seeks and decodes with the shared state unlocked, so `frame` only
  waits to take a queued frame. Until the decoder
  catches up, `next_change` asks again 10 ms later. Every setting but `file` is live.
  A decode error, or a frame of another size, stops the source: `frame` then fails
  with `SourceError::Decode`. With no file, or a file with only audio, the source
  shows one transparent pixel, and `next_change` is `None`.

  The decode thread reads the file's audio in step with its video: audio while it is
  behind the video and less than a second is queued, video while its queue has room,
  so neither stream gets far ahead in the file. While no `MediaAudio` stream is open it
  keeps only the latest second, so a stream opened later starts with what is due. An
  open stream's thread wakes every 10 ms and hands on, in packets of 10 ms at most,
  each block the timeline reaches within the next 100 ms, stamped with the clock time
  the timeline reaches it; the device position counts the frames handed on. Audio more
  than 20 ms behind the timeline is dropped, and a jump (a restart, a seek, the source
  shown again) flags the next packet as a discontinuity, so the mixer re-anchors
  there. It is heard only while the program shows the source and at `speed` 1: at any
  other speed it is silent, since the mixer steers an input's rate by 0.5 % at most.
  An audio-only file has no frames to catch the timeline up, so the audio thread
  seeks when what it drops is more than a second behind. Audio that outlasts the video
  plays on under the last frame: the source isn't behind then, so it doesn't seek, and
  `next_change` is `None`, or for a looping file the time the queued audio runs out,
  when the next play's first frame is due. An audio block of another
  format stops the source, as a frame of another size does.
- `display`, `webcam` and `window` are described only, with the settings B4
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
files. `AssetRoots::open` checks a file the same way for a reader that opens it again by
path (a platform decoder streaming a large file) and returns the open file with its
resolved host path. The caller keeps the file open until that reader has opened the
path: on Windows it is held without delete sharing, so it, and the folders above it,
can't be renamed or replaced meanwhile. Unix has no such hold, so there the path is
only checked.
Errors name an asset by the root and path its setting holds, never by the host path
([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)). A missing or
undecodable file is an error from `create`, never from the edit that set it.

**Dependencies.** `jstream-media` (frames and the media clock), `jstream-platform`
(the `MediaDecoders` trait the `media` kind plays through; its `fake` feature in
tests), `jstream-protocol` (descriptors, settings and references), `serde_json` (setting values), `thiserror`
(`SourceError`), and the pure-Rust decoders `png`, `zune-jpeg` with `zune-core`,
`image-webp` and `gif`, and for fonts `skrifa` (with `read-fonts`, `font-types`,
`bytemuck`, `yoke` and `once_cell`) and `ab_glyph_rasterizer`, each with default
features off. `ttf-parser`, which `ab_glyph` and `fontdue` build on, is unmaintained
(RUSTSEC-2026-0192), so the font crates come from Google's fontations instead
([ADR-0031](../../docs/adr/0031-source-types-and-decoding-scope.md)).

**Tests.** Unit tests cover colour parsing, animation timing, the text outline's
distance transform and reading an audio target from settings, and in `source.rs` the
asset checks refuse a file swapped in from outside the root (Unix) and a junction or
symbolic link inside the root that leads outside it (Windows, both when the path is
resolved and in the walk on its own, as a link swapped in after resolving would meet
it; a symbolic link is skipped, with a note, where the test may not create one).
`tests/sources.rs` checks
the registry and runs the colour and image kinds through the public API on files in a
temporary folder: PNG, GIF and lossless WebP files are encoded by the test, and
`tests/data` holds a JPEG, a lossy WebP and an animated WebP made once with Pillow, as
the test's header describes. `tests/text.rs` draws with the bundled font: wrapping,
alignment, padding, outline, live updates, font assets and limits, plus golden PNGs in
`tests/data/text` (`JSTREAM_BLESS=1` rewrites them). `tests/media.rs` plays
`jstream-platform`'s fake media files (`FakeMediaDecoders`, frames told apart by
colour): frames on the clock, the last frame held, looping and the seek after a gap,
speed and a speed change, restarting when shown, turning `loop` on after the end, no
file, and the errors and limits; and its audio (fake blocks told apart by level),
handed to a recording handler: nothing until heard, stamps 100 ms ahead of the
timeline with continuous positions, a restart as a discontinuity, silence at another
speed, an audio-only file looping without a seam and seeking after a long gap, and
audio that outlasts the video playing on without a seek, then looping to the first
frame.
