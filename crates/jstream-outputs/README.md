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
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-outputs", "docs/ARCHITECTURE.md#threading", "docs/adr/0034-youtube-interaction-scope-v2.md", "docs/adr/0031-source-types-and-decoding-scope.md"]
verified = { hash = "b3:ae584ed009d49f3a", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Stale via ARCH#threading's handover paragraph; the RTMP client reads no snapshots or live values, so the stopped-marker and fence rule doesn't apply" }
-->

Layer-4 stream outputs ([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)). The
first piece is an **RTMP and RTMPS publish client** in the `rtmp` module, written in Rust
as [ADR-0031](../../docs/adr/0031-source-types-and-decoding-scope.md) requires and needed for the
generic RTMP/RTMPS output of [ADR-0034](../../docs/adr/0034-youtube-interaction-scope-v2.md). The
`Output` trait, recording and the virtual-camera output come later. Its JStream
dependencies are `jstream-media` (for `MediaTime`) and the layer-2 `jstream-mux`, whose
public `amf0` codec and `flv::to_ms` conversion the client uses. Its third-party
dependencies are `thiserror`, `tracing` for logging
([R-RS-005](../../docs/RULES.md#R-RS-005)), and for TLS `rustls` (with the `ring` provider, per
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
  `rustls-native-certs` loads on each `connect`. Store files it can't read and
  certificates that don't parse are skipped, but logged at `warn` (each load error, and
  the count of unparsable certificates), because an incomplete store otherwise shows up
  only as an `UnknownIssuer` handshake failure; no usable root at all is
  `RtmpError::NoRootCertificates`. The handshake completes inside
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
- **Reconnect** (`reconnect`): `ReconnectingPublisher` wraps a published session and a
  `Connector`, the function that opens and publishes a replacement (`rtmp_connector`
  for TCP and TLS, built on `connect_publisher`). A send or `poll` that fails because
  the link is gone (a write that fails or times out, a read error, the server
  closing, an error status) drops the session and starts an outage; errors in the caller's input
  (`PayloadSize`, timestamps) are returned and leave the link alone. Attempts follow
  `ReconnectPolicy`: the wait before attempt *n* is `min(1 s × 2ⁿ, 30 s)` scaled by a
  random factor in [0.5, 1] ("equal jitter", so outputs that lost the same ingest
  don't retry in step, while each wait keeps a floor). It gives up when the next
  attempt would start more than 5 minutes after the loss (or after `max_attempts`, no
  limit by default). Five minutes rides out a router restart or a network change, and
  still tells the operator within minutes that the stream is down rather than
  retrying silently; with the 30 s cap, a recovered network is used again within
  30 s. A session that drops again within 30 s of reconnecting continues the same
  outage (attempt count, backoff step, start time), so a server that accepts and
  then drops at once can't keep it retrying forever at the shortest wait. Each
  attempt runs on its own short-lived thread, one at a time, and every call only
  checks whether one has finished or is due, so the caller's thread never waits for
  a reconnect. While the link is down, media is dropped and counted (`ReconnectStats`),
  never queued ([R-RT-004](../../docs/RULES.md#R-RT-004)). The last `onMetaData`, AVC
  sequence header and AAC sequence header are kept as they pass through. After a
  reconnect, video and audio are dropped (and counted) until the next video keyframe;
  `onMetaData` and the AVC sequence header go out just before it, and the AAC
  sequence header just before the next audio packet, each at that packet's
  timestamp. An audio-only stream resumes at once. Timestamps continue from the lost
  session. `state()` is `Active`, `Reconnecting` or `Failed`, which a host maps onto
  its output states; only giving up is an error (`RtmpError::ReconnectGaveUp`). The
  backoff reads time through an injectable `Clock`. `connect_publisher` reads with
  the connect timeout until publishing has started and only then shortens the read
  timeout for polling, so the publish replies aren't cut off by the polling timeout.
  Logs carry only `RtmpError` text, never the key ([R-SEC-001](../../docs/RULES.md#R-SEC-001)).
  `jstream-core`'s `RtmpPublish` wraps it, with `rtmp_connector` and the default
  policy, so a live output goes `reconnecting` on a lost connection and fails only
  when it gives up.
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
transport with a short read timeout (over TLS too, see Transport above). Only a
reconnect attempt, which blocks for up to its connect timeouts, runs on a thread of
its own, so the output thread keeps dropping and counting media meanwhile. The `Output`
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

**Not yet:** enhanced RTMP (HEVC/AV1), AMF3 payloads, and the signed ("complex")
handshake.

**Tests** are hermetic ([R-TEST-004](../../docs/RULES.md#R-TEST-004)): exact handshake
and chunk bytes, chunking across chunk-size boundaries, extended timestamps, and full
publish sessions against fake servers over in-memory pipes, which check the order of
every message the client sends. One sends a `jstream-mux` `onMetaData` tag and checks
that exactly one `@setDataFrame` prefix reaches the wire. TLS tests run an in-process
rustls server on loopback TCP with a CA and server certificate that `rcgen` generates
for the test, and a client that trusts only that CA: a round trip over TLS 1.3 and 1.2,
a full publish session, a read timeout that splits a TLS record during `poll_incoming`,
a handshake timeout, and refusal of a certificate for another host or from an untrusted
CA. Reconnect tests drive a clock the test advances by hand: a fake server on loopback
TCP that hangs up mid-stream, after which the test checks the wait before the first
attempt, the drop counts, the re-sent headers and the resume at the keyframe on the
second connection, and the state sequence; a write that fails over an in-memory pipe
(the failed packet counted, audio-only resuming at once); giving up after
`max_attempts` and at the outage limit; a session that drops again at once
continuing its outage, and one that stayed up starting a new one. Nothing contacts a
real ingest server or reads the OS trust store. AMF0 tests live
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
- **Reconnect timestamps:** a resumed session continues the old timestamps rather
  than starting again near 0. Whether YouTube (and other ingests) accept that on a
  new connection has not been checked against a live ingest; rebasing would need the
  offset applied to every later tag.
- **Retrying rejections:** every failure is retried, including a `publish` rejection
  on reconnect (for example a key revoked meanwhile), which then fails only at the
  outage limit. A server that rejects a reconnect while the previous session still
  holds the key is the reason not to fail fast; which codes are final is not known.
- **Resume latency:** after a reconnect, video waits for the encoder's next keyframe
  (up to one keyframe interval). A keyframe request would shorten that; see
  [jstream-encode › Open questions](../jstream-encode/README.md#open-questions).
- **Threading:** whether [ARCH › Threads](../../docs/ARCHITECTURE.md#threading) should
  list "one blocking thread per network output" explicitly once the `Output` layer lands.
