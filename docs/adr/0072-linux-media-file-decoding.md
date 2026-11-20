<!-- @doc
id = "ADR-0072"
title = "Linux media-file decoding: VA-API and pure Rust first, the system's FFmpeg for the gaps"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0072: Linux media-file decoding: VA-API and pure Rust first, the system's FFmpeg for the gaps

<!-- @generated begin adr-status -->
> **Status:** accepted
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

The maintainer allowed FFmpeg wherever no OS or pure-Rust choice can do the job: "I feel
like we can do ffmpeg is there are no other valid choices".

The forces:
- **R-DEP-002 before this ADR.** [R-DEP-002](../RULES.md#R-DEP-002) banned
  FFmpeg, GStreamer and every bundled C codec library, and `deny.toml` bans their binding
  crates. Its one
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
The maintainer accepted the recommendations on (a) to (d), (f) and (g), and chose "any
build" on (e): the system's FFmpeg, GPL builds included. FFmpeg loading then has a second
gate beside (g): the [GPL legal view](#gpl-precondition) below. The options not chosen are
under [Options considered](#options).

**(a) Linux gets a media-file source.** `jstream-platform-linux` implements
`MediaDecoders`, within the limits of (b) to (g). Every other platform has this M2 kind.

**(b) Video decode: VA-API, then FFmpeg as the software fallback.**
- H.264 goes through VA-API first. Only where VA-API can't decode the file does it fall
  back to FFmpeg, under (e) to (g).
- Only this choice plays H.264 on NVIDIA, on Fedora's stock Mesa and in virtual machines.
- VP8, VP9 and AV1 go through VA-API first, then through FFmpeg the same way.
- Until the fallback's gates pass, a file VA-API can't decode is a missing component. Its
  remedy names the driver or the distribution's full build. Every FFmpeg path waits for
  both gates, as [(g)](#patent-scope) says.

**(c) Audio decode: symphonia first, FFmpeg for what it can't decode.**
- symphonia decodes the patent-free codecs: PCM, FLAC, Vorbis and MP3.
- AAC-LC goes through symphonia too, once (g) clears it.
- FFmpeg decodes only what symphonia can't: HE-AAC, Opus, and anything else (e) allows.
- Both AAC paths wait for (g). Until then, a file whose only audio is AAC plays its video
  without sound, and the source reports the audio codec as unsupported.

**(d) Containers: pure-Rust demuxers first, then `libavformat`.**
- The pure-Rust demuxers read MP4 and MOV, Matroska and WebM. symphonia's readers add
  WAV, FLAC, Ogg and MP3.
- `libavformat` opens only a file none of them can read, such as AVI, MPEG-TS or FLV.

**(e) Which FFmpeg: the system's `libav*` libraries, any build, loaded at run time.**
- The libraries are `libavformat`, `libavcodec`, `libavutil` and `libswresample`, as the
  distribution installs them.
- They are opened with `dlopen` on first use, as libva is (ADR-0060 §8), through
  hand-written declarations. No `-sys` or binding crate is used, and `deny.toml`'s FFmpeg
  bans stay.
- Any build loads, LGPL or GPL. So the fallback works with Debian's and Ubuntu's GPL builds
  and with RPM Fusion's full build.
- Each library's licence string (`avcodec_license()` and its siblings) is read when it
  loads and logged, so a bug report shows which build ran.
- JStream never ships, builds, links or downloads FFmpeg.

**(f) The rule change: a narrow exception in R-DEP-002.** It is scoped to all of these at
once:
- Linux only, and decoding only;
- the system's FFmpeg libraries, loaded at run time;
- a fallback after VA-API and the pure-Rust crates.

Because the exception names its limits, lint-deps and `deny.toml` go on banning the rest:
binding crates, bundled builds, and encoding through FFmpeg.
- R-DEP-002's [new text](#rule-text) lands with this ADR.
- ADR-0061's "OS APIs only" line for media files is superseded by an ADR-0061 v3, which
  restates it with this exception. That lands with the implementing PR.

**(g) Patent exposure: a decoder patent check before any software H.264 or AAC decode
ships.** Decoding H.264 or AAC through FFmpeg, or AAC through symphonia, is still decoding.
A licence for FFmpeg's code isn't a patent licence.
- The check is a follow-up ADR like ADR-0062. It covers:
  - symphonia's AAC-LC decoder, and FFmpeg's H.264 and AAC decoders;
  - who holds the licence when a system library does the decoding: the distribution, the
    user, or JStream;
  - whether JStream needs one at all, when it only calls a library the user installed.
- Until the check says yes, those paths stay off.
- VA-API decode isn't affected: the GPU's driver does the decoding, as it does the
  encoding today.

<a id="patent-scope"></a>
**Every FFmpeg path waits for (g), as well as for the [GPL legal view](#gpl-precondition).**
This is the reading the maintainer accepted: (b)'s fallback comes "only after (g)'s patent
check", and VP9 and AV1 go through FFmpeg "the same way". So (g)'s follow-up ADR covers
every codec decoded through FFmpeg, not only H.264 and AAC:

| Path | GPL legal view | (g)'s patent check |
|---|---|---|
| FFmpeg: H.264, AAC (LC and HE) | needed | needed |
| FFmpeg: VP8, VP9, AV1, Opus, the extra containers | needed | needed |
| symphonia: AAC-LC | not needed (no FFmpeg) | needed |
| symphonia: PCM, FLAC, Vorbis, MP3 | not needed | not needed |
| VA-API | not needed | not needed |

**Open question, for a later maintainer-tier ADR:** could the royalty-free FFmpeg paths
(VP8, VP9, AV1, Opus and the extra containers) go ahead after the GPL legal view alone,
leaving only software H.264 and AAC behind (g)? Their makers license them royalty-free,
but the follow-up would need to check third-party patent claims on them too. Until a
maintainer decision says otherwise, they wait for (g).

**Either way:**
- A container or codec outside the chosen set is `Unsupported`, naming it.
- Decoded audio is interleaved `f32` at the file's own rate, as the trait requires.
- Video comes out in the configured CPU pixel format.
- The other platforms are unchanged.

<a id="gpl-precondition"></a>
## Precondition: a legal view on loading a GPL FFmpeg
**Nothing that loads FFmpeg ships until this is settled.**
- The FFmpeg fallback can be written and reviewed on a branch.
- It doesn't land in a wave or on `main`, and no build turns it on, until the maintainer
  records counsel's answer in a maintainer-tier ADR.
- (g)'s patent check is a separate gate. Both must pass.

**This is not legal advice.** An agent wrote this section from what it knows of the public
debate, without re-reading the sources here. Settling the question needs a lawyer, not an
agent.

### The question
Does loading a GPL-licensed `libav*` build at run time make JStream a derivative work under
the GPL's obligations? The facts:
- JStream opens the library with `dlopen`, and has no link-time dependency on it.
- It calls the library through hand-written declarations of FFmpeg's public C API.
- It doesn't redistribute FFmpeg in any form.

Two sub-questions go with it:
- If the answer is yes, who carries the obligations: the JStream project, the user who
  installed both, or whoever distributes the two together?
- Does the answer change for a `--enable-nonfree` build, whose licence forbids
  redistribution?

### The view that it is a combined work
This is the Free Software Foundation's position, as its GPL FAQ states it:
- Suppose a program and a library are linked, statically or dynamically, and run in one
  address space, calling each other's functions and sharing data structures. Then they
  form a single combined program.
- A plug-in loaded that way is part of the program, so the combination as a whole must be
  under GPL-compatible terms.
- On this view, JStream decoding through a GPL `libavcodec` is a combined work, however
  the library is loaded.

How JStream's licence fits this view:
- JStream is `MIT OR Apache-2.0`. MIT is compatible with GPLv2 and GPLv3, and Apache-2.0
  only with GPLv3. So JStream's code can be part of a GPL whole without changing its
  licence. With a GPLv2 build, that is through the MIT side.
- The GPL's conditions apply to conveying (distributing) the work, not to running it.
  JStream doesn't convey FFmpeg, so the combination exists only on the user's machine.
- The risk is in distribution:
  - Anyone who ships JStream with a GPL FFmpeg would convey the combined work, and owe the
    GPL's terms for all of it. That covers a distribution package, a Flatpak or an
    AppImage.
  - Some would argue that JStream's own release, built to load FFmpeg, is already a work
    based on it.
- JStream's code would stay permissive. But a proprietary product built on JStream could
  not keep the FFmpeg path.

### The view that it is not
Many lawyers and commentators reject the FSF's line:
- Under copyright law, a derivative work must copy or adapt protectable expression from
  the original. How two programs are joined at run time is a technical fact, not the legal
  test.
- JStream's binary contains no FFmpeg code, only declarations of a public interface.
  Courts have treated interfaces as functional or their reuse as fair use: *Google v.
  Oracle* in the US, and *SAS Institute v. World Programming* in the EU, alongside the
  Software Directive's interoperability provisions.
- The library is optional and replaceable. JStream works without it, and an LGPL build
  does the same job. That weighs against JStream being based on any one build.
- The user puts the two together on their own machine, not JStream, and the GPL doesn't
  restrict running a program.

How JStream's licence fits this view: nothing changes. `MIT OR Apache-2.0` covers all of
JStream for everyone, and the GPL binds only whoever distributes FFmpeg itself.

### What the maintainer would give counsel
- JStream's licence (`MIT OR Apache-2.0`, in `LICENSE-MIT` and `LICENSE-APACHE`). Also
  whether the permissive promise must hold for every redistributor, proprietary forks
  included.
- How the code reaches FFmpeg, from this ADR and, once it exists, the implementing PR:
  - `dlopen` on first use, with no link-time dependency;
  - the hand-written declarations, and that they are written from FFmpeg's public headers,
    which are LGPL files in FFmpeg's sources;
  - what crosses the boundary: packets go in and decoded frames come out, in one address
    space;
  - that it is only a fallback, used when VA-API and the pure-Rust crates can't read the
    file, and that an LGPL build works the same.
- How JStream is distributed on Linux (a tarball, a Flatpak, an AppImage, distribution
  packages), and whether any channel would bundle FFmpeg or pull it in.
- Which builds users would load:
  - Debian's, Ubuntu's and RPM Fusion's GPL builds;
  - the GPL version of each (`--enable-version3` makes a build GPLv3);
  - whether non-free builds are in scope.
- What the app and its docs tell users, such as whether they suggest installing a GPL
  build.
- The jurisdictions that matter: where the maintainer is, and where JStream is offered.
- That (g)'s patent question is separate, so counsel can say whether to take it on too.

<a id="rule-text"></a>
## R-DEP-002's new text
R-DEP-002 now counts three media exceptions. It adds this one after the OpenH264
exception:

> On Linux, the media-file source may decode through the system's FFmpeg libraries
> (`libavformat`, `libavcodec`, `libavutil`, `libswresample`) as a fallback, for a file
> that VA-API and the pure-Rust crates can't read. They are loaded at run time through
> hand-written declarations. JStream never ships, builds or links them, so the FFmpeg
> binding crates stay banned, and nothing encodes through FFmpeg. The fallback lands only
> once ADR-0072's GPL legal view and its decoder patent check are both done (ADR-0072).

In RULES.md, that last ADR-0072 links here, and the rule's first sentence cites ADR-0072
beside ADR-0061. The rule's title, its enforcement and `deny.toml`'s bans don't change.

<a id="options"></a>
## Options considered
- **(a)** No Linux source, as ADR-0060 §9 left it.
- **(b)**
  - VA-API only.
  - VA-API, then OpenH264's decoder. It goes against ADR-0061's "nothing decodes through
    OpenH264", and Cisco's licence would need checking for decoding.
  - VA-API, then a pure-Rust decoder, once a mature one exists.
- **(c)** symphonia's patent-free codecs only, or those plus AAC-LC, without FFmpeg.
- **(d)** Pure-Rust demuxers only, or MP4 and MOV only.
- **(e)**
  - LGPL builds only, refusing GPL ones. It was the narrowest exception, but the fallback
    wouldn't load on Debian or Ubuntu.
  - A bundled LGPL build made by the project. JStream would then distribute FFmpeg, with
    the LGPL's obligations: its source, its notice, and the user's right to replace it.
    It would also carry the decoders' patent exposure.
  - No FFmpeg.
- **(f)** Lifting the FFmpeg ban generally. That would allow binding crates, bundled
  builds and encoding through FFmpeg.
- **(g)** Shipping the software decoders without a check, and treating a system library as
  the system's responsibility, as on Windows. But Windows' and macOS's decoders come with
  the OS vendor's licences, and a Linux distribution's FFmpeg usually doesn't.

<a id="consequences"></a>
## Consequences
- **Playback grows in stages:**
  - **First:** H.264, VP8, VP9 and AV1 on a VA-API GPU that decodes them, PCM, FLAC,
    Vorbis and MP3 audio, and MP4, MOV, Matroska, WebM, WAV, FLAC, Ogg and MP3 files.
  - **After (g)'s check says yes:** AAC-LC through symphonia.
  - **After both (g) and the GPL legal view:** the whole FFmpeg fallback: H.264, VP8, VP9
    and AV1 without VA-API, HE-AAC, Opus, and the other containers.
  - **Until then:** most `.mp4` files play without sound, and machines without VA-API
    H.264 play no H.264 video.
- **The work is large.** VA-API decode means parsing H.264 headers, filling libva's
  picture and slice parameters, keeping the decoded picture buffer, and reordering into
  presentation order. The FFmpeg fallback is smaller: open, read packets, decode and
  convert, all through declarations written by hand.
- **With this ADR:** R-DEP-002 gains the FFmpeg exception, worded as
  [above](#rule-text).
- **With the implementing PR:**
  - ADR-0061 v3 supersedes ADR-0061, with the same text apart from the Linux media-file
    line. ADR-0060 stays accepted, its §9 media-file bullet answered here.
  - The `jstream-platform-linux` README names this ADR.
  - ROADMAP's M3 Linux line adds the media-file item, the GPL legal view and the decoder
    patent check.
- **Tests:**
  - Demux, header parsing and symphonia decoding are tested on any platform, on small
    files committed under the crate's test data, in licences the repository allows.
  - VA-API decode and the FFmpeg fallback are `hw-tests` tests on a machine that has
    them.
  - Reading and logging each library's licence string is tested hermetically, and so is
    the switch that keeps each FFmpeg path off until its gates pass.
