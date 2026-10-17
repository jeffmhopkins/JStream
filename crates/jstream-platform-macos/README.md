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
traits, as [ADR-0063](../../docs/adr/0063-macos-platform-support.md) lays them out. This
crate is its first part, and the rest follows in later PRs. Layer 2, and one of the crates
allowed `unsafe` ([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001)): every system call is
wrapped here behind safe APIs, with RAII types for what the system hands back
([R-UNSAFE-003](../../docs/RULES.md#R-UNSAFE-003)).

**Bindings are maintained crates** (ADR-0063 §12, [R-UNSAFE-004](../../docs/RULES.md#R-UNSAFE-004)).
Apple's frameworks are called through the objc2 framework crates, at the versions wgpu's
Metal backend already uses, and Mach time through `mach2`. They carry their generated
bindings and link frameworks with `#[link]`, so the crate type-checks from any host with
`cargo clippy -p jstream-platform-macos --target aarch64-apple-darwin`; linking and the
macOS tests need a Mac. Everything that calls macOS is under `cfg(target_os = "macos")`.
On other targets the crate builds just its platform-independent helpers, which are tested
on every platform.

This first part has:
- **`MachClock`** (`MediaClock`): `mach_absolute_time` in **100 ns ticks** (`timebase()`),
  the unit the Windows and Linux backends use. Core Media's host clock, ScreenCaptureKit,
  AVFoundation and Core Audio's host times are on the same time line in Mach ticks, so
  `from_host_time` (and `hns_from_mach`, with the `mach_timebase_info` ratio) maps them.
  `now` never decreases, and doesn't allocate, lock or block.
- **`MacosVersion`**: the running release, read from `NSProcessInfo`, and parsed from
  strings. **`support_tier`** gives its ADR-0063 tier: Apple Silicon at or above macOS
  14.0 is supported, Intel is best effort, anything older is not supported
  (`check_floor`). **`Feature::ProcessTaps`** needs 14.2, and an older release gets a
  missing-component error that names the update.
- **`fourcc`**, **`fourcc_str`** and **`os_status`**: four-character codes, and
  `OSStatus` errors shown as their characters or their number. **`cv_pixel_format`** and
  **`pixel_format_from_cv`** map the engine's pixel formats to Core Video's (4:2:0 in
  video range).
- **`vt_encoder_settings`**: what a VideoToolbox compression session is opened with for a
  `VideoEncoderConfig`: H.264 or HEVC, NV12 or BGRA input (P010 for HEVC), CBR or average
  bit rate with a peak, and the hardware encoder enabled or required as the preference
  says. AV1 and other inputs are refused. **`aac_settings`** does the same for an AAC-LC
  `AudioConverter`: mono or stereo, 44.1 or 48 kHz, 32 to 320 kbps.
- **`VideoToolboxCodecs`** (`Codecs`) checks a configuration with those, then reports
  `Unsupported` until the sessions are built; listing codecs and decoding report
  `Unsupported` too.
- **`ScreenCaptureKitCapture`** (`Capture`) and **`CoreAudioIo`** (`AudioIo`) check the
  floor when created. Listing and starting report `Unsupported` for now. `CoreAudioIo`
  first refuses desktop and application audio before 14.2 (`check_input`).

Not here yet, in roughly the order they follow:
- ScreenCaptureKit targets from `SCShareableContent`, the Screen Recording permission
  check and request, and capture streams (CPU frames first, then IOSurface into Metal
  and wgpu once a spike proves the synchronisation);
- Core Audio devices and streams, and process taps for desktop and application audio;
- the VideoToolbox H.264 and HEVC sessions (with Annex B conversion) and the AudioToolbox
  AAC converter;
- AVFoundation webcams;
- the Keychain stream-key and token store;
- video decode and the media-file source.

The virtual camera is out of scope (`Backends::vcam` is `None` on macOS) until its own
ADR.

Tests run on every platform where they can. The macOS ones (the clock and reading the
release) need no permission. The `hw-tests` feature is reserved for tests that need the
Screen Recording, Camera, Microphone or system audio recording permission, or the hardware
encoders, which run by hand on a Mac.
