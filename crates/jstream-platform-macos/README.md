<!-- @doc
id = "CRATE-jstream-platform-macos"
title = "jstream-platform-macos"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-platform-macos

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-platform-macos/src/**", "crates/jstream-platform-macos/Cargo.toml"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-platform-macos", "docs/ARCHITECTURE.md#threading", "docs/adr/0063-macos-platform-support.md", "docs/adr/0061-source-types-and-decoding-scope-v2.md", "docs/adr/0014-media-clock.md", "docs/RULES.md#unsafe", "crates/jstream-platform/README.md#purpose"]
-->

The macOS implementations of the [`jstream-platform`](../jstream-platform/README.md#purpose)
traits, as [ADR-0063](../../docs/adr/0063-macos-platform-support.md) lays them out. It is
built in parts, and the rest follows in later PRs. Layer 2, and one of the crates
allowed `unsafe` ([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001)): every system call is
wrapped here behind safe APIs, with RAII types for what the system hands back
([R-UNSAFE-003](../../docs/RULES.md#R-UNSAFE-003)).

**Bindings are maintained crates** (ADR-0063 §12, [R-UNSAFE-004](../../docs/RULES.md#R-UNSAFE-004)).
Apple's frameworks are called through the objc2 framework crates, at the versions wgpu's
Metal backend already uses (`objc2-foundation`, `-core-foundation`, `-core-media`,
`-core-video`, `-video-toolbox`, `-audio-toolbox`, `-av-foundation`, `-core-audio`, `-core-audio-types`,
`-screen-capture-kit`, `-core-graphics` and `-security` so far, with `objc2`, `block2` and `dispatch2`
for ScreenCaptureKit's classes, blocks and queues), and Mach time through `mach2`. They
carry their generated bindings and link frameworks with `#[link]`, so the crate type-checks from any host with `cargo clippy -p jstream-platform-macos --target aarch64-apple-darwin`; linking and the
macOS tests need a Mac. Everything that calls macOS is under `cfg(target_os = "macos")`.
On other targets the crate builds just its platform-independent helpers, which are tested
on every platform.

It has:
- **`MachClock`** (`MediaClock`): `mach_absolute_time` in **100 ns ticks** (`timebase()`),
  the unit the Windows and Linux backends use. Core Media's host clock, ScreenCaptureKit,
  AVFoundation and Core Audio's host times are on the same time line in Mach ticks, so
  `from_host_time` (and `hns_from_mach`, with the `mach_timebase_info` ratio) maps them.
  `now` never decreases, and doesn't allocate, lock or block.
- **`MacosVersion`**: the running release, read from `NSProcessInfo`, and parsed from
  strings. **`support_tier`** gives its ADR-0063 tier: Apple Silicon on a release Apple
  still patches (`MacosVersion::OLDEST_PATCHED`, 15 today, mirroring the
  `macos-security-updated` fact) is supported; macOS 14, and Intel on any release from
  14.0, are best effort; anything older than 14.0 is not supported (`check_floor`).
  `OLDEST_PATCHED` moves when that fact does. **`Feature::ProcessTaps`** needs 14.2,
  and an older release gets a missing-component error that names the update.
- **`fourcc`**, **`fourcc_str`** and **`os_status`**: four-character codes, and
  `OSStatus` errors shown as their characters or their number. **`cv_pixel_format`** and
  **`pixel_format_from_cv`** map the engine's pixel formats to Core Video's (4:2:0 in
  video range).
- **`vt_encoder_settings`**: what a VideoToolbox compression session is opened with for a
  `VideoEncoderConfig`: H.264 or HEVC, NV12 or BGRA input (P010 for HEVC), CBR or average
  bit rate with a peak, and the hardware encoder enabled or required as the preference
  says. AV1 and other inputs are refused. **`aac_settings`** does the same for an AAC-LC
  `AudioConverter`: mono or stereo, 44.1 or 48 kHz, 32 to 320 kbps.
- **`VideoToolboxCodecs`** (`Codecs`) lists VideoToolbox's H.264 and HEVC encoders
  (`VTCopyVideoEncoderList`) and opens a **VideoToolbox encoder** for a configuration
  `vt_encoder_settings` accepts: a `VTCompressionSession` that is real time, never
  reorders frames (so `dts` is `pts`), uses the High (H.264) or Main/Main10 (HEVC)
  profile at an automatic level, and keeps the keyframe interval. CBR is
  `ConstantBitRate`; the software encoders lack it, so they get an average bit rate with
  a one-second hard limit at the same rate. It takes **CPU frames**, copied into pixel
  buffers from the session's own IOSurface-backed pool, and forces a keyframe on request.
  VideoToolbox's output callback, which catches any panic, converts each sample on its
  own thread and hands it over a channel that `poll` drains without blocking; at most
  four frames are in flight, after which `can_accept` says no. The session's maximum
  frame delay is three, so the encoder can't hold all four while it waits for input that
  `can_accept` keeps out; VideoToolbox's default is no limit. An encoder without the
  property (Apple's software H.264 encoder) keeps its own and may hold all four, so there
  `encode`, once four are queued, completes frames up to the oldest one's `pts`
  (`VTCompressionSessionCompleteFrames`), and its `CodecInfo` name says so (`software,
  oldest frame completed when full`). Either way,
  `encode` may wait for the oldest to be emitted. It also lists and opens
  the **AudioToolbox AAC-LC encoder** for a configuration `aac_settings` accepts: an
  `AudioConverter` from interleaved `f32` PCM (planar input is interleaved first) at
  constant bit rate, at the rate it offers nearest the configured one
  (`nearest_bit_rate`). Its magic cookie must carry the `AudioSpecificConfig` the muxers
  write from the format (`aac_lc_config`, read with `asc_from_magic_cookie`), or opening
  fails. It also opens a **VideoToolbox H.264 or HEVC decoder** for a configuration
  **`vt_decoder_settings`** accepts (NV12 or BGRA output, P010 from HEVC; the hardware
  decoder enabled, required, or avoided as the preference says). Decoders aren't listed:
  VideoToolbox has no list of them. The `VTDecompressionSession` opens on the first
  packet with a complete set of parameter sets, and again whenever a keyframe brings
  different ones; packets before that are dropped. Each packet is decoded synchronously,
  and the output callback, which catches any panic, copies each frame into a **CPU
  frame** in the configured format. Frames come out in presentation order, and `poll`
  never blocks. A packet of another codec is refused, and so is use after `finish`. A
  `finish` that fails still releases the frames already decoded, so a second `finish`
  hands them out.
- **`split_access_unit`**: an Annex B packet as its parameter sets (VPS, SPS, then PPS,
  as Core Media takes them) and a sample of 4-byte length-prefixed NAL units, without
  access unit delimiters (**`annexb_units`** finds the units, and **`sets_complete`**
  says whether the sets make a format description). **`Reorder`** holds decoded frames
  until no later packet can present earlier: up to the newest packet's `dts`, and all of
  them at the end.
- **`AvMediaDecoders`** (`MediaDecoders`) plays local media files through AVFoundation,
  for the cross-platform media-file source in `jstream-sources`: an `AVURLAsset` whose
  first video and audio tracks (loaded with `loadTracksWithMediaType`, waited on) an
  `AVAssetReader` demuxes and decodes. So it plays what AVFoundation does: MP4, MOV and
  M4V, and H.264, HEVC and ProRes video, among others, but not MKV or WebM. Video is read
  as BGRA or NV12 (**`av_reader_settings`**); RGBA, which the source asks for, is read as
  BGRA and its red and blue swapped (**`bgra_to_rgba`**). The first frame is read when
  the file opens, for the size of every frame. Audio is read as interleaved `f32` at the
  track's own rate, mono, or mixed down to stereo by AVFoundation for anything wider
  (**`audio_layout`**). Seeking starts a new reader from the seek target to the end. A
  missing file is `NotFound`, a container or codec AVFoundation doesn't play is
  `Unsupported` (**`av_error_is_unsupported`**), and so is a hardware preference other
  than `Prefer`, since AVFoundation picks the decoder itself. A file whose sound
  AVFoundation won't read in that format (its reader refuses the audio output, or won't
  start with it, as `Unsupported`) opens again without it and plays its picture alone,
  with no audio in its `MediaInfo`, as on Windows. A file of sound alone, opened without
  audio, has nothing to play and is `Unsupported` too.
- **`AacFramer`**: the AAC encoder's input and packet timing, as on Linux and Windows.
  The converter pulls the queued input through its input proc (which catches any panic)
  one raw access unit at a time, with no ADTS. The last unit is padded with silence. Each
  packet's `pts` (and `dts`) is the first buffer's `pts` plus 1024 frames per packet
  before it. The encoder's priming is not subtracted, so the decoded audio lags by it:
  2112 frames on Apple's encoder, against 1024 on `jstream-aac`.
- **`annexb_access_unit`**: VideoToolbox's length-prefixed NAL units as Annex B, with the
  format description's parameter sets (SPS and PPS, and VPS for HEVC) in front of each
  keyframe, as Media Foundation emits them. **`cm_time_parts`** and
  **`media_time_from_cm`** map media-clock times to Core Media's value and timescale and
  back without rounding, and **`copy_rows`** copies a plane between row strides.
- **`ScreenCaptureKitCapture`** (`Capture`) checks the floor when created. `targets`
  lists the displays from `SCShareableContent` as `display:<CGDirectDisplayID>` (named
  `Display <id>`, the main one primary) and the on-screen windows of other applications
  at layer 0 as `window:<CGWindowID>` (named by title, else application), each with its
  size in pixels (the content's points times its scale). `start` captures one through an
  `SCStream` in BGRA, sRGB, with the cursor, at that size; a resized window is scaled to
  fit, keeping its aspect ratio. Frames arrive on the stream's serial dispatch queue,
  whose callbacks catch panics. Each complete frame (`SCStreamFrameInfoStatus`; idle,
  blank and suspended frames are skipped) is converted to NV12, BT.709 limited range,
  with `jstream_platform::nv12` (an odd size loses its last column or row), timed by its
  presentation time on the host clock, and put in the shared `Mailbox`, reusing a frame
  the reader skipped. A stopped stream (the menu bar's Stop, or macOS stopping it)
  makes `latest` report `DeviceLost`. Dropping the stream stops it and waits for its
  queue.
- **The Screen Recording permission**: listing and starting check it first
  (`CGPreflightScreenCaptureAccess`). When it isn't granted, the backend asks macOS once
  (`CGRequestScreenCaptureAccess`, which prompts the first time) and returns a
  `PlatformError::Os` naming System Settings › Privacy & Security › Screen & System
  Audio Recording (Screen Recording on macOS 14); later calls report it without asking
  again, and nothing retries on its own. ScreenCaptureKit's `UserDeclined` maps to the same error.
  `screen_recording_granted` reads the state without prompting. A grant takes effect
  when JStream restarts.
- **`MacosKeychain`** (`StreamKeyStore` and `OAuthTokenStore`): stream keys and OAuth
  refresh tokens as generic-password items in the user's login keychain (ADR-0063 §9),
  through `SecItemAdd`, `SecItemUpdate`, `SecItemCopyMatching` and `SecItemDelete`. Each
  is named by a **`KeychainItem`**: the service `JStream/stream-key` or
  `JStream/oauth-token`, so a key and a token for one ID never share an item, and the ID
  as its account; an empty ID or one with a control character, and an empty secret, are
  `Unsupported`. It is the login keychain, not the data-protection keychain, which needs
  a signed app, and nothing syncs to iCloud. Setting updates the item, or adds it with a
  label Keychain Access shows (`JStream stream key (<id>)`). `is_set` asks for no data,
  so the secret isn't read. A value read back is a `Secret`; the framework's own copies
  are released, not zeroed. A refused or dismissed access prompt (which an unsigned build
  gets again after each rebuild) and a locked keychain get errors that say so; other
  failures carry the framework's message. No error names the secret.
- **`CoreAudioIo`** (`AudioIo`) checks the floor when created. **`devices`** lists
  the HAL's devices (`kAudioHardwarePropertyDevices`) under their persistent UID
  (`kAudioDevicePropertyDeviceUID`), once for each direction they have channels in, each
  with its name and whether it is that direction's system default. **`open_input`**
  records the default input, followed when the system default changes, or one input by
  UID. It plans the device first: every input stream's virtual format must be
  native-endian `f32` PCM and the nominal rate a whole number of hertz, or it reports
  `Unsupported`. One channel is mono and two are stereo; any other count is recorded as
  stereo from the device's preferred stereo pair
  (`kAudioDevicePropertyPreferredChannelsForStereo`, else its first two channels), 5.1
  included. The other channels are not mixed in: this selects a pair, unlike the downmix
  [jstream-platform](../jstream-platform/README.md#open-questions) proposes for other
  layouts. It never resamples. An IOProc on the device (`AudioDeviceCreateIOProcID`) runs on Core
  Audio's real-time IO thread: it interleaves each cycle's buffers into a buffer
  allocated before the start, in chunks of that buffer (at least 512 frames, or the
  device's IO buffer size), and hands each chunk to the handler without allocating,
  locking or logging, catching any panic. A packet's device position counts frames from
  the stream's first sample time (`mSampleTime`), so lost frames show as a jump, flagged
  as a discontinuity; its time is the cycle's host time on `MachClock`, plus the chunk's
  offset. A watcher thread per stream, woken by HAL property listeners (the default
  input, and the device's liveness, nominal rate and input configuration) and every
  500 ms, stops the IOProc before changing anything: it moves a default stream to a new
  default with the same format (positions carry on, flagged as a discontinuity) and ends
  a stream whose device is gone or changed format with `DeviceLost`. `input_format`
  plans the same way without opening. **The Microphone permission**: without it macOS
  would record silence, so `open_input` checks it (`AVCaptureDevice`'s authorization
  status for audio) once the device is found and its format planned: an unknown device
  is `NotFound` without a prompt. When macOS hasn't asked yet, the backend asks once
  (`requestAccessForMediaType:`, which prompts) and returns a `PlatformError::Os` naming
  System Settings › Privacy & Security › Microphone; a refused permission gets that error
  without asking again, and a restricted one (a configuration profile or Screen Time) an
  error saying whoever manages the Mac has to allow it. A grant counts from the next
  open; nothing retries on its own. Listing devices and `input_format` need no
  permission. The prompt's usage string is in the daemon's embedded `Info.plist`
  ([jstream-daemon](../../apps/jstream-daemon/README.md#purpose)).
  **`open_output`** plays on the default output, followed when the system default
  changes, or one output by UID, with no permission. It is planned and watched the same
  way, from the output streams' formats and the output's preferred stereo pair. Each
  IOProc cycle clears the device's output buffers, then has the handler fill a buffer
  allocated before the start, in the same chunks, each with its device position (frames
  from the stream's first output sample time) and the host time its first frame is
  expected to play, on `MachClock`. The handler's channels go to the planned device
  channels, and the rest stay silent; a buffer list the plan doesn't match plays
  silence until the watcher re-plans. Desktop and application audio are refused before
  14.2 (`check_input`) and, like `processes`, report `Unsupported` for now.

Not here yet, in roughly the order they follow:
- capture frames as IOSurfaces imported into Metal and wgpu (GPU frames), once a spike
  proves the synchronisation;
- process taps for desktop and application audio;
- IOSurface encode targets (GPU frames) for the VideoToolbox encoder once the Metal
  spike lands;
- AVFoundation webcams;
- GPU frames (IOSurface pixel buffers imported into Metal) from the VideoToolbox decoder
  and the media-file reader, once the Metal spike lands.

The virtual camera is out of scope (`Backends::vcam` is `None` on macOS) until its own
ADR.

Tests run on every platform where they can: the capture's target IDs, names, sizes, frame
layouts and statuses, permission steps, the keychain item names and the error mapping are
platform independent. The macOS ones (the clock, reading the release, encoding with
VideoToolbox, which falls back to Apple's software encoder on a Mac without a hardware
one, decoding what the VideoToolbox encoder produced, from the start and from a later
keyframe, playing a system sound as an audio-only media file and refusing a missing or
unreadable one, encoding AAC with AudioToolbox, opening the capture backend, round trips through
the login keychain with uniquely named items, deleted afterwards, listing audio
devices and their input formats, refusing an unknown output, and reading the Microphone
status without prompting) need no permission, so they run in the `macos` CI job. The
`hw-tests` feature is for tests that need the Screen Recording, Camera, Microphone or
system audio recording permission (listing and capturing a display, the
missing-permission error, and recording the default input), speakers (playing a tone on
the default output), or the hardware encoders, which run by hand on a Mac. So do the
media-file tests that need a video file, which play the one `JSTREAM_TEST_MEDIA` names.
