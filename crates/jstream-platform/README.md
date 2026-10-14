<!-- @doc
id = "CRATE-jstream-platform"
title = "jstream-platform"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-platform

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-platform/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-platform", "docs/ARCHITECTURE.md#threading", "docs/adr/0031-source-types-and-decoding-scope.md", "docs/adr/0004-wgpu-with-native-interop.md", "docs/adr/0014-media-clock.md", "docs/adr/0020-portable-windows-distribution.md", "crates/jstream-media/README.md#purpose"]
verified = { hash = "b3:1547a2e5343d02ba", by = "agent:grok@bc-154a1201-b120-5757-9b7f-71d45e41d992", note = "Re-read against the new audio sentence in Threads. The backend traits are unchanged. AudioIo still receives the handler the mixer builds." }
-->

Layer-1 backend traits: the seam between the engine and the OS. Platform crates
(`jstream-platform-windows` first, macOS and Linux later) implement them, and the engine
holds the backends as injected trait objects, so no engine crate names a platform API.
This crate is the **single owner** of the encoder and decoder traits
([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)). It depends only on
[`jstream-media`](../jstream-media/README.md#purpose), `thiserror` and `wgpu` (for the
GPU-frame types, pinned exactly per
[ADR-0004](../../docs/adr/0004-wgpu-with-native-interop.md) and re-exported as
`jstream_platform::wgpu`), and has no `unsafe`.

The shapes follow what spikes S1 (GPU and encode), S2 (virtual camera) and S3 (clock)
do on Windows. This is a first cut: the traits will change once `jstream-platform-windows`
implements them.

Contents:
- **Clock:** `MediaClock` (`Send + Sync`): `timebase` and a never-decreasing `now`. It
  is the one monotonic media clock of [ADR-0014](../../docs/adr/0014-media-clock.md)
  (QPC on Windows). Every timestamp a backend hands over (capture frames, audio packets,
  decoded frames) is already on it.
- **Frames:** `VideoSurface`, the frame type that crosses the seam: `Cpu`, a
  `VideoFrame` in system memory, or `Gpu`, a `GpuFrame` in a texture shared with the
  platform. It is `#[non_exhaustive]`.
- **GPU interop** ([GPU frames](#gpu-frames)): `GpuInterop` (`Send + Sync`) opens the
  engine's `GpuContext` (adapter, device, queue, `AdapterId`, and the platform's own
  state, which drops first) and allocates NV12 `encode_target`s. A `GpuFrame` holds a
  `wgpu::Texture`, its `VideoFrameInfo` and an `Arc<dyn FrameSync>`; `GpuFrame::stage`
  stages its fence wait and signal around the engine's next submit. Frames compare by
  texture, hold and info.
- **Capture:** `Capture` (`Send + Sync`) lists displays and windows and starts a
  `CaptureStream` (`Send`). The backend's capture thread fills a single-slot
  "latest frame" mailbox, as in
  [ARCH › Threads, clock and sync](../../docs/ARCHITECTURE.md#threading), and the render
  thread takes from it without blocking; `None` means "repeat the last frame". Spike S1
  drains Windows.Graphics.Capture's pool to the newest frame the same way.
  `CaptureStats` counts captured and skipped frames.
- **Audio:** `AudioIo` (`Send + Sync`) lists devices and opens input (microphone,
  output loopback, process loopback) and output streams. Each stream calls an
  engine-supplied `AudioInputHandler` or `AudioOutputHandler` on the backend's real-time
  audio thread, so handlers must not allocate, lock, block or log
  ([R-RT-001](../../docs/RULES.md#R-RT-001)). An `AudioPacket` carries interleaved `f32`
  samples in the device's own format, the device position and the media-clock time of
  its first frame (`None` when the device flagged it), plus discontinuity and silent
  flags: what spike S3 needs to fit each device's drift. Backends never resample;
  adaptive resampling is the engine's job.
- **Codecs:** `Codecs` (`Send + Sync`) lists the OS codecs (`CodecInfo`) and opens
  `VideoEncoder`, `AudioEncoder` and `VideoDecoder` sessions (each `Send`). The call
  pattern is spike S1's Media Foundation one: `can_accept`, then `encode` (or
  `decode`), then a non-blocking `poll` for ready output, and `finish` to drain. A full
  encoder returns `PlatformError::Busy`, so the caller drops and counts the frame
  ([R-RT-004](../../docs/RULES.md#R-RT-004)). Packets keep their frame's `pts` on the
  media clock and follow `jstream-media`'s framing (H.264 Annex B, raw AAC).
  `HardwarePreference` defaults to hardware first with the software codec as fallback,
  as [ADR-0031](../../docs/adr/0031-source-types-and-decoding-scope.md) says.
- **Virtual camera:** `VcamSink` (`Send`): `start` with a `VcamFormat`, `publish`
  frames (the newest wins, and it never blocks on a consumer that may have died, as in
  spike S2), `stop`. Registering the camera's DLL is the app's job
  ([ADR-0020](../../docs/adr/0020-portable-windows-distribution.md)), not this trait's.
- **Injection:** `Backends`, a plain struct holding one platform's clock, capture, audio,
  codec and GPU-interop backends and its optional virtual-camera sink.
- **IDs and errors:** `DeviceId`, an opaque backend-chosen string. `PlatformError`, one
  `thiserror` enum for every trait; OS failures carry the call and the raw status code
  (`HRESULT`, `OSStatus`, `errno`).
- **Fakes** (`fake.rs`, feature `fake`): deterministic in-memory backends for engine and
  scenario tests ([R-TEST-004](../../docs/RULES.md#R-TEST-004)): `FakeClock` (time moves
  only when advanced), `FakeCapture`, `FakeAudioIo`, `FakeCodecs` (correct timing and
  keyframe pattern, placeholder payloads; the decoder returns frames in presentation
  order, holding each until a packet's `dts` reaches its `pts`) and `FakeVcamSink`
  (dropping it stops it; a test watches it through a `FakeVcamProbe`), plus
  `FakeGpuInterop` (no GPU: `open_device` is `Unsupported`, as on CI) and
  `fake::backends`. The crate's tests drive every trait through trait objects only,
  keeping the concrete fakes for test-side controls and probes, and check the
  `Send`/`Sync` bounds above.

**Not yet:** a platform that delivers or consumes GPU frames (the Windows
`GpuInterop` opens the device and encode targets; its capture and encoders don't use them
yet), webcam capture (an MJPEG `Codec` variant and a
camera `CaptureTarget`), on-demand keyframes, and encoder selection by GPU adapter.

<a id="gpu-frames"></a>
## GPU frames
<!-- @section
depends_on = ["docs/ARCHITECTURE.md#crate-map", "docs/ARCHITECTURE.md#gpu-pipeline", "docs/adr/0004-wgpu-with-native-interop.md", "docs/RULES.md#unsafe"]
verified = { hash = "b3:418109e505e0d384", by = "agent:grok@bc-154a1201-b120-5757-9b7f-71d45e41d992", note = "Re-read GpuFrame, FrameSync and the unsafe-import split against the gpu-pipeline sentence that names legacy_quad. Render still uses this device when present, opens its own otherwise, and does not write the NV12 encode target." }
-->

The design for frames that stay on the GPU, which ARCH's pipeline needs (capture → wgpu
→ NV12 → encoder with no CPU copy). The types below are built in this crate, and the
Windows `GpuInterop` opens the device and encode targets. `jstream-render` composites one
scene on that device when the engine has one, and opens its own (software adapter first)
when it does not; it does not write the NV12 encode target. Capture into shared textures
and the encoders' GPU input come next.

**Where the import lives.** The open question was whether this crate should hold `wgpu`
types, or only an opaque handle that `jstream-render` imports. The import has to call
wgpu-hal's `unsafe` functions (`texture_from_raw`, `create_texture_from_hal`), and the
fence staging around a submit does too (`Queue::as_hal`, `add_wait_fence`,
`add_signal_fence`, as spike S1 did). R-UNSAFE-001 allows `unsafe` only in platform
crates, so `jstream-render` can't do either. So:
- This crate defines `GpuInterop` and the GPU frame type **with `wgpu` types**, as the
  [crate map](../../docs/ARCHITECTURE.md#crate-map) already says.
- Platform crates implement them with wgpu-hal, as
  [ADR-0004](../../docs/adr/0004-wgpu-with-native-interop.md) and
  [ARCH › GPU pipeline](../../docs/ARCHITECTURE.md#gpu-pipeline) step 2 say.
- `jstream-render` only calls safe methods.
- `wgpu` becomes a dependency of this crate, pinned exactly (ADR-0004). The workspace
  manifest pins it once, so every crate follows a single pin.

The earlier proposed default (keep `wgpu` out of this crate) is dropped for this reason.

**Types.**
- **`GpuInterop`** (`Send + Sync`), one per platform, in `Backends`:
  - `open_device()` returns a **`GpuContext`**: the `wgpu::Adapter`, `Device` and `Queue`,
    opened on the **media adapter**, plus an `AdapterId` (the LUID on Windows).
    - The media adapter is the one that drives the captured display, with
      `TEXTURE_FORMAT_NV12`, as S1 chose it. `jstream-render` uses this device when the
      engine has one, so capture, render and encode share one adapter. With no interop it
      opens its own, software adapter first. On a hybrid-GPU machine where sharing an
      adapter isn't possible, the platform says so (ARCH step 6).
    - The platform keeps the raw objects it took from the device (S1's
      `ID3D12Device` clone) inside its own state. It releases them before the wgpu device,
      as ARCH step 2 requires: `GpuContext` drops the platform's state first.
  - `encode_target(&GpuContext, FrameSize)` allocates an **NV12 texture shared with the
    encoders** and returns it as a `GpuFrame` that render writes into.
- **`GpuFrame`** is a new `VideoSurface::Gpu` variant. It holds:
  - the `wgpu::Texture`;
  - the frame's `VideoFrameInfo` (size, format, colour, `pts`), so `VideoSurface::info`
    works for both variants;
  - a **`FrameSync`** (`Arc<dyn FrameSync>`): the platform's hold on the shared fence for
    this use of the texture.
- **`FrameSync::stage(&wgpu::Queue)`** is called by render for each frame that its next
  submit reads (a capture frame) or writes (an encode target). The platform stages a wait
  for the value that makes the frame ready, and a signal for the value that releases it
  (S1's `stage_fences`). It is safe to call; the `unsafe` hal access is inside the
  platform crate. A frame that is never staged is never waited on. Each call is one use,
  so an encode target written every tick is staged every tick. The engine stages right
  before a submit that will happen: on D3D12 the staged wait and signal attach to the
  queue's next submit, whichever it is. That submit ends by transitioning the texture to
  `wgpu::TextureUses::PRESENT`, the D3D12 `COMMON` state the platform's other APIs
  expect, as S1 did.
- **Encoders and the virtual camera** accept `VideoSurface::Gpu` from the same platform.
  They wait on the D3D11 side for the release value before reading (ARCH step 5). A
  `GpuFrame` from another platform, or from another `GpuContext`, is `Unsupported`.
- **Equality and `Debug`:** two `GpuFrame`s are equal when they have the same texture
  (wgpu's resource identity), the same `FrameSync` hold (the same allocation, compared by
  data pointer with `ptr::addr_eq`, so two vtables for one hold don't matter) and equal
  `VideoFrameInfo`. `FrameSync` itself has no equality; it only needs `Debug`. So
  `VideoSurface` keeps its derives.

**When there is no interop.** If `open_device` fails (no platform interop, no adapter
with NV12, or a test run), the engine opens a device of its own and uses the CPU frame
paths. On CI and headless machines that device is on a software adapter, as ARCH
principle 7 says, and capture, encode and the virtual camera exchange `Cpu` frames
(ARCH step 6).

**Driver lifetime.** Raw driver objects can outlive the `GpuContext`, because
`VideoSurface` is `Clone` and a frame can sit in an encoder queue or the virtual camera.
So every platform object that holds a raw driver interface (a `FrameSync` hold, a
capture ring slot, an encode target) also holds a `wgpu::Device` clone, dropped after
its raw fields. The driver (`d3d12.dll`) then can't be unloaded before them, which is
how spike S1 crashed at exit. Inside `GpuFrame`, the hold drops before the texture for
the same reason.

**What stays CPU.** `VideoSurface::Cpu` remains for readback, tests and the fallbacks
(ARCH step 6), and every trait keeps accepting it. The fakes stay CPU-only.
`FakeGpuInterop::open_device` returns `Unsupported`, because CI has no GPU (R-TEST-004).

**Open within the design.**
- **Encoder adapter matching.** AMD's encoder MFTs don't report `MFT_ENUM_ADAPTER_LUID`
  (S1). The Windows implementation will match by the adapter's vendor ID when the LUID
  is missing, and take the first hardware MFT when only one adapter has an encoder.
  `AdapterId` carries both the LUID and the vendor ID for that.
- **Frame lifetime.** Capture uses a small ring of shared textures. A slot comes free
  in one of two ways:
  - **Staged:** once the fence has passed the value that render's signal released it
    with.
  - **Not staged:** as soon as every hold on it is dropped. This is the common case
    when capture outruns the mix and render skips frames; no GPU work references the
    slot, so it frees at once.

  If render holds a frame across ticks, capture skips that slot. When every slot is
  held, the new frame is dropped and counted in `CaptureStats`. Capture never blocks
  ([R-RT-004](../../docs/RULES.md#R-RT-004)).

<a id="open-questions"></a>
## Open questions
<!-- @section
-->

- **GPU frames.** Designed and typed in [GPU frames](#gpu-frames) above. The Windows
  `GpuInterop` opens the device and encode targets, but no platform delivers or consumes
  GPU frames yet. How capture and the encoders reach the context is an open question in
  [jstream-platform-windows](../jstream-platform-windows/README.md#open-questions).
- **Device formats.** `jstream-media`'s `ChannelLayout` has mono, stereo and 5.1 only.
  **Proposed default:** backends downmix any other layout to stereo, and new layouts
  (e.g. 7.1) are added only when a real device needs them. The Windows backend already
  does this: it asks WASAPI to downmix
  ([jstream-platform-windows](../jstream-platform-windows/README.md#purpose)).
- **Clock unit.** `MediaClock::timebase` lets each platform choose its unit (spike S3
  uses 100 ns ticks, WASAPI's unit). **Proposed default:** the engine fixes one unit,
  100 ns ticks, and other platforms convert at their edge, so engine timestamps compare
  without per-platform conversions. The Windows backend uses 100 ns ticks. Settled with
  `jstream-core`.
