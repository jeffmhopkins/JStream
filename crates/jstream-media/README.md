<!-- @doc
id = "CRATE-jstream-media"
title = "jstream-media"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-media

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-media/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map", "docs/adr/0031-source-types-and-decoding-scope.md", "docs/adr/0014-media-clock.md"]
verified = { hash = "b3:ac7ca3a1a249c6d9", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "depends_on and the Codec link moved from ADR-0003 to ADR-0031, whose restated media decision keeps the same baseline codecs (H.264, AAC via OS APIs); crate-map change is only the sources row" }
-->

Layer-0 media value types: plain data and the invariants that keep it consistent. No I/O,
no GPU handles, no platform code, no `unsafe`, and no dependencies on other JStream
crates (its only dependency is `thiserror`). Layering allows only strictly lower layers
([R-ARCH-002](../../docs/RULES.md#R-ARCH-002)), so this crate can't use
`jstream-types` (also layer 0); `Timebase` is its own type rather than a `Rational`.

Contents:
- **Time:** `Timebase` (a positive, reduced `num/den` seconds per tick, with constants for
  nanoseconds, microseconds, milliseconds and 90 kHz) and `MediaTime` (signed ticks in a
  time base). Media-clock timestamps ([ADR-0014](../../docs/adr/0014-media-clock.md)) are
  carried as `MediaTime`; `NANOS` is a convenient base for clock values, but the clock's
  unit is decided with spike S3 and `jstream-core`, not here. Arithmetic needs one
  time base and checks for overflow; `rescale` converts exactly, with explicit
  `Rounding`. Comparison is by value across time bases.
- **Pixels:** `PixelFormat` (NV12, P010, I420, BGRA8, RGBA8) with plane count, chroma
  subsampling, component size and `PlaneGeometry` per plane. Odd sizes round chroma up.
  `FrameSize` is non-zero.
- **Color:** `ColorInfo` (BT.601/709/2020, limited or full range), defaulting to BT.709
  limited, the engine's default output ([ARCH › Data model](../../docs/ARCHITECTURE.md#data-model)).
- **Video:** `VideoFrameInfo` (size, format, color, pts) and a CPU-backed `VideoFrame`
  whose plane count, strides and buffer lengths are checked against the format.
- **Audio:** `AudioFormat` (non-zero sample rate, `ChannelLayout`, `f32` interleaved or
  planar `SampleFormat`) and `AudioBuffer`, whose sample count is always whole frames.
- **Packets:** `Packet` (codec, keyframe flag, pts, dts no later than pts, non-empty
  shared payload). `Codec` covers the [ADR-0031](../../docs/adr/0031-source-types-and-decoding-scope.md)
  baseline: H.264 and AAC. Payload framing is a convention this crate doesn't check:
  H.264 payloads are Annex B (start codes, as OS encoders emit them) and AAC payloads are
  raw access units without ADTS. Muxers convert to the container's framing
  ([`jstream-mux`](../jstream-mux/README.md#purpose)).
- **Errors:** one `thiserror` enum, `MediaError`, whose variants carry the offending
  values ([R-RS-003](../../docs/RULES.md#R-RS-003)).
