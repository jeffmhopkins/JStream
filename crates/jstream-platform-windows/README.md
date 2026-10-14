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
depends_on = ["docs/ARCHITECTURE.md#crate-map", "docs/ARCHITECTURE.md#threading", "docs/adr/0003-media-stack-and-codecs.md", "docs/adr/0014-media-clock.md", "docs/RULES.md#unsafe", "docs/RULES.md#real-time", "crates/jstream-platform/README.md#purpose"]
verified = { hash = "b3:b2de198c374dcb08", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Vs main (AAC from #58): adds the CPU-path H.264 encoder (hardware then software MFTs, CBR/VBR, GOP, no B-frames, High, BT.709 limited, exact pts round trip, CleanPoint keyframes, async event pumping, 5 s drain) and its hardware/system-memory open question; Not-yet and Tests updated; checked against codec/{mod,mf,h264}.rs" }
-->

The Windows implementations of the [`jstream-platform`](../jstream-platform/README.md#purpose)
traits. Layer 2, and one of the crates allowed `unsafe`
([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001)): every OS call is wrapped here, behind
the traits' safe APIs, with RAII types for COM objects, events and wave formats
([R-UNSAFE-003](../../docs/RULES.md#R-UNSAFE-003)). It uses Microsoft's `windows` crate
([R-UNSAFE-004](../../docs/RULES.md#R-UNSAFE-004)). The backends exist only on Windows;
elsewhere the crate builds just its platform-independent helpers, which are tested on
every platform.

The code is ported from spikes S1 to S3. This first cut has:
- **`QpcClock`** (`MediaClock`): QueryPerformanceCounter in **100 ns ticks**
  (`timebase()`), the unit WASAPI and Media Foundation use, and the unit the
  `jstream-platform` README proposes for every platform. A raw reading is converted with
  the formula WASAPI uses for `pu64QPCPosition`, so clock readings and packet times share
  one time line ([ADR-0014](../../docs/adr/0014-media-clock.md)). `now` never decreases.
- **`WasapiAudio`** (`AudioIo`):
  - **Listing** the active capture and render endpoints, with the console-role default
    of each.
  - **Streams** for every target the traits define: an input, the default input, loopback
    of an output or the default output, process loopback (a process and its children,
    through the virtual process-loopback device), an output and the default output.
    Each stream is event-driven and shared-mode, on its own thread registered with MMCSS
    "Pro Audio" ([ARCH › Threads, clock and sync](../../docs/ARCHITECTURE.md#threading)).
    That thread owns the stream's COM objects. In its loop it calls the handler and does
    not allocate, lock or log ([R-RT-001](../../docs/RULES.md#R-RT-001)): the sample
    buffer is allocated once, before the stream starts.
  - **Format:** samples are interleaved `f32` at the device's own rate. A device whose
    mix format is already `f32` in a layout `jstream-media` has (mono, stereo, 5.1) is
    opened as is. For any other format the backend asks WASAPI to convert
    (`AUTOCONVERTPCM`) to `f32` at the same rate: it converts integer samples and
    downmixes other channel counts to stereo, and never resamples. For an output with
    another channel count the stream is stereo, and WASAPI maps it onto the device's
    speakers. `jstream-media`'s 5.1 has its surround pair at the back, so a 5.1 device
    whose mix puts it at the side is opened through the same conversion, and WASAPI
    maps its channels into that order. Process loopback is asked for 48 kHz stereo,
    since it has no mix format.
  - **Times and positions:** packet times are WASAPI's QPC positions, `None` when the
    device flags a timestamp error. Device positions count frames since the stream
    started.
  - **Default devices are followed:** a stream on a default device moves to the new
    default when the user changes it or the device goes away. Its positions continue, and
    the first packet after the move is flagged as a discontinuity. If the new device has
    a different format, the stream ends with `DeviceLost` so the engine can reopen it. A
    stream on one device ends with `DeviceLost` when that device goes away.
  - **Failures and panics:** a failure in a stream's loop is reported once through the
    handler's `on_error`, and the stream ends. A panic on the stream thread, the
    handler's included, is caught there. The stream ends, and `on_error` gets
    `InvalidState`. COM callbacks (activation completion, device notifications) catch
    panics too ([R-UNSAFE-005](../../docs/RULES.md#R-UNSAFE-005)).
  - **Buffers:** capture and loopback ask for a 200 ms buffer, as S3 did, so a late
    thread loses nothing. Render asks for 40 ms, which bounds how far ahead the output
    handler is asked for samples.

- **`MfCodecs`** (`Codecs`), Media Foundation codecs
  ([ADR-0003](../../docs/adr/0003-media-stack-and-codecs.md)): the **AAC encoder** and
  the **H.264 encoder from CPU frames**. AAC:
  - **Encoder:** the first AAC encoder MFT Media Foundation ranks for 16-bit PCM in. On
    Windows that is Microsoft's software AAC encoder. It takes 44.1 or 48 kHz; mono or
    stereo at 96, 128, 160 or 192 kbps, or 5.1 at six times those (576, 768, 960 or
    1152 kbps). Any other configuration is `Unsupported`.
  - **Input:** samples are converted from `f32` to 16-bit PCM, clamped and rounded.
  - **Output:** one raw AAC-LC access unit (1024 frames, no ADTS) per packet, the framing
    `jstream-media` defines. Packet times count from the first buffer's `pts`, in the
    input's sample time base, as the `AudioEncoder` trait says. `finish` pads the last
    access unit with silence, then drains the MFT. An empty output sample is skipped.
  - **Threads:** each session keeps Media Foundation started and the process MTA alive
    (`CoIncrementMTAUsage`), so the engine's encode thread can drive it without
    initialising COM. Sessions are opened from a thread that isn't an STA.
  - **Teardown:** dropping a session ends streaming and calls `ShutdownObject` on the
    activation object, as Microsoft asks. One output sample is reused for every pull.
  - **H.264 encoder (CPU path):** this takes NV12 `VideoSurface::Cpu` frames and copies
    them into Media Foundation memory buffers. It is ARCH's fallback path; the zero-copy
    D3D11 path arrives with the GPU frame type
    ([ARCH › GPU pipeline](../../docs/ARCHITECTURE.md#gpu-pipeline), step 6).
    - **Choosing an encoder:** hardware encoder MFTs come first, then the OS software
      encoder, following `HardwarePreference`. The first one that accepts the media
      types wins, and `CodecInfo::hardware` says which kind it was.
    - **Configuration:** CBR or peak-constrained VBR, the keyframe interval as the GOP
      size, no B-frames (so packets come out in presentation order and `dts == pts`),
      low latency, the High profile, and BT.709 limited range. `ICodecAPI` settings are
      best effort; the media type always carries the bit rate.
    - **Timing:** each packet gets back its frame's exact `pts`, even when the frame's
      time base doesn't round evenly to the encoder's 100 ns. Keyframes come from
      `MFSampleExtension_CleanPoint`.
    - **Asynchronous MFTs** (most hardware encoders) ask for input and announce output
      through events. `encode` and `poll` pump those events, so `can_accept` reflects the
      last pump, and the encode thread polls every tick. `finish` waits up to 5 s for the
      drain to complete.
  - **Not yet:** video decoders, which report `Unsupported` until then.

**Not yet:** Windows.Graphics.Capture, Media Foundation video decoders, the GPU interop
path ([ARCH › GPU pipeline](../../docs/ARCHITECTURE.md#gpu-pipeline)), the
virtual-camera sink, and a `Backends` constructor, which needs all of them.

**Tests:** the clock conversion, the stream helpers (format choice, sample copies,
packet flags, positions across a device move), the AAC helpers (configuration check,
PCM conversion, packet timing and padding) and the H.264 helpers (configuration check,
NV12 packing, `pts` mapping) run on every platform, and the QPC clock test
runs on Windows. Tests against real devices and the OS encoder are platform tests: behind
the `hw-tests` feature and `#[ignore]`d, because CI machines may have no audio devices
or Media Foundation ([R-TEST-004](../../docs/RULES.md#R-TEST-004)). Run them on a Windows
machine with a microphone and speakers:
`cargo test -p jstream-platform-windows --features hw-tests -- --ignored`.

<a id="open-questions"></a>
## Open questions
<!-- @section
-->

- **Render times.** `AudioRenderBuffer::time` (when the first frame will play) is always
  `None` for now. `IAudioClock` can give it once the engine needs output sync, for
  example for monitoring with delay compensation.
- **Hardware encoders and CPU frames.** A hardware MFT that takes NV12 when its media
  types are set might still refuse system-memory samples at `ProcessInput`. If it does,
  `HardwarePreference::Prefer` fails instead of falling back. The fallback only happens
  when the media types are refused, which is enough for software-only machines. The
  `hw-tests` hardware test will show whether this happens. The GPU path, which feeds
  hardware encoders D3D11 textures as S1 did, makes the question moot for them.
- **Moving to a default device with another format.** For now the stream ends with
  `DeviceLost` and the engine reopens it. Instead the backend could ask WASAPI to convert
  to the old format, which would keep the stream running but would resample if the
  rates differ. Decided with `jstream-audio`.
