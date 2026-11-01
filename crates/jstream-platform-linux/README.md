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
traits, as [ADR-0060](../../docs/adr/0060-linux-platform-support.md) lays them out. That
ADR is **accepted**; this crate has its first three parts (the platform basics, screen
capture, then VA-API H.264 and HEVC encoding), the software H.264 and AAC encoders, PipeWire and
ALSA audio input and output, V4L2 webcams and capture cards, the capture's DMA-BUF import into Vulkan, and the secret
store of [ADR-0071](../../docs/adr/0071-linux-secret-store.md), and the rest follows in later
PRs. Layer 2, and one
of the crates allowed `unsafe` ([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001)): every
system call is wrapped here behind safe APIs, with RAII types for what the system hands
back ([R-UNSAFE-003](../../docs/RULES.md#R-UNSAFE-003)).

**System libraries are loaded at run time** (ADR-0060 §8). libpipewire, libva and
libasound are opened with `dlopen` through `libloading` when a backend first needs them, and only the
functions the crate calls are declared, each with its C signature in a `SAFETY` comment.
Nothing is linked, so the crate builds and tests on any machine, CI included, without
their headers. The Vulkan loader is loaded the same way, by wgpu-hal through `ash`. This departs from [R-UNSAFE-004](../../docs/RULES.md#R-UNSAFE-004)'s
preference for the `pipewire` crate for the reason ADR-0060 gives: that crate needs
libpipewire's headers and libclang wherever the workspace builds. A missing or too-old
library is `PlatformError::MissingComponent`, with the package to install. Everything that
calls Linux is under `cfg(target_os = "linux")`. On other targets the crate builds just its
platform-independent helpers, which are tested on every platform.

It has:
- **`MonotonicClock`** (`MediaClock`): `CLOCK_MONOTONIC` in **100 ns ticks**
  (`timebase()`), the unit the Windows backend uses. PipeWire and V4L2 stamp their data
  on the same clock in nanoseconds, so their timestamps map with a division
  (`hns_from_timespec`). `now` never decreases, and doesn't allocate, lock or block.
- **`Session`**: the session type (Wayland, X11, headless) and the desktops, read from
  `XDG_SESSION_TYPE`, `WAYLAND_DISPLAY`, `DISPLAY` and `XDG_CURRENT_DESKTOP`, and its
  ADR-0060 tier: GNOME or KDE on Wayland is supported, any other graphical session is
  best effort.
- **`PortalCapture`** (`Capture`): lists one target per kind, "a display" and "a
  window", which the desktop's ScreenCast portal picker resolves when the capture starts.
  A headless session lists none. `start` runs the portal's session flow on the session
  bus: `CreateSession`, `SelectSources` (one source of the picked kind, the cursor drawn
  into the frames, or sent as metadata for `CursorCapture::Metadata` where the portal
  offers that mode; for `CursorCapture::Hidden`, cursor mode hidden where it's offered,
  else metadata that the stream doesn't ask PipeWire for, else drawn in;
  `persist_mode` 2 with the last restore token), `Start` (the picker;
  `start` blocks while it is open, up to five minutes) and `OpenPipeWireRemote`. Each
  request's `Response` is matched on the request path computed before the call, or on
  the handle the call returns. Only the portal is listened to: its unique bus name
  (from `GetNameOwner`) must be the sender of every reply and signal the flow takes,
  and the match rules name it, so another process on the session bus can't forge a
  response or end the session. A cancelled picker is an `Os` error with code 1. The
  flow is a state machine without I/O (`portal/handshake.rs`), tested message by
  message. The new restore token `Start` returns replaces the spent one, in memory and,
  with `PortalCapture::with_token_dir`, in a file only the user can read: written 0600
  to a fresh file and renamed into place, and read only from a regular file the user
  owns that no one else can read or write. So the next start skips the picker. The
  tokens are kept per grant: `start` keeps one per kind of pick, and `start_with` one per
  source and kind, so two window sources each reopen their own window and a new source
  gets the picker rather than the last pick. A source's file is
  `screencast-<kind>-source-<hex of its ID>.token`, so any ID stays a plain name in the
  folder (an ID over 96 bytes keeps its token in memory only), and `forget_source`
  removes both of its files when the source is deleted; a start of that source still
  waiting on the picker then doesn't store the token it is granted. A
  thread keeps the portal's connection open for the session's
  life and reports the portal closing it (sharing stopped, grant revoked) as
  `DeviceLost`. `on_context`, on a context `LinuxGpu` opened with DMA-BUF import, gives
  the same backend (sharing its restore tokens) on the GPU path, below; on one without,
  `Ok(None)`, and on another platform's context, `InvalidState`.
- **`LinuxGpu`** (`GpuInterop`) opens the engine's wgpu device on Vulkan: the first
  hardware adapter, those with DMA-BUF import (wgpu's `VULKAN_EXTERNAL_MEMORY_DMA_BUF`)
  before the rest and integrated before discrete GPUs, since the desktop doesn't say
  which GPU it composites on. It records, per format the stream offers, the DRM format
  modifiers the device can import as a single-plane DMA-BUF and sample
  (`vkGetPhysicalDeviceFormatProperties2` and `…ImageFormatProperties2` through `ash`),
  and which formats a V4L2 buffer plane may be imported as in the `LINEAR` layout
  (`Bgra8Unorm`, `Rgba8Unorm`).
  Multi-plane layouts (Intel CCS, AMD DCC with metadata) are left out, because
  wgpu-hal's `texture_from_dmabuf_fd` takes one plane. Its encode targets
  (`encode_target`, `gpu/export.rs`) are NV12 images in the `LINEAR` DRM layout with
  dedicated memory exported as a DMA-BUF (`vkGetMemoryFdKHR`), one object holding both
  planes. The image lists its view formats (NV12, and the R8 and RG8 plane views),
  which a mutable-format image in a DRM layout must, and the export check asks about
  the same image. A device has them when it can export such an image render draws
  into and samples, has Vulkan 1.2 (timeline semaphores) and wgpu's NV12 and DMA-BUF features,
  and names its DRM render node (`VK_EXT_physical_device_drm`); otherwise
  `encode_target` is `Unsupported` and the encoders take CPU frames. A target's
  `FrameSync` adds a signal of the context's timeline semaphore to the submit that
  writes it, and the encoder waits for that value. Render hands a target back in
  `PRESENT` (`PRESENT_SRC_KHR`), not through a release to the foreign queue family; a
  `LINEAR` image has no compression, so its memory holds the picture whatever the
  layout.
- **The D-Bus client** (`dbus.rs`) is the crate's own: the wire format in both byte
  orders, `EXTERNAL` authentication with file descriptor passing, and blocking calls
  with timeouts over the session bus socket. Received messages are untrusted: lengths
  are checked, nesting is bounded, and malformed input is an error. The portal needs a
  few calls and two signals, which didn't justify a D-Bus crate and its async runtime,
  and libdbus would be one more C library to load. As a best effort, it zeroes the
  buffers of the messages it encodes and decodes, spare capacity included, before
  freeing them, since the secret store's carry secrets; copies left when its receive
  buffer grows are not reached.
- **`LinuxSecrets`** (`StreamKeyStore`, `OAuthTokenStore`;
  [ADR-0071](../../docs/adr/0071-linux-secret-store.md),
  [R-SEC-001](../../docs/RULES.md#R-SEC-001)) keeps stream keys and OAuth refresh tokens
  in the desktop's keyring through the Secret Service, on the crate's D-Bus client. Each
  call opens the session bus and a `plain` session (the bus may start the service).
  Items go in the default collection, created when the keyring has none, with the
  attributes `application = "jstream"`, `kind` and `id`, labelled with the Windows
  credential name (`JStream/stream-key/<output id>`). A locked collection or item is
  unlocked, which may show the desktop's prompt for up to five minutes; a dismissed
  prompt fails the call. **Only when no Secret Service is reachable** (no session bus,
  or the bus has no `org.freedesktop.secrets` and can't start one) do secrets go to the
  fallback file, `secrets` in `$XDG_CONFIG_HOME/jstream` (`LinuxSecrets::config_dir`).
  The file is plaintext, one secret per line, written to a fresh `0600` file (named for
  the process and the write) and renamed into place, in a folder created `0700` (an
  existing folder is left as it is). The file is read only when it is a regular file the
  user owns that nobody else can read or write, and refused, never repaired, otherwise.
  Calls on one store and its clones take turns, so a host clones one store for both
  traits; separate stores on one file don't, and the last update wins. A keyring that refuses, is locked or fails is
  an error, never a reason to use the file. A secret cleared while no keyring is
  reachable is recorded as cleared, so the keyring's copy is deleted later rather than
  coming back. Once the Secret Service is reachable, each call first moves the file's
  entries into the keyring and deletes the file when it is empty.
  `LinuxSecrets::fallback_warning` does the same and returns the warning a host shows
  and logs at startup while the file holds a secret. It names the file and never a
  secret.
- **The PipeWire video stream** (`pipewire/stream.rs`) connects to the granted node on
  the portal's remote, on a `pw_thread_loop`. It offers BGRx, BGRA, RGBx and RGBA at
  any size and rate (`spa.rs` builds and parses the SPA pods by hand, since libspa's
  builder is inline C), asks for mappable shared-memory buffers with header and crop
  metadata, and copies each frame into a CPU `VideoSurface` (`Bgra8` or `Rgba8`,
  padding bytes made opaque, the crop applied) in `jstream_platform::mailbox`, the
  single-slot mailbox Windows' capture uses too. On the GPU path it first offers one
  DMA-BUF format per pixel format with the device's modifiers (flagged mandatory and
  don't-fixate, so the producer picks the one it allocates), then shared memory. A
  DMA-BUF frame is imported once per PipeWire buffer with wgpu-hal's
  `texture_from_dmabuf_fd`, waited on until the producer's writes are done (the
  DMA-BUF polls readable, the implicit-sync fences), and drawn on the context's queue
  into one of four `Rgba8UnormSrgb` frame textures of the stream's own, cropped and
  with padding made opaque; the buffer goes back to PipeWire once that copy is done,
  so render samples only the stream's textures. A copy still running after one second
  (`COPY_WAIT`) ends the GPU path, and that buffer goes back while the GPU may still
  read it, so the last frame can tear; a producer whose writes aren't done after
  100 ms (`PRODUCER_WAIT`) costs a skipped frame. The copies and the frame textures
  are on the context's one queue, so the frames' `FrameSync` only checks the queue.
  Each copy is submitted under the context's `SubmitLock`, so it can't take the
  timeline-semaphore signal render staged on an encode target for its own next
  submit (#667). A frame that doesn't import or copy ends the GPU path: the
  stream offers shared memory only, PipeWire renegotiates, and the capture goes on
  with CPU frames. Its `pts` is
  the buffer header's `CLOCK_MONOTONIC` time when that is plausible, else the clock's
  `now`. With cursor metadata the stream also asks for `SPA_META_Cursor`, with room for
  a 256-pixel picture, and reads it from every buffer on either path, before the frame
  and whether or not the buffer holds one:
  the position less the hot spot, less the crop's origin, and the picture (RGBA, BGRA
  or their padded forms, made straight RGBA) when the buffer carries a new one. A
  buffer without one keeps the last picture, an empty one or an `id` of 0 hides the
  pointer, and every offset and size is checked against the metadata's own. A change
  goes into a second mailbox, which the stream's `cursor` reads as `CaptureCursor`.
  The callbacks catch panics (R-UNSAFE-005); a stream error is `DeviceLost`.
  The portal protocol's option values are here too: source types, cursor modes,
  `persist_mode`, request handle tokens and request object paths.
- **`PipeWire`** loads `libpipewire-0.3.so.0` and checks it is at least ADR-0060's floor
  (1.0). **`PipeWireAudio`** (`AudioIo`, ADR-0060 §4) is built on it. Each call
  connects to the user's daemon on its own `pw_thread_loop` (`pipewire/audio.rs`) and
  reads the registry with two core round trips: nodes, clients, the core's
  `default.clock.rate`, the `default` metadata's `default.audio.source` and
  `default.audio.sink`, and each device node's info, bound for its channel count
  (`audio.channels`, else the length of `audio.position`), which a registry global
  never carries (a node's proxy is destroyed when its global goes). No daemon is a missing component whose remedy starts it.
  `pipewire/graph.rs` turns that into devices (`Audio/Source`, `Audio/Source/Virtual`,
  `Audio/Sink` and `Audio/Duplex` nodes, with `node.name` as the stable id), the
  processes playing audio (`Stream/Output/Audio` nodes, with the pid from their
  client's `pipewire.sec.pid` and the executable's name from `/proc`, else the
  application's name), and which node each target connects to: `target.object`, with
  `stream.capture.sink` for an output's loopback, and a process's loopback recording
  its first playback stream. A stream is fixed to interleaved `f32` at the graph's rate
  in the device's layout (mono, stereo or 5.1; any other count, or a default with no
  device, as stereo; `spa/audio.rs` builds and parses the format pods), so PipeWire
  converts and the handler never sees a format change. The stream is connected with
  `RT_PROCESS`, so `process` runs on its context's own data loop, PipeWire's real-time
  thread (with real-time priority from `module-rt` where the system grants it), while
  registry, metadata and the stream's state and format events stay on the loop thread.
  `process` touches only the stream's own state, shared with the loop thread through
  atomics (`Gate`: it calls the handler only while the stream hasn't failed, and failing
  it waits for a running call to return), so it doesn't allocate, lock or wait: input
  packets carry their device position and a media-clock time from
  `pw_stream_get_time_n` (the capture time, before the device's delay), a corrupted
  buffer marks a discontinuity and its frames still count in the position, an empty one
  (nothing playing into the source) is a silent packet, and output
  asks the handler to fill each buffer. A named device or process whose node is
  removed is `DeviceLost`; a default follows the session manager. The callbacks catch
  panics (R-UNSAFE-005): a panicking handler is told once, with `InvalidState`, and
  isn't called again. Dropping the stream stops its callbacks before it returns,
  without telling the handler it failed. Each input node's ALSA card (from its info's
  `api.alsa.pcm.card`) is kept for pairing webcams.
- **`AlsaAudio`** (`AudioIo`): audio input and output through ALSA, the fallback
  ADR-0060 §4 names for a machine without a PipeWire daemon, on `libasound.so.2` loaded
  at run time. `devices` lists the PCMs in ALSA's name hints (`default`, `sysdefault:`,
  `hw:`, `plughw:`, a sound server's own PCM), each in the directions its hint gives, with
  `default` as the default; it leaves out `null` and the speaker-position and S/PDIF
  aliases (`front:`, `surround51:`, `iec958:` and the like), which can still be opened by
  name. A stream asks for stereo at 48 kHz and takes the device's nearest channel count
  and rate with ALSA's resampling off, so a `hw:` device runs at a rate it has (a plugin
  PCM such as `default` may still convert below the API). Samples are `FLOAT_LE`, else
  `S32_LE` or `S16_LE` converted to and from `f32`; 1, 2 and 6 channels map to mono,
  stereo and 5.1, and any other count is `Unsupported`. Each stream runs on its own thread
  with 10 ms periods and a 40 ms buffer, non-blocking, waking at least every 100 ms, so
  dropping it stops it promptly; its buffers are allocated before its loop. A packet's
  time is the media clock at the read less the frames still queued and its own; a render
  buffer's is the clock plus the frames queued ahead. An overrun or underrun is
  recovered (a capture stream is then started again, since recovery only prepares it;
  playback restarts with its next write, dropping the rest of that period) and flags the
  next packet as a discontinuity (ALSA doesn't say how many frames were lost, so the
  device position doesn't jump); a device that can't be recovered ends the stream with
  `DeviceLost`. A short write is finished before the next period is rendered. An unknown
  PCM is `NotFound`. Desktop and application audio (`*Loopback`, `processes`) are `Unsupported`: ALSA has no loopback,
  and ADR-0060 §4 leaves them to PipeWire. alsa-lib prints its own error lines to stderr
  (such as for an unknown PCM); silencing them needs a variadic C callback, which stable
  Rust can't define.
- **`V4l2Devices`** (`VideoDevices`): webcams and capture cards through V4L2, the "plain
  V4L2 backend" ADR-0060 §9 keeps in scope. There is no library: the nodes are driven
  with `ioctl`s declared by hand from `<linux/videodev2.h>` (`v4l2/abi.rs`, the 64-bit
  layouts, checked against the kernel headers; the backend isn't built for 32-bit
  targets). `devices` lists each `/dev/videoN` that captures video with streaming I/O
  through the single-planar API or, failing that, the multi-planar one (SoC cameras and
  HDMI capture bridges; a UVC webcam's metadata node captures neither), named by its
  card name, with udev's `/dev/v4l/by-id/` link as the ID where there is one (it
  survives replugging and renumbering), else the node's path. Only such paths are ever
  opened, since an ID can come from a saved collection. Nodes the user can't open are
  left out. Built with an audio backend (`with_audio`), it pairs each device with its
  microphone (`pairing.rs`): the node's device in sysfs (the USB device, for a USB
  interface) holds its sound cards, and the backend names its input on the first of them
  that has one, PipeWire by card index and ALSA by the card's id
  (`sysdefault:CARD=<id>`, else `plughw:`, `dsnoop:` or `hw:`). A capture card whose
  audio is another PCI function isn't paired, and a backend that can't list its inputs
  pairs nothing. `formats` lists every enumerated size and frame interval of the pixel
  formats it delivers: YUYV (as `Yuy2`, converted to NV12), NV12 and I420 as they are
  (also the multi-planar `NV12M` and `YUV420M`, each image plane in its own buffer
  plane, mapped on its own and copied with `jstream_platform::yuv::copy_planes` at that
  plane's stride, refused as `Unsupported` if the driver settles on another plane
  count), 32-bit RGB with the alpha or padding byte last as it is (`AR24` and `XR24` as
  `Bgra8`, `AB24` and `XB24` as `Rgba8`, as capture cards and `vivid` offer them; the
  engine reads a capture's alpha as opaque), and MJPEG (`MJPG` or `JPEG`) for the
  engine's decoder. Where two FourCCs
  deliver the same encoding, `open` asks for one whose own sizes list the format. A
  stepwise size range offers its largest size and the common sizes its steps reach, a
  stepwise interval its shortest, and a driver that enumerates no sizes or no intervals
  its current ones. `open` sets the format (refused as `Unsupported` if the driver
  settles on another) and frame interval, maps four driver buffers, and starts a capture
  thread that waits on the node and an `eventfd`, converts each filled buffer with
  `jstream_platform::yuv` or hands it to the decoder, queues it back, and fills the
  single-slot mailbox. A frame's time is its `CLOCK_MONOTONIC` buffer timestamp when it
  is recent, else the clock's `now`. Buffers the driver marks as corrupt and short raw
  frames are dropped, rejected JPEGs counted. Raw colour is what the driver reports,
  else the colour space's default, else BT.601 below 720 lines and BT.709 from there, in
  limited range. An unplug is `DeviceLost`, a device another application streams from
  `Busy`. Dropping the stream wakes and joins the thread, which turns the stream off,
  unmaps the buffers and closes the node.
  `on_context`, on a context `LinuxGpu` opened with DMA-BUF import of a format V4L2
  buffers come in, in the `LINEAR` layout they have, gives the backend on the GPU path
  (`v4l2/gpu.rs`); on one without, `Ok(None)`, and on another platform's context,
  `InvalidState`. It opens the 32-bit RGB formats and YUYV there: once the stream is
  on, it exports each driver buffer as a DMA-BUF (`VIDIOC_EXPBUF`, read-only) and
  imports it once (`gpu/dmabuf.rs`), RGB as a `Bgra8Unorm` or `Rgba8Unorm` texture and
  YUYV as `Rgba8Unorm` at half the width, each texel `Y0 U Y1 V`. The capture thread
  draws each filled buffer into the stream's own `blit::Ring` texture on the context's
  queue: RGB copied and made opaque, YUYV converted to R'G'B' with the frame's matrix
  and range as the engine's CPU path converts NV12 (each pixel its own luma and its
  pair's chroma, limited range scaled to full). It waits for that draw before it hands
  the buffer back with `VIDIOC_QBUF`, so the driver never writes into a frame the GPU
  still reads. The ring texture is the `Gpu` frame. Other formats and compressed ones
  are `Unsupported` there, and a device whose buffers don't export or import fails the
  open with the OS error; either way the engine opens it on the CPU path. A buffer that
  can't be read on the GPU (one whose data doesn't start at its memory's start) or a
  draw that fails or isn't done within a second ends the stream as `DeviceLost`, without handing that buffer back, and the backend on that
  context then refuses every format, so the engine's retry opens the device on the CPU
  path.
- **`LinuxBackends`**: the backend set, ADR-0060 §4's choice made once when it is built
  (`backends.rs`). Audio is `PipeWireAudio` when libpipewire loads and the daemon
  answers a device listing, else `AlsaAudio`; `audio_backend` says which and
  `pipewire_unavailable` why PipeWire wasn't used, and with neither it is a missing
  component naming both reasons. The same audio backend is handed to `V4l2Devices` for
  pairing, so a paired microphone is one the engine can open. It holds the clock, portal
  capture (with a restore-token folder when given), the paired video devices, the audio
  backend and `VaapiCodecs`; `into_backends` adds the GPU interop and media decoders the
  host supplies (`LinuxGpu` is this crate's GPU interop; it has no media decoders yet)
  for the engine's `Backends`.
- **`probe_encoders`** opens each DRM render node (`/dev/dri/renderD*`) through libva and
  libva-drm, and records the best H.264 profile (High, then Main, then Constrained
  Baseline) its driver offers, then HEVC Main, each with its encode entry point (full,
  then low-power). The display is terminated before its node is closed.
  **`VaapiCodecs`** (`Codecs`) lists those as hardware H.264 and HEVC encoders and picks
  one for a configuration with `choose_encoder`: the first that encodes its codec. That
  refuses other codecs and a software-only request (software H.264 is OpenH264's, below,
  and there is no software HEVC), and reports a machine with no encode driver for the
  codec as a missing component with the distributions' packages (ADR-0060 §6). Decoding is out of the ADR's scope. Its
  `on_context`, on a context with encode targets, keeps the encoders whose render node
  (the node file's device number) is the context's, since the DMA-BUF imports only on
  the GPU that made it; their sessions take GPU frames, and a software-only request is
  `Unsupported` there. With none, or on a context without encode targets, it is
  `Ok(None)`, and on another platform's context `InvalidState`.
- **The VA-API session** (`VideoEncoder`), H.264 or HEVC, opens its own display on the chosen
  node: a config with the rate-control mode (CBR or VBR, refused if the driver lacks it),
  a context, four input surfaces with a coded buffer each, and three reconstructed
  surfaces. Opened through `on_context` it takes the context's NV12 encode targets
  instead: each target's DMA-BUF is imported once as an input surface (`vaCreateSurfaces`
  with a DRM PRIME 2 descriptor of one object and one two-plane NV12 layer), cached
  against a weak hold on the target so the encode ring still sees it free, and destroyed
  once the target is gone. An encode waits up to a second for render's write of the
  frame, and the session holds the frame until its picture is collected, since the
  driver reads the surface until then; up to four targets are held that way. Its
  context's render targets are only the reconstructed surfaces. Otherwise it takes CPU
  frames: NV12, or the compositor's RGBA and BGRA converted to
  BT.709 limited NV12 by `jstream_platform::nv12`, the Windows encoder's converter,
  written through `vaDeriveImage`, or a staging image and `vaPutImage` where the driver
  can't map its
  surfaces. The stream is I and P frames with one reference and one slice per picture,
  so packets are Annex B in presentation order with `dts == pts`. IDR frames come every
  `keyframe_interval` frames and on `request_keyframe`, which restarts the cadence, and
  carry the parameter sets (H.264's SPS and PPS, HEVC's VPS, SPS and PPS): JStream's own
  (packed headers: BT.709 limited range, frame timing, no reordering) when the driver
  takes them, the driver's otherwise. HEVC is Main profile and Main tier, coded with the
  block sizes and tools (AMP, SAO, temporal MVP, transform skip, CU QP deltas) the
  driver's config attributes advertise, or FFmpeg's defaults where it advertises none.
  A driver that requires a tool JStream's parameter sets turn off (scaling lists, PCM,
  sign data hiding, weighted prediction and the like) is refused as unsupported.
  JStream writes each slice header too where the driver takes packed slice headers along
  with JStream's packed parameter sets, and a driver that codes no P slices gets each P
  frame as a B slice with the reference in both lists. The level is the lowest that fits the size, frame rate and peak rate. `poll` never blocks; up
  to four frames are in flight, and a fifth is refused as busy. The libva structs are
  hand-written and checked against libva 2.20.0's headers, with their sizes and offsets
  asserted at compile time.
- **Cisco's OpenH264** is the software H.264 encoder
  ([ADR-0061](../../docs/adr/0061-source-types-and-decoding-scope-v2.md)), last in line:
  `VaapiCodecs` opens it for `HardwarePreference::Avoid`, and for `Prefer` when there is
  no VA-API H.264 encoder (`wants_software`), and lists it once it is downloaded. JStream
  never ships or builds it. **`OpenH264Store`** keeps it in the data folder
  (`<data>/codecs/openh264`, a 0700 folder holding a 0600 file). `install`, which only a
  host acting on the user's explicit request calls, after showing `OPENH264_NOTICE` and
  a link to `OPENH264_LICENSE_URL`, fetches the one pinned release (`OPENH264_PIN`) from
  Cisco's server through a `Fetch`, checks the file's SHA-256, unpacks it with the
  crate's own bzip2 decoder (`openh264/bzip2.rs`; the `bzip2` crate's backend has a
  licence `deny.toml` allows only at build time), checks the library's SHA-256, and
  writes it through a temporary file and a rename. A download that doesn't match is
  discarded and reported, never written. `remove` deletes it. Each time an encoder
  opens, the stored file's SHA-256 is checked again before `dlopen`, and the library
  must report the pinned version, since the hand-written declarations (`ffi.rs`, only
  the encoder calls JStream makes) follow that release's headers. A copy from anywhere
  else is never loaded. The pin's two SHA-256 values are not recorded yet, so for now
  `install` refuses before any request is made. **`HttpsFetch`** is the real `Fetch`:
  one `GET` over rustls with the OS trust store, no redirects. **`OpenH264Encoder`**
  (`VideoEncoder`) takes CPU NV12 or I420 frames of even size, CBR or VBR (a peak at
  least the target, set on its one spatial layer; OpenH264 enforces it only by skipping
  frames, which is off, so VBR encodes at the target rate) and a keyframe interval of at
  least one frame, and encodes each frame synchronously (camera real-time mode, no frame skipping, the configured
  IDR interval) into one Annex B packet whose `dts` is its `pts`; `request_keyframe`
  forces an IDR on the next frame. The library's own logging is turned off.
- **`RustAacEncoder`** (`AudioEncoder`): AAC encoding, which `VaapiCodecs` lists and
  opens. It wraps JStream's own AAC-LC encoder,
  [`jstream-aac`](../jstream-aac/README.md#purpose)
  ([ADR-0061](../../docs/adr/0061-source-types-and-decoding-scope-v2.md), within
  [ADR-0062](../../docs/adr/0062-aac-lc-encoder-patent-check.md)'s limits), for 44.1 or
  48 kHz, mono or stereo, interleaved or planar `f32`, at 16 to 256 kbps per channel
  (`aac_config` says why it refuses anything else). As with the Media Foundation encoder,
  each packet is one raw access unit of 1024 frames without ADTS, a keyframe, and timed
  from the first buffer's `pts` plus the frames before it. The muxers write the matching
  `AudioSpecificConfig` from the format. The decoder's output lags by the usual AAC
  priming of 1024 frames. It is built with the `aac-encoder` feature, which is on by
  default. ADR-0062's re-check switches the encoder off in a release by taking that
  feature out of the defaults. Without it, `jstream-aac` leaves the build, the list has
  no AAC encoder, and opening one reports `Unsupported`.

The engine starts each display and window source's capture with `start_with` and
`CaptureOptions::for_source`, asks for `Drawn` when the source's `show_cursor` setting
(protocol 0.36) is on and `Hidden` when it's off, and never draws the pointer itself.
It calls `forget_source` when the source is deleted.

Not here yet, in roughly the order they follow:
- V4L2's NV12 and NV12M on the GPU path (32-bit RGB and YUYV go there so far);
- media-file decoders, which `LinuxBackends` leaves to the host;
- an app keeping its stream keys and OAuth tokens in `LinuxSecrets`, the protocol field
  that tells the UI the secret store's fallback is active, and the `jstream.toml`
  setting that turns the fallback off (ADR-0071 §4, §5).

The virtual camera is out of scope (`Backends::vcam` is `None` on Linux).

Tests run on every platform where they can: D-Bus messages, the portal handshake, SPA
pods (the DMA-BUF offer was checked byte for byte against libspa's builder), the frame
copy, the cursor metadata, the H.264 and HEVC parameters, parameter sets (checked with FFmpeg's
`trace_headers`, as are HEVC's slice headers), GOPs and upload, and V4L2's ABI layouts,
formats list, colour, buffer times and device IDs are platform-independent. The GPU interop's tests open a device on a software Vulkan
adapter where there is no GPU, and skip without one: the frame copy (crop, opaque
padding, slots reused only once free), the context checks, and V4L2's GPU delivery
from plain textures standing in for imported buffers (each buffer's texels reaching an
opaque RGBA frame, YUYV converted as the CPU path converts it in BT.601, BT.709 and
BT.2020, limited and full range, short buffers dropped, an unreadable buffer ending the
GPU path). The VA-API surface
attributes and DRM PRIME descriptor are checked against libva's layout at compile time
and built in a test, and render-node numbers are split as glibc does. DMA-BUF import
and export need a GPU driver, so they are tested only in the `hw-tests` portal capture
on the GPU, the V4L2 capture on the GPU path, and the GPU encode of the context's
encode targets. The Linux ones (the clock, loading libc through the loader,
passing a file descriptor over a socket pair, restore token files, the PipeWire and
libva probes, and ALSA through its sound-card-free `null` PCM: recording, playing,
stopping on drop, a capture stream running again after an overrun, loopback refused, an
unknown PCM and the listing; V4L2's refusal of IDs that aren't video nodes and of a
node that isn't V4L2, and its handling of filled buffers) pass with or without those libraries installed and need no
GPU, session or daemon; a missing PipeWire daemon is tried on a socket name nothing
listens on. The secret store's names, file format and Secret Service messages are
platform-independent; its file handling, refusals, moves into the keyring and concurrent
writes (clones taking turns, separate stores never colliding) are tested on Linux
against a keyring in memory. The registry reading and stream planning (`pipewire/graph.rs`), the audio
format pods, the `process` gate's hand-off between threads (`pipewire/audio.rs`),
ALSA's sample conversion, channel layouts, listed names and frame times, the audio
backend choice, and pairing (finding a node's sound cards in a sysfs tree laid out in a
temporary folder, and each backend's preferred input on a card) are
platform-independent. The OpenH264 tests use a fake download and a fake encoder
behind the library's vtable, and the bzip2 decoder is tested on files `bzip2` wrote
(`openh264/testdata`). The one test that downloads from Cisco, which is how the pin is
recorded, needs the `openh264-download` feature and `--ignored`; CI runs neither. The
`hw-tests` feature holds the tests against a real desktop, `#[ignore]`d
and run by hand: the session bus (`dbus-run-session` is enough), a portal capture in a
GNOME or KDE session with PipeWire (on the CPU path, and on the GPU path with a Vulkan
driver that imports DMA-BUFs, and the pointer as metadata), VA-API H.264 and HEVC encodes from CPU frames on a GPU
with an encode driver, and H.264 from the context's encode targets (which needs the Vulkan and
VA-API drivers on one render node), `PipeWireAudio` recording (on the data loop),
playing and looping back on a PipeWire daemon (`pipewire/audio_tests.rs`; null nodes
are enough, no sound card, and one says it is on ALSA card 7 for the pairing test),
`LinuxBackends` picking PipeWire with a daemon and ALSA without, the secret store against a real Secret Service (gnome-keyring under
`dbus-run-session` is enough), and a second of ALSA's default input on a sound card (`null`
isn't paced, so only a real device shows real-time recording), and V4L2 capture from a webcam or capture card (the `vivid` test
driver will do), on the CPU path and, with a Vulkan driver that imports DMA-BUFs, on
the GPU path in a format it takes (`vivid`'s 32-bit RGB or YUYV, a UVC webcam's YUYV).
