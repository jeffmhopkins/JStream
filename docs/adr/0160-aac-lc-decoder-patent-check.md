<!-- @doc
id = "ADR-0160"
title = "AAC-LC decoder patent check: may Linux decode AAC-LC through symphonia?"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0160: AAC-LC decoder patent check: may Linux decode AAC-LC through symphonia?

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0072](0072-linux-media-file-decoding.md) (g) keeps every software AAC decode off on
Linux until a decoder patent check, like [ADR-0062](0062-aac-lc-encoder-patent-check.md)'s
for the encoder, says yes. Until then, a media file's AAC audio is skipped: the video
plays without sound, and an AAC-only file doesn't play. AAC is the audio of almost every
MP4 and MOV file (phone recordings, screen recordings, downloads), so this is the largest
gap left in Linux media files.

(g) asks the check to cover three things: symphonia's AAC-LC decoder, FFmpeg's H.264 and
AAC decoders, and who holds a licence when a system library decodes. **This ADR answers
the first only.** FFmpeg also waits for the separate GPL legal view, and Linux decodes
H.264 on the GPU (VA-API, which (g) doesn't gate), so neither is needed for AAC-LC. The
FFmpeg paths stay gated, for a later ADR.

It is maintainer tier because its outcome is a licensing decision and possibly a spending
one ([ADR-0043](0043-narrow-maintainer-tier.md), [R-GOV-005](../RULES.md#R-GOV-005)).

**This is not legal advice.** An agent wrote it, from ADR-0062's findings (which the
maintainer accepted) and the source of the decoder in question. Nobody qualified to give
a legal opinion has reviewed it.

### What JStream would ship
symphonia's `aac` feature (`symphonia-codec-aac` 0.6), pure Rust, MPL-2.0 like the rest of
symphonia, already in the tree as a test-only reference decoder for `jstream-aac`. Read
from its source:
- It decodes the MPEG-4 AAC-LC object type only, mono or stereo, 1024-sample frames. It
  refuses every other object type, more than two channels, coupling channels, program
  config elements, and streams that signal SBR explicitly (HE-AAC's object types 5 and
  29).
- Its tools are those of the MPEG-2 AAC LC profile (filterbank and window switching, TNS,
  M/S and intensity stereo, scalefactors, Huffman decoding), plus PNS, the one tool
  MPEG-4 added to the LC object type (standardised in 1999).
- It never runs SBR or parametric stereo. A stream that hides SBR inside an AAC-LC stream
  (implicit signalling) decodes its LC core only: the audio plays at half the sample rate
  with its top octave missing, rather than not at all.

<a id="findings"></a>
## Findings
ADR-0062's findings apply to decoding as they do to encoding, with these differences.

1. **The pool licenses decoders the same way.** Via LA's
   [AAC programme](https://www.via-la.com/licensing-programs/aac/) covers "encoder and/or
   decoder products", on the same per-unit terms ADR-0062 records, with free distribution
   not exempt. There is still no separate AAC-LC licence.
2. **A decoder practises the standard's essential patents directly.** An encoder's
   exposure is partly to non-essential technique patents (psychoacoustic models, rate
   loops) that no pool lists. A decoder has almost none of those: what it does is fixed by
   the standard. So its exposure is mostly the essential patents, which is what the pool
   lists.
3. **The tools are old.** Every tool this decoder runs was standardised by 1997, and PNS
   by 1999. As ADR-0062 found, a patent essential to them would normally have been filed
   by then and so would have expired around 2017 to 2019 in the US, the EPO's states and
   Japan. **That is an inference from the dates, not a patent-by-patent finding,** as it
   was for the encoder.
4. **The long-lived patents ADR-0062 checked are SBR, parametric stereo and multichannel.**
   This decoder runs none of those. The last of the US ones expires on 2028-12-09.
   Decoding only an HE-AAC stream's LC core doesn't practise SBR.
5. **Distributors ship AAC-LC decoding without a disclosed licence.** Fedora's
   [`fdk-aac-free`](https://bugzilla.redhat.com/show_bug.cgi?id=1501522), cleared by Red
   Hat Legal and described in [Debian's upload](https://ftp-master.debian.org/new/fdk-aac-free_2.0.2-3.html)
   as stripping "newer high efficiency profiles with patent concerns", includes FDK's
   decoder, which decodes AAC-LC and plays HE-AAC files with reduced functionality. As in
   ADR-0062, that shows how Red Hat assessed the risk for its distribution; it is not a
   statement by a patent holder.
6. **What this check could not confirm** is what ADR-0062 couldn't: that no unexpired
   patent anywhere reads on AAC-LC decoding, especially outside the US and among patents
   filed after 2003, and whether Via LA would treat a free decoder as needing a licence.

<a id="options"></a>
## Options
- **A. Turn on symphonia's AAC-LC decoder, LC only, with ADR-0062's limits.** Linux
  decodes AAC-LC (and the LC core of an HE-AAC stream) in media files, worldwide, with no
  pool licence. It never decodes SBR or parametric stereo, and never through FFmpeg. The
  residual risk is ADR-0062's: inference rather than proof outside the US and after 2003.
- **B. As A, but refuse HE-AAC streams outright** instead of decoding their LC core. Less
  audio plays, for no difference in the tools run: the LC core decode is the same code.
- **C. Wait for a system library** (FFmpeg or fdk-aac loaded at run time). It needs the
  GPL legal view for FFmpeg, its own R-DEP-002 exception, and the licence-holder question
  (g) leaves open. It also runs only where the distribution ships one.
- **D. Take a pool licence.** A spending decision, with the terms ADR-0062 found don't fit
  a free product.
- **E. Keep AAC off on Linux.** MP4 and MOV files keep playing without sound.

<a id="decision"></a>
## Decision
**Proposed: option A.** Linux turns on symphonia's `aac` feature for media files:
- **Tools.** AAC-LC only, as `symphonia-codec-aac` decodes it: the MPEG-2 AAC LC tools and
  PNS. Never SBR, parametric stereo, or any later profile's tools. A change that would
  decode one needs a new maintainer-tier ADR.
- **HE-AAC.** An implicitly signalled HE-AAC stream plays its LC core; an explicitly
  signalled one is `Unsupported`, naming it.
- **No FFmpeg.** This settles (g) for symphonia only. FFmpeg's decoders stay behind (g)
  and the GPL legal view.
- **Notice and re-check** as ADR-0062: the app's licence information says JStream holds no
  AAC patent licence and names Via LA; if a licensor or Via LA raises a patent against
  AAC-LC decoding, decoding is switched off in the next release and the question returns
  to the maintainer.

<a id="questions"></a>
## Questions for the maintainer
1. **Which option: A (recommended), B, C, D or E?**
2. **Is ADR-0062's evidence enough for decoding too,** or do you want a legal opinion
   first (a spending decision)?
3. **HE-AAC:** do you accept playing an implicit HE-AAC stream's LC core (A), or should
   HE-AAC be refused (B)?

<a id="consequences"></a>
## Consequences
- **If A:** the implementing PR turns on `symphonia`'s `aac` feature in
  `jstream-platform-linux` (a governance change under R-DEP-002's feature list), maps MP4's
  `mp4a` (`esds`) and Matroska's `A_AAC` tracks to it, and updates ADR-0072's table: the
  "symphonia: AAC-LC" row's (g) cell is settled. AAC in MP4, MOV, Matroska and ADTS files
  then plays on Linux.
- **The decision rests on inference, not proof,** outside the US and for patents filed
  after 2003, as ADR-0062's does.
- **Unchanged:** every FFmpeg path, and the GPL legal view.
