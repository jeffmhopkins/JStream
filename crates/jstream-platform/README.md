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
depends_on = ["docs/ARCHITECTURE.md#crate-map", "docs/ARCHITECTURE.md#threading", "docs/adr/0003-media-stack-and-codecs.md", "docs/adr/0004-wgpu-with-native-interop.md", "docs/adr/0014-media-clock.md", "docs/adr/0020-portable-windows-distribution.md", "crates/jstream-media/README.md#purpose"]
verified = { hash = "b3:478dbb7e1c94c192", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Upstream ARCH#threading only gained the S3 result paragraph (confirms QPC mapping + adaptive resampling the clock traits assume); purpose text unchanged" }
-->

Layer-1 backend traits: the seam between the engine and the OS. Platform crates
(`jstream-platform-windows` first, macOS and Linux later) implement them, and the engine
holds the backends as injected trait objects, so no engine crate names a platform API.
This crate is the **single owner** of the encoder and decoder traits
([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)). It depends only on
[`jstream-media`](../jstream-media/README.md#purpose) and `thiserror`, and has no
`unsafe`.

The shapes follow what spikes S1 (GPU and encode), S2 (virtual camera) and S3 (clock)
do on Windows. This is a first cut: the traits will change once `jstream-platform-windows`
implements them.

Contents:
- **Clock:** `MediaClock` (`Send + Sync`): `timebase` and a never-decreasing `now`. It
  is the one monotonic media clock of [ADR-0014](../../docs/adr/0014-media-clock.md)
  (QPC on Windows). Every timestamp a backend hands over (capture frames, audio packets,
  decoded frames) is already on it.
- **Frames:** `VideoSurface`, the frame type that crosses the seam. Today it has one
  variant, a CPU `VideoFrame`. It is `#[non_exhaustive]` so a GPU variant can be added
  with `GpuInterop` (see [Open questions](#open-questions)).
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
  as [ADR-0003](../../docs/adr/0003-media-stack-and-codecs.md) says.
- **Virtual camera:** `VcamSink` (`Send`): `start` with a `VcamFormat`, `publish`
  frames (the newest wins, and it never blocks on a consumer that may have died, as in
  spike S2), `stop`. Registering the camera's DLL is the app's job
  ([ADR-0020](../../docs/adr/0020-portable-windows-distribution.md)), not this trait's.
- **Injection:** `Backends`, a plain struct holding one platform's clock, capture, audio
  and codec backends and its optional virtual-camera sink.
- **IDs and errors:** `DeviceId`, an opaque backend-chosen string. `PlatformError`, one
  `thiserror` enum for every trait; OS failures carry the call and the raw status code
  (`HRESULT`, `OSStatus`, `errno`).
- **Fakes** (`fake.rs`, feature `fake`): deterministic in-memory backends for engine and
  scenario tests ([R-TEST-004](../../docs/RULES.md#R-TEST-004)): `FakeClock` (time moves
  only when advanced), `FakeCapture`, `FakeAudioIo`, `FakeCodecs` (correct timing and
  keyframe pattern, placeholder payloads; the decoder returns frames in presentation
  order, holding each until a packet's `dts` reaches its `pts`) and `FakeVcamSink`
  (dropping it stops it; a test watches it through a `FakeVcamProbe`), plus
  `fake::backends`. The crate's tests drive every trait through trait objects only,
  keeping the concrete fakes for test-side controls and probes, and check the
  `Send`/`Sync` bounds above.

**Not yet:** `GpuInterop` and GPU frames, webcam capture (an MJPEG `Codec` variant and a
camera `CaptureTarget`), on-demand keyframes, and encoder selection by GPU adapter.

<a id="open-questions"></a>
## Open questions
<!-- @section
-->

- **`GpuInterop` and `wgpu`.** The crate map says `GpuInterop` uses `wgpu` types. It is
  deferred, and `wgpu` is not a dependency yet. Spike S1's result is recorded in
  [ARCH › GPU pipeline](../../docs/ARCHITECTURE.md#gpu-pipeline). NV12 plane views
  worked as render targets on the one machine tested, so compute-plus-copy stays a
  fallback. The shared fence is handed between D3D11, wgpu and the encoder as ARCH steps
  2 to 5 describe. Adding `GpuInterop` means a `VideoSurface` GPU variant (a texture plus
  a fence value) and an adapter identity. S1 tried to match the encoder MFT by adapter
  LUID, but AMD's MFTs don't report one, so another way to match is still needed. It
  also means `wgpu` pinned exactly
  ([ADR-0004](../../docs/adr/0004-wgpu-with-native-interop.md)). **Proposed default:** keep
  `wgpu` out of this crate. This crate gets an opaque GPU frame (texture handle plus fence
  value), and `jstream-render`, which already depends on `wgpu`, does the import. That
  keeps this L1 crate light and leaves one crate tracking `wgpu`'s exact pin. S1 doesn't
  settle where the import lives; that is decided when `GpuInterop` is designed. Raw
  D3D12 objects must be released before the wgpu device either way, as ARCH step 2 says.
- **Device formats.** `jstream-media`'s `ChannelLayout` has mono, stereo and 5.1 only.
  A device with another channel count is refused as `Unsupported` for now. **Proposed
  default:** backends downmix any other layout to stereo and log it, and new layouts
  (e.g. 7.1) are added only when a real device needs them.
- **Clock unit.** `MediaClock::timebase` lets each platform choose its unit (spike S3
  uses 100 ns ticks, WASAPI's unit). **Proposed default:** the engine fixes one unit,
  100 ns ticks, and other platforms convert at their edge, so engine timestamps compare
  without per-platform conversions. Settled with S3's result and `jstream-core`.
