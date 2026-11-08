<!-- @doc
id = "ADR-0140"
title = "Source types and decoding scope v3: Linux decodes the engine's own H.264 through Cisco's OpenH264"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0140: Source types and decoding scope v3: Linux decodes the engine's own H.264 through Cisco's OpenH264

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

Supersedes [ADR-0061](0061-source-types-and-decoding-scope-v2.md).

<a id="context"></a>
## Context
[ADR-0132](0132-bench-gates-views-and-quality-probe.md) §3 plans `quality.probe`: the
engine decodes the packets one of its own streams just made and compares each picture
with the frame it encoded (PSNR and SSIM). That needs an H.264 decoder on every platform
the engine encodes on. #1337 found two of them have one within the rules and one doesn't:

- **Windows** has Media Foundation's H.264 decoder MFT, and **macOS** has VideoToolbox's
  decoder. Both are OS codecs, which the media stack below already allows.
- **Linux has no decoder by decision.** [ADR-0061](0061-source-types-and-decoding-scope-v2.md)
  says of Cisco's OpenH264: "Encode only, Linux only, last in line… Nothing decodes
  through OpenH264." So ADR-0132 has the probe answer `action_failed` on Linux, and CI's
  Linux bench, the only one that runs on every PR, skips the quality gates.

The ways Linux could decode the engine's own H.264:

1. **Cisco's OpenH264 binary, the one JStream already downloads.** Cisco's library is a
   decoder as well as an encoder (`WelsCreateDecoder`, in the same file the pin checks).
   Its patent licence covers both. The licence page
   ([Cisco's binary licence](https://www.openh264.org/BINARY_LICENSE.txt)) grants the
   AVC patent portfolio licence for personal use "to (i) encode video in compliance with
   the AVC standard … and/or (ii) decode AVC video that was encoded by a consumer engaged
   in a personal activity", under the same four conditions JStream already meets for
   encoding: Cisco's binary, downloaded separately to the user's device, which the user
   controls, with Cisco's notice shown. A stream the user's own engine just encoded is
   video "encoded by a consumer", so decoding it sits inside clause (ii). Nothing new is
   downloaded, built, linked or shipped, and `deny.toml` doesn't change.
2. **VA-API decode.** Allowed already (the GPU driver decodes), but it isn't there on
   NVIDIA, on Fedora's stock Mesa or in virtual machines
   ([ADR-0072](0072-linux-media-file-decoding.md) lists why), nor on CI's lavapipe
   runner, so the CI bench would still skip the quality gates. It also means writing an
   H.264 header parser, slice parameters and a decoded picture buffer, which ADR-0072
   counts as large work.
3. **The system's FFmpeg.** ADR-0072 gates it on a GPL legal view and a decoder patent
   check, neither done.
4. **A pure-Rust decoder.** None mature enough is known (ADR-0072).

The maintainer asked for the probe on Linux too, on the ticket's question:

> "I think we build it for Linux as well unless there's a solid reason that we can't even
> though we're not testing it yet. Once we get into Linux testing obviously we can open
> that back up, but I see no reason to actually not build it."

Option 1 is the only one that builds and runs everywhere the engine encodes H.264 on
Linux, CI included, without a new dependency or licence. It is recommended here.

This ADR is maintainer tier ([ADR-0043](0043-narrow-maintainer-tier.md)): it widens a
licensing exception, Cisco's binary, from encoding to decoding. ADRs can't be partially
amended ([R-DOC-009](../RULES.md#R-DOC-009)), so it restates ADR-0061's whole decision,
changes only what decoding through OpenH264 changes, and supersedes it. Each change is
marked *(changed)*; everything else is ADR-0061's text, carried over as it stands. It also
takes in the one line [ADR-0072](0072-linux-media-file-decoding.md) planned for an
"ADR-0061 v3": Linux's media-file source is ADR-0072's, no longer out of scope.

The background ADR-0061 carried over from ADR-0031 is unchanged:
- The project is "fully Rust". FFmpeg would give broad codec coverage, but it brings a
  large C dependency, build complexity and LGPL/GPL concerns.
- YouTube RTMP ingest needs H.264 (or HEVC/AV1 via
  [enhanced RTMP](https://github.com/veovera/enhanced-rtmp)) and **AAC** audio. There is no
  production-quality pure-Rust H.264 or AAC encoder.
- Windows and macOS ship both encoders in the OS. Linux ships neither.

<a id="decision"></a>
## Decision

### Source types for v1
Unchanged from ADR-0031. The maintainer chose the **staged** option. Each source kind lands
in this milestone:

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
- *(changed)* All JStream code is Rust. **Audio and video codecs** (encoding and decoding)
  use **OS and GPU-vendor APIs**, with exactly two additions, both below: Cisco's OpenH264
  binary as Linux's software H.264 fallback and the decoder of the engine's own H.264
  streams there, and JStream's own AAC-LC encoder. No FFmpeg,
  GStreamer, or bundled C/C++ codec libraries (OpenH264 built or shipped by anyone but
  Cisco, fdk-aac, x264). This is enforced by [R-DEP-002](../RULES.md#R-DEP-002).
- **Images and fonts are not media codecs.** The maintainer chose
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
- *(changed)* **Linux (M3):** video encode as [ADR-0060](0060-linux-platform-support.md)
  decides (VA-API H.264, with NVENC as NVIDIA's follow-up), Cisco's OpenH264 binary as the
  software H.264 fallback and as the decoder of the engine's own H.264 streams, and
  JStream's own AAC-LC encoder for audio, each as below. Linux streams once an H.264 path
  and the AAC encoder both work on the machine.
- *(changed)* **Decoding the engine's own streams**, for checks such as ADR-0132's quality
  probe: Media Foundation's H.264 decoder MFT on Windows, VideoToolbox on macOS, and
  Cisco's OpenH264 on Linux (below).
- Muxers (FLV, MP4/fMP4) and the RTMP(S) client are written in Rust.
- Non-media C dependencies (e.g. `ring` for TLS) are allowed through the lint-deps
  allowlist.
- The bounded exception in [ADR-0021](0021-browser-source.md) still stands: the prebuilt
  CEF runtime may carry its own media stack, FFmpeg included, for decoding inside browser
  pages only. No JStream crate calls its codecs.

### Software H.264 on Linux: Cisco's OpenH264 binary *(changed)*
The maintainer chose **Cisco's OpenH264 binary**. It is a second bounded exception to "no
bundled C codec libraries", for one artifact only, in the way ADR-0021's is for CEF:
- **Cisco's binary only, never JStream's.** JStream never ships, bundles, mirrors, builds
  or patches OpenH264. The engine downloads Cisco's prebuilt Linux library from Cisco's
  own server, directly to the user's device, the way Cisco's patent licence requires. A
  copy from anywhere else (a distribution package, a file the user drops in, a build from
  source) is not loaded, because Cisco's licence doesn't cover it.
- **On the user's request only.** Nothing downloads at install or first run. The UI offers
  the download, naming Cisco and its licence, when the user asks for software encoding, or
  when an output needs H.264 and the machine has no hardware encoder. Nothing downloads
  without that explicit request. The user can delete the library again, and JStream then
  reports software H.264 as not installed.
- **Pinned and checked.** The engine pins one OpenH264 release: its download address on
  Cisco's server and the SHA-256 of that file, recorded when the pin is set. A download whose hash differs is
  discarded and reported, never loaded. A newer release reaches users through a JStream
  update that moves the pin.
- **Loaded at run time, through JStream's own bindings.** The library is stored in the
  data folder and loaded with `dlopen` when an encoder is created, as
  [ADR-0060](0060-linux-platform-support.md) §8 does for libpipewire and libva. The
  declarations are hand-written in `jstream-platform-linux`, cover only the encoder and
  decoder calls JStream makes, and sit behind a safe API under the `unsafe` rules
  ([R-UNSAFE-002](../RULES.md#R-UNSAFE-002)). No crate links or builds OpenH264, so the
  `openh264` and `openh264-sys2` bans in `deny.toml` stay: those crates build Cisco's
  source themselves, which the patent licence doesn't cover.
- **Encode, Linux only, last in line.** It is a software `VideoEncoder` behind the
  `Codecs` trait. Hardware encoders come first, and it serves only when there is none or
  the user chose software ([`HardwarePreference`](../../crates/jstream-platform/src/codec.rs)).
  Windows keeps the Media Foundation software MFT, and macOS gets VideoToolbox.
- *(changed)* **Decode, of the engine's own streams only, Linux only.** It is also the
  `VideoDecoder` behind `Codecs::video_decoder` for H.264 on Linux, from the same pinned
  file, and nothing else on Linux decodes through it:
  - It decodes only packets the engine itself encoded in the same process, such as the
    quality probe's (ADR-0132 §3). That is video "encoded by a consumer engaged in a
    personal activity", which Cisco's licence covers.
  - It never decodes a media file, a capture device's stream, a stream received from
    another program or the network, or anything a person hands the engine. Linux's
    media-file source decodes as [ADR-0072](0072-linux-media-file-decoding.md) decides,
    which this ADR doesn't change.
  - Decoding downloads nothing. Where the library isn't installed, the decoder is a
    missing component whose remedy names the download, and the caller reports that (the
    probe answers `action_failed`). The download stays on the person's request, as above.
  - It decodes in software, into CPU frames, off the real-time threads.
- **Notices.** The implementing PR re-reads Cisco's binary licence page, and the app
  shows every notice and attribution it requires wherever the download is offered and in
  the app's licence information.

### AAC on Linux: an AAC-LC encoder in Rust *(changed)*
The maintainer chose **an AAC-LC encoder written in Rust in this repository**, as its own
roadmap item, after a patent check:
- **The patent check comes first, and gates the encoder.** The item's first deliverable is
  a written check of the patents that read on AAC-LC encoding: which are still in force in
  the places JStream is distributed, and whether distributing a free, open-source encoder
  needs a licence, from whom, and at what cost. The outcome is a licensing decision, and
  possibly a spending one, so it goes to the maintainer as its own maintainer-tier ADR. No
  encoder code merges until the maintainer accepts that outcome. If the check says the
  encoder can't ship, Linux streaming waits on a new decision.
- **What it is.** An AAC-LC encoder only, no decoder and no HE-AAC, producing the
  48 kHz stereo stream at the bit rates YouTube's ingest asks for. It is safe Rust with no
  C, in the workspace, and implements the `AudioEncoder` trait like the Media Foundation
  encoder. Which crate holds it is the item's design call.
- **Where it is used.** It is Linux's AAC encoder. Windows and macOS keep their OS AAC
  encoders. Using it anywhere else, for example on Windows N editions without the Media
  Feature Pack, is a later decision.

<a id="consequences"></a>
## Consequences
- *(changed)* **Licensing and build:** the code JStream ships stays permissively licensed.
  The media exceptions stay ADR-0021's CEF runtime, Cisco's OpenH264 binary, which JStream
  never ships but downloads on the user's request, and ADR-0072's FFmpeg fallback. Cisco's
  binary now also decodes the engine's own streams, under the same licence and conditions.
  The build stays simple: nothing links OpenH264, and `deny.toml` doesn't change.
- *(changed)* **Every platform can decode what the engine encodes**, so ADR-0132's quality
  probe runs on Linux wherever OpenH264 is installed, CI's lavapipe bench included, rather
  than answering `action_failed` there. ADR-0132's Linux consequence is answered here: its
  next version says so.
- *(changed)* **Known gaps:** Windows N editions need the Media Feature Pack for AAC (the
  engine detects this and reports it). Linux can't stream until both an H.264 path and the
  AAC-LC encoder work, and the encoder waits on the patent check, whose outcome may stop
  it.
- **The download is a network dependency the engine didn't have.** It needs Cisco's
  server to be reachable, a pinned hash kept current, and an error for a download that
  fails or doesn't match. It happens only when the user asks.
- **Writing an AAC encoder is real work.** It needs psychoacoustics, quantisation and rate
  control good enough for a live stream, and tests against a reference decoder. It is the
  largest item Linux streaming depends on.
- **Images and text are cross-platform.** The image and text sources decode and render
  the same on Windows, macOS and Linux, and no platform crate grows image or font code.
  The cost is a few pure-Rust dependencies, each justified when added.
- **Media-file source (M2)** depends on OS demux and decode APIs (Media Foundation on
  Windows), so the containers and codecs it plays are those the OS supports. It needs the
  `VideoDecoder` platform trait, looping, restart rules tied to the source lifecycle, and
  A/V sync on the media clock. *(changed)* Linux has no OS demuxer, so its media-file
  source is [ADR-0072](0072-linux-media-file-decoding.md)'s. OpenH264's decoder is not
  one of its paths: it decodes only the engine's own streams.
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
  - ADR-0061: `status = "superseded"` and
    `superseded_by = "docs/adr/0140-source-types-and-decoding-scope-v3.md"`, then
    `cargo xtask docs generate` for its banner and the ADR index.
  - [R-DEP-002](../RULES.md#R-DEP-002) cites this ADR in place of ADR-0061, and its
    OpenH264 sentence becomes: "On Linux, Cisco's prebuilt OpenH264 binary is a software
    H.264 encoder, and the decoder of the engine's own H.264 streams, that JStream
    downloads from Cisco on the user's request and loads at run time; JStream never ships
    or builds it, so the `openh264` crates stay banned (ADR-0140)." The ADR is a link, as
    in the rule today.
  - Every other link to ADR-0061 from normative docs and code moves here, since a link to
    a superseded ADR fails. The list to work from is `git grep -l
    'ADR-0061\|0061-source'` at the acceptance push.
  - `docs/external.toml`'s entry for Cisco's licence page names clause (ii), decoding, in
    what JStream relies on.
  - ADR-0072's "ADR-0061 v3" consequence is met by this ADR; ADR-0072 stays accepted.
