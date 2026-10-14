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
verified = { hash = "b3:6e1fe5ccd845a5e3", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Dependency ARCH#crate-map only gained ADR-0035's planned M3 rows (jstream-plugin-proto L0, jstream-plugin-clap L2 unsafe) and jstream-plugin-host on the apps row; this crate's own row and layer and the section's claims are unchanged; Dependency ARCH#threading only gained ADR-0035's planned out-of-process CLAP host paragraph; the RTMP client's threading is unaffected" }
-->

Layer-4 stream outputs ([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)). The
first piece is an **RTMP and RTMPS publish client** in the `rtmp` module, written in Rust
as [ADR-0003](../../docs/adr/0003-media-stack-and-codecs.md) requires and needed for the
generic RTMP/RTMPS output of [ADR-0009](../../docs/adr/0009-youtube-rtmps-and-api.md). The
`Output` trait, recording and the virtual-camera output come later. Its JStream
dependencies are `jstream-media` (for `MediaTime`) and the layer-2 `jstream-mux`, whose
public `amf0` codec and `flv::to_ms` conversion the client uses. Its third-party
dependencies are `thiserror`, and for TLS `rustls` (with the `ring` provider, per
[R-DEP-006](../../docs/RULES.md#R-DEP-006)) and `rustls-native-certs`. Tests also use
`rcgen` to generate certificates.

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
- **Commands** (`publisher`): `connect` (`app`, `type`, `flashVer`, `tcUrl`), then
  `releaseStream`, `FCPublish`, `createStream`, and `publish(key, "live")` on the new
  stream. `_error` to `connect`, `createStream` or `publish`, or an `onStatus` of level
  `error`, becomes `RtmpError::Rejected` with the server's code (for example
  `NetStream.Publish.BadName`). Publishing has started once `NetStream.Publish.Start`
  arrives. `finish` sends `FCUnpublish` and `deleteStream`.
- **Media** (`publisher`): `send_metadata` prefixes `@setDataFrame` to an `onMetaData`
  script-tag body, so it takes a `jstream-mux` tag built with `MetadataStyle::File`;
  `send_video` and `send_audio` send VIDEODATA and AUDIODATA tag bodies on their own
  chunk streams. `timestamp_ms` converts a `MediaTime` with `jstream_mux::flv::to_ms`
  (rounded to nearest, as the muxer's tag timestamps are), then rejects negative times
  and wraps at 2^32.
- **Transport** (`transport`): `connect` opens TCP (Nagle off) for `rtmp://` and TLS
  over TCP for `rtmps://`, returning a `Transport` (`Tcp` or `Tls`) that implements
  `Read + Write`. TLS is rustls on the `ring` provider, TLS 1.3 and 1.2, no client
  certificate. The server name (SNI) is the URL's host, and the certificate must match
  it and chain to a root in the operating system's trust store, which
  `rustls-native-certs` loads on each `connect`. The handshake completes inside
  `connect`, so certificate problems surface there as `RtmpError::Tls`; the `timeout`
  bounds each connection attempt and each handshake read and write. Timeouts on the
  returned transport are set on its TCP socket and apply through TLS: a read that times
  out in the middle of a TLS record keeps the part already read, and the next read
  resumes it. `Transport::shutdown` sends TLS `close_notify` before closing the socket.
- **URLs and keys** (`url`): `rtmp(s)://host[:port]/app[/...]`, with the whole path as the
  application name and the stream key separate. `StreamKey` prints as `***`, and no error
  carries the key or the URL ([R-SEC-001](../../docs/RULES.md#R-SEC-001)). The server's
  status code and description are untrusted text, so `RtmpError::Rejected` replaces any
  echo of the key in them with `***`.
- **AMF0:** commands are encoded and server replies decoded with `jstream-mux`'s
  `amf0::Amf0Value`, the same codec that writes FLV script tags. Its decoder treats
  server input as untrusted (nesting limit, no allocation from claimed counts). Its
  errors reach callers as `RtmpError::Amf0`, with the `MuxError` as the source.

**Why blocking I/O over `Read + Write`.** [ARCH › Threads](../../docs/ARCHITECTURE.md#threading)
puts network outputs on the control plane, whose rules allow allocating and waiting.
A blocking client over a generic byte stream fits there without tying this crate to an
async runtime: the output runs on its own thread (a `std` thread, or tokio's
`spawn_blocking`), so a slow ingest server never stalls the control runtime, and the
same code runs over TCP, TLS, or the in-memory pipes the tests use. The session needs
only one thread: `poll_incoming` reads what the server has sent without waiting, given a
transport with a short read timeout (over TLS too, see Transport above). The `Output`
layer will feed it from the encoder fan-out through a bounded queue that drops and
counts packets under overload ([R-RT-004](../../docs/RULES.md#R-RT-004)); that queue is
not part of this client.

**Why the OS trust store.** The usual compiled-in root set, `webpki-roots` (Mozilla's
CA list), is licensed CDLA-Permissive-2.0, which is not on the `deny.toml` allow list
([R-DEP-001](../../docs/RULES.md#R-DEP-001)). `rustls-native-certs` reads the platform's
store instead (the current user's `ROOT` store through `schannel` on Windows, the
Security framework on macOS, the OpenSSL-style bundle on Linux) and compiles no C. The
trade-off: on Windows the local `ROOT` store is filled on demand by Windows Update, so a
root no Windows program has needed yet may be missing (see
[Open questions](#open-questions)).

**Not yet:**
- **Reconnect with backoff**, planned for M1d ([ROADMAP](../../ROADMAP.md#m1)).
- Enhanced RTMP (HEVC/AV1), AMF3 payloads, and the signed ("complex") handshake.

**Tests** are hermetic ([R-TEST-004](../../docs/RULES.md#R-TEST-004)): exact handshake
and chunk bytes, chunking across chunk-size boundaries, extended timestamps, and full
publish sessions against fake servers over in-memory pipes, which check the order of
every message the client sends. One sends a `jstream-mux` `onMetaData` tag and checks
that exactly one `@setDataFrame` prefix reaches the wire. TLS tests run an in-process
rustls server on loopback TCP with a CA and server certificate that `rcgen` generates
for the test, and a client that trusts only that CA: a round trip over TLS 1.3 and 1.2,
a full publish session, a read timeout that splits a TLS record during `poll_incoming`,
a handshake timeout, and refusal of a certificate for another host or from an untrusted
CA. Nothing contacts a real ingest server or reads the OS trust store. AMF0 tests live
in `jstream-mux`.

<a id="open-questions"></a>
## Open questions
<!-- @section
-->

- **Root certificates:** the OS store through `rustls-native-certs` works without a
  license change, but on Windows it can miss a root that Windows has not fetched yet.
  Alternatives: `rustls-platform-verifier` (the OS verifier itself, which also fetches
  missing roots and checks revocation; more dependencies), or allowing
  CDLA-Permissive-2.0 so `webpki-roots` can be compiled in (an R-DEP-001 change, the
  maintainer's call).
- **Threading:** whether [ARCH › Threads](../../docs/ARCHITECTURE.md#threading) should
  list "one blocking thread per network output" explicitly once the `Output` layer lands.
