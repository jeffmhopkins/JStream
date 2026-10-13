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
verified = { hash = "b3:bd1969b4661a41fd", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "New section: checked each bullet against src/ (MediaClock bounds, VideoSurface non_exhaustive CPU-only, capture mailbox and stats, audio handler/packet fields vs S3, codec can_accept/encode/poll/finish vs S1, VcamSink vs S2/ADR-0020, Backends incl. optional vcam, PlatformError, fake feature) and its dependencies ARCH crate-map/threading, ADR-0003/0004/0014/0020 and jstream-media#purpose" }
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
  keyframe pattern, placeholder payloads) and `FakeVcamSink`, plus `fake::backends`.
  The crate's tests drive every trait through trait objects only, and check the
  `Send`/`Sync` bounds above.

**Not yet:** `GpuInterop` and GPU frames, webcam capture (an MJPEG `Codec` variant and a
camera `CaptureTarget`), on-demand keyframes, and encoder selection by GPU adapter.

<a id="open-questions"></a>
## Open questions
<!-- @section
-->

- **`GpuInterop` and `wgpu`.** The crate map says `GpuInterop` uses `wgpu` types. It is
  deferred, and `wgpu` is not a dependency yet. Its shape depends on spike S1's recorded
  result: whether NV12 plane views work as render targets or the compute-plus-copy
  fallback is needed, and how the shared fence is handed between D3D11, wgpu and the
  encoder ([ARCH › GPU pipeline](../../docs/ARCHITECTURE.md#gpu-pipeline)). Adding it
  means a `VideoSurface` GPU variant (a texture plus a fence value), an adapter
  identity (S1 picks the encoder MFT by adapter LUID), and `wgpu` pinned exactly
  ([ADR-0004](../../docs/adr/0004-wgpu-with-native-interop.md)). An open alternative is to
  keep `wgpu` out of this crate: an opaque GPU frame here, with the `wgpu` import done in
  `jstream-render` against the platform crate.
- **Device formats.** `jstream-media`'s `ChannelLayout` has mono, stereo and 5.1 only.
  A device with another channel count is refused as `Unsupported` until the layout set
  grows or backends downmix.
- **Clock unit.** `MediaClock::timebase` lets each platform choose its unit (spike S3
  uses 100 ns ticks, WASAPI's unit). Whether the engine fixes one unit is still open, with
  S3's result and `jstream-core`.
