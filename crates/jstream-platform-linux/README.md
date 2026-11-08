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
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-platform-linux", "docs/ARCHITECTURE.md#threading", "docs/adr/0060-linux-platform-support.md", "docs/adr/0061-source-types-and-decoding-scope-v2.md", "docs/adr/0062-aac-lc-encoder-patent-check.md", "docs/adr/0014-media-clock.md", "docs/RULES.md#unsafe", "crates/jstream-platform/README.md#purpose", "crates/jstream-aac/README.md#purpose", "docs/adr/0071-linux-secret-store.md"]
-->

The Linux implementations of the [`jstream-platform`](../jstream-platform/README.md#purpose)
traits, as [ADR-0060](../../docs/adr/0060-linux-platform-support.md) lays them out. Layer 2,
and one of the crates allowed `unsafe` ([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001)):
every system call is wrapped here behind safe APIs, with RAII types for what the system
hands back ([R-UNSAFE-003](../../docs/RULES.md#R-UNSAFE-003)).

This README is the crate's map. How each backend behaves (formats, timing, error
mapping, threads) is in its module's rustdoc, next to the code:
`cargo doc -p jstream-platform-linux --document-private-items --open`.

**System libraries are loaded at run time** (ADR-0060 §8). libpipewire, libva and
libasound are opened with `dlopen` through `libloading` when a backend first needs them,
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
| `V4l2Devices` | `VideoDevices` | `v4l2/`, `pairing.rs` | webcams and capture cards, paired with their microphones, on the CPU or GPU path |
| `LinuxBackends` | | `backends.rs` | the backend set, with ADR-0060 §4's audio choice made once |
| `NoMediaDecoders` | `MediaDecoders` | `unbuilt.rs` | refuses every file, naming [ADR-0072](../../docs/adr/0072-linux-media-file-decoding.md) |
| `LinuxProcessCpu`, `LinuxThreadCpu` | `ProcessCpu`, `ThreadCpu` | `process.rs` | the process's and each thread's CPU time |
| `VaapiCodecs` | `Codecs` | `vaapi/`, `codec.rs` | VA-API H.264 and HEVC encoders, on CPU frames or the context's encode targets |
| `OpenH264Store`, `OpenH264Encoder` | `VideoEncoder` | `openh264/` | Cisco's OpenH264, the software H.264 encoder, downloaded only on the user's request and checked against its pin ([ADR-0061](../../docs/adr/0061-source-types-and-decoding-scope-v2.md)) |
| `RustAacEncoder` | `AudioEncoder` | `aac.rs` | [`jstream-aac`](../jstream-aac/README.md#purpose) behind the `aac-encoder` feature, within [ADR-0062](../../docs/adr/0062-aac-lc-encoder-patent-check.md)'s limits |

JStream never ships or builds OpenH264. The stored file's SHA-256 is checked again before
each `dlopen`, and a copy from anywhere else is never loaded.

Not here yet:
- media-file decoders (ADR-0072), for which `NoMediaDecoders` stands in;
- an app keeping its OAuth tokens in `LinuxSecrets` (the daemon keeps its stream keys
  there), the protocol field that tells the UI the secret store's fallback is active,
  and the `jstream.toml` setting that turns the fallback off (ADR-0071 §4, §5).

The virtual camera is out of scope (`Backends::vcam` is `None` on Linux).

**Tests.** Everything that doesn't call Linux (D-Bus messages, the portal handshake, SPA
pods, VA-API and V4L2 layouts, parameter sets, the audio graph planning, the secret
store's formats) is tested on every platform. The Linux tests pass with or without the
system libraries and need no GPU, session or daemon; GPU tests run on a software Vulkan
adapter where there is no GPU. Three kinds are opt-in:
- the `hw-tests` feature holds `#[ignore]`d tests run by hand against a real desktop,
  GPU, PipeWire daemon, Secret Service, sound card or webcam;
- the `openh264-download` feature and `--ignored` run the one test that downloads from
  Cisco, which is how the pin is recorded (CI's `e2e-stream` job runs it);
- DMA-BUF import and export need a GPU driver, so they are covered only by `hw-tests`.
