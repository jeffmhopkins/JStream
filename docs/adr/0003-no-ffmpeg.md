---
id: ADR-0003
title: "Rust media stack on OS/GPU APIs, no FFmpeg"
kind: adr
status: accepted
decided: 2026-10-13
rev: 1
review_every: 180d
depends_on: []
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version, recorded from design Q&A with the maintainer"}
---

# 0003: Rust media stack on OS/GPU APIs, no FFmpeg


## Context
The project is "fully Rust". FFmpeg would give broad codec coverage but brings a large
C dependency, build complexity and LGPL/GPL concerns.

## Decision
All JStream code is Rust. Encoding/decoding uses OS and GPU-vendor APIs
(NVENC, AMF, QSV, Media Foundation, VideoToolbox, VAAPI). Muxers (FLV, MP4/fMP4) and
the RTMP client are written in Rust. No FFmpeg or bundled C/C++ media libraries
(enforced by [[RULES#R-DEP-002]]).

## Consequences
- Clean permissive licensing and a smaller, simpler build.
- Codec support is limited to what the platform provides; a software H.264 fallback
  needs a separate decision (pure-Rust option or OS encoder).
- Media-file sources are more work (OS demux/decode APIs or Rust demuxers).
