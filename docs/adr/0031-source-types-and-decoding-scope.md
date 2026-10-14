<!-- @doc
id = "ADR-0031"
title = "Source types for v1 and decoding scope"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0031: Source types for v1 and decoding scope

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Supersedes [ADR-0003](0003-media-stack-and-codecs.md).

<a id="context"></a>
## Context
Two questions about sources are open, and both are product scope.

**Which source types ship, and when.** The UI prototype offers more source types than the
roadmap plans:
- **Capture:** display, window, video capture device (webcam or capture card, with its
  audio), and a Spout2 receiver.
- **Audio:** audio input, desktop audio, and application audio.
- **Media and web:** media file (video or audio, looping), image (PNG, JPEG, WebP,
  animated GIF), and browser.
- **Generated:** text, colour, audio waveform and audio spectrum.
- **Scenes:** scene.

Today the roadmap's M1b lists window, webcam (including MJPEG decode), image, colour, text,
browser ([ADR-0021](0021-browser-source.md)) and scenes. M1a's thin slice already captures
the display, a microphone and desktop audio. The media-file source sits under "Later".
WASAPI process loopback, the API behind application audio, was chosen in
[ADR-0008](0008-windows-support-policy.md) and already exists in the Windows platform
crate. Spout2, waveform and spectrum appear nowhere in the plan.

**What "OS codecs only" covers.** [ADR-0003](0003-media-stack-and-codecs.md) decided that
"encoding and decoding use OS and GPU-vendor APIs only". Its context is audio and video
codecs (H.264, AAC), but the decision text is unqualified. The image source must decode
PNG, JPEG, WebP and animated GIF, and the text source needs fonts. Pure-Rust decoders for
these exist and are not C, but they are not OS APIs either. The OS alternative is WIC and
DirectWrite on Windows, ImageIO and Core Text on macOS, and nothing equivalent for images
on Linux.

ADRs can't be partially amended ([R-DOC-009](../RULES.md#R-DOC-009)). So this ADR restates
ADR-0003's whole decision with the clarification, adds the source-type list, and
supersedes ADR-0003. Superseding an ADR is Tier 1
([ADR-0017](0017-adr-decision-tiers.md)).

The background for ADR-0003 is unchanged:
- The project is "fully Rust". FFmpeg would give broad codec coverage, but it brings a
  large C dependency, build complexity and LGPL/GPL concerns.
- YouTube RTMP ingest needs H.264 (or HEVC/AV1 via
  [enhanced RTMP](https://github.com/veovera/enhanced-rtmp)) and **AAC** audio. There is no
  production-quality pure-Rust H.264 or AAC encoder.
- Windows and macOS ship both encoders in the OS. Linux ships neither.

<a id="decision"></a>
## Decision

### Source types for v1
The maintainer chose the **staged** option. Each source kind lands in this milestone:

| Kind | Milestone | Notes |
|---|---|---|
| Display | M1b | Captured since M1a's thin slice; becomes a source kind in M1b. |
| Window | M1b | |
| Video capture device | M1b | Webcam or capture card, including MJPEG decode, with its paired audio stream. |
| Image | M1b | PNG, JPEG, WebP and animated GIF. |
| Colour | M1b | |
| Text | M1b | |
| Browser | M1b | As [ADR-0021](0021-browser-source.md) decides, after spike S4. |
| Audio input | M1b | Captured since M1a's thin slice. |
| Desktop audio | M1b | Captured since M1a's thin slice. |
| Application audio | M1b | WASAPI process loopback ([ADR-0008](0008-windows-support-policy.md)), already built in the platform crate. |
| Scene | M1b | A scene used as a source, with cycles rejected. |
| Media file | M2 | Video or audio, looping. Demux and decode use OS APIs (below), with A/V sync on the media clock ([ADR-0014](0014-media-clock.md)). |
| Audio waveform | M2 | A video source that draws another source's audio. |
| Audio spectrum | M2 | A video source that draws another source's audio. |
| Spout2 receiver | Later, when wanted | Windows only. A Syphon receiver on macOS is the follow-up, considered with the macOS platform crate in M3. |

Every kind lives in `jstream-sources`, with its OS backend injected from a platform crate,
as for the browser source.

### Media stack and decoding scope
This restates ADR-0003's decision. The only change is the clarified first bullet, marked
*(clarified)*.

- All JStream code is Rust. **Audio and video codecs** (encoding and decoding) use **OS
  and GPU-vendor APIs only**. No FFmpeg, GStreamer, or bundled C/C++ codec libraries
  (OpenH264, fdk-aac, x264). This is enforced by
  [R-DEP-002](../RULES.md#R-DEP-002).
- *(clarified)* **Images and fonts are not media codecs.** The maintainer chose
  **"media codecs only"**: the rule above covers audio and video codecs. Still images and
  animated images (PNG, JPEG, WebP, GIF and similar) and fonts (discovery, shaping and
  rasterization) use **pure-Rust crates, the same on every platform**, not WIC,
  DirectWrite, ImageIO or Core Text. Each such crate is added under the license policy
  ([R-DEP-001](../RULES.md#R-DEP-001)) and with the usual justification
  ([R-DEP-004](../RULES.md#R-DEP-004)). A crate that binds a C image or font library does
  not qualify.
- A video stream is media even when its frames are JPEG: MJPEG from a capture device is
  decoded by the OS or GPU-vendor path, like any other video codec.
- **Windows baseline (M1):** Media Foundation H.264 and AAC. Hardware encoding first goes
  through Media Foundation's hardware MFTs, which cover NVIDIA, AMD and Intel through one
  D3D11 path, with the software H.264 MFT as fallback. Direct NVENC/AMF are later
  optimizations, each needing measurements. QSV is reached only through Media Foundation
  (the oneVPL dispatcher is C++).
- **macOS baseline (M3):** VideoToolbox and AudioToolbox.
- **Linux:** a separate ADR before M3 decides the codec path (VAAPI for video; AAC is
  open). Until then Linux can't stream.
- Muxers (FLV, MP4/fMP4) and the RTMP(S) client are written in Rust.
- Non-media C dependencies (e.g. `ring` for TLS) are allowed through the lint-deps
  allowlist.
- The bounded exception in [ADR-0021](0021-browser-source.md) still stands: the prebuilt
  CEF runtime may carry its own media stack, FFmpeg included, for decoding inside browser
  pages only. No JStream crate calls its codecs.

<a id="consequences"></a>
## Consequences
- **Licensing and build:** licensing stays permissive apart from ADR-0021's named CEF
  exception, and the build stays simple.
- **Known gaps (unchanged):** Windows N editions need the Media Feature Pack for AAC (the
  engine detects this and reports it), and Linux streaming is blocked on a future ADR.
- **Images and text are cross-platform.** The image and text sources decode and render
  the same on Windows, macOS and Linux, and no platform crate grows image or font code.
  The cost is a few pure-Rust dependencies, each justified when added.
- **Media-file source (M2)** depends on OS demux and decode APIs (Media Foundation on
  Windows), so the containers and codecs it plays are those the OS supports. It needs the
  `VideoDecoder` platform trait, looping, restart rules tied to the source lifecycle, and
  A/V sync on the media clock.
- **Waveform and spectrum (M2)** add a new **audio → render data path**: the mixer
  publishes per-source analysis (peaks, FFT magnitudes) through a lock-free hand-off that
  the render thread reads ([R-RT-003](../RULES.md#R-RT-003)). The analysis must not
  allocate on the mixer thread, or it runs on a separate analysis thread. A setting that
  references another source adds a source → source edge, which needs the same cycle
  rules as scenes.
- **Spout2**, when it is wanted, is Windows-only. It receives a D3D11 shared texture
  through the same interop path as capture ([ADR-0004](0004-wgpu-with-native-interop.md)),
  in `jstream-platform-windows` under the `unsafe` rules. macOS parity (Syphon) is
  decided with the macOS platform crate.
- **Edits that follow acceptance** (none are made while this ADR is proposed):
  - ADR-0003: `status = "superseded"` and
    `superseded_by = "docs/adr/0031-source-types-and-decoding-scope.md"`.
  - Links to ADR-0003 from normative docs move to this ADR, since a link to a superseded
    ADR fails: [R-DEP-002](../RULES.md#R-DEP-002) (which also gains a sentence that
    pure-Rust image and font crates are not banned codec libraries), DOCS's lint-deps
    paragraph, and the ROADMAP M1 section's `depends_on`. The ADR-0003 mentions in
    `deny.toml`, `xtask/src/lint_deps.rs` and the codec comments in `jstream-platform` and
    `jstream-platform-windows` move too.
  - ROADMAP:
    - **M1b** adds audio input, desktop audio, application audio and display as source
      kinds;
    - **M2** gains media file, audio waveform and audio spectrum;
    - **M3** notes the Syphon follow-up with the macOS platform crate;
    - **Later** drops the media-file source and adds the Spout2 receiver.
  - ARCHITECTURE › Crate map: the `jstream-sources` row lists every kind in the table
    above, and the `jstream-platform-windows` row gains the Spout2 receiver when it is
    built.
