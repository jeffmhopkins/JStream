<!-- @doc
id = "CRATE-jstream-outputs"
title = "jstream-outputs"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-outputs

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-outputs/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map", "docs/ARCHITECTURE.md#threading", "docs/adr/0009-youtube-rtmps-and-api.md", "docs/adr/0003-media-stack-and-codecs.md"]
verified = { hash = "b3:5982d30573db8b4d", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "New section: checked each bullet against src/rtmp (handshake, chunk header choice and limits, control handling, command sequence and errors, media send, URL/key redaction, AMF0) and the blocking-I/O rationale against ARCH#threading, ADR-0003 and ADR-0009" }
-->

Layer-4 stream outputs ([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)). The
first piece is a pure-Rust **RTMP publish client** in the `rtmp` module, written in Rust
as [ADR-0003](../../docs/adr/0003-media-stack-and-codecs.md) requires and needed for the
generic RTMP output of [ADR-0009](../../docs/adr/0009-youtube-rtmps-and-api.md). The
`Output` trait, recording and the virtual-camera output come later. The only JStream
dependency is `jstream-media` (for `MediaTime` conversion); it has no third-party
dependencies besides `thiserror`.

**What the client does.** The RTMP layer only carries bytes: callers hand it
already-built FLV tag bodies and millisecond timestamps. Building those bodies (FLV
muxing, AVCC conversion, `onMetaData`) is `jstream-mux`'s job.

- **Handshake** (`handshake`): the simple, unsigned C0/C1/C2 ↔ S0/S1/S2 exchange. C1 has
  a zero time and version field and pseudo-random filler; C2 echoes S1; S0 must be
  version 3. S2 is read but not compared with C1, since servers differ in what they echo.
- **Chunk stream** (`chunk`): a pure, I/O-free encoder and decoder. The encoder picks the
  smallest header type (0–3) the previous message on the same chunk stream allows; it
  never follows a type-0 header with a type-3 one, whose implied delta peers read
  differently. Timestamps and deltas at or above `0xFFFFFF` go in the extended field,
  repeated on every continuation chunk. The decoder consumes only complete chunks, so a
  read may split input anywhere, and caps incoming messages at 1 MiB and partial data at
  8 MiB so a hostile server can't exhaust memory.
- **Protocol control** (`publisher`): the client sends Set Chunk Size (4096 by default)
  before `connect`; applies the server's Set Chunk Size and Abort; acknowledges each
  Window Acknowledgement Size worth of received bytes; answers Set Peer Bandwidth with
  its own Window Acknowledgement Size; and answers User Control ping requests.
- **Commands** (`publisher`, `amf0`): `connect` (`app`, `type`, `flashVer`, `tcUrl`), then
  `releaseStream`, `FCPublish`, `createStream`, and `publish(key, "live")` on the new
  stream. `_error` to `connect`, `createStream` or `publish`, or an `onStatus` of level
  `error`, becomes `RtmpError::Rejected` with the server's code (for example
  `NetStream.Publish.BadName`). Publishing has started once `NetStream.Publish.Start`
  arrives. `finish` sends `FCUnpublish` and `deleteStream`.
- **Media** (`publisher`): `send_metadata` prefixes `@setDataFrame` to an `onMetaData`
  script-tag body; `send_video` and `send_audio` send VIDEODATA and AUDIODATA tag bodies
  on their own chunk streams. `timestamp_ms` converts a `MediaTime`.
- **URLs and keys** (`url`): `rtmp(s)://host[:port]/app[/...]`, with the whole path as the
  application name and the stream key separate. `StreamKey` prints as `***`, and no error
  carries the key or the URL ([R-SEC-001](../../docs/RULES.md#R-SEC-001)).
- **AMF0** (`amf0`): a minimal, crate-private encoder and decoder for the command
  messages, with a nesting limit on decoding. **Unify it with `jstream-mux`'s AMF0
  encoder once both crates have landed.**

**Why blocking I/O over `Read + Write`.** [ARCH › Threads](../../docs/ARCHITECTURE.md#threading)
puts network outputs on the control plane, whose rules allow allocating and waiting.
A blocking client over a generic byte stream fits there without tying this crate to an
async runtime: the output runs on its own thread (a `std` thread, or tokio's
`spawn_blocking`), so a slow ingest server never stalls the control runtime, and the
same code runs over TCP, a future TLS stream, or the in-memory pipes the tests use. The
session needs only one thread: `poll_incoming` reads what the server has sent without
waiting, given a transport with a short read timeout. The `Output` layer will feed it from
the encoder fan-out through a bounded queue that drops and counts packets under
overload ([R-RT-004](../../docs/RULES.md#R-RT-004)); that queue is not part of this
client.

**Not yet:**
- **TLS (`rtmps://`).** `connect_tcp` refuses `rtmps://` with `RtmpError::TlsUnsupported`;
  `RtmpPublisher` already accepts any TLS stream that implements `Read + Write`. Every
  Rust TLS option compiles C or assembly and so needs a native-code allowlist entry in
  `xtask/src/lint_deps.rs`, which is a maintainer decision (see
  [Open questions](#open-questions)).
- **Reconnect with backoff**, planned for M1d ([ROADMAP](../../ROADMAP.md#m1)).
- Enhanced RTMP (HEVC/AV1), AMF3 payloads, and the signed ("complex") handshake.

**Tests** are hermetic ([R-TEST-004](../../docs/RULES.md#R-TEST-004)): exact handshake
and chunk bytes, chunking across chunk-size boundaries, extended timestamps, AMF0 round
trips, and full publish sessions against fake servers over in-memory pipes, which check
the order of every message the client sends.

<a id="open-questions"></a>
## Open questions
<!-- @section
-->

- **TLS:** rustls + `ring` vs rustls + `aws-lc-rs` vs Windows SChannel through the
  `windows` crate. Each needs a native-code allowlist entry or a platform-crate boundary,
  so the maintainer decides.
- **Threading:** whether [ARCH › Threads](../../docs/ARCHITECTURE.md#threading) should
  list "one blocking thread per network output" explicitly once the `Output` layer lands.
