<!-- @doc
id = "CRATE-jstream-mux"
title = "jstream-mux"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-mux

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-mux/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-mux", "docs/adr/0031-source-types-and-decoding-scope.md", "docs/adr/0034-youtube-interaction-scope-v2.md", "crates/jstream-media/README.md#purpose"]
verified = { hash = "b3:1fda1c8dfd7be7e6", by = "agent:claude@session_01M7dd6pdhEEieUbct9m5TkJ", note = "F5: re-read Purpose against the enhanced RTMP change: intro and framing paragraph name HEVC (Annex B) and AV1 (low-overhead OBU temporal units, TD and padding dropped by av1::to_sample); new flv Enhanced RTMP bullet matches with_enhanced_video/ExVideoConfig, the ExVideoTagHeader layout, SequenceStart/CodedFrames/CodedFramesX/SequenceEnd choice, AV1 cts error and FourCC videocodecid; new hevc and av1 bullets match hevc.rs and av1.rs; Not yet now lists fourCcList signalling, HEVC/AV1 in MP4 and mid-stream parameter-set changes; ARCH crate-map row (FLV with enhanced RTMP) and ADR-0031/0034 still hold" }
-->

Layer-2 pure-Rust container muxers ([ADR-0031](../../docs/adr/0031-source-types-and-decoding-scope.md):
muxers are written in Rust, no FFmpeg). It writes H.264 and AAC into **FLV**, the
container M1a's RTMP(S) output sends to YouTube
([ADR-0034](../../docs/adr/0034-youtube-interaction-scope-v2.md)), along with HEVC and
AV1 through [enhanced RTMP](https://github.com/veovera/enhanced-rtmp/blob/main/docs/enhanced/enhanced-rtmp-v2.md);
and it writes H.264 and AAC into **fragmented MP4**, the container recording writes. It consumes
[`jstream-media`](../jstream-media/README.md#purpose) `Packet`s, does no network I/O,
has no `unsafe`, and depends only on `jstream-media` and `thiserror`.

**Bitstream framing (decided here).** H.264 `Packet` payloads are **Annex B** (start
codes) throughout the engine, because that is what OS encoders such as Media Foundation
emit. A container that needs AVCC (length prefixes: FLV, RTMP, MP4) converts at mux time
with `h264::annexb_to_avcc`, which copies NAL units unchanged, so emulation-prevention
bytes are preserved. HEVC payloads are Annex B too, and convert the same way. AV1
payloads are one **temporal unit of low-overhead OBUs** (each with its size field, except
perhaps the last), which FLV and MP4 store without temporal delimiters and padding
(`av1::to_sample`). AAC payloads are **raw access units** without ADTS headers, which is
what FLV and MP4 store.

Contents:
- **`flv`:** `FlvTagger` turns packets into `FlvTag`s with no I/O. Each tag is a type, a
  32-bit millisecond timestamp and the tag body exactly as FLV defines it (header byte,
  `AVCPacketType`, composition time, payload), so the RTMP client sends them as RTMP
  messages: the tag type values equal the RTMP message type IDs. `FlvWriter` adds the
  file header and tag framing and writes to any `std::io::Write`.
  - The first tags are `onMetaData` (AMF0 ECMA array: size, frame rate, bitrates, codec
    IDs, audio rate, sample size and stereo flag), then the AVC and AAC sequence headers.
    `MetadataStyle::SetDataFrame` prefixes `@setDataFrame`, for an RTMP client that
    sends the tag body as is. `jstream-outputs`' `RtmpPublisher::send_metadata` adds
    `@setDataFrame` itself, so with it use `MetadataStyle::File`; a test there pins
    that exactly one prefix then reaches the wire.
  - Without a configured `AvcDecoderConfig`, the record is built from the SPS and PPS of
    the first video packet, which must be a keyframe.
  - **Enhanced RTMP:** `FlvTagger::with_enhanced_video` (and
    `FlvWriter::with_enhanced_video`) makes the video track HEVC or AV1, given as an
    `ExVideoConfig` whose decoder configuration, if `None`, is built from the first
    video packet the same way (a keyframe carrying VPS, SPS and PPS, or a sequence
    header OBU). `VideoTrack` still gives the size, rate and bitrate, with `avc` left
    `None`. Video tags then use the `ExVideoTagHeader`: the `IsExHeader` bit, frame type
    and `VideoPacketType` in one byte, then the FourCC (`hvc1`, `av01`). The track
    starts with `SequenceStart` (the `HEVCDecoderConfigurationRecord` or
    `AV1CodecConfigurationRecord`) and ends with `SequenceEnd`. HEVC frames are
    `CodedFrames` with a 24-bit composition time, or `CodedFramesX` when it is zero; AV1
    frames are `CodedFrames` with none, so an AV1 packet whose pts and dts differ is an
    error. `onMetaData`'s `videocodecid` is the FourCC as a number. Neither multitrack,
    `ModEx` nor video metadata packets are written, and FourCC audio is not used: AAC
    stays a legacy tag.
  - Timestamps are each packet's dts, converted to milliseconds by `flv::to_ms`
    (`MediaTime::rescale`, rounded to nearest) from a configurable origin, wrapped to 32
    bits by `flv::wrap_timestamp`, with the high byte in the extended timestamp field.
    `to_ms` is public and is the workspace's one dts-to-ms conversion: the RTMP
    client's `timestamp_ms` builds on it, so the two can't round differently. The
    composition offset is the rounded pts minus the rounded dts (non-zero for B-frames). Packets before the
    origin, decode times going backwards within a track, and offsets beyond 24 bits are
    errors, and a rejected packet leaves the tagger unchanged.
  - `duration` and `filesize` are not written, because the output may be live.
    `FlvWriter::new` builds the start tags before writing the file header, so an invalid
    configuration fails before anything is written.
- **`fmp4`:** `Fmp4Muxer` turns packets into `Segment`s with no I/O: one init segment
  (`ftyp`, then a `moov` whose sample tables are empty and whose `mvex` has a `trex` per
  track), then media segments, each a `moof` (`mfhd`, a `traf` per track with `tfhd`
  default-base-is-moof, a version-1 `tfdt` and a `trun` giving every sample's duration,
  size, flags and, for video, composition offset) and its `mdat`. `Fmp4Writer` writes
  them to any `std::io::Write`. A file cut short keeps every complete fragment.
  - Video is track 1 (`avc1` with `avcC`, 90 kHz), audio is track 2, or track 1 without
    video (`mp4a` with an `esds` holding the `AudioSpecificConfig`, ticking at the
    sample rate). Timestamps are each packet's dts and pts rescaled to the track's time
    scale, rounded to nearest, from a configurable origin, so the tracks stay in sync
    with no edit list.
  - A video keyframe closes the fragment of everything before it: the earlier video and
    the audio that decodes before the keyframe, so every fragment's video starts with a
    keyframe. An audio-only stream cuts a fragment once a second of audio is buffered.
  - A sample's duration is the gap to the next sample of its track, so decode times must
    strictly increase and each track's newest sample waits for the next one. At
    `finish` it gets the duration of the sample before it (one AAC frame or 1/30 s if
    it is alone).
  - Without a configured `AvcDecoderConfig`, the init segment waits for the first video
    packet, which must be a keyframe carrying SPS and PPS; audio before it is buffered.
    The first video packet must be a keyframe in any case. Packets before the origin,
    composition offsets or durations beyond 32 bits, and non-increasing decode times are
    errors, and a rejected packet leaves the muxer unchanged.
  - Times and durations in `mvhd`, `tkhd` and `mdhd` are zero; players take the
    duration from the fragments. No `sidx` or `mfra` index is written.
- **`h264`:** Annex B ↔ AVCC conversion (3- and 4-byte start codes, 4-byte lengths), SPS
  and PPS extraction, and `AvcDecoderConfig`, the ISO/IEC 14496-15 record. For profiles
  other than Baseline, Main and Extended it appends the chroma format and bit depths read
  from the SPS, as FFmpeg does.
- **`hevc`:** NAL unit types and `HevcDecoderConfig`, the ISO/IEC 14496-15
  `HEVCDecoderConfigurationRecord`, built from VPS, SPS and PPS. Profile, tier, level,
  compatibility and constraint flags, chroma format, bit depths and temporal layering
  come from the first SPS. One array each holds the VPS, SPS and PPS, marked incomplete
  because frames keep their in-band copies; spatial segmentation, parallelism and frame
  rate are written as unknown (0). These match FFmpeg's FLV muxer byte for byte on the
  test stream.
- **`av1`:** OBU splitting (`av1::obus`), `av1::to_sample`, and `Av1CodecConfig`, the AV1
  ISOBMFF `AV1CodecConfigurationRecord`, with its profile, level, tier, bit depth,
  monochrome flag and chroma subsampling read from the sequence header OBU, which it
  stores as `configOBUs` (with a size field). No initial presentation delay is written.
- **`aac`:** `AudioSpecificConfig`, built from object type, rate and channels, or kept as
  the encoder produced it.
- **`amf0`:** the workspace's one AMF0 encoder and decoder (number, boolean, string
  and long string, object, null, undefined, ECMA and strict arrays, and date). This
  crate writes `onMetaData` with it, and `jstream-outputs`' RTMP client uses it for its
  commands and to decode server replies. The decoder treats input as untrusted: it
  limits nesting to `amf0::MAX_DEPTH` levels, never allocates from a count the
  input claims, and reports the byte offset of truncated input.
- **Errors:** one `thiserror` enum, `MuxError`.

**Not yet:** signalling enhanced RTMP in the RTMP `connect` (`fourCcList`, which is
`jstream-outputs`' job), HEVC and AV1 in MP4, non-fragmented MP4, and a mid-stream change
of parameter sets (a new sequence header or sample entry).
