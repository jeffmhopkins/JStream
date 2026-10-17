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
  four frames are in flight, after which `can_accept` says no. It also lists and opens
  the **AudioToolbox AAC-LC encoder** for a configuration `aac_settings` accepts: an
  `AudioConverter` from interleaved `f32` PCM (planar input is interleaved first) at
  constant bit rate, at the rate it offers nearest the configured one
  (`nearest_bit_rate`). Its magic cookie must carry the `AudioSpecificConfig` the muxers
  write from the format (`aac_lc_config`, read with `asc_from_magic_cookie`), or opening
  fails. Decoding reports `Unsupported`.
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
  silence until the watcher re-plans. **`processes`** lists, on 14.2 and later, the
  processes Core Audio knows as audio clients (`kAudioHardwarePropertyProcessObjectList`),
  which are the ones a process tap can record, not every process as on Windows. Each has
  its PID, its parent's PID and its executable's file name, read from the kernel by PID
  (`proc_pidpath`, and `proc_pidinfo`'s short BSD info, which the kernel gives for other
  users' processes such as system daemons too), falling back to the kernel's short name,
  the bundle ID, then the PID; one that exits while being read is left out. Before 14.2 it is
  `MissingComponent` naming the update. **Desktop and application audio** are
  `MissingComponent` before 14.2 too (`check_input`); from 14.2 they record a Core
  Audio process tap (`CATapDescription`, `AudioHardwareCreateProcessTap`), always a
  stereo mixdown, through a private aggregate device the stream makes and destroys. The
  aggregate is clocked by the tapped output, or by the default output, and records the
  tap as its last two input channels with the same IOProc and watcher as an input. It
  is left out of the device list. `DefaultOutputLoopback` taps every process except
  JStream's own, on any output, so JStream's monitoring isn't recorded back;
  `OutputLoopback` taps what every process except JStream plays on that device's first
  output stream; `ProcessLoopback` taps the one process, which must be one `processes`
  lists (else `NotFound`). Its child processes aren't included, unlike Windows' process
  loopback of a process tree. The stream ends with `DeviceLost` when the aggregate
  stops working, the output it is clocked by goes (the tapped one, or for desktop and
  application audio the default output when the tap opened, even if the aggregate
  lives on), or the tapped process exits. No Microphone
  permission is checked: a tap needs the system audio recording permission, which has
  no public API to read. macOS asks when a tap first runs, and without the grant the
  tap records silence. JStream accepts that, with no guessing from the samples and no
  reading of the private TCC database, and logs a hint naming System Settings ›
  Privacy & Security › Screen & System Audio Recording when a tap opens.
  `input_format` for a tap makes one for a moment to read its format.
- **`AvFoundationVideoDevices`** (`VideoDevices`, ADR-0063 §10) checks the floor when
  created. **`devices`** lists the built-in, Continuity and external (USB) cameras an
  `AVCaptureDeviceDiscoverySession` finds, under their `uniqueID`s. A camera's `audio`
  is the one audio capture device with the same `modelID`, whose `uniqueID` is its Core
  Audio UID, so `CoreAudioIo` opens it. A USB webcam's camera and microphone share a
  model; a Mac's built-in ones don't, so they stay unpaired, as does a model two audio
  devices share. **`formats`** lists, in the camera's order, one entry per format and
  frame-rate range at that range's highest rate, each `Raw(Nv12)`: AVFoundation
  converts every native format, and decodes MJPEG itself, to the NV12 the backend asks
  for. A camera's YUY2 and MJPEG formats of the same size and rate are therefore one
  entry, and no format needs a `CaptureDecoder`. **`open`** checks the Camera permission
  the way `open_input` checks the Microphone one: it asks once, then returns a
  `PlatformError::Os` naming System Settings › Privacy & Security › Camera. Listing
  needs no permission. It then runs an `AVCaptureSession` with the camera's input, its
  active format and frame duration set to the chosen ones, and an
  `AVCaptureVideoDataOutput` that asks for NV12 in video range and drops late frames.
  Each frame is copied out on the output's serial queue into the capture mailbox,
  reusing a frame the reader skipped. Its `pts` is the sample's presentation time on
  `MachClock`, and its colour is BT.601 below 720 lines and BT.709 from there, in
  limited range, as on Windows. A watcher thread checks every 500 ms that the camera is
  still connected and the session still running, and otherwise the stream reports
  `DeviceLost`.

Not here yet, in roughly the order they follow:
- capture frames as IOSurfaces imported into Metal and wgpu (GPU frames), once a spike
  proves the synchronisation;
- IOSurface encode targets (GPU frames) for the VideoToolbox encoder once the Metal
  spike lands;
- video decode and the media-file source.

The virtual camera is out of scope (`Backends::vcam` is `None` on macOS) until its own
ADR.

Tests run on every platform where they can: the capture's target IDs, names, sizes, frame
layouts and statuses, permission steps, process names and parents, the keychain item
names and the error mapping are platform independent. The macOS ones (the clock, reading the release, encoding with
VideoToolbox, which falls back to Apple's software encoder on a Mac without a hardware
one, encoding AAC with AudioToolbox, opening the capture backend, round trips through
the login keychain with uniquely named items, deleted afterwards, listing audio
devices and their input formats, refusing an unknown output, reading the Microphone
status without prompting, listing processes, refusing to tap an unknown output or
process, and listing cameras and their formats) need no permission, so they run in the `macos` CI job. The `hw-tests` feature
is for tests that need the Screen Recording, Camera, Microphone or system audio
recording permission (listing and capturing a display, the missing-permission error,
recording the default input, and recording desktop audio, the default output and the
test's own process playing a tone through taps, and capturing NV12 frames from the
first camera), speakers (playing a tone on the default output), or the hardware
encoders, which run by hand on a Mac.
