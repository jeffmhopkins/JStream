<!-- @doc
id = "CRATE-jstream-platform-linux"
title = "jstream-platform-linux"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-platform-linux

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-platform-linux/src/**", "crates/jstream-platform-linux/Cargo.toml"]
depends_on = ["docs/adr/0072-linux-media-file-decoding.md", "docs/ARCHITECTURE.md#crate-map/jstream-platform-linux", "docs/ARCHITECTURE.md#threading", "docs/adr/0060-linux-platform-support.md", "docs/adr/0140-source-types-and-decoding-scope-v3.md", "docs/adr/0062-aac-lc-encoder-patent-check.md", "docs/adr/0014-media-clock.md", "docs/RULES.md#unsafe", "crates/jstream-platform/README.md#purpose", "crates/jstream-aac/README.md#purpose", "docs/adr/0071-linux-secret-store.md", "docs/adr/0141-desktop-app-on-linux.md", "docs/adr/0156-linux-virtual-camera-through-v4l2loopback.md", "docs/adr/0159-opus-decodes-in-pure-rust-on-linux.md"]
-->

The Linux implementations of the [`jstream-platform`](../jstream-platform/README.md#purpose)
traits, as [ADR-0060](../../docs/adr/0060-linux-platform-support.md) lays them out. Layer 2,
and one of the crates allowed `unsafe` ([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001)):
every system call is wrapped here behind safe APIs, with RAII types for what the system
hands back ([R-UNSAFE-003](../../docs/RULES.md#R-UNSAFE-003)).

This README is the crate's map. How each backend behaves (formats, timing, error
mapping, threads) is in its module's rustdoc, next to the code:
`cargo doc -p jstream-platform-linux --document-private-items --open`.

**System libraries are loaded at run time** (ADR-0060 §8). libpipewire, libva,
libasound and libxcb are opened with `dlopen` through `libloading` when a backend first needs them,
and only the functions the crate calls are declared, each with its C signature in a
`SAFETY` comment. Nothing is linked, so the crate builds and tests on any machine, CI
included, without their headers. This departs from
[R-UNSAFE-004](../../docs/RULES.md#R-UNSAFE-004)'s preference for the `pipewire` crate
for the reason ADR-0060 gives: that crate needs libpipewire's headers and libclang
wherever the workspace builds. A missing or too-old library is
`PlatformError::MissingComponent`, with the package to install. Everything that calls
Linux is under `cfg(target_os = "linux")`; on other targets the crate builds just its
platform-independent helpers, which are tested on every platform.

| Type | Trait | Module | What it is |
|---|---|---|---|
| `MonotonicClock` | `MediaClock` | `clock.rs` | `CLOCK_MONOTONIC` in 100 ns ticks, the unit every platform uses |
| `Session` | | `session.rs` | Wayland, X11 or headless, the desktop, and its ADR-0060 support tier |
| `PortalCapture` | `Capture` | `portal/`, `pipewire/stream.rs` | display and window capture through the ScreenCast portal's picker and a PipeWire video stream, with restore tokens kept per source |
| (D-Bus client) | | `dbus.rs` | the crate's own minimal D-Bus client; received messages are untrusted input |
| `LinuxGpu` | `GpuInterop` | `gpu/` | the engine's wgpu device on Vulkan, DMA-BUF import of captures and webcams, and NV12 encode targets exported for VA-API |
| `LinuxSecrets` | `StreamKeyStore`, `OAuthTokenStore` | `secrets.rs` | the Secret Service, else a `0600` file ([ADR-0071](../../docs/adr/0071-linux-secret-store.md), [R-SEC-001](../../docs/RULES.md#R-SEC-001)) |
| `PipeWireAudio` | `AudioIo` | `pipewire/` | devices, input, output and loopback on PipeWire (ADR-0060 §4) |
| `AlsaAudio` | `AudioIo` | `alsa.rs` | the fallback without a PipeWire daemon; no loopback |
| `V4l2Devices` | `VideoDevices` | `v4l2/`, `pairing.rs` | webcams and capture cards, paired with their microphones, on the CPU or GPU path, and their camera controls (`VIDIOC_QUERYCTRL`, `VIDIOC_S_CTRL`) |
| `LoopbackVcam` | `VcamSink` | `v4l2/loopback.rs`, `vcam.rs` | the virtual camera: I420 frames written to a v4l2loopback device the user has loaded, one labelled `JStream` first, left out of the webcam list while it writes ([ADR-0156](../../docs/adr/0156-linux-virtual-camera-through-v4l2loopback.md)) |
| `LinuxBackends` | | `backends.rs` | the backend set, with ADR-0060 §4's audio choice made once |
| `LinuxMediaDecoders` | `MediaDecoders` | `media.rs`, `media/` | local media files as [ADR-0072](../../docs/adr/0072-linux-media-file-decoding.md) lays out. So far: WAV, FLAC, Ogg Vorbis and MP3 files through symphonia's pure-Rust readers and decoders, and Ogg Opus files through symphonia's reader and `rusty-opus` ([ADR-0159](../../docs/adr/0159-opus-decodes-in-pure-rust-on-linux.md)), mixed down to stereo where wider, and MP4, MOV, Matroska and WebM files through the crate's own demuxers (`media/mp4.rs`, `media/mkv.rs`), their H.264 video decoded on the GPU through VA-API (`vaapi/decoder.rs`, from the crate's own H.264 header parser and reference planner, `media/h264/`) and their MP3, FLAC, Vorbis or Opus audio through symphonia or `rusty-opus`. Any other container or codec, such as WebM's VP8, VP9 or AV1, is `Unsupported`, naming it; AAC waits for ADR-0072 (g)'s decoder patent check, so a movie with AAC audio plays without sound |
| `portal_pick_path`, `FileChoice` | | `portal/file_chooser.rs` | the desktop app's Browse dialogs through the FileChooser portal: open, save or folder, answered only by the portal's own bus connection, a local `file://` path or nothing ([ADR-0141](../../docs/adr/0141-desktop-app-on-linux.md)) |
| `GlobalHotkeys` | | `hotkeys.rs` | X11 only: the collection's bound chords heard while another app has the focus, grabbed on the root window with `xcb_grab_key`, and wheel chords as buttons 4 and 5 with `xcb_grab_button` ([ADR-0112](../../docs/adr/0112-assignable-camera-zoom-bindings.md) §5 and §6); Wayland has no grab |
| `PortalHotkeys` | | `portal/global_shortcuts.rs` | Wayland's global hotkeys: the GlobalShortcuts portal binds one shortcut per chord (its id the chord, its preferred trigger the same keys in the shortcuts format) and reports each activation, answered only by the portal's own bus connection; wheel chords can't be shortcuts ([ADR-0112](../../docs/adr/0112-assignable-camera-zoom-bindings.md) §5) |
| `LinuxProcessCpu`, `LinuxThreadCpu` | `ProcessCpu`, `ThreadCpu` | `process.rs` | the process's and each thread's CPU time |
| `VaapiCodecs` | `Codecs` | `vaapi/`, `codec.rs` | VA-API H.264 and HEVC encoders, on CPU frames or the context's encode targets |
| `OpenH264Store`, `OpenH264Encoder`, `OpenH264Decoder` | `VideoEncoder`, `VideoDecoder` | `openh264/` | Cisco's OpenH264, the software H.264 encoder and the decoder of the engine's own H.264 streams (the quality probe's), downloaded only on the user's request and checked against its pin ([ADR-0140](../../docs/adr/0140-source-types-and-decoding-scope-v3.md)) |
| `RustAacEncoder` | `AudioEncoder` | `aac.rs` | [`jstream-aac`](../jstream-aac/README.md#purpose) behind the `aac-encoder` feature, within [ADR-0062](../../docs/adr/0062-aac-lc-encoder-patent-check.md)'s limits |

JStream never ships or builds OpenH264. The stored file's SHA-256 is checked again before
each `dlopen`, and a copy from anywhere else is never loaded.

Not here yet:
- the rest of ADR-0072's media-file decoders: video codecs other than H.264 (VP9 and
  AV1 on VA-API among them), compressed Matroska tracks other than header
  stripping, H.264 field pictures and 10-bit or 4:2:2 streams, and the system's
  FFmpeg as the fallback behind its two gates;
- an app keeping its OAuth tokens in `LinuxSecrets` (the daemon keeps its stream keys
  there), the protocol field that tells the UI the secret store's fallback is active,
  and the `jstream.toml` setting that turns the fallback off (ADR-0071 §4, §5).

JStream never loads v4l2loopback or runs anything as root: a camera start without a
writable device fails with the commands that install and load it (ADR-0156).

**Tests.** Everything that doesn't call Linux (D-Bus messages, the portal handshake, SPA
pods, VA-API and V4L2 layouts, parameter sets, the audio graph planning, the secret
store's formats) is tested on every platform. The Linux tests pass with or without the
system libraries and need no GPU, session or daemon; GPU tests run on a software Vulkan
adapter where there is no GPU. Three kinds are opt-in:
- the `hw-tests` feature holds `#[ignore]`d tests run by hand against a real desktop,
  GPU, PipeWire daemon, Secret Service, sound card or webcam;
- the `openh264-download` feature and `--ignored` run the one test that downloads from
  Cisco, which is how the pin is recorded, and round-trips a stream through Cisco's
  encoder and decoder, luma and both chroma planes (CI's `e2e-stream` job runs it);
- DMA-BUF import and export need a GPU driver, so they are covered only by `hw-tests`.

The virtual camera's `hw-tests` test needs v4l2loopback loaded and FFmpeg; CI's
`vcam-linux` job loads the module on its runner and runs it (`-- --ignored v4l2::loopback::`).
