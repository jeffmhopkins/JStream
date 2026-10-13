+++
id = "ADR-0003"
title = "Rust media stack on OS codecs, no FFmpeg"
kind = "adr"
status = "accepted"
+++

# ADR-0003: Rust media stack on OS codecs, no FFmpeg

<a id="context"></a>
## Context
The project is "fully Rust". FFmpeg would give broad codec coverage, but it brings a large C
dependency, build complexity and LGPL/GPL concerns. YouTube RTMP ingest needs H.264 (or
HEVC/AV1 via [enhanced RTMP](https://github.com/veovera/enhanced-rtmp)) and **AAC** audio.
There is no production-quality pure-Rust H.264 or AAC encoder. Windows and macOS ship both
encoders in the OS. Linux ships neither.

<a id="decision"></a>
## Decision
- All JStream code is Rust. Encoding and decoding use **OS and GPU-vendor APIs only**. No
  FFmpeg, GStreamer, or bundled C/C++ codec libraries (OpenH264, fdk-aac, x264). This is
  enforced by [R-DEP-002](../RULES.md#R-DEP-002).
- **Windows baseline (M1):** Media Foundation H.264 and AAC. Hardware encoding first goes
  through Media Foundation's hardware MFTs, which cover NVIDIA, AMD and Intel through one
  D3D11 path, with the software H.264 MFT as fallback. Direct NVENC/AMF are later
  optimizations, each needing measurements. QSV is reached only through Media Foundation
  (the oneVPL dispatcher is C++).
- **macOS baseline (M3):** VideoToolbox and AudioToolbox.
- **Linux:** a separate ADR before M3 decides the codec path (VAAPI for video; AAC is open).
  Until then Linux can't stream.
- Muxers (FLV, MP4/fMP4) and the RTMP(S) client are written in Rust.
- Non-media C dependencies (e.g. `ring` for TLS) are allowed through the lint-deps
  allowlist.

<a id="consequences"></a>
## Consequences
- Clean, permissive licensing and a simpler build.
- **Known gaps:** Windows N editions need the Media Feature Pack for AAC (the engine
  detects this and reports it), and Linux streaming is blocked on a future ADR.
- Media-file sources depend on OS demux and decode APIs.
