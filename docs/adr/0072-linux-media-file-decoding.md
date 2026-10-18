<!-- @doc
id = "ADR-0072"
title = "Linux media-file decoding: VA-API H.264 and patent-free audio, in pure Rust"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0072: Linux media-file decoding: VA-API H.264 and patent-free audio, in pure Rust

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
The media-file source plays a local video or audio file, looping, on the media clock
([ADR-0061](0061-source-types-and-decoding-scope-v2.md)). The engine reaches it through
`jstream_platform::MediaDecoders`: Windows implements it on Media Foundation, and macOS
on AVFoundation's asset reader. Each uses the OS's own demuxers and decoders, so a file
plays where the OS can play it.

Linux has neither. [ADR-0060](0060-linux-platform-support.md) §9 left the media-file
source out of scope there, "each its own later decision", because Linux has no OS
demuxer or audio decoder. ADR-0061 keeps "demux and decode use OS APIs" and restates
that Linux is out of scope. This ADR is the later decision §9 calls for. It is
maintainer tier ([ADR-0043](0043-narrow-maintainer-tier.md)) for two reasons. Whether
Linux gets the source at all is product scope, and decoding AAC raises a patent question
that the licensing tier covers.

The forces:
- **No FFmpeg or GStreamer.** [R-DEP-002](../RULES.md#R-DEP-002) bans them and every
  bundled C codec library. What Linux does have:
  - libva, which this crate already loads at run time for encoding (ADR-0060 §5, §8);
  - pure-Rust crates, which ADR-0061 allows for images and text, "each justified when
    added".
- **H.264 decode.**
  - VA-API decodes H.264 on Intel's and AMD's drivers, but not everywhere:
    - NVIDIA's driver has no VA-API of its own (ADR-0060 §5).
    - Fedora's stock Mesa turns H.264 off for patent reasons, with a full build in RPM
      Fusion. ADR-0060 records this for encode, and the same build leaves out the decoder.
    - Virtual machines usually have no VA-API at all.
  - VA-API takes the stream already parsed: the crate fills each picture's and slice's
    parameter buffers from the stream's headers and keeps the reference picture list. A
    pure-Rust header parser (such as `h264-reader`) covers the parsing. The rest is the
    crate's own code.
  - I know of no software H.264 decoder that this repository allows:
    - ADR-0061 says "nothing decodes through OpenH264", and the `openh264` crates stay
      banned.
    - I could not confirm a mature pure-Rust H.264 decoder.
- **Audio decode.**
  - PCM, FLAC, Vorbis and Opus are royalty-free. MP3's patents have expired.
  - symphonia (MPL-2.0, which is on the allow list) decodes PCM, FLAC, Vorbis and MP3
    in pure Rust, and has an AAC-LC decoder. I found no Opus decoder in it.
  - AAC is still licensed through a patent pool. [ADR-0062](0062-aac-lc-encoder-patent-check.md)
    cleared JStream's own AAC-LC *encoder* only. It says "no decoder", and quotes the
    pool's terms naming developers of "encoder and/or decoder" products, so a shipped
    AAC decoder needs a check of its own.
- **What files people have.** Most `.mp4` and `.mov` files are H.264 video with AAC
  audio. Screen recorders and browsers also write WebM (VP8 or VP9, with Opus).
- **Demux.**
  - Pure-Rust demuxers exist: `mp4` (MP4 and MOV), `matroska-demuxer` (Matroska and
    WebM), and symphonia's own (MP4, Matroska, Ogg, WAV, FLAC, MP3). They read video
    tracks to differing degrees.
  - Each one's licence is checked under [R-DEP-001](../RULES.md#R-DEP-001) when it is
    added.

<a id="decision"></a>
## Decision
**Proposed, for the maintainer.** Each question has options and a recommendation. The
maintainer accepts the recommendations, or names other options, and this ADR is then
rewritten to the chosen ones before it is accepted.

**(a) Does Linux get a media-file source?**
1. **No.** Leave it out of scope, as ADR-0060 §9 does now. Linux scenes can't play
   files.
2. **Yes, within the limits of (b) to (d).** `jstream-platform-linux` implements
   `MediaDecoders` itself, in Rust, with no OS media stack.

*Recommendation: 2.* The media-file source is an M2 kind that every other platform has,
and the limits below keep it within R-DEP-002.

**(b) Where does video decode come from?**
1. **VA-API only.**
   - H.264 decodes on the GPU through libva, which is already loaded at run time.
   - With no H.264 decoder in the driver, a file with H.264 video is a missing component
     whose remedy names the driver or the distribution's full Mesa. A
     `HardwarePreference` that asks for software is `Unsupported`.
   - VP9 and AV1 through VA-API follow later, under this same option, since they are
     royalty-free.
2. **VA-API, then OpenH264's decoder as the software fallback.** Cisco's binary is
   already downloaded on request for encoding (ADR-0061). This changes ADR-0061's
   "nothing decodes through OpenH264". It also needs Cisco's patent licence checked for
   the decoder in that binary.
3. **VA-API, then a pure-Rust software decoder.** None I could confirm is mature enough.
   This waits until one is.

*Recommendation: 1.* It needs no new licence decision, and it matches how Linux
encodes. The machines it leaves without file playback (NVIDIA, Fedora's stock Mesa,
virtual machines) are the ones ADR-0060 already leaves without hardware encode.

**(c) Which audio codecs?**
1. **Patent-free only.** PCM, FLAC, Vorbis and MP3, through symphonia, with Opus once a
   pure-Rust decoder is chosen.
   - A file whose only audio is AAC opens with its video and no audio, and the source
     reports the audio codec as unsupported. A file with only AAC audio and no video is
     `Unsupported`.
2. **Option 1, plus AAC after a decoder patent check.** A follow-up ADR, like
   ADR-0062, decides whether JStream may ship symphonia's AAC-LC decoder. AAC is
   added only if it may.
3. **AAC now, without a check.** Not recommended: it skips the question ADR-0062 left
   open.

*Recommendation: 1 now, and start 2's check alongside it.* Because most `.mp4` files
carry AAC, option 1 alone plays most of them without sound. That is the main cost of
this ADR, and the reason the check shouldn't wait.

**(d) Which containers, and whose demuxers?**
1. **MP4 and MOV, plus Matroska and WebM, through pure-Rust demuxers.**
   - `mp4` (or an equivalent) handles MP4 and MOV.
   - `matroska-demuxer` (or an equivalent) handles Matroska and WebM.
   - symphonia's readers handle audio-only files: WAV, FLAC, Ogg and MP3.
   - Each crate is named, licence-checked (R-DEP-001) and justified in the implementing
     PR, which is agent tier once this ADR is accepted.
2. **MP4 and MOV only.** Fewer dependencies, but WebM recordings don't play.
3. **The crate's own box and EBML parsers.** No new demux dependency, but more code
   to own and fuzz.

*Recommendation: 1.* WebM is common from browsers and recorders, and its usual codecs
(VP9 under (b), Opus under (c)) are royalty-free.

**Either way:**
- A container or codec outside the chosen set is `Unsupported`, naming it.
- Decoded audio is interleaved `f32` at the file's own rate, as the trait requires.
- Video comes out in the configured CPU pixel format, as on Windows.
- The other platforms are unchanged.

<a id="consequences"></a>
## Consequences
- **If accepted as recommended:**
  - Linux plays H.264 files on a VA-API GPU with an H.264 decoder.
  - It plays audio in PCM, FLAC, Vorbis and MP3, then Opus.
  - It plays files in MP4, MOV, Matroska, WebM, WAV, FLAC, Ogg and MP3 containers.
  - Files with AAC audio play without sound until the AAC decoder check says yes.
- **The work is large.** VA-API decode means parsing H.264 headers, filling libva's
  picture and slice parameters, keeping the decoded picture buffer, and reordering into
  presentation order. That is the main share of the work. Demux and audio decode mostly
  come from the crates.
- **ADR-0060 and ADR-0061 stay accepted and unchanged.** This ADR is the later
  decision ADR-0060 §9 names, and it narrows ADR-0061's "OS APIs" for Linux only. On
  acceptance:
  - ADR-0060 §9's media-file bullet is read as answered here.
  - The `jstream-platform-linux` README's "Not here yet" names this ADR.
  - ROADMAP's M3 Linux line adds the media-file item.
- **Tests:**
  - Demux, header parsing and audio decode test on any platform, on small files
    committed under the crate's test data, in licences the repository allows.
  - VA-API decode is a `hw-tests` test on a GPU with an H.264 decoder, as encode is.
- **If (a) is answered "no":** nothing changes, and the source stays Windows and macOS
  only.
