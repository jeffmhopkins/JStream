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
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-platform-windows", "docs/ARCHITECTURE.md#threading", "docs/adr/0031-source-types-and-decoding-scope.md", "docs/adr/0014-media-clock.md", "docs/adr/0020-portable-windows-distribution.md", "docs/RULES.md#unsafe", "docs/RULES.md#real-time", "crates/jstream-platform/README.md#purpose"]
verified = { hash = "b3:ae8288cdee1bd783", by = "agent:claude@session_01SZXaqcgUA1wWCDzxs3hWJ6", note = "Stale via ARCH#threading after Merge of main (H7b ADR-0042: as-built runtime moved from ARCH#threading to core README#runtime) into E1: platform-windows text is main's; E1 doesn't touch it" }
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

- **`WgcCapture`** (`Capture`), Windows.Graphics.Capture of displays and windows.
  `WgcCapture::new()` is the CPU path, ARCH's fallback and the default
  ([ARCH › GPU pipeline](../../docs/ARCHITECTURE.md#gpu-pipeline), step 6).
  `WgcCapture::on_gpu(&GpuContext)` is the GPU path (steps 1 and 2), described after
  this list:
  - **Targets:** the displays, then the top-level windows a user would pick: visible,
    titled, not owned by another window, not tool windows, not cloaked, and not this
    process's own. A display's `DeviceId` is `monitor:` and its GDI device name
    (`monitor:\\.\DISPLAY1`); its name is the device name (`DISPLAY1`), with the current
    mode's size. A window's is `hwnd:` and its handle in hex (`hwnd:0x1a2b`); its name is
    its title. Windows may renumber displays when the topology changes, and may reuse a
    handle once its window is destroyed.
  - **Device:** each capture creates its own D3D11 device, on the adapter that drives the
    display (for a window, the display it is on), else on the default hardware adapter.
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
    capture tests check that every `pts` is at most a second before `QpcClock::now` and
    at most 100 ms after it. It can be ahead: on a 29 Hz display the GPU path, which
    delivers a frame within milliseconds, saw frames stamped about 15 ms after `now`, so
    the time is the composition's target rather than when the frame was ready.
  - **Settings:** the yellow capture border is turned off and the cursor is captured,
    best effort: builds without these settings keep their defaults.
  - **End of a capture:** when the window closes or the display goes away, `latest`
    returns `DeviceLost` from then on. A failure in the frame handler ends the capture
    the same way, with its error. A panic there is caught and reported as `InvalidState`
    ([R-UNSAFE-005](../../docs/RULES.md#R-UNSAFE-005)); the enumeration callbacks catch
    panics too.
  - **Threads:** each capture keeps the process MTA alive (`CoIncrementMTAUsage`), so it
    can be started, read and dropped from threads that never initialised COM. From the
    first capture on, one more usage is held until the process exits: the `windows`
    crate caches agile WinRT activation factories for the process, and once the last
    usage went, COM could unload the DLLs behind them, so a later capture crashed.
  - **Teardown:** dropping a stream unregisters its handlers, closes the session and
    pool, and then releases the handler's devices, textures and ring on the dropping
    thread, waiting for a handler that is mid-frame. A frame left in the mailbox is
    released too.
  - **Start:** a target that goes between the lookup and the start of the capture is
    `NotFound` (except a reused window handle), as for a target that was never there.
  - **GPU path** (`on_gpu`, for a context `WindowsGpu` opened; another backend's is
    `InvalidState`). The trait shapes stay as they are: the backend reaches the context
    through this constructor (see [Open questions](#open-questions)).
    - **Device:** every capture uses the context's D3D11 device, so frames are on the
      context's adapter. A display driven by another adapter is still captured; WGC
      copies across adapters itself.
    - **Frames:** each frame is copied on the GPU (`CopySubresourceRegion`) into a slot
      of the stream's ring of BGRA textures, allocated by D3D11 with a shared NT handle
      and opened on D3D12. It is delivered as `VideoSurface::Gpu`: a `Bgra8UnormSrgb`
      `wgpu::Texture` with `TEXTURE_BINDING | COPY_SRC`, the frame's content size (no
      even-size rounding), `PixelFormat::Bgra8` in sRGB (BT.709 primaries, full range),
      opaque, and `pts` as on the CPU path. That is the format `jstream-render`'s
      `Compositor::import` takes.
    - **Sync:** each stream has its own shared fence, which D3D11 signals after each copy
      (then flushes). `FrameSync::stage` waits on it for the frame's copy and signals the
      context's fence when the submit is done. Only one queue signals each fence, so its
      values only increase.
    - **Ring:** four slots. A slot is free once nothing holds its frame and the context's
      fence has passed its last staged use (a frame never staged frees its slot as soon
      as it drops). The next frame goes to a free slot of its size, else a new one while
      the ring has room. When every slot is held, the frame is dropped and counted in
      `frames_skipped`; capture never waits ([R-RT-004](../../docs/RULES.md#R-RT-004)).
      Free slots of an old size are released when the size changes.
    - **Driver lifetime:** every slot and fence holds the context's platform state, so
      the wgpu device clone outlives their raw objects; the capture keeps the device
      alive, not the `GpuContext`.
  - **Not yet:** the CPU readback is not yet counted in stats or measured against a
    budget, as ARCH step 6 asks.

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
  - **Capture** reaches the context's D3D11 device through `WgcCapture::on_gpu`, above.
  - **`encode_target`** allocates an NV12 texture with a shared NT handle on the D3D11
    side, opens it on D3D12, and wraps it as a `wgpu::Texture` through wgpu-hal (in the
    `COMMON` state, which wgpu calls `PRESENT`).
  - **Fence values:** only the wgpu queue signals the context's fence. Each
    `FrameSync::stage` waits for the value that makes the texture ready and signals the
    next value of one counter per device, so values only increase. D3D11 signals fences
    of its own (capture's, per stream), because two queues signalling one counter's
    values could complete out of order and move a fence back. A queue of another device
    (or backend) is `InvalidState`.
  - **Driver lifetime:** the raw D3D11 and D3D12 objects live in platform state that
    also holds a `wgpu::Device` clone, declared last, so `d3d12.dll` stays loaded until
    they are released.
  - **Not yet:** the encoders' D3D11 side. They don't take GPU frames yet; see
    [Open questions](#open-questions).

- **`WindowsVcam`** (`VcamSink`), the virtual camera's engine side
  ([ADR-0020](../../docs/adr/0020-portable-windows-distribution.md)). It is the producer of
  the [`jstream-vcam-proto`](../jstream-vcam-proto/README.md#purpose) contract that the
  [`jstream-vcam-windows`](../jstream-vcam-windows/README.md#purpose) media source reads
  inside Frame Server:
  - **Format:** only the contract's, 1920x1080 BGRA at 30 fps (`offered_format`); any
    other `VcamFormat` is `Unsupported`. Frames are system-memory BGRA
    (`VideoSurface::Cpu`, any stride); GPU frames are `Unsupported` for now (a GPU
    path would copy the engine's texture into the ring on the D3D11 side).
  - **Start:** the media source must already be registered machine-wide, so `start`
    checks for its `HKLM` class key first and is `Unsupported` without it (registering
    is the app's `--register-vcam` step). It then creates, on a D3D11 device of its own
    on the default hardware adapter, a ring of three shared BGRA textures and a shared
    fence, plus the control block. They are named in **this session's namespace**,
    because a non-elevated engine can't create `Global\` objects
    ([ARCH › Packaging](../../docs/ARCHITECTURE.md#packaging)), and secured with the
    contract's DACL for this user. Last, it creates the camera with
    `MFCreateVirtualCamera` for the current user and this session, and starts it.
  - **One producer per session:** a live control block with a fresh heartbeat in this
    session (another engine, or another sink in this process) makes `start`
    `InvalidState`. A stale one, left by a producer that died, is re-initialised as the
    contract says: marked empty, rewritten, then marked live.
  - **Publish:** each frame is copied into its ring slot (`UpdateSubresource`), the
    fence is signalled with the frame's number, and the number is published as
    `latest_frame`. It never waits on the DLL, which may have died.
  - **Liveness:** a heartbeat thread refreshes the block's heartbeat every 200 ms while
    the sink is started, so a pause in `publish` freezes the camera on its last frame
    instead of switching to the DLL's colour bars. Stopping, dropping the sink or the
    process ending lets the DLL fall back to them.
  - **Stop:** removes the camera, stops the heartbeat, marks the block stopped and
    releases the objects. Dropping a started sink does the same.
  - **Threads:** like the codecs, a started sink keeps Media Foundation started and
    the process MTA alive, so it may be driven from threads that never initialised COM.
    Start it from a thread that isn't an STA.

- **`WindowsCredentials`**, the OS credential store for stream keys and OAuth tokens
  ([ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence),
  [R-SEC-001](../../docs/RULES.md#R-SEC-001)): `set`, `get`, `exists` and `delete` on
  Windows Credential Manager (`CredWriteW`, `CredReadW`, `CredDeleteW`). It also
  implements `jstream-platform`'s `StreamKeyStore`, naming each output's credential
  with `CredentialTarget::stream_key`, so a host hands it to `Engine::with_key_store`,
  and `OAuthTokenStore`, naming each account's refresh token with
  `CredentialTarget::oauth_token`.
  - **Names:** a `CredentialTarget`, `JStream/stream-key/<output id>` or
    `JStream/oauth-token/<account>`. Generic
    credentials are one per-user namespace shared by every app, so the product prefix
    keeps ours apart and groups them in Credential Manager, and the kind segment keeps an
    OAuth token from sharing a name with a stream key. Output and account IDs
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

**Not yet:** Media Foundation video decoders, encoders and the virtual camera taking
`VideoSurface::Gpu`, and
a `Backends` constructor, which needs all of them.

**Tests:** the clock conversion, the stream helpers (format choice, sample copies,
packet flags, positions across a device move), the capture helpers (BGRA to NV12 with
known colours, odd sizes and padded rows; the mailbox, across threads; target IDs), the
AAC helpers (configuration check, PCM conversion, packet timing and padding), the H.264
helpers (configuration check, NV12 packing, `pts` mapping), the GPU capture ring's slot choice (reuse, drop when full,
release on resize), the credential helpers
(target names of both kinds, blob limits, zeroing) and the virtual camera's format and
frame checks run on every platform. The QPC clock
test, the start-race test (a destroyed message-only window maps to `NotFound`), the
credential limits check, the credential round trip (a uniquely named credential
written, replaced, read and deleted in the real store, a non-UTF-8 one refused on read,
deleted again on failure), the same through `StreamKeyStore`, and a refresh token
through `OAuthTokenStore` beside a stream key of the same ID run on Windows. Tests against real devices, the OS encoder, the GPU and a display are platform
tests: behind the `hw-tests` feature and `#[ignore]`d, because CI machines may have no
audio devices, no Media Foundation, no GPU and no display
([R-TEST-004](../../docs/RULES.md#R-TEST-004)). The GPU test renders into an encode
target, stages it and checks that the shared fence passes the staged value. The capture
tests list the displays, capture the primary display for a second, checking the
frames' format and `pts`, start a capture of a destroyed window, which must be
`NotFound`, and capture three times in a row in one process. The GPU capture tests read back twenty staged frames (each opaque, so a
read before the copy finished would show), refuse another device's queue, outlive the
context, and hold every frame until the ring drops new ones, then release them. They
need screen activity, so move the mouse while they run. The virtual-camera test starts
`WindowsVcam`, opens the camera's own media source through a source reader (video
processing on, RGB32 out), and publishes grey until Frame Server serves it back; it also
checks that a second producer is refused and that the block is stopped after `stop`. Run
them on a Windows machine with a microphone, speakers, a
display and a DX12 GPU with a hardware H.264 encoder:
`cargo test -p jstream-platform-windows --features hw-tests -- --ignored`. The
virtual-camera test also needs the `jstream-vcam-windows` DLL registered
([its Hardware check](../jstream-vcam-windows/README.md#hardware-check)).

<a id="open-questions"></a>
## Open questions
<!-- @section
-->

- **How the encoders reach the GPU context.** `Codecs::video_encoder` takes no
  `GpuContext`, but the encoders' D3D11 input needs the context's D3D11 device. Capture
  kept the trait shapes: `WgcCapture::on_gpu(&GpuContext)` is a Windows constructor, so
  the host that builds the backends picks the path. The encoders can do the same
  (`MfCodecs::on_gpu`), or the trait can change so `video_encoder` takes
  `Option<&GpuContext>` (or `VideoEncoderConfig` names it). That is a `jstream-platform`
  API decision for E3c.
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
