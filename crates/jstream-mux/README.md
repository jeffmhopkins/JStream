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
depends_on = ["docs/ARCHITECTURE.md#crate-map", "docs/adr/0003-media-stack-and-codecs.md", "crates/jstream-media/README.md#purpose", "docs/adr/0034-youtube-interaction-scope-v2.md"]
verified = { hash = "b3:a4530e01c6551703", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Merged main (ADR-0035) into the ADR-0034 acceptance: mux/outputs depend on ADR-0034 only (the merge had re-added the superseded ADR-0009); other sections changed only by main's text and verified lines" }
-->

Layer-2 pure-Rust container muxers ([ADR-0003](../../docs/adr/0003-media-stack-and-codecs.md):
muxers are written in Rust, no FFmpeg). This first cut writes **FLV** for H.264 and AAC,
the container M1a's RTMP(S) output sends to YouTube
([ADR-0034](../../docs/adr/0034-youtube-interaction-scope-v2.md)). It consumes
[`jstream-media`](../jstream-media/README.md#purpose) `Packet`s, does no network I/O,
has no `unsafe`, and depends only on `jstream-media` and `thiserror`.

**Bitstream framing (decided here).** H.264 `Packet` payloads are **Annex B** (start
codes) throughout the engine, because that is what OS encoders such as Media Foundation
emit. A container that needs AVCC (length prefixes: FLV, RTMP, MP4) converts at mux time
with `h264::annexb_to_avcc`, which copies NAL units unchanged, so emulation-prevention
bytes are preserved. AAC payloads are **raw access units** without ADTS headers, which is
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
- **`h264`:** Annex B ↔ AVCC conversion (3- and 4-byte start codes, 4-byte lengths), SPS
  and PPS extraction, and `AvcDecoderConfig`, the ISO/IEC 14496-15 record. For profiles
  other than Baseline, Main and Extended it appends the chroma format and bit depths read
  from the SPS, as FFmpeg does.
- **`aac`:** `AudioSpecificConfig`, built from object type, rate and channels, or kept as
  the encoder produced it.
- **`amf0`:** the workspace's one AMF0 encoder and decoder (number, boolean, string
  and long string, object, null, undefined, ECMA and strict arrays, and date). This
  crate writes `onMetaData` with it, and `jstream-outputs`' RTMP client uses it for its
  commands and to decode server replies. The decoder treats input as untrusted: it
  limits nesting to `amf0::MAX_DEPTH` levels, never allocates from a count the
  input claims, and reports the byte offset of truncated input.
- **Errors:** one `thiserror` enum, `MuxError`.

**Not yet:** enhanced RTMP (HEVC and AV1 through FourCC video tags, which ADR-0034 calls
for), MP4/fMP4, and a mid-stream change of SPS/PPS (a new sequence header).
