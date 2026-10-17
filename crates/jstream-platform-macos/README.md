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
`-core-video` and `-video-toolbox` so far), and Mach time through `mach2`. They carry their generated
bindings and link frameworks with `#[link]`, so the crate type-checks from any host with
`cargo clippy -p jstream-platform-macos --target aarch64-apple-darwin`; linking and the
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
  four frames are in flight, after which `can_accept` says no. The AAC converter and
  decoding report `Unsupported`.
- **`annexb_access_unit`**: VideoToolbox's length-prefixed NAL units as Annex B, with the
  format description's parameter sets (SPS and PPS, and VPS for HEVC) in front of each
  keyframe, as Media Foundation emits them. **`cm_time_parts`** and
  **`media_time_from_cm`** map media-clock times to Core Media's value and timescale and
  back without rounding, and **`copy_rows`** copies a plane between row strides.
- **`ScreenCaptureKitCapture`** (`Capture`) and **`CoreAudioIo`** (`AudioIo`) check the
  floor when created. Listing and starting report `Unsupported` for now. `CoreAudioIo`
  first refuses desktop and application audio before 14.2 (`check_input`).

Not here yet, in roughly the order they follow:
- ScreenCaptureKit targets from `SCShareableContent`, the Screen Recording permission
  check and request, and capture streams (CPU frames first, then IOSurface into Metal
  and wgpu once a spike proves the synchronisation);
- Core Audio devices and streams, and process taps for desktop and application audio;
- the AudioToolbox AAC converter, and IOSurface encode targets (GPU frames) for the
  VideoToolbox encoder once the Metal spike lands;
- AVFoundation webcams;
- the Keychain stream-key and token store;
- video decode and the media-file source.

The virtual camera is out of scope (`Backends::vcam` is `None` on macOS) until its own
ADR.

Tests run on every platform where they can. The macOS ones (the clock, reading the
release, and encoding with VideoToolbox, which falls back to Apple's software encoder on a
Mac without a hardware one) need no permission, so they run in the `macos` CI job. The
`hw-tests` feature is for tests that need the Screen Recording, Camera, Microphone or
system audio recording permission, or the hardware encoders, which run by hand on a Mac.
