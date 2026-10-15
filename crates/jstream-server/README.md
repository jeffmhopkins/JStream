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
verified = { hash = "b3:703e24847a9e8fc2", by = "agent:claude@session_01WbL3WRbcjCtYjwjoicNdxV", note = "U2: re-read; adds the Web UI (ui.rs) bullet, config lists the UI directory, gate covers every request; not-yet now TLS and pairing" }
-->

The server: it puts the control API on an authenticated localhost WebSocket, so remote
clients (the CLI, the web UI, a stdio MCP proxy) reach the engine through
`jstream-control`'s WebSocket client, and, when the host turns it on, serves MCP's
streamable-HTTP transport on the same port, so agents reach it directly. When the host
names a UI directory, it serves the web UI's files on that port too. It is written against `dyn Control` and
`jstream-protocol` only, never the engine
([R-ARCH-003](../../docs/RULES.md#R-ARCH-003)): the host app hands it a `Connector`
that opens one session per connection. Layer 4
([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)).

Contents (a first cut; [Design](#design) explains the choices):
- **Server** (`lib.rs`): `Server::bind`, `serve` and `serve_with_shutdown`, the
  `Connector` type, and the `GET /api/v1/ws` upgrade handler.
- **MCP** (`mcp.rs`): the streamable-HTTP transport at `/mcp` (`MCP_PATH`), its
  sessions, and their limits.
- **Web UI** (`ui.rs`): the UI bundle's files from `ServerConfig::ui_dir`, with their
  types and a Content Security Policy.
- **Configuration** (`config.rs`): `ServerConfig` (bind address, extra hosts and
  origins, connection limits, the MCP switch and its limits, the UI directory) and
  `Token`, the control token.
- **Listener** (`listener.rs`): accepts TCP connections and drops one that hasn't sent
  an accepted upgrade request in time.
- **Gate** (`gate.rs`): the `Host`, `Origin` and token checks every request passes
  ([R-SEC-002](../../docs/RULES.md#R-SEC-002)).
- **Session** (`session.rs`): one connection, from the protocol envelope to one `Control`
  session and back, with the bounded outbox that keeps a slow client from holding up
  its events.
- **Errors** (`error.rs`): `ServerError`, a `thiserror` enum.

Not here yet: TLS and pairing ([Open questions](#design)).

Why a crate and not a module of `jstream-control` or `jstream-mcp`: it owns the HTTP
stack (axum, hyper, tokio's network runtime), which neither the WASM-safe
`jstream-control` nor the runtime-free `jstream-mcp` may carry, and it sits above both
(it mounts MCP), so it is its own layer-4 crate, as the crate map plans.

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-server/src/**", "crates/jstream-server/Cargo.toml"]
depends_on = ["docs/ARCHITECTURE.md#protocol", "docs/adr/0006-mcp-full-access.md", "docs/RULES.md#R-SEC-002", "crates/jstream-control/README.md#design"]
verified = { hash = "b3:67095634caee2ace", by = "agent:claude@session_01WbL3WRbcjCtYjwjoicNdxV", note = "U2: re-read against ui.rs and gate.rs: Basic credentials row and paragraph, new Web UI section (paths, table, headers, CSP and trunk nonce), logging lines, open questions 2 and 3 settled" }
-->

### Listening and admission

`ServerConfig::new(token)` listens on `127.0.0.1` with an ephemeral port; any other
address is the host's explicit choice ([ADR-0006](../../docs/adr/0006-mcp-full-access.md)).
The API's route is `GET /api/v1/ws` (`jstream_control::WS_PATH`, shared with the
client), with `/mcp` and the UI's files when the host turns them on. Every request, an
upgrade or not, passes three checks, in this order, before a session is opened or a file
served
([R-SEC-002](../../docs/RULES.md#R-SEC-002)):

| Check | Accepted | Refused with |
|---|---|---|
| `Host` | `127.0.0.1`, `localhost` or `[::1]` with the bound port; the bound address; or a configured extra; without a port only on port 80 (below) | 403 |
| `Origin` | absent, or `http://` plus the accepted `Host` (the server's own origin), or a configured extra | 403 |
| `Authorization` | `Bearer <token>`, or `Basic` credentials whose password is the token (any user name), matched by comparing BLAKE3 digests (below) | 401 with `WWW-Authenticate: Bearer` (`Basic` on the UI's paths) |

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

**`Basic` credentials.** A browser can't put a bearer token on a page load or a
WebSocket upgrade, but it sends the HTTP `Basic` credentials it was given for an
origin with every request to it, upgrades included (checked in Chromium). So the gate
also accepts `Authorization: Basic base64(user:token)`: the password is the token and
the user name is ignored. Credentials without a `:`, or that aren't base64 with
padding, are refused like a wrong token. This is the same token presented differently,
not a new kind of client (ADR-0006).

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

**Connection limits.** Three bounds keep one client, or a crowd of them, from holding
the server's resources. Each is a `ServerConfig` field, and `ServerConfig::new` sets
the default:

| Limit | Default | When it is exceeded |
|---|---|---|
| `max_connections` | 32 open WebSocket connections | An upgrade that passes the checks above gets 503 and no session. The slot is taken just before the upgrade and freed when the connection ends. |
| `header_read_timeout` | 10 s from accept to an accepted upgrade request | The connection is dropped, unanswered. |
| `handshake_timeout` | 10 s from accept to a successful `hello` | The connection is closed with close code 1008 (see *Sessions*). |

The header-read timeout is kept on the socket, by the listener (`listener.rs`), since
hyper's own isn't reachable through `axum::serve`: until the upgrade handler accepts a
request, every read and write fails once the deadline has passed. So it also drops a
connection that sends no request, asks for another path, or stays open after a refusal.
The cap is checked after the `Host`, `Origin` and token checks, so a refused client
learns nothing about the server's load, and a refusal costs no session. The listener
stays on loopback by default; these bounds are what a non-loopback bind would rely on
against a misbehaving client on the network.

### The web UI

With `ServerConfig::ui_dir` set (`ServerConfig::new` leaves it unset; `Server::bind`
refuses a path that isn't a directory with `ServerError::UiDir`), every path the API
doesn't use (anything but `/api/v1/ws`, `/mcp` and other paths under `/api` or `/mcp`)
serves a file from that directory: the UI's built bundle, `trunk build`'s `dist`
(`Server::ui_url` gives the address). Each request passes the gate above first,
so the bundle is served only to holders of the token
([ADR-0006](../../docs/adr/0006-mcp-full-access.md): every listener, the web UI
included, requires it). A refused token is answered with `WWW-Authenticate: Basic
realm="JStream"`, so the browser asks for a user name and password: the user pastes the
token from the token file as the password. The browser then sends it with the bundle's
requests and the UI's WebSocket upgrade, and the UI never handles the token. A request
that passes disarms the header-read deadline, as on `/mcp`, so the bundle loads over a
kept-alive connection.

| Request | Answer |
|---|---|
| `GET` or `HEAD` of a file in the directory | 200, the file (at most 64 MiB) |
| A directory (`/` included) | its `index.html` |
| A missing path whose last segment has no `.` (the UI's own routes) | the root's `index.html` |
| Any other missing path, or one under `/api` or `/mcp` | 404 |
| A path with an empty, `.` or `..` segment, or one that holds `\`, `:` or NUL (percent-escapes decoded first) | 404 |
| A file that resolves outside the directory (a symbolic link) | 404 |
| Any other method | 405 with `Allow: GET, HEAD` |

**Paths.** The path is percent-decoded segment by segment, and a segment that could step
out of the directory or name a drive is refused before the file system is touched. The
file found is then canonicalized and must still lie inside the canonicalized directory,
which also stops a symbolic link that points out of it.

**Headers.** Every answer from the directory carries `Content-Type` from the file's
extension (`application/wasm` for `.wasm`, which browsers need to compile a module as
it streams; `application/octet-stream` for an unknown one), `X-Content-Type-Options:
nosniff`, `Cache-Control: no-cache` (the bundle changes with every build, and loopback
is cheap), `Referrer-Policy: no-referrer`, and this Content Security Policy:

```text
default-src 'self'; script-src 'self' 'wasm-unsafe-eval' 'nonce-N'; style-src 'self' 'nonce-N';
img-src 'self' data: blob:; media-src 'self' blob:; font-src 'self';
connect-src 'self' ws://<Host>; worker-src 'self' blob:; object-src 'none';
base-uri 'none'; form-action 'none'; frame-ancestors 'none'
```

Scripts, styles and connections come only from the server itself (`ws://<Host>`, the
request's accepted `Host`, because older browsers don't match `ws:` with `'self'`), the
WebAssembly module may be compiled, and the page can't be framed, post forms or load
plugins. trunk's loader is an inline module script, so an HTML file's nonce
placeholder (`{{__TRUNK NONCE__}}`, which trunk writes with `create_nonce = true`) is
replaced on every response by a fresh nonce, 16 bytes from `getrandom` as 32 hex
digits, and the policy names it as `'nonce-N'`. Other files get the policy without a
nonce.

### Sessions

Each accepted connection gets a fresh session from the `Connector`
(`Arc<dyn Fn() -> Box<dyn Control>>`), dropped when the connection ends. The connection
task speaks the envelope
([jstream-protocol › Design](../jstream-protocol/README.md#design)):

- `hello` is forwarded to `Control::hello`. On success the task subscribes to the
  session's events and answers `welcome`; from then on events are forwarded. A refused
  first handshake (`unsupported_version`) is answered and the connection closed (close
  code 1008), as the envelope says. A connection whose handshake hasn't succeeded within
  `handshake_timeout` of being accepted is closed with code 1008 too (`no handshake in
  time`).
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
it first ends the session's event subscription, then sends what the outbox still holds
before the close frame. A client that doesn't take them within a short timeout
(`CLOSE_TIMEOUT`) is dropped without one, so a client that stopped reading can't keep
its connection or session open.

### MCP over streamable HTTP

With `ServerConfig::mcp` on (`ServerConfig::new` leaves it off; `jstream-daemon` turns
it on), `/mcp` (`MCP_PATH`) serves MCP's
[streamable-HTTP transport](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports)
on the control port. Every request, whatever its method, first passes the same `Host`,
`Origin` and token checks as an upgrade, with the same refusals (403, 401), so a
request without `Origin` (a native agent) goes on to the token check, as on the
WebSocket API: browsers send `Origin` on a cross-origin `fetch`, and the bearer token
forces a preflight. Each body is one JSON-RPC message, answered by a
`jstream_mcp::Server` ([jstream-mcp › Design](../jstream-mcp/README.md#design)):

| Request | Answer |
|---|---|
| `POST`, a request | 200, `Content-Type: application/json`, the JSON-RPC reply. Never an SSE stream: every request this server serves completes on its own |
| `POST`, a notification or a response | 202, no body |
| `POST` `initialize` | a new session; on success its ID comes back in `Mcp-Session-Id`, and a failed `initialize` keeps none. One that carries `Mcp-Session-Id` ends that session first. 503 when `max_mcp_sessions` are open |
| `POST` with `Mcp-Session-Id` | served by that session; 404 if it is unknown or expired, so the client initializes again |
| `POST` without `Mcp-Session-Id` | served by the one shared stateless server (MCP 2026-07-28 requests carry no session) |
| `DELETE` with `Mcp-Session-Id` | ends that session: 204, or 404 if it is unknown |
| `GET` | 405 with `Allow: POST, DELETE`: the server sends no server-initiated messages |
| A body that isn't `Content-Type: application/json` / is over 4 MiB (refused from its `Content-Length` before it is read, when it declares one) / isn't UTF-8 | 415 / 413 / 400 |
| `MCP-Protocol-Version` naming a revision `jstream-mcp` doesn't serve | 400 |

The `Accept` header isn't checked: the answer is always JSON, which every client
accepts.

**Sessions.** An `initialize` gets a new `jstream_mcp::Server` over a new `Control`
session from the `Connector`: a client of its own, with its own undo history and its
own `events` buffer ([ADR-0013](../../docs/adr/0013-command-model.md)). Its ID is 16
bytes from the OS's random source (`getrandom`) as 32 hex digits. It is not a secret
that admits anyone: every request still needs the token. Requests that carry no session
share one stateless server, opened at the first of them, so those clients share a
session ([jstream-mcp › Open questions](../jstream-mcp/README.md#open-questions)).
Requests to one session may run concurrently.

**Limits.** At most `max_mcp_sessions` (default 16) sessions are open. One unused for
`mcp_idle_timeout` (default 30 minutes) is ended, and its `Control` session dropped,
when the next MCP request arrives; until then it holds its place. These are separate
from `max_connections`, which counts WebSocket connections: an MCP request holds no
connection slot. A request that passes the checks disarms its connection's header-read
deadline, as an accepted upgrade does, so an authenticated client can keep the
connection open for more requests (HTTP keep-alive) and wait on calls longer than the
deadline.

### Logging

The server logs through `tracing` ([R-RS-005](../../docs/RULES.md#R-RS-005)) and
installs no subscriber; the host app does. The listener records each connection's peer
address (axum's `ConnectInfo`), for logging only: admission depends on the headers
alone.

- **Refused upgrades** (and refused UI and MCP requests) are logged at `warn` as `connection refused`, with the peer
  address and the reason (`Host not allowed`, `Origin not allowed`, `missing or wrong
  control token`, `too many connections`, or `too many MCP sessions`). No header value
  is logged: not the token, nor the refused `Host` or `Origin`
  ([R-SEC-001](../../docs/RULES.md#R-SEC-001)). Refused MCP requests are logged the
  same way. The test
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
- **MCP sessions** get an `info` span `mcp_client` with a number (counting from 1 per
  server) and `kind` (`session` or `stateless`), under which the engine session and
  `jstream-mcp`'s own spans nest. Opening one logs `mcp session opened` with the peer
  address, and ending one `mcp session ended` with the reason (`idle`, `deleted by the
  client`, `replaced by a new initialize`, `initialize failed`). The session ID is never logged, nor any message.
- A UI file that can't be read is logged at `warn` with the error (500 to the
  client); the path is not logged.
- A request that isn't a WebSocket upgrade is logged at `debug`, and so is a connection
  dropped by the header-read timeout. A connection closed by the handshake timeout logs
  `connection closed` at `info` with its code and reason, like any other close.

### Dependencies

axum (features `http1`, `tokio`, `ws`, no defaults) for HTTP and the upgrade, which
brings hyper, tower and `tokio-tungstenite` without TLS; tokio for the listener and the
connection tasks; `futures-util` to split the socket, run calls concurrently
(`FuturesUnordered`) and box the writer's send in progress; `blake3` (the workspace's `pure` build, already used by `xtask`)
for the token digest; `tracing` for the spans and events above; `jstream-mcp` for the
MCP server behind `/mcp`; `getrandom` for MCP session IDs, already in the tree through
tungstenite's `rand` and used by `jstream-daemon` for its token. No TLS crate is in the tree, so no `ring` or `rustls`.

### Open questions

1. **TLS.** Plain `ws://` on loopback is the first cut. Remote browsers need `wss://`
   with rustls and the ring provider ([R-DEP-006](../../docs/RULES.md#R-DEP-006)): a TLS
   feature and its wiring. `ring` is already on the lint-deps native-code allowlist, for
   `jstream-outputs`' RTMPS, so that needs no further governance change.
2. **Browser token.** Settled and built for the UI this server serves (see *The web
   UI*): the browser presents the token as `Basic` credentials, which it sends on the
   UI's WebSocket upgrade too, so the UI needs no `Sec-WebSocket-Protocol` token.
   Checked in Chromium; whether every browser sends cached `Basic` credentials on a
   WebSocket upgrade (Safari in particular) is open, and a `Sec-WebSocket-Protocol`
   fallback would be the answer if one doesn't. Pairing a remote browser with a short
   code instead of pasting the token is a maintainer-tier decision of its own.
3. **UI assets.** Settled and built (see *The web UI*). Open: caching with `ETag`s
   instead of `no-cache`, and compressed variants, if the bundle grows enough to
   matter. MCP over streamable HTTP is settled and built (see *MCP over streamable
   HTTP*); whether it should also offer SSE, once the MCP server has notifications to
   push, is open.
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
   left open is tuning: whether 1024 events suits `stats.tick` (about once a second
   while a pipeline runs) and the high-rate meters still to come, or wants a byte
   limit or per-event-kind limits once they exist.
8. **Limit tuning.** The connection limits (see *Listening and admission*) are a
   conservative first cut. Whether a remote bind wants a per-peer cap as well, or the
   host app should expose the limits, is open. Settled: the handshake timeout counts
   from accept, so a client must say `hello` as soon as it connects. `jstream-cli mcp`
   connects when it starts, and `jstream-mcp` sends `hello` at the MCP `initialize` (or
   `server/discover`), not at the first tool call
   ([jstream-mcp › Design](../jstream-mcp/README.md#design)), so an idle agent keeps its
   session.
