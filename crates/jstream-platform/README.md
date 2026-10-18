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
covers = ["crates/jstream-platform/src/**", "crates/jstream-platform/Cargo.toml"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-platform", "docs/ARCHITECTURE.md#threading", "docs/adr/0061-source-types-and-decoding-scope-v2.md", "docs/adr/0004-wgpu-with-native-interop.md", "docs/adr/0014-media-clock.md", "docs/adr/0020-portable-windows-distribution.md", "crates/jstream-media/README.md#purpose"]
-->

Layer-1 backend traits: the seam between the engine and the OS. Platform crates
(`jstream-platform-windows` first, macOS and Linux later) implement them, and the engine
holds the backends as injected trait objects, so no engine crate names a platform API.
This crate is the **single owner** of the encoder and decoder traits
([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)). It depends only on
[`jstream-media`](../jstream-media/README.md#purpose),
[`jstream-plugin-proto`](../jstream-plugin-proto/README.md#purpose) (the plugin-host
contract, itself dependency-free), `thiserror`, `wgpu` (for the
GPU-frame types, pinned exactly per
[ADR-0004](../../docs/adr/0004-wgpu-with-native-interop.md) and re-exported as
`jstream_platform::wgpu`) and `crossbeam-utils` (the mailbox's atomic cell), and has no
`unsafe`.

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
  state, which drops first) and allocates NV12 `encode_target`s. `GpuContext::key`
  is a `ContextKey`, equal only for the same context: `wgpu::Device` equality compares
  IDs each wgpu instance numbers from zero, so it can't tell two instances' devices
  apart. A `GpuFrame` holds a
  `wgpu::Texture`, its `VideoFrameInfo` and an `Arc<dyn FrameSync>`; `GpuFrame::stage`
  stages its fence wait and signal around the engine's next submit, and
  `FrameSync::platform` lets the platform that made a hold recognise it (`None` by
  default). `GpuContext::submit_lock` is the context's `SubmitLock`, which orders the
  submits on its queue around that staging. Frames compare by texture, hold and info.
- **Capture:** `Capture` (`Send + Sync`) lists displays and windows and starts a
  `CaptureStream` (`Send`). The backend's capture thread fills a single-slot
  "latest frame" mailbox, as in
  [ARCH › Threads, clock and sync](../../docs/ARCHITECTURE.md#threading), and the render
  thread takes from it without blocking; `None` means "repeat the last frame". Spike S1
  drains Windows.Graphics.Capture's pool to the newest frame the same way.
  `CaptureStats` counts captured and skipped frames, and the compressed frames a
  `CaptureDecoder` rejected (`frames_undecodable`, always 0 for display capture).
  `Capture::on_context` returns the backend on a `GpuContext`'s device, whose captures
  deliver `Gpu` frames, or `None` (the default) when it has no GPU path; the engine
  then keeps CPU frames. `Capture::start_with` starts a capture with `CaptureOptions`
  (the default is `start`'s). `CaptureOptions::for_source` names the source a capture
  is for, so a backend that keeps a grant per source (the ScreenCast portal's restore
  token, [ADR-0060](../../docs/adr/0060-linux-platform-support.md) §3) keeps each
  source's apart, and `Capture::forget_source` drops it when the source is deleted, once
  its captures have stopped (best effort: a grant file that can't be removed stays). By
  default `start_with` ignores its options and calls `start`, and `forget_source` does
  nothing.
- **Video capture devices** (webcams and capture cards,
  [ADR-0061](../../docs/adr/0061-source-types-and-decoding-scope-v2.md)): `VideoDevices`
  (`Send + Sync`) lists devices (`VideoDeviceInfo`, with the paired `AudioIo` input
  endpoint when the backend can pair one), each device's `VideoDeviceFormat`s (size, frame
  period, `CaptureEncoding`), and opens a device in one format as a `CaptureStream`, the
  same mailbox as display capture. `CaptureEncoding` is `Raw` (an engine pixel format),
  `Yuy2` (converted to NV12 by the backend) or `Mjpeg`. MJPEG is a video codec, so it is
  decoded on the decoder side: opening a compressed format takes a `CaptureDecoder`
  (`Send`), which the capture thread calls with each `CompressedFrame` before the mailbox;
  a frame it rejects is dropped and the stream goes on.
- **Audio:** `AudioIo` (`Send + Sync`) lists devices and running processes (every
  process on Windows, only Core Audio's audio clients on macOS)
  (`AudioProcess`: ID, parent ID, executable name, for process loopback), says which
  format an input target would open in (`input_format`, so the engine can build the
  handler for it before opening), and opens input (microphone, output loopback,
  process loopback) and output streams. Each stream calls an
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
  `decode`), then a non-blocking `poll` for ready output, and `finish` to drain.
  `VideoEncoder::request_keyframe` forces the next frame to be a keyframe; its default
  refuses with `Unsupported`, and an encoder that refuses keeps its regular cadence. A full
  encoder returns `PlatformError::Busy`, so the caller drops and counts the frame
  ([R-RT-004](../../docs/RULES.md#R-RT-004)). Packets keep their frame's `pts` on the
  media clock and follow `jstream-media`'s framing (H.264 Annex B, raw AAC).
  `HardwarePreference` defaults to hardware first with the software codec as fallback,
  as [ADR-0061](../../docs/adr/0061-source-types-and-decoding-scope-v2.md) says.
  `Codecs::on_context` returns the backend on a `GpuContext`, whose video encoders take
  that context's encode targets ([GPU frames](#gpu-frames)); `Ok(None)`, the default,
  when the backend has no GPU path.
- **Media files** (`media_file.rs`,
  [ADR-0061](../../docs/adr/0061-source-types-and-decoding-scope-v2.md)): `MediaDecoders`
  (`Send + Sync`) opens a local file by its host path for a `MediaDecoderConfig` (the
  video pixel format, a `HardwarePreference`, and whether to decode audio too) as a
  `MediaDecoder` (`Send`), which owns the whole file: the platform demuxes and decodes
  it with its own APIs, unlike the packet-level `VideoDecoder`. `info()` gives a
  `MediaInfo`: the first video stream (`MediaVideoInfo`: frame size and nominal frame
  period) or `None`, the first audio stream as it decodes (`MediaAudioInfo`: its
  format, the stream's own rate in a layout `jstream-media` has, interleaved `f32`,
  never resampled) or `None` when there is none, it wasn't asked for or the platform
  can't decode it, and the duration when the container says. `next_video()` and
  `next_audio()` (an `AudioBuffer`) block while they decode the next frame or block of
  their stream, in presentation order, and are `Ok(None)` at its end. Audio is the
  stream's content only: a backend drops the encoder's priming and padding where it can
  tell them. The caller reads
  the two in step by time, since a backend holds what one stream demuxed while the
  other is read. `seek(to)` moves both: video restarts at the key frame at or before
  `to`, so the frames up to it may come out first, and audio at or before `to`. Times
  are on the file's own timeline from its start, not the media clock; the `media`
  source maps them onto the clock.
- **Virtual camera:** `VcamSink` (`Send`): `start` with a `VcamFormat`, `publish`
  frames (the newest wins, and it never blocks on a consumer that may have died, as in
  spike S2), `stop`. Registering the camera's DLL is the app's job
  ([ADR-0020](../../docs/adr/0020-portable-windows-distribution.md)), not this trait's.
- **Stream keys** (`keys.rs`): `StreamKeyStore` (`Send + Sync`), the seam to the OS
  credential store ([R-SEC-001](../../docs/RULES.md#R-SEC-001)): `set`, `get`, `clear`
  and `is_set`, one key per output ID. No error may contain a key. `Secret` carries a
  key across it: it prints as `Secret("***")`, it has no `Display` and isn't `Clone`, it is
  read only through `expose`, and its bytes are zeroed on drop (best effort).
  `Secret::from_utf8` takes a store's bytes and refuses non-UTF-8 without naming them.
  The engine holds the store apart from `Backends`, through `Engine::with_key_store`.
- **OAuth tokens** (`keys.rs`): `OAuthTokenStore` (`Send + Sync`), the same seam for
  OAuth refresh tokens ([R-SEC-001](../../docs/RULES.md#R-SEC-001)): `set`, `get`, `clear`
  and `is_set`, one token per caller-chosen account ID, each carried as a `Secret`. It is
  a separate trait from `StreamKeyStore`, so a token and a stream key never share a name.
  Only refresh tokens are stored; access tokens stay in the caller's memory. No error may
  contain a token. `jstream-youtube` keeps its Google refresh token here.
- **CLAP plugin hosts** (`plugin_host.rs`,
  [ADR-0035](../../docs/adr/0035-clap-plugins-out-of-process.md)): `PluginHosts`
  (`Send + Sync`) lists the CLAP search directories that exist, `scan`s a plugin file
  in a short-lived host process (so a plugin that crashes while scanned costs only that
  process), and `start`s a host for a `HostRequest` (file, plugin id, the block's
  `Config` and a saved state): it creates the block initialised with that
  configuration, starts the process on it and sends `Start`. A `PluginHost` (`Send`)
  gives the block as a `HostBlock` (an `Arc` of anything that is `AsRef<Shared>`) for
  the mixer's CLAP filter, says whether the process has `exited`, receives control
  messages without waiting (`try_recv`), `send`s them (false once the host is gone),
  and is ended by `kill` or by dropping it. Deciding when to restart or give up is the
  engine's job, not this trait's. The engine holds it apart from `Backends`.
- **Injection:** `Backends`, a plain struct holding one platform's clock, capture, video
  capture device, audio, codec, GPU-interop and media-file backends and its optional
  virtual-camera sink.
- **Data folder** (`paths.rs`, [ADR-0020](../../docs/adr/0020-portable-windows-distribution.md)):
  `paths::data_dir`, the one folder every host keeps its state in. It is a folder named
  `data` beside the exe when a file named `portable` sits there (so the app can run from
  a USB stick), and otherwise the platform's per-user data folder: `%APPDATA%\JStream`
  on Windows, `~/Library/Application Support/JStream` on macOS, and
  `$XDG_DATA_HOME/jstream` (else `~/.local/share/jstream`) elsewhere. Only absolute
  variables count, so nothing is written beside the exe without the marker. It reads the
  environment itself: three cases don't pay for `dirs`. A plain function, not a trait:
  no host needs to swap it.
- **Shared backend helpers** (`mailbox.rs`, `nv12.rs`): safe, platform-independent code
  the platform crates share, so it is written and tested once. `mailbox::Mailbox` is the
  capture callbacks' single-slot latest-frame mailbox: `put` swaps a boxed frame in and
  returns the one it replaced (a skipped frame), `take` swaps it out, and neither waits
  ([R-RT-002](../../docs/RULES.md#R-RT-002)); it is `crossbeam-utils`' `AtomicCell`
  around an optional box, one atomic swap. `nv12` converts packed 8-bit BGRA or RGBA to
  NV12 in BT.709 limited range with left-sited chroma (`rgb_to_nv12` into a
  `VideoFrame`, `rgb_to_nv12_image` into an `Nv12Image`, NV12 laid out in one mapped
  buffer), and `even_size` crops an odd source. Windows' capture and H.264 CPU path and
  Linux's PipeWire capture and VA-API upload use them.
- **IDs and errors:** `DeviceId`, an opaque backend-chosen string. `PlatformError`, one
  `thiserror` enum for every trait; OS failures carry the call and the raw status code
  (`HRESULT`, `OSStatus`, `errno`), and a missing OS component (`MissingComponent`, such
  as Media Foundation on a Windows N edition) says what the user must install.
- **Fakes** (`fake.rs`, feature `fake`): deterministic in-memory backends for engine and
  scenario tests ([R-TEST-004](../../docs/RULES.md#R-TEST-004)): `FakeClock` (time moves
  only when advanced), `FakeCapture`, `FakeAudioIo` (process loopback opens on the
  processes a test lists with `set_processes`; `input_targets` says what each open
  input records), `FakeCodecs` (correct timing and
  keyframe pattern, a forced keyframe restarting that pattern, placeholder payloads; the decoder returns frames in presentation
  order, holding each until a packet's `dts` reaches its `pts`) and `FakeVcamSink`
  (dropping it stops it; a test watches it through a `FakeVcamProbe`),
  `FakeVideoDevices` (a webcam paired with the fake microphone offering NV12, YUY2 and
  MJPEG, and a capture card; `unplug` loses a device) with `FakeCaptureDecoder`, plus
  `FakeGpuInterop` (no GPU: `open_device` is `Unsupported`, as on CI),
  `FakeMediaDecoders` (a path registered with `add` plays a `FakeClip`, synthetic RGBA
  frames whose colour tells them apart, seeking to the key frame before as a real
  decoder does, and with a `FakeAudio` blocks of one level each, which tells them
  apart; any other path is `Unsupported`, and nothing is read from disk; its `stats`
  count opens, seeks, frames and audio blocks),
  `MemoryKeyStore` (an in-memory `StreamKeyStore` that can be made to fail),
  `MemoryTokenStore` (the same for `OAuthTokenStore`, apart from any key store) and
  `fake::backends`. `plugin_host::fake::FakePluginHosts` (scans find the files a test
  adds; `refuse_starts` fails starts, `start_hung` starts hosts already hung, and
  `start_loading` starts hosts that don't get ready until `finish_loading`) runs each
  host as a thread acting like a unity-gain plugin with `FAKE_LATENCY` and one
  parameter (`fake_param`): once started it publishes ready and, like the real host,
  only then beats the heartbeat, copies each input block to the output under the same
  number, answers `SaveState` and ends on `Stop` or the `stop` word. A test makes it
  `crash` (it exits) or `hang` (the heartbeat stops but it hasn't exited) through its
  `FakeHostControl`, which also records what the host received. The crate's tests drive every trait through trait objects only,
  keeping the concrete fakes for test-side controls and probes, and check the
  `Send`/`Sync` bounds above.

**Not yet:** a virtual camera that takes GPU frames (Windows capture delivers them
through `WgcCapture::on_gpu`, the engine composites them, and Windows' H.264 encoder
takes encode targets), an engine that opens `Backends::video_devices` (the field is
filled, with `MfVideoDevices` on Windows and `MfMjpegDecoder` as its `CaptureDecoder`,
but no source uses it, so `stats.tick` doesn't report `frames_undecodable`), an MJPEG
`Codec` variant, and encoder selection by vendor ID for an encoder that reports no
adapter LUID.

<a id="gpu-frames"></a>
## GPU frames
<!-- @section
depends_on = ["docs/ARCHITECTURE.md#crate-map", "docs/ARCHITECTURE.md#gpu-pipeline", "docs/adr/0004-wgpu-with-native-interop.md", "docs/RULES.md#unsafe"]
-->

The design for frames that stay on the GPU, which ARCH's pipeline needs (capture → wgpu
→ NV12 → encoder with no CPU copy). The types below are built in this crate, and the
Windows `GpuInterop` opens the device and encode targets, and Windows capture
(`WgcCapture::on_gpu`) delivers frames in a ring of shared BGRA textures, as
**Frame lifetime** below describes. `jstream-render` composites one scene on
that device when the engine has one, sampling capture frames there, and the frames a
browser source's page paints on it, and opens its own (hardware adapter first, the
software one as the fallback) when it does not. It has the NV12 pass that can render
into an encode target's plane views (`Nv12Converter`). Windows' video encoders take
encode targets on the context they reached through `Codecs::on_context`, and the
engine's render thread converts each video mix's composite into the next of that mix's
ring of them and pushes it to the mix's sessions on that path.
On Linux, `LinuxGpu` opens the device on Vulkan, and the portal capture
(`PortalCapture::on_context`) imports PipeWire's DMA-BUF frames there and copies each
into a ring of RGBA textures of its own on the context's queue, so its frames need no
fence. Its encode targets are NV12 images exported as DMA-BUFs, whose `FrameSync`
signals the context's timeline semaphore after render's write; the VA-API encoder it
reached through `Codecs::on_context` imports each one as a surface and waits for that
value. A device that can't export them has none. On one whose render node no VA-API
encoder runs on, `Codecs::on_context` gives no GPU encoder, and the engine encodes CPU
frames ([jstream-platform-linux](../jstream-platform-linux/README.md#purpose)).

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
    - On Windows the media adapter is the one that drives the captured display, with
      `TEXTURE_FORMAT_NV12`, as S1 chose it, and on a hybrid-GPU machine where sharing
      an adapter isn't possible, the platform says so (ARCH step 6). On Linux
      (`LinuxGpu`) it is a guess: a hardware Vulkan adapter, DMA-BUF importers first,
      then integrated before discrete, with NV12 only where it can export encode
      targets. A capture whose frames don't import on it falls back to CPU frames, and
      an encoder on another GPU leaves the context without GPU encode, so a hybrid
      machine is not reported.
    - `jstream-render` uses this device when the engine has one, so capture, render and
      encode share one adapter. With no interop it opens its own, hardware adapter first
      with the software one as the fallback.
    - `GpuContext::key()` identifies the context (a `ContextKey`, compared by
      allocation), so the engine can tell whether a texture is on the context a
      compositor runs on. wgpu's own `Device` equality is per instance and can't.
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
- **`SubmitLock`** (`GpuContext::submit_lock`, shared by clones) keeps another thread's
  submit from taking what render staged. "The next submit" is the queue's, not the
  thread's, and render is not the only thread that submits on a context's queue: the
  browser source copies its pages' paints on it, and on Linux the portal capture copies
  each PipeWire frame on it. So render holds the lock from its first `stage` through
  its submit (the composite's, and `Nv12Converter::submit`'s into an encode target),
  and every other thread holds it around its own submits on that queue. Holders keep it only while recording and submitting, never
  across a wait for the GPU. A device render opens for itself has a lock of its own.
- **Encoders and the virtual camera** accept `VideoSurface::Gpu` from the same platform.
  The encoders reach the context through `Codecs::on_context`, a default method like
  `Capture::on_context`. They wait on the D3D11 side for the value the last write
  signalled before reading, then signal a release value that the next write's stage
  waits for (ARCH step 5), and drop the frame once the read is queued: a producer
  writes a target again only when it holds the frame's only clone. They recognise their
  own holds through `FrameSync::platform`, whose default is `None`. A `GpuFrame` from
  another platform, or from another `GpuContext`, is `Unsupported`. The virtual camera
  doesn't take them yet.
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
- **Encoder adapter matching.** The Windows encoders on a context are enumerated for
  the context's LUID (`MFTEnum2` with `MFT_ENUM_ADAPTER_LUID`). AMD's encoder MFTs
  don't report it (S1), so such a machine may find none and fall back to CPU frames.
  Matching by the adapter's vendor ID when the LUID is missing, and taking the first
  hardware MFT when only one adapter has an encoder, is not built. `AdapterId` carries
  both the LUID and the vendor ID for it.
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

- **GPU frames.** Designed and typed in [GPU frames](#gpu-frames) above. Windows capture
  delivers them, the engine composites them, and Windows' H.264 encoder takes encode
  targets. Capture and the encoders reach the context through default methods,
  `Capture::on_context` and `Codecs::on_context`, so other backends are unchanged. The
  engine's render thread writes encode targets and pushes them to the encoders that
  take them. Open: the virtual camera taking them.
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
