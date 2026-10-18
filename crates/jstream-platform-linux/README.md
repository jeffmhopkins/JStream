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
capture, then VA-API H.264 encoding) and the secret store of
[ADR-0071](../../docs/adr/0071-linux-secret-store.md), and the rest follows in later PRs. Layer 2, and one
of the crates allowed `unsafe` ([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001)): every
system call is wrapped here behind safe APIs, with RAII types for what the system hands
back ([R-UNSAFE-003](../../docs/RULES.md#R-UNSAFE-003)).

**System libraries are loaded at run time** (ADR-0060 §8). libpipewire and libva are
opened with `dlopen` through `libloading` when a backend first needs them, and only the
functions the crate calls are declared, each with its C signature in a `SAFETY` comment.
Nothing is linked, so the crate builds and tests on any machine, CI included, without
their headers. This departs from [R-UNSAFE-004](../../docs/RULES.md#R-UNSAFE-004)'s
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
  into the frames, `persist_mode` 2 with the last restore token), `Start` (the picker;
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
  owns that no one else can read or write. So the next start skips the picker. A
  thread keeps the portal's connection open for the session's
  life and reports the portal closing it (sharing stopped, grant revoked) as
  `DeviceLost`.
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
  single-slot mailbox Windows' capture uses too. Its `pts` is
  the buffer header's `CLOCK_MONOTONIC` time when that is plausible, else the clock's
  `now`. The callbacks catch panics (R-UNSAFE-005); a stream error is `DeviceLost`.
  The portal protocol's option values are here too: source types, cursor modes,
  `persist_mode`, request handle tokens and request object paths.
- **`PipeWire`** loads `libpipewire-0.3.so.0` and checks it is at least ADR-0060's floor
  (1.0). **`PipeWireAudio`** (`AudioIo`) is built on it; listing devices and opening
  streams report `Unsupported` for now.
- **`probe_encoders`** opens each DRM render node (`/dev/dri/renderD*`) through libva and
  libva-drm, and records the best H.264 profile (High, then Main, then Constrained
  Baseline) and encode entry point (full, then low-power) its driver offers. The display
  is terminated before its node is closed. **`VaapiCodecs`** (`Codecs`) lists those as
  hardware H.264 encoders and picks one for a configuration with `choose_encoder`. That
  refuses other codecs and a software-only request (software H.264 is OpenH264's, below),
  and reports a machine with no H.264 encode driver as a missing component with the
  distributions' packages (ADR-0060 §6). Decoding is out of the ADR's scope.
- **The VA-API H.264 session** (`VideoEncoder`) opens its own display on the chosen
  node: a config with the rate-control mode (CBR or VBR, refused if the driver lacks it),
  a context, four input surfaces with a coded buffer each, and three reconstructed
  surfaces. It takes CPU frames: NV12, or the compositor's RGBA and BGRA converted to
  BT.709 limited NV12 by `jstream_platform::nv12`, the Windows encoder's converter,
  written through `vaDeriveImage`, or a staging image and `vaPutImage` where the driver
  can't map its
  surfaces. The stream is I and P frames with one reference and one slice per picture,
  so packets are Annex B in presentation order with `dts == pts`. IDR frames come every
  `keyframe_interval` frames and on `request_keyframe`, which restarts the cadence, and
  carry the SPS and PPS: JStream's own (packed headers: BT.709 limited range, frame
  timing, no reordering) when the driver takes them, the driver's otherwise. The level
  is the lowest that fits the size, frame rate and peak rate. `poll` never blocks; up
  to four frames are in flight, and a fifth is refused as busy. The libva structs are
  hand-written and checked against libva 2.20.0's headers, with their sizes and offsets
  asserted at compile time.
- **Cisco's OpenH264** is the software H.264 encoder
  ([ADR-0061](../../docs/adr/0061-source-types-and-decoding-scope-v2.md)), last in line:
  `VaapiCodecs` opens it for `HardwarePreference::Avoid`, and for `Prefer` when there is
  no VA-API encoder (`wants_software`), and lists it once it is downloaded. JStream
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

Not here yet, in roughly the order they follow:
- DMA-BUF frames imported into the engine's Vulkan device (`Capture::on_context`), in
  place of the CPU copy;
- the cursor as stream metadata, composited by the source, once the source has a
  "show cursor" setting (it is drawn into the frames for now);
- restore tokens kept per source rather than per kind of pick, once the engine names
  the source when it starts a capture;
- PipeWire audio streams, and the ALSA fallback through libasound (ADR-0060 §4, §8);
- DMA-BUF import into the VA-API encoder, so GPU encode targets skip the CPU copy;
- V4L2 webcams;
- the protocol field that tells the UI the secret store's fallback is active, and the
  `jstream.toml` setting that turns the fallback off (ADR-0071 §4, §5).

The virtual camera is out of scope (`Backends::vcam` is `None` on Linux).

Tests run on every platform where they can: D-Bus messages, the portal handshake, SPA
pods, the frame copy, and the H.264 parameters, parameter sets, GOP and upload are
platform-independent. The Linux ones (the clock, loading libc through the loader,
passing a file descriptor over a socket pair, restore token files, the PipeWire and
libva probes) pass with or without those libraries installed and need no GPU, session
or daemon. The secret store's names, file format and Secret Service messages are
platform-independent; its file handling, refusals, moves into the keyring and concurrent
writes (clones taking turns, separate stores never colliding) are tested on Linux
against a keyring in memory. The OpenH264 tests use a fake download and a fake encoder behind the
library's vtable, and the bzip2 decoder is tested on files `bzip2` wrote
(`openh264/testdata`). The one test that downloads from Cisco, which is how the pin is
recorded, needs the `openh264-download` feature and `--ignored`; CI runs neither. The
`hw-tests` feature holds the tests against a real desktop, `#[ignore]`d
and run by hand: the session bus (`dbus-run-session` is enough), a portal capture in a
GNOME or KDE session with PipeWire, VA-API H.264 encodes on a GPU with an encode
driver, and the secret store against a real Secret Service (gnome-keyring under
`dbus-run-session` is enough).
