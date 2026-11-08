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
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-platform-macos", "docs/ARCHITECTURE.md#threading", "docs/adr/0063-macos-platform-support.md", "docs/adr/0144-the-desktop-app-on-macos.md", "docs/adr/0140-source-types-and-decoding-scope-v3.md", "docs/adr/0014-media-clock.md", "docs/RULES.md#unsafe", "crates/jstream-platform/README.md#purpose"]
-->

The macOS implementations of the [`jstream-platform`](../jstream-platform/README.md#purpose)
traits, as [ADR-0063](../../docs/adr/0063-macos-platform-support.md) lays them out, built
in parts. Layer 2, and one of the crates allowed `unsafe`
([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001)): every system call is wrapped here
behind safe APIs, with RAII types for what the system hands back
([R-UNSAFE-003](../../docs/RULES.md#R-UNSAFE-003)).

This README is the crate's map. How each backend behaves (formats, timing, permissions,
error mapping, threads) is in its module's rustdoc, next to the code:
`cargo doc -p jstream-platform-macos --target aarch64-apple-darwin --document-private-items --open`.

**Bindings are maintained crates** (ADR-0063 §12, [R-UNSAFE-004](../../docs/RULES.md#R-UNSAFE-004)).
Apple's frameworks are called through the objc2 framework crates, at the versions wgpu's
Metal backend (and, for AppKit, Tauri's window) already uses, with `objc2`, `block2` and `dispatch2` for classes, blocks and
queues, and Mach time and the thread list through `mach2`. They carry their generated
bindings and link frameworks with `#[link]`. Carbon's Event Manager, for the hotkeys, has
no such crate, so `hotkeys.rs` declares its few functions itself
(ADR-0144 §2). The crate type-checks from any host with
`cargo clippy -p jstream-platform-macos --target aarch64-apple-darwin`; linking and the
macOS tests need a Mac. Everything that calls macOS is under `cfg(target_os = "macos")`;
on other targets the crate builds just its platform-independent helpers, which are
tested on every platform.

| Type | Trait | Module | What it is |
|---|---|---|---|
| `MachClock` | `MediaClock` | `clock.rs` | `mach_absolute_time` in 100 ns ticks, the unit every platform uses |
| `MacosVersion`, `support_tier` | | `version.rs` | the running release and its ADR-0063 tier; `OLDEST_PATCHED` mirrors the `macos-security-updated` fact |
| `MacosProcessCpu`, `MacosThreadCpu` | `ProcessCpu`, `ThreadCpu` | `process.rs` | the process's and each thread's CPU time |
| `VideoToolboxCodecs` | `Codecs` | `codec.rs`, `videotoolbox.rs`, `annexb.rs` | VideoToolbox H.264 and HEVC encoders on CPU frames (`vt_encoder_settings`), Annex B out |
| (AAC encoder), `AacFramer` | `AudioEncoder` | `audiotoolbox.rs`, `aac.rs` | AudioToolbox AAC-LC (`aac_settings`), framed and timed as on the other platforms |
| (H.264 and HEVC decoder) | | `vtdecoder.rs`, `decode.rs` | VideoToolbox decoding on CPU frames, packets split by `split_access_unit`, frames ordered by `Reorder` |
| `AvMediaDecoders` | `MediaDecoders` | `avreader.rs`, `media_file.rs` | local media files through AVFoundation's asset reader (MP4, MOV, M4V; not MKV or WebM) |
| `ScreenCaptureKitCapture` | `Capture` | `capture/` | display and window capture through ScreenCaptureKit, behind the Screen Recording permission |
| `MacosKeychain` | `StreamKeyStore`, `OAuthTokenStore` | `keychain.rs` | generic-password items in the login keychain (ADR-0063 §9, [R-SEC-001](../../docs/RULES.md#R-SEC-001)); nothing syncs to iCloud |
| `CoreAudioIo` | `AudioIo` | `audio/` | Core Audio devices, input (behind the Microphone permission), output, and desktop and application audio through process taps (macOS 14.2) |
| `AvFoundationVideoDevices` | `VideoDevices` | `video_device/` | cameras through AVFoundation, behind the Camera permission (ADR-0063 §10); no camera controls yet (the trait's default, #1196) |
| `NoGpuInterop` | `GpuInterop` | `unbuilt.rs` | a stand-in until the Metal interop exists, so the engine takes the CPU frame paths |
| `GlobalHotkeys` | | `hotkeys.rs` | the desktop app's hotkeys while another app has the focus ([ADR-0112](../../docs/adr/0112-assignable-camera-zoom-bindings.md) §5): Carbon's `RegisterEventHotKey` for keys, on the main thread, and a Quartz event tap for wheel chords, on its own thread while one is set, behind the Accessibility permission; the key table and the wheel's notch counting are pure |
| `pick_path`, `Pick` | | `dialog.rs` | the desktop app's Browse dialogs: AppKit's open and save panels, app-modal on the main thread ([ADR-0144](../../docs/adr/0144-the-desktop-app-on-macos.md)) |

Each permission is asked for once (macOS prompts the first time); after that a missing
grant is a `PlatformError::Os` naming the System Settings pane, and nothing retries on its
own.

Not here yet, in roughly the order they follow:
- capture frames as IOSurfaces imported into Metal and wgpu (GPU frames), once a spike
  proves the synchronisation, in place of `NoGpuInterop`;
- IOSurface encode targets (GPU frames) for the VideoToolbox encoder once the Metal
  spike lands;
- GPU frames (IOSurface pixel buffers imported into Metal) from the VideoToolbox decoder
  and the media-file reader, once the Metal spike lands.

The virtual camera is out of scope (`Backends::vcam` is `None` on macOS) until its own
ADR.

**Tests.** Everything that doesn't call macOS (capture targets and frame layouts,
permission steps, keychain item names, error mapping, the decoder list, the hotkeys' key
table and wheel counting, the panels' extension and path checks) is tested on
every platform. The macOS tests that need no permission run in the `macos` CI job. The
`hw-tests` feature holds the ones that need the Screen Recording, Camera, Microphone or
system audio recording permission, speakers, or the hardware encoders, run by hand on a
Mac, and the media-file tests that need a video file play the one `JSTREAM_TEST_MEDIA`
names.
