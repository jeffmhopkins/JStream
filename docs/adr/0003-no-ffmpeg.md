# 0003: Rust media stack on OS/GPU APIs, no FFmpeg

- Status: Accepted
- Date: 2026-10-13

## Context
The project is "fully Rust". FFmpeg would give broad codec coverage but brings a large
C dependency, build complexity and LGPL/GPL concerns.

## Decision
All JStream code is Rust. Encoding/decoding uses OS and GPU-vendor APIs
(NVENC, AMF, QSV, Media Foundation, VideoToolbox, VAAPI). Muxers (FLV, MP4/fMP4) and
the RTMP client are written in Rust. No FFmpeg or bundled C/C++ media libraries.

## Consequences
- Clean permissive licensing and a smaller, simpler build.
- Codec support is limited to what the platform provides; a software H.264 fallback
  needs a separate decision (pure-Rust option or OS encoder).
- Media-file sources are more work (OS demux/decode APIs or Rust demuxers).
