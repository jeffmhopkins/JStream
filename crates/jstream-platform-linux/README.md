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
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-platform-linux", "docs/ARCHITECTURE.md#threading", "docs/adr/0060-linux-platform-support.md", "docs/adr/0031-source-types-and-decoding-scope.md", "docs/adr/0014-media-clock.md", "docs/RULES.md#unsafe", "crates/jstream-platform/README.md#purpose"]
-->

The Linux implementations of the [`jstream-platform`](../jstream-platform/README.md#purpose)
traits, as [ADR-0060](../../docs/adr/0060-linux-platform-support.md) lays them out. That
ADR is **accepted**; this crate is its first part, and the rest follows in later PRs. Layer 2, and one of
the crates allowed `unsafe` ([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001)): every
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

This first part has:
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
  A headless session lists none. `start` reports `Unsupported` for now. The portal
  protocol's platform-independent parts are here and tested: the `SelectSources` option
  values (source types, cursor modes with metadata preferred, `persist_mode` 2 so the
  grant survives restarts), request handle tokens and the request object path a reply
  arrives on.
- **`PipeWire`** loads `libpipewire-0.3.so.0` and checks it is at least ADR-0060's floor
  (1.0). **`PipeWireAudio`** (`AudioIo`) is built on it; listing devices and opening
  streams report `Unsupported` for now.
- **`probe_encoders`** opens each DRM render node (`/dev/dri/renderD*`) through libva and
  libva-drm, and records the best H.264 profile (High, then Main, then Constrained
  Baseline) and encode entry point (full, then low-power) its driver offers. The display
  is terminated before its node is closed. **`VaapiCodecs`** (`Codecs`) lists those as
  hardware H.264 encoders and picks one for a configuration with `choose_encoder`. That
  refuses other codecs and a software-only request (no software encoder until Cisco's
  OpenH264 download lands, ADR-0060 question 1), and reports a machine with no H.264
  encode driver as a missing component with the distributions' packages (ADR-0060 §6).
  Audio encoding reports `Unsupported` until the Rust AAC-LC encoder exists (ADR-0060
  question 2), and decoding is out of the ADR's scope.
- **The VA-API H.264 session** (`VideoEncoder`) opens its own display on the chosen
  node: a config with the rate-control mode (CBR or VBR, refused if the driver lacks it),
  a context, four input surfaces with a coded buffer each, and three reconstructed
  surfaces. It takes CPU frames: NV12, or the compositor's RGBA and BGRA converted to
  BT.709 limited NV12 with the Windows encoder's arithmetic, written through
  `vaDeriveImage`, or a staging image and `vaPutImage` where the driver can't map its
  surfaces. The stream is I and P frames with one reference and one slice per picture,
  so packets are Annex B in presentation order with `dts == pts`. IDR frames come every
  `keyframe_interval` frames and on `request_keyframe`, which restarts the cadence, and
  carry the SPS and PPS: JStream's own (packed headers: BT.709 limited range, frame
  timing, no reordering) when the driver takes them, the driver's otherwise. The level
  is the lowest that fits the size, frame rate and peak rate. `poll` never blocks; up
  to four frames are in flight, and a fifth is refused as busy. The libva structs are
  hand-written and checked against libva 2.20.0's headers, with their sizes and offsets
  asserted at compile time.

Not here yet, in roughly the order they follow:
- the portal's D-Bus transport and the PipeWire video stream (SHM first, then DMA-BUF
  into Vulkan);
- PipeWire audio streams, and the ALSA fallback through libasound (ADR-0060 §4, §8);
- keeping the portal's restore token in the data folder (ADR-0060 §3);
- DMA-BUF import into the VA-API encoder, so GPU encode targets skip the CPU copy;
- V4L2 webcams;
- the stream-key store.

The virtual camera is out of scope (`Backends::vcam` is `None` on Linux).

Tests run on every platform where they can. The Linux ones (the clock, loading libc
through the loader, the PipeWire and libva probes) pass with or without those libraries
installed and need no GPU, session or daemon, and the H.264 parameters, parameter sets,
GOP and upload are tested everywhere. The `hw-tests` feature holds tests against a real
desktop (portal, PipeWire daemon, VA-API GPU), which run by hand; today those are the
VA-API encode tests.
