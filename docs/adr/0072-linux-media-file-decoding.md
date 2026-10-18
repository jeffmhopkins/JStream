<!-- @doc
id = "ADR-0072"
title = "Linux media-file decoding: VA-API and pure Rust first, the system's FFmpeg for the gaps"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0072: Linux media-file decoding: VA-API and pure Rust first, the system's FFmpeg for the gaps

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
The media-file source plays a local video or audio file, looping, on the media clock
([ADR-0061](0061-source-types-and-decoding-scope-v2.md)). The engine reaches it through
`jstream_platform::MediaDecoders`. Windows implements it on Media Foundation, using the
OS's own demuxers and decoders, so a file plays where the OS can play it. A macOS
implementation on AVFoundation's asset reader is in review (#625).

Linux has no OS media stack. [ADR-0060](0060-linux-platform-support.md) §9 left the
media-file source out of scope there, "each its own later decision". ADR-0061, which
superseded ADR-0031, keeps "demux and decode use OS APIs" and restates that Linux is out
of scope. This ADR is the later decision §9 calls for.

It is maintainer tier ([ADR-0043](0043-narrow-maintainer-tier.md)) for three reasons:
- Whether Linux gets the source at all is product scope.
- Decoding H.264 and AAC in software raises a patent question.
- Using FFmpeg is a licensing decision, and it changes R-DEP-002.

On the first draft, the maintainer said: "I feel like we can do ffmpeg is there are no
other valid choices". So FFmpeg is an option below wherever no OS or pure-Rust choice can
do the job.

The forces:
- **R-DEP-002 today.** [R-DEP-002](../RULES.md#R-DEP-002) bans FFmpeg, GStreamer and
  every bundled C codec library, and `deny.toml` bans their binding crates. Its one
  run-time exception is a precedent: Cisco's prebuilt OpenH264 binary. JStream never
  builds or ships it; it is loaded at run time through hand-written declarations, and the
  `openh264` crates stay banned (ADR-0061, [ADR-0060](0060-linux-platform-support.md) §8).
- **H.264 decode through VA-API.**
  - VA-API decodes H.264 on Intel's and AMD's drivers. libva is already loaded at run
    time for encoding (ADR-0060 §6, §8).
  - It isn't available everywhere:
    - NVIDIA's driver has no VA-API of its own (ADR-0060 §6).
    - Fedora's stock Mesa turns H.264 off for patent reasons, with a full build in RPM
      Fusion. ADR-0060 §6 records this for encode, and the same build leaves out the
      decoder.
    - Virtual machines usually have no VA-API at all.
  - VA-API takes the stream already parsed. The crate fills each picture's and slice's
    parameters from the stream's headers and keeps the reference pictures. A pure-Rust
    header parser (such as `h264-reader`) covers the parsing; the rest is the crate's own
    code.
- **H.264 decode in software.** I know of no choice this repository allows:
  - ADR-0061 says "nothing decodes through OpenH264".
  - I could not confirm a mature pure-Rust H.264 decoder.
- **Audio decode.**
  - PCM, FLAC, Vorbis and Opus are royalty-free. MP3's patents have expired.
  - symphonia (MPL-2.0, on the allow list) decodes PCM, FLAC, Vorbis and MP3 in pure
    Rust. It also has an AAC-LC decoder, but no HE-AAC and, as far as I found, no Opus.
- **AAC patents.** AAC is still licensed through a patent pool.
  [ADR-0062](0062-aac-lc-encoder-patent-check.md) cleared JStream's own AAC-LC *encoder*
  only. It says "no decoder", and it quotes the pool's terms naming developers of
  "encoder and/or decoder" products. H.264 has its own pool, with the same shape.
- **What files people have.** Most `.mp4` and `.mov` files are H.264 video with AAC
  audio. Screen recorders and browsers also write WebM (VP8 or VP9, with Opus).
- **Demux.**
  - Pure-Rust demuxers exist: `mp4` (MP4 and MOV), `matroska-demuxer` (Matroska and
    WebM), and symphonia's own (MP4, Matroska, Ogg, WAV, FLAC, MP3).
  - Older containers (AVI, MPEG-TS, FLV) have no pure-Rust demuxer I could confirm.
  - Each crate's licence is checked under [R-DEP-001](../RULES.md#R-DEP-001) when it is
    added.
- **FFmpeg's licensing.**
  - FFmpeg's libraries (`libavformat`, `libavcodec`, `libavutil`, `libswresample`) are
    LGPL-2.1-or-later as long as the build turns on no GPL or non-free parts. Their
    native H.264 and AAC decoders are in the LGPL part.
  - Each library reports how it was built: `avcodec_license()` and its siblings return
    the licence string.
  - Distributions differ. Debian and Ubuntu build FFmpeg with `--enable-gpl`, so their
    `libav*` are GPL as built. Fedora's `ffmpeg-free` leaves out patented codecs, and
    RPM Fusion's full build is GPL as well.
  - So "the system's FFmpeg" is often a GPL build, or one without H.264 and AAC.
    Each distribution's build has to be checked, not assumed.

<a id="decision"></a>
## Decision
**Proposed, for the maintainer.** Each question has options and a recommendation. The
maintainer accepts the recommendations, or names other options, and this ADR is then
rewritten to the chosen ones before it is accepted.

**(a) Does Linux get a media-file source?**
1. **No.** Leave it out of scope, as ADR-0060 §9 does now.
2. **Yes, within the limits of (b) to (g).** `jstream-platform-linux` implements
   `MediaDecoders`.

*Recommendation: 2.* Every other platform has this M2 kind.

**(b) Video decode.**
1. **VA-API only.** On a driver without H.264 decode, a file with H.264 video is a
   missing component whose remedy names the driver or the distribution's full build.
2. **VA-API, then FFmpeg as the software fallback** where VA-API can't decode the file.
   This is FFmpeg under (e) to (g).
3. **VA-API, then OpenH264's decoder.** This changes ADR-0061's "nothing decodes through
   OpenH264", and Cisco's patent licence would need checking for decoding.
4. **VA-API, then a pure-Rust decoder**, once a mature one exists.

*Recommendation: 2.* It is the only option that plays H.264 on NVIDIA, on Fedora's stock
Mesa and in virtual machines. It falls back only when VA-API refuses, and only after
(g)'s patent check. Until then, the source behaves as in option 1. VP9 and AV1 go through
VA-API first, and then through FFmpeg the same way.

**(c) Audio decode.**
1. **symphonia for the patent-free codecs** (PCM, FLAC, Vorbis, MP3), and nothing else.
   A file whose only audio is AAC plays its video without sound, and the source reports
   the audio codec as unsupported.
2. **Option 1, plus AAC-LC through symphonia**, once (g) clears it.
3. **Options 1 and 2, plus FFmpeg for what symphonia can't decode:** HE-AAC, Opus, and
   anything else (e) allows.

*Recommendation: 3.* symphonia is pure Rust, so it comes first. FFmpeg serves only the
codecs symphonia lacks. Both AAC paths wait for (g).

**(d) Containers.**
1. **Pure-Rust demuxers only:** MP4 and MOV, Matroska and WebM, and symphonia's
   audio-only readers (WAV, FLAC, Ogg, MP3).
2. **Option 1, plus FFmpeg's `libavformat` for containers they can't read**, such as
   AVI, MPEG-TS and FLV.
3. **MP4 and MOV only.**

*Recommendation: 2.* The pure-Rust demuxers come first. `libavformat` opens only a file
that none of them can read.

**(e) Which FFmpeg, and how it is reached.**
1. **The system's `libav*` libraries, loaded at run time, and only LGPL builds.**
   - They are opened with `dlopen` on first use, as libva is (ADR-0060 §8), through
     hand-written declarations. No `-sys` or binding crate is used, and `deny.toml`'s
     FFmpeg bans stay.
   - Each library's licence string is checked when it loads. A GPL or non-free build is
     refused as a missing component, whose remedy explains why.
   - JStream never ships or builds FFmpeg.
2. **The system's `libav*`, whatever their licence.** This works on Debian and Ubuntu's
   GPL builds. Whether loading a GPL library at run time puts JStream under the GPL is
   a legal question for the maintainer.
3. **A bundled LGPL build** made by the project: decoders only, no GPL parts, shipped
   beside the app and dynamically linked.
   - It works the same on every distribution.
   - But JStream then distributes FFmpeg, with LGPL obligations (its source, its notice,
     and the user's right to replace it), and it distributes the decoders, which puts the
     patent exposure on JStream itself.
4. **No FFmpeg.** (b) to (d) take their option 1.

*Recommendation: 1.* It is the narrowest exception, and it matches the OpenH264 and libva
precedents.

Its cost should be stated plainly: on Debian and Ubuntu, whose system FFmpeg is GPL, the
fallback doesn't load. Users there get option 1's "missing component" until they
install an LGPL build. If the maintainer prefers coverage there, option 2 is the
alternative, and it needs a legal view first.

**(f) The rule change.**
1. **A narrow exception in R-DEP-002**, scoped to all of these at once:
   - Linux only, and decoding only;
   - the system's FFmpeg libraries, loaded at run time, LGPL builds only;
   - a fallback after VA-API and the pure-Rust crates.
   - It is written next to the CEF and OpenH264 exceptions. ADR-0061's "OS APIs only"
     line for media files is superseded by an ADR-0061 v3 that restates it with this
     exception.
2. **Lift the FFmpeg ban generally.** Not recommended: it would allow binding crates,
   bundled builds and encoding through FFmpeg.

*Recommendation: 1.* The exception names its limits, so lint-deps and `deny.toml` keep
banning everything else. The R-DEP-002 edit and the ADR-0061 v3 land with the
implementing PR, once this ADR is accepted.

**(g) Patent exposure.** Decoding H.264 or AAC through FFmpeg, or AAC through symphonia,
is still decoding. A licence for FFmpeg's code isn't a patent licence.
1. **A decoder patent check before any software H.264 or AAC decode ships.** It is a
   follow-up ADR like ADR-0062, covering:
   - symphonia's AAC-LC decoder, and FFmpeg's H.264 and AAC decoders;
   - who holds the licence when a system library does the decoding (the distribution,
     the user, or JStream);
   - whether JStream's role, calling a library the user installed, needs one.
   - Until it says yes, those paths stay off.
2. **Ship them, and treat a system library as the system's responsibility**, as on
   Windows. Not recommended without the check: Windows' and macOS's decoders come with
   the OS vendor's licences, and a Linux distribution's FFmpeg usually doesn't.

*Recommendation: 1.* VA-API decode isn't affected: the GPU's driver does the decoding,
as it does the encoding today.

**Either way:**
- A container or codec outside the chosen set is `Unsupported`, naming it.
- Decoded audio is interleaved `f32` at the file's own rate, as the trait requires.
- Video comes out in the configured CPU pixel format.
- The other platforms are unchanged.

<a id="consequences"></a>
## Consequences
- **If accepted as recommended, playback grows in stages:**
  - **First:** H.264 on a VA-API GPU, PCM, FLAC, Vorbis and MP3 audio, and MP4, MOV,
    Matroska, WebM, WAV, FLAC, Ogg and MP3 files.
  - **After (g)'s check says yes:** AAC-LC through symphonia, and the FFmpeg fallback for
    H.264 without VA-API, HE-AAC, Opus and the other containers, where the system has an
    LGPL FFmpeg.
  - **Until then:** most `.mp4` files play without sound, and machines without VA-API
    H.264 play no H.264 video.
- **The work is large.** VA-API decode means parsing H.264 headers, filling libva's
  picture and slice parameters, keeping the decoded picture buffer, and reordering into
  presentation order. The FFmpeg fallback is smaller: open, read packets, decode and
  convert, all through declarations written by hand.
- **On acceptance:**
  - R-DEP-002 gains the FFmpeg exception, worded as (f) says.
  - ADR-0061 v3 supersedes ADR-0061, with the same text apart from the Linux media-file
    line. ADR-0060 stays accepted, its §9 media-file bullet answered here.
  - The `jstream-platform-linux` README names this ADR.
  - ROADMAP's M3 Linux line adds the media-file item and the decoder patent check.
- **Tests:**
  - Demux, header parsing and symphonia decoding are tested on any platform, on small
    files committed under the crate's test data, in licences the repository allows.
  - VA-API decode and the FFmpeg fallback are `hw-tests` tests on a machine that has
    them.
  - The licence check that refuses a GPL build is tested hermetically.
- **If (a) is "no":** nothing changes. If (e) is "no FFmpeg", the source stays at its
  first stage, plus AAC-LC if (g) clears it.
