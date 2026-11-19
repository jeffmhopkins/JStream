<!-- @doc
id = "ADR-0159"
title = "Opus decodes in pure Rust on Linux"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0159: Opus decodes in pure Rust on Linux

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0072](0072-linux-media-file-decoding.md) (c) decodes Linux media-file audio through
symphonia first, and leaves to FFmpeg what symphonia can't decode: HE-AAC and Opus. It
found no pure-Rust Opus decoder then. Every FFmpeg path waits for two maintainer gates,
the GPL legal view and (g)'s patent check, so until both pass a WebM file plays without
its sound. Opus is the audio of almost every WebM file that screen recorders and browsers
write, and of `.opus` files.

ADR-0072's own order is pure Rust before FFmpeg. Opus is royalty-free, so (g)'s table
puts no pure-Rust Opus path behind the patent check; only its FFmpeg path waits.

Pure-Rust Opus decoders now exist. Three were compared against libopus's own decoding of
the same streams, in CELT, SILK, hybrid, 2.5 ms low-delay and 60 ms frames, and fed
20 000 random packets each in a debug build:

| Crate | Licence | Against libopus | Random packets |
|---|---|---|---|
| `rusty-opus` 1.2 | BSD-3-Clause | about 72 dB SNR in every mode | no panics |
| `opus-rs` 0.1 | BSD-3-Clause | 72 dB, but 21 dB on 2.5 ms frames | no panics |
| `opus-decoder` 0.1 | MIT OR Apache-2.0 | 24 dB on 60 ms frames | 33 panics |

`rusty-opus` has no dependencies, decodes multistream (surround) packets from an
`OpusHead`'s layout, and documents that a malformed packet never panics. Its `unsafe` is
confined to SIMD kernels, each with a scalar twin it is tested against.

<a id="decision"></a>
## Decision
Linux decodes Opus through `rusty-opus`, in place of ADR-0072 (c)'s FFmpeg path for Opus.
- It decodes Opus in Ogg (`.opus`, through symphonia's Ogg reader) and in Matroska and
  WebM (through the crate's demuxer), with mapping families 0, 1 and 255.
- Audio comes out as ADR-0072 requires: interleaved `f32`, at Opus's 48 kHz, mixed down to
  stereo beyond two channels.
- The header's output gain is applied. The container's pre-skip is cut: Ogg's from the
  track's delay, which symphonia's reader leaves in its granule clock, and Matroska's
  through `CodecDelay`. A seek decodes from 80 ms before its target, or from Matroska's
  `SeekPreRoll` when the file gives one.

ADR-0072 stays accepted. Only its Opus line changes: FFmpeg, once its gates pass, decodes
HE-AAC and anything else (e) allows, but not Opus.

<a id="consequences"></a>
## Consequences
- A WebM file's Opus audio plays on Linux now, and an audio-only WebM or `.opus` file
  plays in full. WebM's VP8, VP9 and AV1 video still need their VA-API decoders.
- One dependency joins the tree, with no transitive ones. Its encoder and analysis code
  are compiled but unused.
- Tests decode committed Opus files in Ogg and WebM on every platform's CI that builds the
  movie path, with no GPU.
