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
depends_on = ["docs/ARCHITECTURE.md#crate-map", "docs/adr/0003-media-stack-and-codecs.md", "docs/adr/0014-media-clock.md"]
verified = { hash = "b3:6b9deb54fa50f76a", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Written with the crate and re-read against crates/jstream-media/src: Timebase/MediaTime, five PixelFormats, ColorInfo default BT.709 limited (ARCH#data-model), VideoFrame stride checks, AudioBuffer whole-frame invariant, Packet H.264/AAC per ADR-0003, NANOS clock per ADR-0014; no JStream deps per R-ARCH-002" }
-->

Layer-0 media value types: plain data and the invariants that keep it consistent. No I/O,
no GPU handles, no platform code, no `unsafe`, and no dependencies on other JStream
crates. Layering allows only strictly lower layers
([R-ARCH-002](../../docs/RULES.md#R-ARCH-002)), so this crate can't use
`jstream-types` (also layer 0); `Timebase` is its own type rather than a `Rational`.

Contents:
- **Time:** `Timebase` (a positive, reduced `num/den` seconds per tick, with constants for
  the nanosecond media clock of [ADR-0014](../../docs/adr/0014-media-clock.md), 90 kHz
  and milliseconds) and `MediaTime` (signed ticks in a time base). Arithmetic needs one
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
  shared payload). `Codec` covers the [ADR-0003](../../docs/adr/0003-media-stack-and-codecs.md)
  baseline: H.264 and AAC.
- **Errors:** one `MediaError` enum whose variants carry the offending values.
