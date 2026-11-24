<!-- @doc
id = "CRATE-jstream-platform-windows"
title = "jstream-platform-windows"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-platform-windows

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-platform-windows/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-platform-windows", "docs/ARCHITECTURE.md#threading", "docs/adr/0140-source-types-and-decoding-scope-v3.md", "docs/adr/0014-media-clock.md", "docs/adr/0066-portable-windows-distribution-v2.md", "docs/RULES.md#unsafe", "docs/RULES.md#real-time", "crates/jstream-platform/README.md#purpose"]
-->

The Windows implementations of the [`jstream-platform`](../jstream-platform/README.md#purpose)
traits, ported from spikes S1 to S3. Layer 2, and one of the crates allowed `unsafe`
([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001)): every OS call is wrapped here, behind
the traits' safe APIs (and the credential store's own), with RAII types for COM objects,
events, wave formats and credentials Windows returns
([R-UNSAFE-003](../../docs/RULES.md#R-UNSAFE-003)). It uses Microsoft's `windows` crate
([R-UNSAFE-004](../../docs/RULES.md#R-UNSAFE-004)), except for NVENC and AMF, which have no maintained binding: their few entries are declared by hand from the vendors' MIT-licensed headers, with their layouts asserted at compile time ([ADR-0217](../../docs/adr/0217-direct-nvenc-and-amf-encoders-on-the-gpu-path.md)). The backends exist only on Windows;
elsewhere the crate builds just its platform-independent helpers, which are tested on
every platform.

This README is the crate's map. How each backend behaves (formats, timing, threads,
teardown, error mapping) is in its module's rustdoc, under **Behaviour**:
`cargo doc -p jstream-platform-windows --target x86_64-pc-windows-msvc --document-private-items --open`.

| Type | Trait | Module | What it is |
|---|---|---|---|
| `QpcClock` | `MediaClock` | `clock.rs` | QueryPerformanceCounter in 100 ns ticks, WASAPI's time line ([ADR-0014](../../docs/adr/0014-media-clock.md)) |
| `WindowsProcessCpu`, `WindowsThreadCpu`, `WindowsThreadPriority` | `ProcessCpu`, `ThreadCpu`, `ThreadPriority` | `process.rs` | CPU time, each thread's under its description or, without one, the module it started in, and MMCSS for the real-time threads |
| `WasapiAudio` | `AudioIo` | `audio/` | endpoints, inputs, outputs, loopback and process loopback; event-driven, shared mode, one MMCSS "Pro Audio" thread per stream |
| `WgcCapture` | `Capture` | `capture/` | Windows.Graphics.Capture of displays and windows, on the CPU path or the GPU path (`on_gpu`) |
| `MfCodecs` | `Codecs` | `codec/` | Media Foundation AAC and H.264, HEVC and AV1 encoders, on CPU frames or the context's encode targets, with the first-frame software fallback, and the H.264 decoder MFT for the engine's own streams ([ADR-0140](../../docs/adr/0140-source-types-and-decoding-scope-v3.md)). On the context's encode targets it tries the GPU vendor's own encoder first, NVENC or AMF through the driver's runtime DLL (`codec/direct/`), and opens the MFT when that refuses; `JSTREAM_DIRECT_ENCODERS=off` keeps every GPU encoder on Media Foundation ([ADR-0217](../../docs/adr/0217-direct-nvenc-and-amf-encoders-on-the-gpu-path.md)) |
| `MfVideoDevices`, `MfMjpegDecoder` | `VideoDevices`, `CaptureDecoder` | `video_device/` | webcams and capture cards, paired with their microphones, their camera controls (`IAMVideoProcAmp` and `IAMCameraControl` on the media source), and MJPEG decoding; a raw frame is copied in one pass into memory the engine handed back |
| `MfMediaDecoders` | `MediaDecoders` | `decode/` | local media files through the Source Reader |
| `WindowsGpu` | `GpuInterop` | `gpu/` | the DX12 wgpu device on the media adapter, a D3D11 device beside it, shared NV12 encode targets and fences ([ARCH › GPU pipeline](../../docs/ARCHITECTURE.md#gpu-pipeline)). The first open also asks Windows for the process's highest GPU scheduling class (`realtime`, else `high`) and the `high` CPU priority class, so the engine's work comes before other programs'; `JSTREAM_GPU_PRIORITY` and `JSTREAM_CPU_PRIORITY` lower either (`gpu_priority.rs`, [ADR-0195](../../docs/adr/0195-the-engine-runs-at-top-priority.md)) |
| `WindowsVcam` | `VcamSink` | `vcam/` | the virtual camera's engine side, publishing to [`jstream-vcam-windows`](../jstream-vcam-windows/README.md) |
| `register_vcam`, `unregister_vcam`, `vcam_registration` | | `vcam_register.rs` | the camera's opt-in registration from an administrators-only copy ([ADR-0066](../../docs/adr/0066-portable-windows-distribution-v2.md)) |
| `WindowsCredentials` | `StreamKeyStore`, `OAuthTokenStore` | `credential.rs` | Windows Credential Manager ([R-SEC-001](../../docs/RULES.md#R-SEC-001)) |
| `create_private_file`, `check_private_file` | | `private_file.rs` | files only the user can read, for a host's token files |
| `check_media_foundation` | | `n_edition.rs` | whether Media Foundation is present, and the advice for N editions |
| `PluginBlock`, `ProAudioThread`, `WindowsPluginHosts` | `PluginHosts` | `plugin_block.rs`, `plugin_hosts.rs` | CLAP filters hosted out of process: the shared block, the audio thread and the host processes ([ADR-0035](../../docs/adr/0035-clap-plugins-out-of-process.md)) |
| `pick_path` | | `dialog.rs` | the desktop app's Browse dialogs: a file, a save or a folder |
| `user_folder` | | `folders.rs` | the known Documents and Downloads folders, where the desktop app keeps its default project and saves screenshots ([ADR-0163](../../docs/adr/0163-projects-and-screenshots-in-the-persons-folders.md)) |

Not here yet: packet-level decoders for HEVC and AV1 (`MfCodecs` decodes H.264 only;
media files decode through `MfMediaDecoders`), the virtual camera taking
`VideoSurface::Gpu`, and a `Backends` constructor, which needs all of them.

**Tests.** `lib.rs`'s rustdoc lists every test and what it checks. The helpers that don't call Windows (format choice, target IDs, AAC and video
encoder configuration, the first-frame fallback, the direct encoders' vendor choice, variable, input-slot bookkeeping, settings and FFI layouts, the H.264 decoder's configuration
check and picture cropping, the GPU capture ring, credential names
and limits, the virtual camera's format, the media-file conversions, the dialog filter, the GPU
and CPU priority classes the variables name)
run on every platform. The clock, the credential round trip and the Media Foundation
probe run in CI's Windows job. Tests against real devices, the OS encoders, the GPU and
a display are behind the `hw-tests` feature and `#[ignore]`d, because CI machines may
have none of them ([R-TEST-004](../../docs/RULES.md#R-TEST-004)). Run them on a Windows
machine with a microphone, speakers, a display, a webcam and a DX12 GPU with hardware
H.264, HEVC and AV1 encoders:
`cargo test -p jstream-platform-windows --features hw-tests -- --ignored`. Move the mouse
while the GPU capture tests run (they need screen activity). The virtual-camera test
also needs the `jstream-vcam-windows` DLL registered
([its Hardware check](../jstream-vcam-windows/README.md#hardware-check)), the media-file
tests play the file `JSTREAM_TEST_MEDIA` names, and the unplug test runs only when
`JSTREAM_UNPLUG_TEST` is set. The two `keeps_up_with_1080p30` tests feed the hardware
H.264 encoder a live 1080p30 stream for ten seconds, from encode targets and from CPU
frames, as the engine's render thread and encode thread do; one that falls behind says
in its failure where the time went. `keeps_up_with_1080p30_directly` runs the same feed
through the GPU vendor's own encoder (NVENC or AMF), and the `_directly` GPU encode tests
repeat the encode-target tests through it, with a forced keyframe; run the two live
tests with `--nocapture` to compare their reports ([ADR-0217](../../docs/adr/0217-direct-nvenc-and-amf-encoders-on-the-gpu-path.md)). HEVC and AV1 run only on a GPU that encodes them (AV1
needs an NVIDIA RTX 40, AMD RX 7000, Intel Arc or newer).

<a id="open-questions"></a>
## Open questions
<!-- @section
-->

- **How the encoders reach the GPU context** (answered): as capture does, through a
  default trait method, `Codecs::on_context`, which `MfCodecs` answers with a backend
  on the context. `video_encoder` and `VideoEncoderConfig` are unchanged.
- **Allocator bind flags.** The GPU path's input textures take the encode targets'
  bind flags (render target and shader resource), which the adapter is known to
  allocate in NV12. Whether every vendor's encoder reads those, or some want
  `D3D11_BIND_VIDEO_ENCODER`, shows only on hardware. The direct encoders' own input
  textures take the same flags.
- **Direct encoders on hardware.** NVENC and AMF are checked against their headers'
  layouts at build time, not against a GPU. Each needs a `hw-tests` run on its vendor's
  hardware (the `_directly` tests) before its output is trusted.
- **Render times.** `AudioRenderBuffer::time` (when the first frame will play) is always
  `None` for now. `IAudioClock` can give it once the engine needs output sync, for
  example for monitoring with delay compensation.
- **Hardware encoders and CPU frames** (answered for the first frame, #443). A hardware
  MFT that takes NV12 when its media types are set might still refuse system-memory
  samples at `ProcessInput`. With `Prefer`, a refusal of the first frame falls back to
  the software MFT (the first-frame fallback in `codec`'s rustdoc); a later one fails the output. Whether
  Intel and NVIDIA encoders refuse at all still needs a `hw-tests` run on them. The GPU
  path, which feeds hardware encoders D3D11 textures as S1 did, makes the question moot
  for them.
- **Moving to a default device with another format.** For now the stream ends with
  `DeviceLost` and the engine reopens it. Instead the backend could ask WASAPI to convert
  to the old format, which would keep the stream running but would resample if the
  rates differ. Decided with `jstream-audio`.
