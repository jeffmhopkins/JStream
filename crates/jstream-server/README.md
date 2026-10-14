<!-- @doc
id = "CRATE-jstream-server"
title = "jstream-server"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-server

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-server/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-server"]
verified = { hash = "b3:b38edc44a2c6346e", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Edited Session contents line to mention the bounded outbox in session.rs; lib.rs, config.rs, gate.rs unchanged, so the rest of the list holds" }
-->

The server: it puts the control API on an authenticated localhost WebSocket, so remote
clients (the CLI, the web UI, a stdio MCP proxy) reach the engine through
`jstream-control`'s WebSocket client. It is written against `dyn Control` and
`jstream-protocol` only, never the engine
([R-ARCH-003](../../docs/RULES.md#R-ARCH-003)): the host app hands it a `Connector`
that opens one session per connection. Layer 4
([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)).

Contents (a first cut; [Design](#design) explains the choices):
- **Server** (`lib.rs`): `Server::bind`, `serve` and `serve_with_shutdown`, the
  `Connector` type, and the `GET /api/v1/ws` upgrade handler.
- **Configuration** (`config.rs`): `ServerConfig` (bind address, extra hosts and
  origins) and `Token`, the control token.
- **Gate** (`gate.rs`): the `Host`, `Origin` and token checks every upgrade passes
  ([R-SEC-002](../../docs/RULES.md#R-SEC-002)).
- **Session** (`session.rs`): one connection, from the protocol envelope to one `Control`
  session and back, with the bounded outbox that keeps a slow client from holding up
  its events.
- **Errors** (`error.rs`): `ServerError`, a `thiserror` enum.

Not here yet: serving the UI assets, the MCP streamable-HTTP transport (decided: served
here on the control port) and TLS ([Open questions](#design)).

Why a crate and not a module of `jstream-control` or `jstream-mcp`: it owns the HTTP
stack (axum, hyper, tokio's network runtime), which neither the WASM-safe
`jstream-control` nor the runtime-free `jstream-mcp` may carry, and it sits above both
(it will mount MCP), so it is its own layer-4 crate, as the crate map plans.

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-server/src/**", "crates/jstream-server/Cargo.toml"]
depends_on = ["docs/ARCHITECTURE.md#protocol", "docs/adr/0006-mcp-full-access.md", "docs/RULES.md#R-SEC-002", "crates/jstream-control/README.md#design"]
verified = { hash = "b3:a7fc6a1d7a48cfd4", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Edited: new Slow clients bullet (outbox + writer future, EVENT_BUFFER 1024 drop-all with one coalesced events.dropped, replies kept in order, REPLY_BACKLOG stops reading requests), close sends queued messages before the frame, warn log of the drop count, futures-util use, OQ7 now built with tuning open; admission and missing-Origin text untouched" }
-->

### Listening and admission

`ServerConfig::new(token)` listens on `127.0.0.1` with an ephemeral port; any other
address is the host's explicit choice ([ADR-0006](../../docs/adr/0006-mcp-full-access.md)).
The one route is `GET /api/v1/ws` (`jstream_control::WS_PATH`, shared with the client).
Every upgrade request passes three checks, in this order, before a session is opened
([R-SEC-002](../../docs/RULES.md#R-SEC-002)):

| Check | Accepted | Refused with |
|---|---|---|
| `Host` | `127.0.0.1`, `localhost` or `[::1]` with the bound port; the bound address; or a configured extra; without a port only on port 80 (below) | 403 |
| `Origin` | absent, or `http://` plus the accepted `Host` (the server's own origin), or a configured extra | 403 |
| `Authorization` | `Bearer <token>`, matched by comparing BLAKE3 digests (below) | 401 with `WWW-Authenticate: Bearer` |

`Host` and `Authorization` must appear exactly once and `Origin` at most once, and
comparisons ignore ASCII case (except the token). The `Host` check defeats DNS
rebinding: a page on an attacker's domain that resolves to `127.0.0.1` still sends its
own name. The `Origin` check stops foreign pages, since browsers always send `Origin` on
a WebSocket upgrade, and an opaque `null` origin is refused like any other foreign one.
A request without `Origin` is a native client (the CLI, `websocat`, a script) and goes
on to the token check, which is the gate (open question 4). The token is never logged (see *Logging*), and `Token`'s `Debug` form hides it.

**`Host` without a port.** A `Host` (or `Origin`) without a port names the scheme's
default port, 80 for `http` and `ws`, and browsers leave `:80` out. So when the server
listens on port 80, `localhost` is the same as `localhost:80` and is accepted (and
`http://localhost` is its own origin). On any other port, the default and ephemeral
ones included, a `Host` without a port names a different port and is refused.

**Extra hosts.** Each configured extra host must be `name:port` with one port from 1
to 65535, and an IPv6 address must be in brackets (`[::1]:4455`), as it is in a
request's `Host`. Any other entry (no port, two ports as in `name:80:80`, or a bare
`fe80::1`) could never match, since the gate compares hosts with their port, so
`Server::bind` refuses it (`ServerError::InvalidHost`) before opening the socket rather
than ignoring it.

**Token compare.** `Token` keeps only the BLAKE3 digest of the token. The presented
value is hashed with BLAKE3 too, and the two 32-byte digests are compared with
`blake3::Hash`'s constant-time equality. So the compare reveals nothing about the
token: not where the two differ, and not its length.

### Sessions

Each accepted connection gets a fresh session from the `Connector`
(`Arc<dyn Fn() -> Box<dyn Control>>`), dropped when the connection ends. The connection
task speaks the envelope
([jstream-protocol › Design](../jstream-protocol/README.md#design)):

- `hello` is forwarded to `Control::hello`. On success the task subscribes to the
  session's events and answers `welcome`; from then on events are forwarded. A refused
  first handshake (`unsupported_version`) is answered and the connection closed (close
  code 1008), as the envelope says.
- Each `request` is forwarded to `Control::call` and may stay in flight while others
  run; answers go back as they complete, carrying the client's request ID. A request
  before `hello` is answered by the session itself (`handshake_required`).
- A message that doesn't parse gets a `malformed` error, with the request ID when one is
  readable. Binary frames get `malformed` too, until binary streams are designed.
  Messages over 4 MiB close the connection.
- **Request IDs.** A `Control` session assigns its own IDs, which appear in the
  `state.changed` events its calls cause. The task maps them back to the client's wire
  IDs, so a client matches its event to its response by the ID it chose, as in the
  protocol README's example. An event can arrive before the answer that reveals the
  mapping, so while calls are in flight it is held back, with every event after it to
  keep the order, until the answer arrives or nothing is left in flight (or 1024 events
  are held). The last 1024 pairs are remembered. Other clients' `state.changed` events
  pass unchanged: their IDs mean nothing to this client.
- **Slow clients.** The connection task never waits on the socket: what it sends goes
  into an outbox, and a writer that owns the socket's sending half sends it, one message
  at a time, while the task keeps reading the client and draining the session's events.
  So a client that reads slowly leaves nothing piling up in the engine's subscription
  queue, and never slows the engine or other clients. The outbox holds at most 1024
  events (`EVENT_BUFFER`): about 100 KB of today's events, 17 seconds of a 60 Hz live
  drag. When one more arrives, every queued event is dropped, one `events.dropped
  {count}` notice is queued at the end, and the new event after it; while that notice
  is still unsent, later drops add to its count instead of queuing another. Replies
  (`welcome`, `response`, `error`) are never dropped, and what remains keeps its order,
  so a `state.changed` that survives still follows its call's response. The client
  re-reads the state with `state.get`. Each drop is logged at `warn` with its count
  (see *Logging*). Replies are bounded instead by reading: while 1024 of them wait
  unsent (`REPLY_BACKLOG`), the task reads no more requests, so a client that sends
  requests without reading the answers is held back by the socket.
- The connection ends when the client closes it, when the session's event stream ends
  (the engine went away; close code 1001), when the session reports `Disconnected`
  (close code 1011), or when the server shuts down.

`serve_with_shutdown(signal)` stops accepting when `signal` completes and closes every
open connection (close code 1001), so their clients' pending and later calls fail with
`Disconnected` and their event streams end. Whenever the server closes a connection,
what its outbox still holds is sent before the close frame.

### Logging

The server logs through `tracing` ([R-RS-005](../../docs/RULES.md#R-RS-005)) and
installs no subscriber; the host app does. The listener records each connection's peer
address (axum's `ConnectInfo`), for logging only: admission depends on the headers
alone.

- **Refused upgrades** are logged at `warn` as `connection refused`, with the peer
  address and the reason (`Host not allowed`, `Origin not allowed`, or `missing or wrong
  control token`). No header value is logged: not the token, nor the refused `Host` or
  `Origin` ([R-SEC-001](../../docs/RULES.md#R-SEC-001)). The test
  `gate::tests::refusals_are_logged_without_the_token` checks this with a capturing
  subscriber, and `jstream-daemon`'s end-to-end test checks its stderr.
- **Accepted connections** get an `info` span `connection` with a number (counting from
  1 per server) and the peer address, and log `connection accepted`. The session the
  `Connector` opens is created inside that span, so the engine's own `session` and
  `command` spans nest under it. The connection task logs the handshake's outcome
  (`client_id`, or the refusal's code) and how the connection ended, at `info`, or
  `warn` when the engine failed (close code 1011). A slow client's dropped events are
  logged at `warn` (`client reads too slowly: events dropped`) with their count. Message
  contents are never logged.
- A request that isn't a WebSocket upgrade is logged at `debug`.

### Dependencies

axum (features `http1`, `tokio`, `ws`, no defaults) for HTTP and the upgrade, which
brings hyper, tower and `tokio-tungstenite` without TLS; tokio for the listener and the
connection tasks; `futures-util` to split the socket, run calls concurrently
(`FuturesUnordered`) and box the writer's send in progress; `blake3` (the workspace's `pure` build, already used by `xtask`)
for the token digest; `tracing` for the spans and events above. No TLS crate is in the tree, so no `ring` or `rustls`.

### Open questions

1. **TLS.** Plain `ws://` on loopback is the first cut. Remote browsers need `wss://`
   with rustls and the ring provider ([R-DEP-006](../../docs/RULES.md#R-DEP-006)): a TLS
   feature and its wiring. `ring` is already on the lint-deps native-code allowlist, for
   `jstream-outputs`' RTMPS, so that needs no further governance change.
2. **Browser token.** Browsers can't set `Authorization` on a WebSocket. The client
   design sends the token in `Sec-WebSocket-Protocol` instead
   ([jstream-control › Design](../jstream-control/README.md#design)); the subprotocol
   naming is not decided, so the server accepts only `Authorization` today.
3. **UI assets and MCP over HTTP.** Serving the web UI is deferred. Decided for MCP:
   this server serves MCP's streamable-HTTP transport (`jstream_mcp::Server::handle`) on
   the control port, behind the same token, `Origin` and `Host` checks, and stdio stays
   in the CLI. Neither is built yet.
4. **Requests without `Origin`.** Settled and built (#44): an upgrade with no `Origin`
   header is allowed, because the token is the gate, so non-browser tools (`websocat`,
   scripts) needn't invent one. An `Origin` that is present must still be the server's
   own or a configured extra. Browsers always send `Origin`, so this doesn't open the
   server to foreign pages.
5. **Default port and token file.** The host apps choose the port and create the
   user-only token file ([ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence)),
   so the server takes both from its caller. `jstream-daemon` creates or reuses the
   token file at a path it is given and listens on a free port by default; a default
   port and file location are its open questions
   ([jstream-daemon › Design](../../apps/jstream-daemon/README.md#design)).
6. **Logging volume.** Every refused upgrade is logged at `warn` (see *Logging*). A
   foreign page that keeps retrying can fill the log; whether refusals need rate
   limiting or a summary line is open.
7. **Backpressure.** Settled and built (see *Sessions*): each connection has a bounded
   event buffer, and on overflow the server drops the queued events and sends one
   `events.dropped` notice, so the engine is never slowed down by one client. What is
   left open is tuning: whether 1024 events suits the high-rate events still to come
   (`stats.tick`, meters), or wants a byte limit or per-event-kind limits once they exist.
