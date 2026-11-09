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
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-sources", "docs/adr/0140-source-types-and-decoding-scope-v3.md", "docs/adr/0025-typed-settings-kind-descriptors.md", "docs/adr/0073-sources-drawn-at-their-shown-size.md", "docs/adr/0079-text-density-bounded-by-pixels.md", "docs/adr/0137-midi-visualization-sources.md", "docs/adr/0142-feeding-the-midi-sources-and-their-beat.md"]
-->

Layer-4 source kinds ([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)). Every
kind lives here ([ADR-0140](../../docs/adr/0140-source-types-and-decoding-scope-v3.md)): its
descriptor, which the engine reports through `kind.list` and `kind.describe` and
validates settings against
([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)), and the code that
produces its frames. `jstream-core` takes its kinds from `Registry::builtin()`.

**Seams.**
- `SourceKind`: a kind's descriptor and `create(settings, ctx)`, which builds a
  `Source` from a complete settings map, as the graph stores it. Creating may block on
  reading and decoding a file, so it never runs on a real-time thread.
  `keeps_time(old, new)` says whether a source built from `new` in place of one running
  with `old` keeps the old one's time zero, so its `at` runs on instead of starting from
  zero; the engine asks before it builds a source again for a settings change. The
  default is `false`; the `timer` kind keeps it unless `mode`, `duration` or `restart`
  changed.
- `Source`: `frame(at)` returns the frame to show at `at`, the time since the source
  started on the media clock ([ADR-0014](../../docs/adr/0014-media-clock.md)), as an
  `Arc<VideoFrame>`. For the built-in kinds `frame` neither blocks on I/O nor
  allocates (apart from a still image's once nothing holds its frame, below), and a
  frame handed out stays valid after an update. Most make their frames
  when created or updated; a `media` file decodes them ahead on a thread of its own;
  the `waveform`, `spectrum`, `chat_message`, `timer` and `midi_*` kinds draw theirs in a
  `Painter` instead, and `frame`
  hands out the one painted last. `next_change(at)` says when that frame is next
  replaced (`None`: never), so a renderer can skip re-uploading a still. `show(at)`
  says the program mix shows the source again after plans that didn't reach it; a kind
  may restart there, and the default does nothing. `restart(at)` starts the source again
  whatever its settings, for `source.restart`
  ([ADR-0115](../../docs/adr/0115-timelines-on-the-wire.md)): a `browser` page reloads
  and a `media` file plays from its start; the default does nothing. `sound()` gives a kind's sound that
  plays while the program shows it (`MediaAudio`, a `media` file's), for the engine to
  mix; the default is `None`. `update(settings)` applies a change
  to `live` parameters only; `needs_restart(descriptor, old, new)` says when a change
  needs a new source instead. `painter()` (default `None`) returns the source's
  `Painter`. A source whose picture is on the GPU also returns it from `gpu_frame`
  (`None` by default), and one that makes its own audio whether or not it is shown
  returns it from `audio` as a `SourceAudio`: its format, and `open(handler)`, which
  feeds an `AudioInputHandler` until the returned stream is dropped. Only `browser` and
  its `youtube_chat` preset have either; `opens_browser(kind)` names those kinds.
  `page_api()` is `true` for a source whose page has the page API
  ([ADR-0102](../../docs/adr/0102-page-api-for-browser-sources.md)); only such a source
  is handed a `PageView` (`page_view`: the clock now, whether it is shown on program and
  on preview, the two scenes and every shared value) after anything that can change
  one, and the clock now on each look (`page_clock`). Both do nothing by default;
  `page_view` returns when an update it held to keep to one per page frame may go, so
  the engine looks again then.
  `model()` (default `None`) is a `ShownModel` for a source that is a 3D model: the
  `Model` the engine draws in 3D where an item places it, with its `Lighting`
  ([ADR-0107](../../docs/adr/0107-3d-models.md)) and whether it casts and receives
  shadows. Only the `model` kind has one.
  `light()` (default `None`) is a `ShownLight` for the `light` kind: its `LightType`,
  colour, intensity, range, cone and softness, and whether it casts shadows and how
  softly, which the engine places where an item
  places the source ([ADR-0143](../../docs/adr/0143-scene-lights-and-depth-aware-scene-filters.md)).
  `capture()` is `Some` for a `display`, `window` or `webcam` source: what the platform
  captures for it (a `CaptureSpec`), and `show_cursor()` whether the pointer shows in it
  (`true`, the default, for a webcam and every other source). Its frames come from the
  platform's capture or video-device backend on the engine's render thread, so its
  `frame` is `SourceError::Captured`.
  `set_density(d)` asks a source to draw its frames `d` times as many pixels each way
  as its own size, so an item that shows it enlarged stays sharp, and returns that own
  size, which its frames are placed at; the default is `None`, a kind whose frames are
  their own size whatever is asked. Only `text` and `timer` draw at a density (see *Kinds*).
- `Painter`: `paint(at)` draws the frame for `at` ahead of `frame`, for a source
  whose frames follow something live. Drawing can take milliseconds, so a host that
  asks for frames under a lock paints before taking it, when `next_change` said a
  frame is due (the engine's `jstream-sources` thread does). A host that skipped a
  source nothing showed may paint it once under its lock, to catch it up before it is
  shown again (the engine does, for a painter a plan reaches again). The painter also
  hands out the frame it painted last (`frame`) and says when the next is due
  (`next_change`), as its source does, so a host can take new frames from it without
  the source or its lock (the engine stores them in a frame slot, ADR-0117 §3). The `waveform` and
  `spectrum` painters allocate only while their frame pool grows, or when every pooled
  frame is still held; `chat_message`'s allocates when it redraws for a newly featured
  message, and `timer`'s when the value it shows changes, once a second at most.
  An animated `image`, a `media` file with video and a `browser` page that paints on
  the CPU have a painter too, which draws nothing: the image's picks the frame due
  (its frames are decoded when it is built), the media file's takes the decoded frames
  due from its player's queue, and the page's hands out its latest paint. So their new
  frames also reach the picture without a plan. A still image, a file with no video and
  a page on the GPU have none.
- `Still`: a source's one frame that can be let go of and made again, from
  `Source::still()` (default `None`; only a still `image` has one, #1139). Every frame
  its `frame()` hands out shares one copy, freed once all are dropped; the next call
  reads and decodes the file again, so it blocks and never runs on a real-time thread.
  `frame_if_held()` gives the copy only if someone holds it (or the one decoded when the
  source was built, until it is taken), and `held()` says whether anyone does. A file
  that decodes again at another `size()` fails with `SourceError::Decode`, since what
  shows the still is placed at the size it had. The image source's `frame` is its
  still's, so a host that drops every frame it was handed makes the next `frame`
  decode; the engine keeps a shown still's frame only while a draw needs it
  (`jstream-core`'s `still`).
- `Registry`: every kind by ID. `descriptors()` (ordered by ID), `descriptor(id)` and
  `create(id, settings, ctx)`. The engine creates its sources through it on its own
  `jstream-sources` thread, never under its collection lock or on the render thread,
  and its program mix asks each source for `capture`, else `frame` and `next_change`,
  and calls `show`, and `cue` and `uncue` for a source on a warm scene
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
  ([ADR-0140](../../docs/adr/0140-source-types-and-decoding-scope-v3.md)'s audio → render
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
  `spectrum` draws, for the engine to feed. For a kind outside this crate with the same
  `audio` and `audio_source` settings (a shader source that hears audio, ADR-0093),
  `heard_source(settings)` names that source and `heard_scope(scopes, settings)` finds
  the scope it hears.
- `AudioMeter` (`scope/meter.rs`): what a shader source hears of a scope each frame, as
  `Metered`: the window's RMS level and peak, an onset (1 when the level jumps above its
  recent average, falling back), `METER_BANDS` (64) log-spaced bands from 20 Hz on the
  `spectrum` kind's dB scale, and the window at 64 evenly spaced points. Levels and bands
  rise at once and fall over a few frames. `AudioMeter::new` plans the FFT and allocates
  its buffers, so `measure(scope, ms)` allocates nothing and takes no lock, and can run
  on the engine's render thread. It hears silence once the ring has stood still for a
  quarter second, as the scope sources do, and a non-finite sample as silence. For an
  audio motion modifier ([ADR-0100](../../docs/adr/0100-audio-reactive-motion-modifiers.md)),
  `raw` gives the latest window's level, peak and onset before any fall
  (`RawMeasure`), `measure_levels` measures without the FFT for a caller that reads
  only those, and `band(low_hz, high_hz)` the spectrum between two frequencies as an
  amplitude: the square root of its bins' summed power, scaled so a full-scale sine in
  the band reads 1, a band narrower than a bin reading the bin it falls in. Tests
  measure a 1 kHz tone, then silence, a window with NaN and infinite samples, and a
  half-scale sine read at its amplitude by a band around it and not by one away from
  it, an inverted or NaN band reading 0, with its raw level, peak and onset, and a levels-only
  measure hearing the level with no spectrum.
- `MidiState` and `MidiInputs` (`midi/`, [ADR-0137](../../docs/adr/0137-midi-visualization-sources.md)):
  what the `midi_*` kinds draw. A `MidiState` holds one input's held velocity per
  channel and key, its controllers, pitch bend and channel pressure, and a log of the
  last 1024 note events (`recent(out, most)`, oldest first), all in atomics, so any
  thread pushes a `jstream_midi::Message` with `push(at_ms, message)` and any reads it
  without a lock or an allocation. A reader skips a log slot a push is writing. CC 120
  and 123 release the channel's held keys. `MidiInputs` keeps one state per input name
  (`input(name)`, at most `MAX_INPUTS`, 64) and one that every input feeds (`any()`);
  `drawn(name)` is the state a source's `input` setting draws. An input thread pushes
  through a `MidiFeed` (`feed(name)`): its own state and the every-input one, looked up
  once, and a `jstream-midi` `Tempo` tracker its clock and transport messages run,
  which publishes a `TempoReading` (time, last timing clock, BPM, beat, playing) to both
  states (`MidiState::tempo`) through a sequence-numbered cell, without a lock. Only a
  timing clock (`F8`) counts as a running clock. The every-input state takes one feed's
  clock at a time, until that clock has been silent for `CLOCK_TIMEOUT_MS` (500); a
  lone stop or song position doesn't take it from a clock that runs. `set_clock` gives it the
  host's media clock once (`now_ms`), and the kinds measure a note's age on it, the
  clock messages are stamped on, rather than on the paint time a source is given, which
  counts from its own start. `Context::midi` carries it; with none, the kinds draw their
  first frame and never change.

**Kinds.**
- `backdrop`: a generated studio backdrop, like the painted canvas, muslin or seamless
  paper a photographer hangs behind a subject, `width` by `height` pixels and opaque.
  `style` (`canvas`, `muslin` or `paper`) sets how the two colours, `color` and
  `accent`, mottle: domain-warped fractal gradient noise, with smaller dabs on a
  canvas, worked out on a grid 4 pixels coarse and interpolated. `texture` scales the
  mottle and `scale` its size; `light`, `light_x` and `light_y` place a soft pool of
  light; `vignette` darkens the edges; a fine fibre noise and `grain` go over
  everything, and the frame is dithered to 8 bits so smooth sweeps don't band. Colours
  mix and light in linear light. The same settings always draw the same picture
  (integer hashes, no platform randomness), and `seed` picks another. Every setting but
  the size is live; an update draws the frame again on the sources thread, about a
  third of a second for 1080p in a release build. So the size is capped below other
  kinds': at most 7680 a side and 7680 × 4320 pixels in all, either way round, which
  `create` checks before drawing, naming the side to shrink. A backdrop is soft and
  scales up with no visible loss.
- `color`: one frame of the `color` setting (`#rrggbb` or `#rrggbbaa`, straight
  alpha), `width` by `height` pixels. The colour is live; a size change needs a new
  source. The frame must also fit the decoded-memory cap: 16384x16384 RGBA is exactly
  1 GiB, so the default cap allows every size the descriptor does, and a lower cap
  makes `create` fail naming the larger side.
- `shape`: a `shape` (`rectangle` or `ellipse`) drawn at exactly `width` by `height`
  pixels, transparent around it, for plates, bars, chips, pills, badges and dots. A
  rectangle's corners round by `radius`, clamped to half the shorter side, which makes a
  pill; an ellipse fills the frame, a circle when it is square. `fill` is `solid`
  (`color`), `linear` (from `color` to `color2` along `angle`: 0 runs left to right, 90
  top to bottom, its ends on the frame's extent that way) or `radial` (`color` at the
  centre to `color2` at the inscribed ellipse). `border_width` draws `border_color`
  inside the edge. Coverage comes from a signed distance (for an ellipse, its implicit
  function over its gradient's length), so edges antialias over a pixel. Colours mix
  premultiplied in linear light, so a gradient into a transparent colour keeps its hue,
  and are dithered to 8 bits. Every setting is live, the size too: an update draws the
  frame again. Unlike `color`, a shape is meant to be drawn at its item's box, so no
  `fit` letterboxes it.
- `image`: the `file` asset, decoded whole when the source is created. The format is
  told from the file's first bytes, not its extension. PNG (first image only; an APNG
  plays as its still) through `png`, JPEG through `zune-jpeg`, WebP (still or
  animated, lossy or lossless) through `image-webp`, and GIF (still or animated)
  through `gif`. GIF frames are composited onto the logical screen with their disposal
  methods; "restore to background" clears to transparent, as browsers do. A frame of
  10 ms or less shows for 100 ms, as in browsers. A GIF plays once plus its
  NETSCAPE loop count, or forever; a WebP plays its loop count, or forever; after the
  last play the last frame stays. With no file, the source shows one transparent pixel.
  A still image (one frame, or frames with no delay, which show the last) keeps only
  that frame, as a `Still` that reads the file again once its pixels are let go of; an
  animation keeps every frame.
  Colour profiles, gamma and JPEG EXIF orientation are not applied yet.
- `midi_keyboard`, `midi_piano_roll`, `midi_pads`, `midi_particles` and `midi_cc`
  ([ADR-0137](../../docs/adr/0137-midi-visualization-sources.md)): a MIDI input's state,
  drawn on the CPU `width` by `height`. `input` names the input (empty: every input) and
  `channel` the channel (1 to 16, 0 for every one). A change of size or `input` restarts
  the source; `channel`, the colours and every kind-specific setting are live. A colour scales with velocity from 35% to full. The
  keyboard lights the held keys from `low_key` to `high_key`, black keys over the line
  between two white ones. The piano roll draws each note as a bar in its key's lane,
  from its note-on to its note-off, scrolling left from the right edge over `seconds`
  (0.5 to 30); it rebuilds the bars each frame from the log, and draws a key still held
  after its note-on left the log from the left edge. The pads are a `rows` by `columns`
  grid from `first_key` (the bottom-left pad, 36 by default, on channel 10), each lit by
  its key while held and fading over `decay_ms` after its last note-on, so a short drum
  hit shows. The particles (#1224) burst each note-on from `low_key` to `high_key`
  (21 to 108 by default) into `particles` squares (1 to 64), starting at its key's lane
  across the width, a third of the way up. They fly out up to `spread` pixels and slow
  as they go, `size` pixels wide; a softer note's go less far, are smaller and dimmer.
  A burst stays while its note is held and fades over `decay_ms` after its note-off
  (or a retrigger). Like the roll, it is rebuilt each frame from the log, and each
  particle's direction and reach come from the note's time and key alone, so a burst is
  the same every frame and the kind keeps nothing between frames. A held burst that has
  flown out stops moving, so the source settles. The meters draw a bar for each controller in `controllers` (a
  comma-separated list, at most 16) and, with `pitch_bend`, one more from the middle.
  Each paints at 30 fps through a `Painter` from a pool of four frames, as the scope
  sources do, and settles on one frame, polled every 250 ms, while its input has no new
  message and nothing it draws is moving. Tests push synthetic messages and read the
  pixels: velocity, channel and input filtering, the fade, the scroll, the lost note-on,
  a burst at its key that fades after its note-off,
  settling, live and restarting changes, and every setting refused.
- `midi_beat` ([ADR-0142](../../docs/adr/0142-feeding-the-midi-sources-and-their-beat.md) §4): a row
  of `beats_per_bar` cells (1 to 16, default 4) for the bar its `input`'s MIDI clock is
  in, the beat it is on lit: the bar's first in `downbeat`, the others in `color`,
  flashing at full as the beat starts and dimming to 35% as it passes. Stopped, the
  beat the song is on shows at 35%, whether or not the sender's clock still runs;
  playing with no timing clock for half a second, nothing is lit. It has no `channel`,
  since clock is for the whole input. Size and `input` restart it; the rest is live.
  While a clock runs, each of its 24 messages a beat counts as a change, so every MIDI
  source of that input (and of every input) redraws at its 30 fps. Tests feed clock and
  transport and read the pixels: the beat lit, the downbeat, stopped with and without a
  clock, and a playing song's clock going quiet.
- `model` ([ADR-0107](../../docs/adr/0107-3d-models.md)): `ModelKind`, a glTF 2.0 file
  (`file`, a `.glb` or a `.gltf` whose buffers and images are read beside it under the
  same asset root, or embedded as `data:` URIs) or a built-in `shape` (`plane`, `cube`,
  `sphere`, `cylinder`, `cone`, one unit across). The loader is the crate's own, on
  `serde_json`: the default scene's node transforms baked into one mesh, triangles,
  strips and fans, the metallic-roughness material with its five textures (only
  `TEXCOORD_0`), `KHR_materials_unlit` and `KHR_materials_emissive_strength`, with
  normals and tangents made where the file has none. Skins, morph targets, animations,
  cameras and lights are ignored with an `info` log; a required extension it doesn't
  know, sparse accessors and a URL are errors, `SourceError::Model` naming the file. A
  file the model names must pass `check_asset_path` as a relative path (no `..`, no
  absolute or drive path, no `:`, no device name), whatever root the model is under, so
  it stays below the model's folder. Every attribute must have one element per vertex,
  and a `TANGENT` must be a VEC4. ADR-0107's caps are checked while the model is built,
  not after: triangles and bytes as each primitive is added, textures before each image
  is decoded, an accessor's count (at most 3 × 2,000,000 + 2) before it is read, a
  primitive's vertex count before anything is built from it, and at most 100,000 node
  and primitive instances together, so a hierarchy that names the same children many
  times stops early even when its primitives are empty (those are skipped). Buffers that
  name the same file (once percent-decoded) share one read, and all buffers together, a
  `.glb`'s binary chunk included, hold at most 512 MiB. Images decode as the `image` kind decodes them, within the same
  `Limits`, and over 4096 px are shrunk. `color` (multiplying a file's own), `metallic`, `roughness`,
  `double_sided` (the last three for a shape only), `lighting` (`studio`, `unlit`,
  or `scene`, lit by the `light` sources of its 3D scene), and `casts_shadows` and
  `receives_shadows` (both default on,
  [ADR-0151](../../docs/adr/0151-the-rest-of-richer-3d-scenes.md) decision 3; settings
  saved without them read as on) are live. Its frame is a transparent stand-in at the model's bounds, the
  longer side 512 px (`NATURAL_SIDE`), which is the size a new item takes, and
  `Source::model` hands the engine the model, which an item draws in the stand-in's
  place (`jstream-core`'s README).
- `light` ([ADR-0143](../../docs/adr/0143-scene-lights-and-depth-aware-scene-filters.md)): `LightKind`, a light for the `scene`-lit models of a 3D
  scene. `type` (`point`, `spot`, `directional` or `ambient`), `color`, `intensity` (0
  to 64), `range_px` (0 never fades), `cone_deg` (1 to 170) and `softness` (0 to 1, the
  last two shown for a spot only), `shadows` (default off: a spot or directional light
  casts shadows) and `shadow_softness` (0 to 1, default 0.3,
  [ADR-0151](../../docs/adr/0151-the-rest-of-richer-3d-scenes.md) decision 3; settings
  saved without them read as no shadows) are all live. `type` `environment`
  ([ADR-0151](../../docs/adr/0151-the-rest-of-richer-3d-scenes.md) decision 9) is the
  sky around the whole scene, shown with `sky_color`, `horizon_color` and `ground_color`
  (defaults `#9cc2e8`, white and `#3a3a3a`) and `image` (a video source whose picture is
  the sky instead, as an equirectangular panorama; null for none), which
  `ShownLight::environment` carries as a `ShownEnvironment` (settings saved without
  them read as their defaults). Its frame is one transparent pixel: it
  draws nothing, and `Source::light` hands the engine its settings, which the item that
  places it gives a position, an aim and a strength (`jstream-core`'s README).
- `audio_input` (a `device` ID; empty follows the system's default input),
  `desktop_audio` (what the default output plays) and `app_audio` (an `exe` file name,
  found again when the application restarts) are audio-only: their descriptors have
  audio and no video, `create` fails with `SourceError::AudioOnly`, and the engine
  records them through the platform's audio I/O from the `AudioTarget` that
  `AudioTarget::of(kind, settings)` reads from their settings (see `jstream-core`'s
  *Audio sources*). Both settings are free text rather than a list of what is present,
  so a collection naming an unplugged device still loads.
- `network_audio` ([ADR-0122](../../docs/adr/0122-controller-input-midi-and-one-binding-system.md)
  §6) is audio-only too: a network sender's audio, recorded by `jstream-core`'s JTunnel
  receiver rather than the platform's audio I/O. `transport` is `jtunnel` (the only one
  so far), `sender` the tunnel name (at most `MAX_SENDER_NAME`, 31 bytes, trimmed; free
  text like a device ID, and recorded only once this machine accepts it), and
  `latency_ms` how long frames wait before they play (`LATENCY_MS`: 5 to 500, 40 by
  default; out of range reads as the default). Its target is
  `AudioTarget::Network { transport: Transport::Jtunnel, sender, latency_ms }`.
- `text`: the `text` string drawn with the `font` asset (TrueType or OpenType, or the
  first font of a collection), or with the bundled face `face` names when it is `null`
  (`regular`, `bold` or `condensed_bold`; see *Bundled fonts*). Fonts are
  read with `skrifa` and glyph outlines rasterised with `ab_glyph_rasterizer`, unhinted.
  `size` is the em size in pixels, as CSS's `font-size`. Lines break at `\n`; with a
  `wrap_width` above 0 they also wrap at spaces, and a word wider than the wrap breaks
  inside the word. `letter_spacing` adds that many pixels after every character
  (negative draws them closer), though not after a line's last. The line box is as
  wide as the widest line, or `wrap_width` when it wraps, and `align` places each line
  within it. `outline_width` grows the glyphs by that many pixels in `outline_color`,
  drawn under the fill; `background` fills the line box plus `padding` on every side,
  grown to fit any glyph ink or outline that reaches past it. A shadow (`shadow_color`;
  fully transparent, the default, draws none) is the ink with its outline, moved by
  `shadow_x` and `shadow_y` and blurred by `shadow_blur` (CSS's blur radius: twice a
  Gaussian's standard deviation, approximated by three box blurs each way), drawn over
  the background and under the outline and fill. The frame grows to hold it, and that
  margin stays transparent: the background keeps to its box. Every setting but `font`
  is live: an update redraws the frame (swapping the bundled face when `face` changes),
  and a new font needs a new source. There is no shaping and no
  kerning: each character is its font's glyph for that code point, placed by its
  advance, so kerning, ligatures, complex scripts and right-to-left text need a shaper
  later. A character the font lacks draws its `.notdef` box; tabs draw as one space and
  other control characters are dropped. The outline comes from an exact Euclidean
  distance transform of the fill, so its cost doesn't grow with its width. Fill, outline
  and background are blended on sRGB values, not in linear light. The frame must fit
  `Limits`: its sides within `max_side`, and its working memory (8 bytes a pixel, 4
  more with an outline and 8 more with a shadow) within `max_decoded_bytes`; otherwise `create` or `update` fails
  naming `text`. It draws at a density (as `timer` does, the same way)
  ([ADR-0073](../../docs/adr/0073-sources-drawn-at-their-shown-size.md)):
  `set_density(d)` draws the same layout with every length (the em size, the glyphs'
  positions, the outline, the padding, the shadow) multiplied by `d`, so the frame has `d` times the
  pixels each way and stands for the same size, which `set_density` returns and the
  engine places it at. Densities run from 1 to `MAX_DENSITY` (32), lowered to the
  highest whose frame holds at most a 4K canvas's pixels and is at most 8192 each way
  ([ADR-0079](../../docs/adr/0079-text-density-bounded-by-pixels.md)); one whose frame
  would still be over the limits is halved until it fits, down to 1. An update draws at
  the density set last, lowered the same way.
- `timer` (#1027): a countdown, a stopwatch or a clock, drawn as `text` draws (every
  `text` setting but `text` and `wrap_width`). A `countdown` shows the seconds left of
  `duration` (up to a day), rounded up (05:00 at the start, 00:00 at the end), then
  `end_text` when that isn't empty; a `stopwatch` shows the whole seconds since its
  start and stops at 99:59:59 (99:59 written `mm:ss`). `format` writes them `auto`
  (`mm:ss`, or `h:mm:ss` from an hour), `mm:ss` (minutes past an hour keep counting) or
  `hh:mm:ss`. Both count on the media clock from the source's time zero, its `at`, and
  `keeps_time` keeps that zero through every change but `mode`, `duration` and
  `restart`, so the engine, which builds a source again for any change and any new
  density, keeps the count going when the look is edited or the item resized. `restart`
  (#1040) is a counter whose value means nothing: changing it, to any other number,
  starts the count again at the same `mode` and `duration`, in one edit (and its undo
  starts it again too). `mode`, `duration` and `restart` are the settings that aren't
  live. A `clock` shows the wall-clock time of day
  (`clock_format` `24h`, `12h` with AM or PM, or either without seconds) in the
  machine's time zone, or the IANA zone `time_zone` names, through `jiff` (with no zone
  found for the machine it shows UTC; an unknown name is a setting error). Every frame
  has one size: when the source is created or updated it measures every shape a value
  can take (each digit at each of 0 to 9) and `end_text`, wraps the text at the widest
  of them so `align` places each value in one box, and draws every value into the union
  of their bounds (`raster::render_within`), cutting off any ink outside. So the frame
  the engine placed, and the size `source.sizes` reports, never change as the count
  does, and a frame too large for the limits (its text, padding, outline or shadow)
  fails `create` or `update`, naming `end_text` when that is the widest value, else
  `size`. The `Painter` draws a value when the one shown is out of
  date, and `frame` only hands out the one drawn last; `next_change` is the value's next
  second (a clock's next wall-clock second, or minute without seconds), `at` itself while
  the painter hasn't caught up, and `None` once a countdown ends or a stopwatch stops.
  Every setting but `font`, `mode`, `duration` and `restart` is live, and it draws at a density as
  `text` does.
- `display` and `window`: what the platform captures, by the `target` setting, a
  capture-backend target ID (on Windows `monitor:` and the display's GDI device name, or
  `hwnd:0x` and the window handle). An empty `display` target is the primary display,
  which is what a `display` source showed before it had settings, so the kind keeps
  version 1 ([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)). An
  empty `window` target captures nothing. A new target needs a new source. The engine's
  `capture.targets` query lists the IDs a `target` can take, and the parameter's type,
  `capture_target` of kind `display` or `window` (protocol 0.50), says so, so a UI lists
  them to pick from. `show_cursor` (on by
  default, which is what the kinds did before it, so they keep version 1) says whether
  the pointer shows; `Source::show_cursor` reports it, and the engine asks the capture
  backend to draw the pointer in when it is on and to leave it out when it is off.
  Changing it needs a new source, so the capture starts again.
- `media`: the `file` asset, a local video or audio file demuxed and decoded by the platform's
  `MediaDecoders` (the Media Foundation Source Reader on Windows,
  [ADR-0140](../../docs/adr/0140-source-types-and-decoding-scope-v3.md)), so it plays the
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
  included. Cued first (`Source::cue`, when it is on a warm scene no mix draws,
  ADR-0117 §7), such a file goes to its start and holds there, its timeline still and
  its first frame shown once decoded, with the audio from the start kept: `show` then
  plays it from there with no seek, and `uncue` (a preview draws it meanwhile) does
  the same without the program. When the frame shown
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
  new source; a value that isn't a bus reference or `null` is a setting error, whose
  reason shows the reference's shape as the graph's does (`not_a_reference`, #1081). The
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
  that isn't a source reference or `null` is a setting error, with the same reason. `create`
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
  `http://` or `https://`), the page itself as `html` (at most 128 KiB, its CSS and
  scripts inline; it wins over the URL when it isn't blank), or the `file` asset
  (`.html` or `.htm`, which wins over both when set), opened at `width` by `height` with a frame-rate cap (`fps`, 0 to 60; the default 0 follows the program's base rate, `Context.frame_rate`, and the engine opens such a page again when that rate changes, #1405) through
  `Context.browser` ([`jstream-browser`](../jstream-browser/README.md)). An empty URL
  with no file is a blank page. `css` (at most 64 KiB; the default makes the page's
  background transparent) is the one live setting: `update` replaces the page's custom
  CSS without reloading it. Through the engine a `css` edit still reloads the page, until
  the engine updates live settings in place (core's README › Open items).
  `restart_on_show` (live, default off) reloads the page each time the program shows it
  again (`show`), so an HTML graphic or stinger plays its entrance every time. It is
  transparent while it reloads, never the end of its previous run.
  While no mix draws the source (`set_drawn(false)`) its page is hidden
  (`BrowserPage::set_hidden`), so it stops painting and its animation frames stop; its
  scripts and audio go on, throttled as a background tab's. Its frames keep the last
  paint until it is drawn again (#1071).
  `local_network` (advanced, default off) lets a page from the internet make requests
  to this computer and the local network, for widgets that talk to an app running here;
  off, `jstream-browser`'s request policy refuses them (#347), for a page given as
  `html` too. On, it covers the page's own origin only, and only an `https` page (a page
  given as `html` is one): Chromium refuses a plain `http` page's local requests.
  `youtube_chat` never sets
  it. The **Page API** group (ADR-0102 and ADR-0124, none of it live, all off by
  default, each kind of data its own checkbox shown under `page_api`, and each with a
  `warning` the Properties dialog shows beside it):
  `page_api` gives the page `window.jstream` with the engine's clock and whether the
  source is shown on program and preview; `page_api_scenes` adds the two scenes;
  `page_api_shared_values` adds the shared values that `page_api_values` (at most 1 KiB,
  read only while the checkbox is on) names, as names or
  prefixes ending in `.*`, separated by commas, each name checked as `values.set`
  checks one; `page_api_audio` adds `jstream.levels()` for the bus `page_api_bus` names
  (read only while the checkbox is on), that
  bus's post-fader peak and RMS per channel, as `meters.tick` meters them, and 64
  log-spaced bands from 20 Hz to 20 kHz, lowest first, each 0 to 1 on the dB scale from
  -90 to 0 dBFS, from the bus's scope (`AudioMeter`); or `null` while no mixer runs or
  the bus is gone. A spectrogram page, for example, draws each reading as a column:

  ```js
  const g = document.querySelector("canvas").getContext("2d");
  jstream.addEventListener("levels", (r) => {
    const { width: w, height: h } = g.canvas;
    g.drawImage(g.canvas, -1, 0);      // scroll left a column
    g.clearRect(w - 1, 0, 1, h);
    if (r === null) return;            // no output runs, or the bus is gone
    r.bands.forEach((b, i) => {        // 64 bands, lowest first, 0 to 1
      g.fillStyle = `hsl(${240 - 240 * b} 90% ${50 * b}%)`;
      g.fillRect(w - 1, h - (i + 1) * h / 64, 1, h / 64);
    });
  });
  ```

  `page_api_tempo` adds the engine's `tempo.*` values, which no `page_api_values` entry
  covers without it (ADR-0124 decision 7), and a MIDI reading's tempo.
  `page_api_midi` adds `jstream.midi()` and a `midi` event for the MIDI input that
  `page_api_midi_input` names (empty for every input; read only while the checkbox is
  on), as [ADR-0147](../../docs/adr/0147-midi-tempo-and-state-for-pages.md) decides:
  its tempo (`{bpm, playing, clock, beat, at}`, or `null` without the tempo grant),
  held notes, non-zero
  controllers, bend and pressure, with channels 1 to 16, and the last two seconds'
  note-ons as `[at, channel, key, velocity]`, times on `jstream.clock`; at most 128
  notes, 256 controllers and 32 recent notes. Never raw bytes or SysEx.

  The source sends its page only what changed (`page_api.rs`, `Feed`):
  the visibility with the clock time each part last became true, the scenes, the
  levels, the MIDI reading, and the
  covered values, with a removal for one that went or left the grant's cover; and a
  clock anchor first, every 250 ms after that, and at once when the clock goes back or
  jumps more than a second. The anchor's lead is one frame of `fps` plus the mean age of
  the page's latest frame when the engine looks at it. Updates go at most once per frame
  of `fps`: what changes sooner is merged (the latest visibility, scenes and anchor, and
  each value's latest), held, and sent once the frame has passed, a held anchor moved to
  the clock's time as it goes. While the granted bus has a reading, the next page frame
  is always due, so the page reads the bus every frame. While the granted MIDI input
  moves (a message since the last look, or, with the tempo granted, a song playing) the
  next page frame is due too; while it rests, the source looks again every quarter
  second, unless the engine gave it no MIDI state. `youtube_chat` never sets any
  of it. Where the backend paints on the GPU (Windows), the page's picture is only there
  (`gpu_frame`), so `frame` is a transparent frame of the page's size and never changes.
  Where it paints on the CPU (CEF on Linux,
  [ADR-0095](../../docs/adr/0095-browser-helper-on-linux.md)), `frame` is the page's
  latest paint (transparent before the first), and `next_change` asks for the source
  again one frame of `fps` later, rounded up to a millisecond. The page's audio is the
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
  platform's video-device backend lists it by (`VideoDevices::devices`), which
  `capture.targets` lists (a `capture_target` of kind `device`, protocol 0.90). An empty
  `device` captures the first device listed. `resolution` (`<width>x<height>`), `fps`
  and `pixel_format` (`capture_format` parameters, in the Format group) ask for a format
  the device lists; empty or 0 leaves that part to the engine, and a device that offers
  no format matching them opens in the engine's choice. Like `display` and `window`, it
  only says what to capture (`CaptureSpec::Device`, with a `DeviceFormatWant`), and the
  engine opens the device. A new device or format needs a new source. `camera_controls`
  (the Camera group, live, protocol 0.105, #1196) sets the device's controls: an object
  of control name to a number or `"auto"`, as `capture.targets` lists them for the
  device; a control it leaves out keeps what the device has. `webcam_controls(kind,
  settings)` reads it, with the device as `webcam_device` gives it, as
  `CameraControlSetting`s, and the engine sets them on the device while it captures,
  never restarting the capture. The kind keeps version 1:
  before it had a setting, its sources couldn't start. It has audio too (#716): the
  input the platform pairs with the device, a webcam's microphone or a capture card's
  HDMI audio, which the engine records as an audio input's
  (`webcam_device(kind, settings)` says whose pair: `Some(Some(id))` for the `device`
  named, `Some(None)` for the first device listed, and `None` for any other kind). So a new webcam gets audio settings that follow `when_shown` and a
  master send, as ADR-0028 gives a source with video and audio.

Every frame is packed `Rgba8` with straight alpha, sRGB, full range.

**Installed fonts** (`fonts.rs`, [ADR-0092](../../docs/adr/0092-installed-fonts-query.md)). `installed_fonts(dirs)` lists the
fonts in the folders `system_font_dirs()` names for the OS (Linux's
`/usr/share/fonts`, `/usr/local/share/fonts`, the user's data `fonts` and `~/.fonts`;
macOS's system, library and user `Fonts`; Windows' `%WINDIR%\Fonts` and the user's),
for the engine's `font.list`: each `.ttf`, `.otf` and `.ttc` (a collection's first
font) with the family and style its name table gives (typographic names first, the US
English Windows entry first), control characters dropped and cut at 128 characters,
ordered by family ignoring case. The walk goes at most `MAX_DEPTH` (8) folders deep and
`MAX_FONTS` (8192) files, follows links to files but not to folders, and reads only a
file's header, table directory and name table (at most 1 MiB), so 160 fonts take a few
milliseconds. A file that doesn't parse or names no family is left out. Its tests walk
a folder of the bundled Roboto files, a file that isn't a font and the depth limit.

**Bundled fonts.** Three Latin subsets of Roboto by Google, under the Apache License
2.0 (`assets/fonts/LICENSE-Roboto.txt`), are the text source's faces while `font` is
`null`; `chat_message` draws with the regular one, and the tests draw with all three.

- `assets/fonts/Roboto-Regular-subset.ttf` (`face` `regular`, the default) was cut from
  `src/hinted/Roboto-Regular.ttf` in [googlefonts/roboto](https://github.com/googlefonts/roboto)
  (Roboto 2.001047, SHA-256
  `56a45233d29f11b4dfb86d248e921939d115778f87325e7ae8cc108383d6664d`).
- `assets/fonts/Roboto-Bold-subset.ttf` (`bold`) and
  `assets/fonts/RobotoCondensed-Bold-subset.ttf` (`condensed_bold`) were cut from
  `fonts/roboto/Roboto-Bold.woff2` and `fonts/roboto-condensed/Roboto-Condensed-Bold.woff2`
  in the [roboto-fontface](https://www.npmjs.com/package/roboto-fontface) package 0.10.0
  (Roboto 2.137, Apache-2.0; SHA-256
  `8e44376b735dcc9027acbcc8a0df64c3f886a23529eff27b022f344d719e90f2` and
  `427b7abb7c2f9fda9abd3c60553314237e300f8366c9ec3575cb033c811c1f00`), unpacked with
  `fonttools ttLib.woff2 decompress`.

Each was cut with fontTools 4.67:

```sh
fonttools subset Roboto-Regular.ttf \
  --unicodes="U+0020-007E,U+00A0-017F,U+2010-2027,U+2030-203A,U+20AC,U+2122,U+FFFD" \
  --no-hinting --layout-features='' --drop-tables+=GPOS,GSUB,GDEF,gasp \
  --name-IDs='*' --notdef-outline --output-file=Roboto-Regular-subset.ttf
```

Each subset keeps Basic Latin, Latin-1, Latin Extended-A, common punctuation, the euro
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
references), `jstream-browser` (the browser seam), `serde_json` (setting values)
and `serde` (the glTF loader's typed JSON), `tracing` (what a model file holds that isn't drawn),
`thiserror` (`SourceError`), the pure-Rust decoders `png`, `zune-jpeg` with
`zune-core`, `image-webp` and `gif`, `realfft` (with `rustfft`, scalar only) for the
spectrum's FFT, and for fonts `skrifa` (with `read-fonts`, `font-types`, `bytemuck`,
`yoke` and `once_cell`) and `ab_glyph_rasterizer`, and `jiff` (with `jiff-core`, and on
Windows its bundled `jiff-tzdb`) for the `timer`'s time of day, each with default
features off.
`ttf-parser`, which `ab_glyph` and `fontdue` build on, is unmaintained
(RUSTSEC-2026-0192), so the font crates come from Google's fontations instead
([ADR-0140](../../docs/adr/0140-source-types-and-decoding-scope-v3.md)).

**Tests.** Unit tests cover colour parsing, animation timing, the text outline's
distance transform, the glTF loader (`model/gltf/tests.rs`: GLB and `.gltf` files built
by the test, node transforms and mirroring, materials and textures, refused extensions,
URLs and overflowing accessors, attributes of another length and VEC3 tangents, a fanning hierarchy with full or empty primitives, an accessor or unindexed primitive past the caps, the texture cap before decoding, one read for a file many buffers name or spell differently, and the cap on all buffers together with a `.glb`'s chunk, and file URIs that would leave the model's folder) and the `model` kind (`model/tests.rs`: shapes, live
looks, the stand-in's size, buffers read beside a file and not outside its root), the
`light` kind (`light/tests.rs`: its defaults, every setting changed live, bad values
refused), reading an audio target from settings and taking a YouTube video
ID from an ID or link (`youtube_chat.rs`), and in `source.rs` the
asset checks refuse a file swapped in from outside the root (Unix) and a junction or
symbolic link inside the root that leads outside it (Windows, both when the path is
resolved and in the walk on its own, as a link swapped in after resolving would meet
it; a symbolic link is skipped, with a note, where the test may not create one).
`tests/kinds.rs` snapshots every built-in kind's descriptor (`schema/kinds.snap`, an
`insta` snapshot): adding or changing a kind changes no protocol schema, so this diff is
where a kind's settings are reviewed like an API diff (ADR-0025). `INSTA_UPDATE=always
cargo test -p jstream-sources --test kinds` rewrites it. `jstream-core` snapshots the
same source kinds again, with the audio and video filter kinds (its `schema/kinds.snap`), so a
changed source kind needs `INSTA_UPDATE=always cargo test -p jstream-core --lib kinds`
too. `tests/sources.rs` checks
the registry and runs the colour and image kinds through the public API on files in a
temporary folder: PNG, GIF and lossless WebP files are encoded by the test, and
`tests/data` holds a JPEG, a lossy WebP and an animated WebP made once with Pillow, as
the test's header describes. `tests/text.rs` draws with the bundled font: wrapping,
alignment, padding, outline, live updates, font assets and limits, drawing at a
density (twice the pixels for the same extent, kept across an update, clamped to
`MAX_DENSITY`, lowered to a 4K canvas's pixels, halved to fit the limits, and a short
line filling the canvas drawn at the 13x it is shown at, a bundled face picked live, letter spacing widening a line, and a shadow growing the frame but not the background), plus golden PNGs in `tests/data/text`, one of
them the styled text at a density of 2^0.75 (`JSTREAM_BLESS=1` rewrites them). `tests/backdrop.rs` checks the backdrop's descriptor (all but the size live), that a backdrop is opaque and the same for the same settings, that the light and vignette brighten its pool and darken its edges, that the style and texture set how much it mottles, that a look changes live while a new size restarts it, and that the size is capped at 7680 a side and 7680 × 4320 pixels, a larger one refused naming the side to shrink (#1025). `tests/shape.rs` draws a
rectangle at its exact size, a pill whose corners are clear and whose edge is
antialiased, a circle that is symmetric, linear gradients that run their angle without
banding and mix in linear light, a radial one, and a border inside the edge, and
checks that every setting is live. `tests/timer.rs` checks that a countdown keeps its
frame size when it turns to its `end_text` and a stopwatch past an hour, that a clock has
one size and changes within a second, that `frame` hands out what the painter drew and
`next_change` asks for a paint while it is out of date, that a countdown stops changing
at its end, that an `end_text` too wide for the limits fails `create` and `update`, that
a frame its padding takes past the limits names `end_text` rather than `text`, a
density, a live update that keeps the count, which changes keep the time zero (a
change of `restart` doesn't), and an
unknown time zone; its unit tests check the values in every mode and format, the times
they change at, and the candidates the box is measured by. The unit tests check the shape's distances (a
rectangle's, a pill's corner, an ellipse's axes) and premultiplied mixing. The scope's unit tests check the
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
over URL, setting and backend errors, live CSS, the `local_network` opt-in, the page
API's opt-in and that a page is sent only what its grant covers, closing on
drop, the audio hand-off, and a page that paints on the CPU shown as the source's frame.
`tests/youtube_chat.rs` runs the `youtube_chat` preset the same way: the popout chat
page a video or link opens, a blank page for an empty setting, refused links, the
default CSS and live CSS.
