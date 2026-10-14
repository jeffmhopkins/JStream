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
depends_on = ["docs/ARCHITECTURE.md#crate-map"]
verified = { hash = "b3:4a3e1d725d432dc1", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Crate-map server row now says it serves MCP's streamable HTTP on the control port (not built); edited 'Not here yet' to say that transport is decided to be served here; Layer 4 and contents unchanged" }
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
  session and back.
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
verified = { hash = "b3:581e2c8d46641a75", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Edited per #44 N3 and #37 N1: open questions 3, 4 and 7 now record the decisions (MCP HTTP here behind the gate; missing Origin allowed with token, present Origin checked; bounded buffer with events.dropped and state re-read), each marked not built; admission text says missing Origin is refused today and will change; code unchanged" }
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
| `Origin` | present, and `http://` plus the accepted `Host` (the server's own origin), or a configured extra | 403 |
| `Authorization` | `Bearer <token>`, compared in time that reveals neither the token nor its length | 401 with `WWW-Authenticate: Bearer` |

Each header must appear exactly once, and comparisons ignore ASCII case (except the
token). The `Host` check defeats DNS rebinding: a page on an attacker's domain that
resolves to `127.0.0.1` still sends its own name. The `Origin` check stops foreign
pages, since browsers always send `Origin` on a WebSocket upgrade; native clients send
the server's own. A request without `Origin` is refused too today, so every client must
send one. That is decided to change (open question 4): a missing `Origin` will be
accepted, because the token is the gate, and an `Origin` that is present must still pass
this check. The token is never logged, and `Token`'s `Debug` form hides it.

**`Host` without a port.** A `Host` (or `Origin`) without a port names the scheme's
default port, 80 for `http` and `ws`, and browsers leave `:80` out. So when the server
listens on port 80, `localhost` is the same as `localhost:80` and is accepted (and
`http://localhost` is its own origin). On any other port, the default and ephemeral
ones included, a `Host` without a port names a different port and is refused.

**Extra hosts.** Each configured extra host must be `name:port` with a port from 1 to
65535. An entry without a port could never match, since the gate compares hosts with
their port, so `Server::bind` refuses it (`ServerError::InvalidHost`) before opening
the socket rather than ignoring it.

**Token compare.** `Token` keeps only the BLAKE3 digest of the token. The presented
value is hashed to the same fixed 32 bytes and the digests are compared with
`blake3::Hash`'s constant-time equality, so the time taken depends only on the
presented value's length: neither the first difference nor the token's length shows.

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
- The connection ends when the client closes it, when the session's event stream ends
  (the engine went away; close code 1001), when the session reports `Disconnected`
  (close code 1011), or when the server shuts down.

`serve_with_shutdown(signal)` stops accepting when `signal` completes and closes every
open connection (close code 1001), so their clients' pending and later calls fail with
`Disconnected` and their event streams end.

### Dependencies

axum (features `http1`, `tokio`, `ws`, no defaults) for HTTP and the upgrade, which
brings hyper, tower and `tokio-tungstenite` without TLS; tokio for the listener and the
connection tasks; `futures-util` to split the socket and run calls concurrently
(`FuturesUnordered`); `blake3` (the workspace's `pure` build, already used by `xtask`)
for the token digest. No TLS crate is in the tree, so no `ring` or `rustls`.

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
4. **Requests without `Origin`.** Decided, not built yet: an upgrade with no `Origin`
   header is allowed, because the token is the gate, so non-browser tools (`websocat`,
   scripts) needn't invent one. An `Origin` that is present must still be the server's
   own or a configured extra. Browsers always send `Origin`, so this doesn't open the
   server to foreign pages. Today a missing `Origin` is still refused.
5. **Default port and token file.** The host apps choose the port and create the
   user-only token file ([ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence)),
   so the server takes both from its caller. `jstream-daemon` creates or reuses the
   token file at a path it is given and listens on a free port by default; a default
   port and file location are its open questions
   ([jstream-daemon › Design](../../apps/jstream-daemon/README.md#design)).
6. **Logging.** [R-RS-005](../../docs/RULES.md#R-RS-005) asks for `tracing` spans around
   sessions. The first cut logs nothing; adding `tracing` is a new dependency for a
   later PR.
7. **Backpressure.** Decided, not built yet: each connection gets a bounded event
   buffer. When it overflows, the server drops the queued events and sends one
   `events.dropped` notice, and the client re-reads state (`state.get` carries `rev`).
   The engine is never slowed down by one client. Today events and answers are written
   as they come, and a client that stops reading makes the connection task wait on the
   socket, which holds that session's events in its unbounded buffer
   ([jstream-control open question 1](../jstream-control/README.md#design)). The buffer
   size and the notice's fields are settled when it is built.
