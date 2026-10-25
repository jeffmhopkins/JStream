<!-- @doc
id = "ADR-0074"
title = "A too_large error for oversized requests"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0074: A too_large error for oversized requests

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0037](0037-kind-descriptors-for-transitions-and-plugins.md) §2 gives a CLAP filter's
saved state a `state` parameter, capped at 2 MiB, "so a settings map still fits in one
protocol message". As base64, 2 MiB is about 2.7 MiB of message text. The server reads
at most 4 MiB per message (`MAX_MESSAGE_BYTES` in `jstream-server`), and a WebSocket
message over that closes the connection. So one batch that carries two full states, for
example a copied plugin preset pasted onto two filters, drops the client's connection.
It gets no error, and every open live-value gesture of that client expires. Issue #102
(#101's review, N3) records this. The maintainer deferred it to M3, where CLAP filters
are built (roadmap CL2): "a maintainer-tier ADR superseding ADR-0037's `state` clause
adds a stable `too_large` error for requests over a soft limit, read below a higher hard
frame limit, so an oversized batch fails cleanly" ([ROADMAP › M3](../../ROADMAP.md#m3)).

The MCP endpoint has the same limit. It answers a body over 4 MiB with HTTP 413, which an
MCP client reports as a transport failure, not as a tool error.

This is maintainer tier ([R-GOV-005](../RULES.md#R-GOV-005)) for two reasons:
- **A public compatibility promise.** A stable error code and an advertised request
  limit are things clients build on, and they can't be taken back in a minor
  ([R-API-001](../RULES.md#R-API-001)).
- **Security posture.** Reading a message past today's 4 MiB, in order to answer it,
  raises how much memory one connection can make the server hold.

<a id="decision"></a>
## Decision
This amends ADR-0037 §2's `state` clause. The rest of ADR-0037 stands.

**1. Two limits.** The server has a **request limit** of 4 MiB of message text, which is
today's limit, and a **frame limit** of 16 MiB.
- A message at or under the request limit is handled as today.
- A message over the request limit but at or under the frame limit is read and refused
  with `too_large`. The connection stays open. Nothing in the message is applied.
- A message over the frame limit closes the connection, as a message over 4 MiB does
  today, with WebSocket close code 1009 (message too big).

**2. The `too_large` error.** `ProtocolError` gains `too_large { size_bytes, limit_bytes }`:
the message's size in bytes and the request limit. It answers the request's `id` when the
server can read one from the message (the same rule as `malformed`). The server reads
only the envelope's `id` from an oversized message. It never parses the call, so a
`too_large` message is never forwarded to the engine and never counts against the
connection's in-flight requests. Retrying the same message won't help. A client splits
the work: a batch of `state` writes goes one filter per batch.

**3. `welcome` advertises the request limit.** `Welcome` gains `max_request_bytes`, the
request limit, so a client can check a message before sending it. A client of an older
server reads it as missing and assumes 4 MiB.

**4. The `state` cap stays 2 MiB.** One state, about 2.7 MiB as base64, still fits under
the request limit with room for the rest of a batch. The engine checks the decoded size
of every `state` value against its descriptor's `max_bytes` (at most 2 MiB), as ADR-0037
already says. A batch that is under the request limit but holds a state over its cap
stays `invalid_argument` on that field. ADR-0037's "so a settings map still fits in one
protocol message" now reads: one `state` fits in one request, and a request that doesn't
fit is refused with `too_large`.

**5. MCP.** The MCP endpoint takes the same two limits. A body over the request limit
and at or under the frame limit is answered with a JSON-RPC error that carries the
`too_large` code and both sizes, so the agent sees a tool error it can act on. A body
over the frame limit, or one that declares a `Content-Length` over it, stays HTTP 413
and is refused before it is read.

**6. What the server sends is not capped here.** These are limits on what clients send.
A large `state.get` answer is bounded by the collection, not by this ADR. A client must
read server messages at least up to the frame limit; `jstream-control`'s WebSocket client
takes `tokio-tungstenite`'s default of 64 MiB.

<a id="consequences"></a>
## Consequences
- **An oversized batch fails cleanly.** The client keeps its connection, its gestures and
  its subscriptions, and is told how much was too much. A UI can say "too large to send
  in one step" instead of reconnecting.
- **More memory per connection, bounded.** A connection can make the server buffer up to
  16 MiB for one oversized message, against 4 MiB today. The message is dropped as soon
  as it is answered, so it is one message at a time per connection, apart from the in-flight
  requests, which stay bounded at 64 of up to 4 MiB each. With the server's connection
  cap, the worst case grows by 12 MiB per connection. Only a connection the server's
  gate has let in (its token or a paired device, when the server requires one) can send
  one.
- **Protocol follow-up:** one additive minor ([R-API-001](../RULES.md#R-API-001)), with
  schema snapshot updates ([R-TEST-002](../RULES.md#R-TEST-002)): the `too_large` code and
  `Welcome.max_request_bytes`. It goes through the serial protocol-bump queue in the
  earliest open wave ([runbook › Protocol versions](../runbooks/waves.md#protocol)).
- **Tests the build adds:** a message just over the request limit is refused with
  `too_large` and the request's `id`, and the next request on the same connection works;
  a message over the frame limit closes with 1009; a batch with two 2 MiB states is
  refused with `too_large` and changes nothing; and the MCP endpoint's JSON-RPC error and
  its 413.
- **Follow-on doc edits, made by the build PR:** the server README (*Sessions* and *Slow
  clients*, which say 4 MiB closes the connection, and the MCP table's 413 row), ARCH ›
  Control protocol, the protocol README's error list, and the ROADMAP's CL2 text.
- **Rejected alternatives:**
  - **A smaller `state` cap** (about 1.3 MiB, so two states always fit). It still breaks
    on three, every plugin preset over the cap would fail, and real plugins (sampler and
    convolution reverbs) save states of several MiB.
  - **Raise the one limit to 16 MiB and keep closing above it.** That moves the cliff
    but doesn't remove it, and lets every request hold four times the memory while in
    flight.
  - **Send `state` out of band** (a binary upload, then a reference in the settings).
    That is a new upload path with its own lifetime and cleanup, and ADR-0037 keeps one
    settings map and one undo path. It can come later if states grow past 2 MiB.
