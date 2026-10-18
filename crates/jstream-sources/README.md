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
  `Arc<VideoFrame>`. For the built-in kinds `frame` neither blocks on I/O nor
  allocates, and a frame handed out stays valid after an update. Most make their frames
  when created or updated; a `media` file decodes them ahead on a thread of its own;
  the `waveform`, `spectrum` and `chat_message` kinds draw theirs in a `Painter`
  instead, and `frame`
  hands out the one painted last. `next_change(at)` says when that frame is next
  replaced (`None`: never), so a renderer can skip re-uploading a still. `show(at)`
  says the program mix shows the source again after plans that didn't reach it; a kind
  may restart there, and the default does nothing. `sound()` gives a kind's sound that
  plays while the program shows it (`MediaAudio`, a `media` file's), for the engine to
  mix; the default is `None`. `update(settings)` applies a change
  to `live` parameters only; `needs_restart(descriptor, old, new)` says when a change
  needs a new source instead. `painter()` (default `None`) returns the source's
  `Painter`. A source whose picture is on the GPU also returns it from `gpu_frame`
  (`None` by default), and one that makes its own audio whether or not it is shown
  returns it from `audio` as a `SourceAudio`: its format, and `open(handler)`, which
  feeds an `AudioInputHandler` until the returned stream is dropped. Only `browser` and
  its `youtube_chat` preset have either; `opens_browser(kind)` names those kinds.
  `capture()` is `Some` for a `display` or `window` source: what the platform captures
  for it (a `CaptureSpec`). Its frames come from the platform's capture backend on the
  engine's render thread, so its `frame` is `SourceError::Captured`.
- `Painter`: `paint(at)` draws the frame for `at` ahead of `frame`, for a source
  whose frames follow something live. Drawing can take milliseconds, so a host that
  asks for frames under a lock paints before taking it, when `next_change` said a
  frame is due (the engine's `jstream-sources` thread does). The `waveform` and
  `spectrum` painters allocate only while their frame pool grows, or when every pooled
  frame is still held; `chat_message`'s allocates when it redraws for a newly featured
  message.
- `Registry`: every kind by ID. `descriptors()` (ordered by ID), `descriptor(id)` and
  `create(id, settings, ctx)`. The engine creates its sources through it on its own
  `jstream-sources` thread, never under its collection lock or on the render thread,
  and its program mix asks each source for `capture`, else `frame` and `next_change`,
  and calls `show`
  (see `jstream-core`'s *Live sources*).
- `MediaAudio`: a `media` file's sound as the engine sees it. `format()` is the file's
  rate and layout, interleaved `f32`. `open(handler, clock, start)` starts a
  `jstream-media-audio` thread that hands the audio to `handler` (a mixer input's
  producer, `jstream-platform`'s `AudioInputHandler`) as device packets, and returns
  the `AudioStream` whose drop stops it; `start` is the source's time zero on `clock`.
  `set_audible` says whether the program shows the source; until it does, nothing is
  handed on.
- `Context`: the featured chat message (`chat`, see `chat_message`), the folders
  behind the `collection` and `app` asset roots
  (`AssetRoots`), the platform's media files (`media`, an
  `Option<Arc<dyn MediaDecoders>>` from `jstream-platform`, `None` until the host
  injects its backends), the injected `BrowserBackend` that `browser` and
  `youtube_chat` sources open their pages in (`browser`; none by default), and `Limits`
  on what a file may decode to: 256 MiB files, 16384 px
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
  limit. `Context` also carries the `AudioScopes` the `waveform` and `spectrum`
  sources find their audio in; with none they draw silence.
- `decode_still(bytes, path, limits)`: an image file's first frame as a `StillImage`
  (straight sRGB RGBA8), decoded as the `image` kind decodes, within the same `Limits`,
  with the same errors. `jstream-core` reads the images video filters use with it (an
  `image_mask`'s mask, an `apply_lut` table stored as tiles), which never animate.
- `AudioScope`: the latest audio of one mix, as mono (the mean of left and right), in
  a lock-free ring of 16384 samples (341 ms at 48 kHz) stored as atomics. One thread
  pushes interleaved stereo blocks; any number read the latest window of up to 2048
  samples. Neither locks nor allocates, so the engine's mixer thread pushes every
  program block into it ([R-RT-003](../../docs/RULES.md#R-RT-003)). The read is a
  seqlock: a push claims the slots it is about to overwrite before it writes them, and
  a read retries when a claim reached a slot it copied, so it never returns a window
  mixed from two passes of the ring. The analysis (the FFT included) runs in the
  source's `Painter`, on the thread that paints, never on the mixer thread
  ([ADR-0031](../../docs/adr/0031-source-types-and-decoding-scope.md)'s audio → render
  path).
- `AudioScopes`: where those sources find their scope, by their `audio_source` and
  `audio` settings: the program mix's (`program()`, which always exists), a bus's
  (`bus(id)`), or another source's (`source(id)`). `retain(drawn)` keeps a scope for
  each bus in `drawn`, making the missing ones, and drops the others, and
  `retain_sources(drawn)` does the same for sources; `buses()` and `sources()` list
  them. Before the first `retain`, `bus(id)` makes a bus's scope the first time it is
  asked for; after one, only the buses the last `retain` kept have scopes, and for any
  other it returns a scope of its own that nothing feeds and `buses()` doesn't list, so
  a source built from settings an edit has since changed can't bring back a scope the
  edit dropped (#701). `source(id)` does the same with `retain_sources`. The engine
  keeps one, calls both before it builds the sources an edit changed, and feeds every
  scope in it from its mixer thread. Its locks are taken only to find, make or drop a
  scope, never by the thread that pushes. `drawn_source(kind, settings)` names the source a `waveform` or
  `spectrum` draws, for the engine to feed.

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
- `display` and `window`: what the platform captures, by the `target` setting, a
  capture-backend target ID (on Windows `monitor:` and the display's GDI device name, or
  `hwnd:0x` and the window handle). An empty `display` target is the primary display,
  which is what a `display` source showed before it had settings, so the kind keeps
  version 1 ([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)). An
  empty `window` target captures nothing. A new target needs a new source. The engine's
  `capture.targets` query lists the IDs a `target` can take.
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
  timeline running on: a play lasts as long as its longest stream, from that stream's
  first frame or block to the end of its last, so the next play follows straight on
  even when a stream starts late in the file (Media Foundation leaves an MP4's B-frame
  delay in its video times); without it the last frame stays, and turning `loop` on
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
  keeps only the latest second, so a stream opened later starts with what is due. It
  queues the audio in blocks of 10 ms at most, cut from what the decoder hands over,
  which can be far longer. An open stream's thread wakes every 10 ms and hands on, as
  one packet each, the blocks the timeline reaches within the next 100 ms, stamped with
  the clock time the timeline reaches them, so it never runs further ahead than a mixer
  input's ring holds; the device position counts the frames handed on. Audio more
  than 20 ms behind the timeline is dropped, and a jump (a restart, a seek, the source
  shown again) flags the next packet as a discontinuity, so the mixer re-anchors
  there. Silence in the file itself, up to a second with nothing dropped (a looping
  file's sound ending before its picture), is instead a gap in the device position,
  which the mixer fills where it falls; flagged, the mixer would take the stamp for an
  outlier and re-anchor some 400 ms later, a second hole. It is heard only while the program shows the source and at `speed` 1: at any
  other speed it is silent, since the mixer steers an input's rate by 0.5 % at most.
  An audio-only file has no frames to catch the timeline up, so the audio thread
  seeks when what it drops is more than a second behind. Audio that outlasts the video
  plays on under the last frame: the source isn't behind then, so it doesn't seek, and
  `next_change` is `None`, or for a looping file the time the queued audio runs out,
  when the next play's first frame is due. An audio block of another
  format stops the source, as a frame of another size does.
- `waveform` and `spectrum` (C10) draw the latest 2048 samples (43 ms at 48 kHz) of
  the `AudioScope` their `audio` setting names in the `Context`'s `AudioScopes`
  (#362): `null`, the default, is the program mix, the master after its filters as
  the outputs hear it; a `bus` reference is that bus after its fader, before the
  master's filters. The parameter's description says so, and a change of it needs a
  new source; a value that isn't a bus reference or `null` is a setting error. The
  graph checks that the bus exists and refuses to delete it while a source draws it.
  A second setting, `audio_source` (#459), a source reference restricted to audio
  (`null` by default, so a source from before it draws what it drew, and the kinds'
  version stays 1), names a source to draw instead and overrides `audio`: that source
  after its own filters, fader, mute and follow, before any bus, as the audience
  hears it, from `AudioScopes::source`. It is silent while that source isn't playing
  into the mixer. One setting holding a bus or a source would change `audio`'s type,
  which ADR-0025 makes a kind version bump with a migration on load, so it is a second
  setting. The graph checks that the source exists and produces audio (a `scene`
  source doesn't: it has no channel of its own), and the reference is an ordinary
  source → source edge, so the drawn source's delete is refused, and `delete.plan`
  clears it, as for any source reference. A change of it needs a new source; a value
  that isn't a source reference or `null` is a setting error. `create`
  paints the first frame; after that their `Painter` draws a new one each 1/30 s tick
  of `at` (`next_change` is the next tick; with no scope, `None`), row by row into a
  pool of up to four frames reused once no one holds them, so a frame handed out
  never changes and, once the pool has filled, painting doesn't allocate. `frame`
  only hands out the frame painted last, and a live change is drawn by the next
  paint. When the scope's ring hasn't moved for 250 ms (no output runs, so the
  program mix doesn't), they draw silence; once every level has fallen to it, they
  keep their frame and `next_change` checks every 250 ms instead of every tick. `width` and `height` need a new source, and four frames of that size
  must fit `max_decoded_bytes`; `color`, `background`, `smoothing` and `scale` are
  live. `smoothing` (0 to 0.99) is how slowly a level falls: a rise shows at once,
  and each tick keeps that share of the gap. The `waveform` (640x160 by default)
  draws each pixel column from its lowest to its highest sample, on a `linear` scale
  or a `db` one from -60 dBFS (quiet audio shows). The `spectrum` (640x240) is a
  2048-point real FFT (`realfft`, planned on create, run with buffers made on create)
  of the Hann-windowed samples, drawn as `bars` (1 to 1024, live, 64 by default) of
  each bar's loudest bin, in dB from -90 dBFS to a full-scale sine's 0, up to
  20 kHz (or half the sample rate). The `scale` spreads frequencies `log` from
  20 Hz (each octave as wide, the default) or `linear` from 0 Hz, and a bar narrower
  than a bin shows its nearest bin. Bars at least three pixels wide have a
  one-pixel gap.
- `browser` ([ADR-0021](../../docs/adr/0021-browser-source.md)): a web page (`url`,
  `http://` or `https://`) or the `file` asset (`.html` or `.htm`, which wins when set),
  opened at `width` by `height` with a frame-rate cap (`fps`, 1 to 60, default 30) through
  `Context.browser` ([`jstream-browser`](../jstream-browser/README.md)). An empty URL
  with no file is a blank page. `css` (at most 64 KiB; the default makes the page's
  background transparent) is the one live setting: `update` replaces the page's custom
  CSS without reloading it. Through the engine a `css` edit still reloads the page, until
  the engine updates live settings in place (core's README › Open items).
  `local_network` (advanced, default off) lets a page from the internet make requests
  to this computer and the local network, for widgets that talk to an app running here;
  off, `jstream-browser`'s request policy refuses them (#347). `youtube_chat` never sets
  it. The page's picture is only on the GPU (`gpu_frame`), so
  `frame` is a transparent frame of the page's size, and the page's audio is the
  source's `audio`. With no backend, `create` fails with `SourceError::NoBackend`; a
  backend's failure is `SourceError::Browser`. Dropping the source closes the page.
- `youtube_chat` ("YouTube chat"): a preset of `browser` that shows a live video's chat
  through YouTube's public popout chat page, with no sign-in and no Google OAuth client
  (ADR-0051 §1, kept by ADR-0058). `video` takes the video's ID or a `youtube.com/watch?v=`,
  `youtube.com/live/`, `youtube.com/live_chat?v=` or `youtu.be/` link (with or without
  `www.` or `m.`); the ID must be letters, digits, `-` and `_` only, so it can't reshape
  the page's URL, and anything else is refused naming `video`. Empty is a blank page. A
  channel's `/live` link names no video, so it is refused. `width` (400), `height`
  (600), `fps` and `css` work as in `browser`; the default CSS (`YOUTUBE_CHAT_CSS`)
  makes the page transparent and hides the header, ticker and message box, so only
  messages show. It only shows chat: sending, featuring and moderating need the
  optional API path. YouTube owns the page, so if YouTube changes it, the default CSS
  may stop hiding things.
- `chat_message` (named "Chat message"): the featured live chat message (ADR-0041 §2,
  kept by [ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md)). It
  belongs to the optional API path: messages come from the Live API's chat, which needs
  JStream's Google OAuth client (built in, empty for now; ADR-0058 §1) and the
  collection's API chat switch, off by default (ADR-0058 §5). The message is runtime
  state, not a setting: `Context::chat` holds a `ChatFeature`, a shared slot the host
  fills with `set(Some(FeaturedMessage { author, badge, text, amount }))` or clears with
  `set(None)`, and every source created from that context draws what it holds. A header
  line (badge, a middle dot, the author's name, then a Super Chat's amount) in
  `author_color` sits over the text in `text_color`, left-aligned, each as narrow as it
  fits and wrapped at `max_width` less the `padding` (0 doesn't wrap), on `background`
  with `padding` around both. It draws with the text kind's renderer, so the same font
  and outline rules apply, and each block counts against the limits as a text source's
  does; stacking them counts the frame and the blocks it is drawn from. A message these
  settings or limits can't draw (too wide with no wrapping, or over the memory cap)
  shows the one transparent pixel instead, and the next message featured is drawn
  again. `frame` and the painter never fail, and `create` and `update` fail only for
  the settings themselves, never for the featured message. The bundled
  font covers Latin text only, so other scripts need a `font`. Every field is a
  stranger's text and is drawn as plain text: line breaks and other control characters
  become spaces, and text is cut to 1000 characters and the name, badge and amount to
  100 each. With nothing featured the frame is one transparent pixel. The slot changes
  outside the source, so `next_change` asks to be looked at again every 100 ms; the
  source's `Painter` takes the slot's lock and redraws (and so allocates) only when
  another message was featured since, and `frame` hands out the frame drawn last
  without drawing. Every setting but `font` is live.
- `webcam`: a webcam or capture card, by the `device` setting, the ID the
  platform's video-device backend lists it by (`VideoDevices::devices`). An empty
  `device` captures the first device listed. Like `display` and `window`, it only says
  what to capture (`CaptureSpec::Device`), and the engine opens the device. A new
  device needs a new source. No query lists the device IDs yet. The kind keeps version 1:
  before it had a setting, its sources couldn't start. It has audio too (#716): the
  input the platform pairs with the device, a webcam's microphone or a capture card's
  HDMI audio, which the engine records as an audio input's
  (`webcam_device(kind, settings)` says whose pair: the `device`, or `None` for the first
  device listed). So a new webcam gets audio settings that follow `when_shown` and a
  master send, as ADR-0028 gives a source with video and audio.

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
(the `MediaDecoders` trait the `media` kind plays through, GPU frames and audio
handlers; its `fake` feature in tests), `jstream-protocol` (descriptors, settings and
references), `jstream-browser` (the browser seam), `serde_json` (setting values),
`thiserror` (`SourceError`), the pure-Rust decoders `png`, `zune-jpeg` with
`zune-core`, `image-webp` and `gif`, `realfft` (with `rustfft`, scalar only) for the
spectrum's FFT, and for fonts `skrifa` (with `read-fonts`, `font-types`, `bytemuck`,
`yoke` and `once_cell`) and `ab_glyph_rasterizer`, each with default features off.
`ttf-parser`, which `ab_glyph` and `fontdue` build on, is unmaintained
(RUSTSEC-2026-0192), so the font crates come from Google's fontations instead
([ADR-0031](../../docs/adr/0031-source-types-and-decoding-scope.md)).

**Tests.** Unit tests cover colour parsing, animation timing, the text outline's
distance transform, reading an audio target from settings and taking a YouTube video
ID from an ID or link (`youtube_chat.rs`), and in `source.rs` the
asset checks refuse a file swapped in from outside the root (Unix) and a junction or
symbolic link inside the root that leads outside it (Windows, both when the path is
resolved and in the walk on its own, as a link swapped in after resolving would meet
it; a symbolic link is skipped, with a note, where the test may not create one).
`tests/kinds.rs` snapshots every built-in kind's descriptor (`schema/kinds.snap`, an
`insta` snapshot): adding or changing a kind changes no protocol schema, so this diff is
where a kind's settings are reviewed like an API diff (ADR-0025). `INSTA_UPDATE=always
cargo test -p jstream-sources --test kinds` rewrites it. `tests/sources.rs` checks
the registry and runs the colour and image kinds through the public API on files in a
temporary folder: PNG, GIF and lossless WebP files are encoded by the test, and
`tests/data` holds a JPEG, a lossy WebP and an animated WebP made once with Pillow, as
the test's header describes. `tests/text.rs` draws with the bundled font: wrapping,
alignment, padding, outline, live updates, font assets and limits, plus golden PNGs in
`tests/data/text` (`JSTREAM_BLESS=1` rewrites them). The scope's unit tests check the
ring (latest window, wrap-around, and a reader racing a writer never seeing a torn
window), the frame ticks and easing, and draw both kinds from known audio: each
waveform column's range, the dB scale, a tone lighting its own spectrum bar, every
log bar showing, smoothing and a still ring falling to silence, a silent source
settling on one frame and polling slowly until a live change redraws it, which
settings are live, and the `audio` setting (#362): both kinds describe it as a bus
reference defaulting to the program mix
(`the_audio_setting_names_a_bus_and_defaults_to_the_program_mix`), a source draws the
mix it names and not the other (`a_scope_source_draws_the_mix_its_audio_setting_names`),
and the lookup keeps one scope per drawn bus, and once a bus isn't drawn, asking for
its scope lists nothing until it is drawn again (`the_lookup_keeps_one_scope_per_drawn_bus`);
and the `audio_source` setting (#459): both kinds describe it as an audio source
reference defaulting to `null` with their version unchanged
(`the_audio_source_setting_names_an_audio_source_and_defaults_to_none`), a source
draws the source it names rather than its bus or the program mix, and a value that
isn't a source reference is refused
(`a_scope_source_draws_the_source_its_audio_source_setting_names`), and the lookup
keeps one scope per drawn source, which `drawn_source` names
(`the_lookup_keeps_one_scope_per_drawn_source`).
`tests/scope_no_alloc.rs`, its own binary under `assert_no_alloc`'s allocator,
checks that a push, a read, and painting and taking both kinds' frames, of the
program mix, of a bus and of another source, don't allocate once the pool has filled. `tests/media.rs` plays
`jstream-platform`'s fake media files (`FakeMediaDecoders`, frames told apart by
colour): frames on the clock, the last frame held, looping and the seek after a gap,
speed and a speed change, restarting when shown, turning `loop` on after the end, no
file, and the errors and limits; and its audio (fake blocks told apart by level),
handed to a recording handler: nothing until heard, stamps 100 ms ahead of the
timeline with continuous positions, a restart as a discontinuity, silence at another
speed, an audio-only file looping without a seam and seeking after a long gap, and
audio that outlasts the video playing on without a seek, then looping to the first
frame, silence in the file before the next play stepped over as a gap rather than a
discontinuity, and a decoder's long blocks still handed on only 100 ms ahead,
continuous across the block seam. `player.rs`'s own tests loop a stream that starts late
in the file one frame period apart, and cut a block at a rate where 10 ms isn't a whole
number of frames, keeping every frame and the pieces contiguous. `tests/browser.rs` runs
the `browser` kind against `jstream-browser`'s fake backend: the page spec it opens, file
over URL, setting and backend errors, live CSS, the `local_network` opt-in, closing on
drop and the audio hand-off.
`tests/youtube_chat.rs` runs the `youtube_chat` preset the same way: the popout chat
page a video or link opens, a blank page for an empty setting, refused links, the
default CSS and live CSS.
