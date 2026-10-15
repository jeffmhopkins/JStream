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
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-platform-windows", "docs/ARCHITECTURE.md#threading", "docs/adr/0031-source-types-and-decoding-scope.md", "docs/adr/0014-media-clock.md", "docs/RULES.md#unsafe", "docs/RULES.md#real-time", "crates/jstream-platform/README.md#purpose"]
verified = { hash = "b3:e7521abb9d8188bb", by = "agent:claude@session_017dgMHhk9KuuwXxpbutfCzi", note = "Merge of main (#142 WindowsCredentials as StreamKeyStore, kept as main wrote it) into C4a: stale also through jstream-platform#purpose, which adds the VideoDevices trait; this section claims no VideoDevices backend (C4b adds it), so it holds" }
-->

The Windows implementations of the [`jstream-platform`](../jstream-platform/README.md#purpose)
traits. Layer 2, and one of the crates allowed `unsafe`
([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001)): every OS call is wrapped here, behind
the traits' safe APIs (and the credential store's own), with RAII types for COM objects,
events, wave formats and credentials Windows returns
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

- **`WgcCapture`** (`Capture`), Windows.Graphics.Capture of displays and windows. This
  is the CPU path, ARCH's fallback
  ([ARCH › GPU pipeline](../../docs/ARCHITECTURE.md#gpu-pipeline), step 6):
  - **Targets:** the displays, then the top-level windows a user would pick: visible,
    titled, not owned by another window, not tool windows, not cloaked, and not this
    process's own. A display's `DeviceId` is `monitor:` and its GDI device name
    (`monitor:\\.\DISPLAY1`); its name is the device name (`DISPLAY1`), with the current
    mode's size. A window's is `hwnd:` and its handle in hex (`hwnd:0x1a2b`); its name is
    its title. Windows may renumber displays when the topology changes, and may reuse a
    handle once its window is destroyed.
  - **Device:** each capture creates its own D3D11 device, on the adapter that drives the
    display (for a window, the display it is on), else on the default hardware adapter.
    It doesn't use the `GpuContext`; see [Open questions](#open-questions).
  - **Frames:** a free-threaded frame pool of two BGRA buffers. On each `FrameArrived`,
    on a WinRT thread-pool thread, the frame is copied into a staging texture, read back
    and converted to NV12 in BT.709 limited range, which is what the H.264 encoder's CPU
    path takes. Chroma is left-sited, the H.264 default: each chroma sample filters the
    two rows of its block with a `[1 2 1]` tap centred on the block's left column. NV12
    needs even dimensions, so an odd width or height loses its last column or row. When
    the content size changes, the pool is recreated at the new size, and the frames
    after it have that size.
  - **Mailbox:** frames go to a single-slot mailbox, one atomic pointer that both sides
    swap, so `latest` never waits ([R-RT-002](../../docs/RULES.md#R-RT-002)). A frame
    replaced before `latest` took it counts in `frames_skipped`, and its planes are reused
    for the next frame of the same size. Otherwise each frame allocates its planes, on
    the capture thread, and the mailbox boxes every frame it is given.
  - **Times:** `pts` is the frame's `SystemRelativeTime`, which WGC reports as QPC in
    100 ns units. That is `QpcClock`'s time line and unit, so it is used as is. The
    capture test checks that every `pts` is at most a second before `QpcClock::now`, and
    not after it.
  - **Settings:** the yellow capture border is turned off and the cursor is captured,
    best effort: builds without these settings keep their defaults.
  - **End of a capture:** when the window closes or the display goes away, `latest`
    returns `DeviceLost` from then on. A failure in the frame handler ends the capture
    the same way, with its error. A panic there is caught and reported as `InvalidState`
    ([R-UNSAFE-005](../../docs/RULES.md#R-UNSAFE-005)); the enumeration callbacks catch
    panics too.
  - **Threads:** each capture keeps the process MTA alive (`CoIncrementMTAUsage`), so it
    can be started, read and dropped from threads that never initialised COM.
  - **Start:** a target that goes between the lookup and the start of the capture is
    `NotFound` (except a reused window handle), as for a target that was never there.
  - **Not yet:** the GPU path, capture into shared textures (ARCH steps 1 and 2). The
    CPU readback is not yet counted in stats or measured against a budget, as ARCH step
    6 asks.

- **`MfCodecs`** (`Codecs`), Media Foundation codecs
  ([ADR-0031](../../docs/adr/0031-source-types-and-decoding-scope.md)): the **AAC encoder** and
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
    D3D11 path arrives with the Windows `GpuInterop` implementation
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
    - **Keyframe on demand:** `request_keyframe` sets `CODECAPI_AVEncVideoForceKeyFrame`,
      so the next input becomes an IDR frame. An encoder without `ICodecAPI`, or one
      that refuses the property, reports `Unsupported` and keeps its GOP cadence.
      Whether the GOP count restarts at a forced keyframe is up to the encoder.
    - **Asynchronous MFTs** (most hardware encoders) ask for input and announce output
      through events. `encode` and `poll` pump those events, so `can_accept` reflects the
      last pump, and the encode thread polls every tick. Each output event gets exactly
      one `ProcessOutput` call, as Microsoft's asynchronous-MFT model requires. `finish`
      waits up to 5 s for the drain to complete.
    - **Stream changes:** when an encoder changes its output type mid-stream, the
      configured frame size, frame rate, bit rate and profile are re-applied to the type
      it offers. If it refuses them, the offered type is taken as is. The output buffer
      size is then read again.
  - **Not yet:** video decoders, which report `Unsupported` until then.

- **`WindowsGpu`** (`GpuInterop`, [ARCH › GPU pipeline](../../docs/ARCHITECTURE.md#gpu-pipeline)
  step 2, plus the D3D11 device that steps 1 and 5 will use; ported from spike S1):
  - **`open_device`** opens the DX12 wgpu device on the media adapter. That is the
    hardware adapter that drives the primary display, else the first hardware adapter
    with NV12 textures. It also creates a multithread-protected D3D11 device on the same
    adapter, and a fence shared between D3D11 and D3D12. The `AdapterId` carries the
    adapter's LUID and PCI vendor ID.
  - **`encode_target`** allocates an NV12 texture with a shared NT handle on the D3D11
    side, opens it on D3D12, and wraps it as a `wgpu::Texture` through wgpu-hal (in the
    `COMMON` state, which wgpu calls `PRESENT`).
  - **Fence values:** each `FrameSync::stage` waits for the value that last released
    the texture and signals the next value of one counter per device, so values only
    increase. A queue of another device (or backend) is `InvalidState`.
  - **Driver lifetime:** the raw D3D11 and D3D12 objects live in platform state that
    also holds a `wgpu::Device` clone, declared last, so `d3d12.dll` stays loaded until
    they are released.
  - **Not yet:** the D3D11 side. Capture and the encoders don't take GPU frames yet; see
    [Open questions](#open-questions).

- **`WindowsCredentials`**, the OS credential store for stream keys
  ([ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence),
  [R-SEC-001](../../docs/RULES.md#R-SEC-001)): `set`, `get`, `exists` and `delete` on
  Windows Credential Manager (`CredWriteW`, `CredReadW`, `CredDeleteW`). It also
  implements `jstream-platform`'s `StreamKeyStore`, naming each output's credential
  with `CredentialTarget::stream_key`, so a host hands it to `Engine::with_key_store`.
  - **Names:** a `CredentialTarget`, `JStream/stream-key/<output id>`. Generic
    credentials are one per-user namespace shared by every app, so the product prefix
    keeps ours apart and groups them in Credential Manager, and the kind segment keeps a
    future secret kind (OAuth tokens) from sharing a name with a stream key. Output IDs
    are opaque, so any ID is taken except an empty one, one with a control character (a
    NUL would cut the name short, so two IDs could name one credential) or one that makes
    the name longer than `CRED_MAX_GENERIC_TARGET_NAME_LENGTH` UTF-16 units.
  - **Storage:** `CRED_TYPE_GENERIC`, persisted `CRED_PERSIST_LOCAL_MACHINE`: it survives
    logoff and reboot, for this user on this machine, and doesn't roam with a roaming
    profile, so a key typed into a portable install stays on that machine. Windows
    encrypts it for the user; any process running as the user can read it.
  - **Secrets:** a secret is 1 to `CRED_MAX_CREDENTIAL_BLOB_SIZE` bytes; anything else is
    `Unsupported`. The limits are copied so the checks are tested on every platform, and
    a Windows test compares them with the SDK's. `get` returns `jstream-platform`'s `Secret`, which zeroes its
    bytes on drop, prints as `Secret("***")` and isn't `Clone`; a stored blob that
    isn't UTF-8 is refused as `Unsupported`, without naming it. The copy Windows returns is zeroed before
    `CredFree`. Nothing here logs, and errors carry no secret or its length.

**Not yet:** Media Foundation video decoders, the D3D11 side of the GPU path (capture
into shared textures, encoders taking `VideoSurface::Gpu`), the virtual-camera sink, and
a `Backends` constructor, which needs all of them.

**Tests:** the clock conversion, the stream helpers (format choice, sample copies,
packet flags, positions across a device move), the capture helpers (BGRA to NV12 with
known colours, odd sizes and padded rows; the mailbox, across threads; target IDs), the
AAC helpers (configuration check, PCM conversion, packet timing and padding), the H.264
helpers (configuration check, NV12 packing, `pts` mapping) and the credential helpers
(target names, blob limits, zeroing) run on every platform. The QPC clock
test, the start-race test (a destroyed message-only window maps to `NotFound`), the
credential limits check, the credential round trip (a uniquely named credential
written, replaced, read and deleted in the real store, a non-UTF-8 one refused on read,
deleted again on failure) and the same through `StreamKeyStore` run on Windows. Tests against real devices, the OS encoder, the GPU and a display are platform
tests: behind the `hw-tests` feature and `#[ignore]`d, because CI machines may have no
audio devices, no Media Foundation, no GPU and no display
([R-TEST-004](../../docs/RULES.md#R-TEST-004)). The GPU test renders into an encode
target, stages it and checks that the shared fence passes the staged value. The capture
tests list the displays, capture the primary display for a second, checking the
frames' format and `pts`, and start a capture of a destroyed window, which must be
`NotFound`. Run them on a Windows machine with a microphone, speakers, a
display and a DX12 GPU with a hardware H.264 encoder:
`cargo test -p jstream-platform-windows --features hw-tests -- --ignored`.

<a id="open-questions"></a>
## Open questions
<!-- @section
-->

- **How capture and the encoders reach the GPU context.** `Capture::start` and
  `Codecs::video_encoder` take no `GpuContext`, but WGC capture into shared textures and
  the encoders' D3D11 input both need the context's D3D11 device. One option is to keep
  the trait shapes: the Windows backends share the context the engine opened, through
  `WindowsGpu`. The other is to change them, so that `start` and `video_encoder` take
  `Option<&GpuContext>` (or `VideoEncoderConfig` names it). This is a `jstream-platform`
  API decision, so it is left for review before the D3D11 side is built.
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
