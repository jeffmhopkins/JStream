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
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-platform-linux", "docs/ARCHITECTURE.md#threading", "docs/adr/0060-linux-platform-support.md", "docs/adr/0061-source-types-and-decoding-scope-v2.md", "docs/adr/0062-aac-lc-encoder-patent-check.md", "docs/adr/0014-media-clock.md", "docs/RULES.md#unsafe", "crates/jstream-platform/README.md#purpose", "crates/jstream-aac/README.md#purpose"]
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
  Opening the session reports `Unsupported` for now. Decoding is out of the ADR's scope.
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
- the portal's D-Bus transport and the PipeWire video stream (SHM first, then DMA-BUF
  into Vulkan);
- PipeWire audio streams, and the ALSA fallback through libasound (ADR-0060 §4, §8);
- keeping the portal's restore token in the data folder (ADR-0060 §3);
- the VA-API H.264 encode session (parameter buffers, NV12 upload, packed headers);
- V4L2 webcams;
- the stream-key store.

The virtual camera is out of scope (`Backends::vcam` is `None` on Linux).

Tests run on every platform where they can. The Linux ones (the clock, loading libc
through the loader, the PipeWire and libva probes) pass with or without those libraries
installed and need no GPU, session or daemon. The `hw-tests` feature is reserved for
tests against a real desktop (portal, PipeWire daemon, VA-API GPU), which run by hand.
