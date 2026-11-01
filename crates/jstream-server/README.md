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
  origins, connection limits, the ping interval and liveness timeout, the MCP switch and its limits, the UI directory, the
  device file, the UI settings file, the `tls` flag, the `pairing` switch) and `Token`,
  the control token.
- **Devices** (`devices.rs`): the paired devices, the open pairing code, and the device
  file that keeps the devices' token digests
  ([ADR-0044](../../docs/adr/0044-pairing-remote-browsers.md)).
- **UI settings** (`ui_settings.rs`): the settings `ui_settings.set` keeps for every
  client, such as the web UI's dock, and the file that keeps them
  ([ADR-0084](../../docs/adr/0084-ui-settings-kept-by-the-server.md)).
- **Pairing** (`pair.rs`): `POST /api/v1/pair` (`PAIR_PATH`), which trades the open code
  for a device token, or for the device cookie in a browser.
- **Pairing page** (`pair_page.rs`, `pair.html`): `GET /pair` (`PAIR_PAGE_PATH`), the
  token-free page a browser pairs on, on a TLS listener only
  ([ADR-0047](../../docs/adr/0047-browser-pairing-page-and-device-cookie.md)).
- **Admitted session** (`admitted.rs`): the `Control` wrapper every connection and MCP
  session gets, which answers `pairing.start`, `device.list` and `device.revoke` itself
  unless `ServerConfig::pairing` is off
  ([ADR-0057](../../docs/adr/0057-server-answers-credential-calls.md)).
- **Listener** (`listener.rs`): accepts TCP connections, closes one accepted while
  `max_tcp_connections` are open, and drops one that hasn't sent an accepted upgrade
  request in time.
- **Gate** (`gate.rs`): the `Host`, `Origin` and token checks every request passes
  ([R-SEC-002](../../docs/RULES.md#R-SEC-002)), the device cookie among them, and the
  limit on how many failed credentials are logged.
- **Session** (`session.rs`): one connection, from the protocol envelope to one `Control`
  session and back, with the bounded outbox that keeps a slow client from holding up
  its events, the cap on calls in flight, and the pings that close a connection that
  went silent.
- **Preview frames** (`preview.rs`): the session's binary channel, queued per
  connection and sent as WebSocket binary messages, dropping up to the next keyframe
  when the client reads slowly.
- **Errors** (`error.rs`): `ServerError`, a `thiserror` enum.

Not here yet: a TLS listener ([Open questions](#design)). `ServerConfig::tls` says
whether clients reach the listener over TLS, and only tests turn it on until there is
one.

Why a crate and not a module of `jstream-control` or `jstream-mcp`: it owns the HTTP
stack (axum, hyper, tokio's network runtime), which neither the WASM-safe
`jstream-control` nor the runtime-free `jstream-mcp` may carry, and it sits above both
(it mounts MCP), so it is its own layer-4 crate, as the crate map plans.

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-server/src/**", "crates/jstream-server/Cargo.toml"]
depends_on = ["docs/ARCHITECTURE.md#protocol", "docs/adr/0006-mcp-full-access.md", "docs/RULES.md#R-SEC-002", "crates/jstream-control/README.md#design"]
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
| `Host` | `127.0.0.1`, `localhost` or `[::1]` with the bound port; the bound address; or a configured extra; without a port only on the scheme's default port (below) | 403 |
| `Origin` | absent, or `http://` (`https://` with `ServerConfig::tls`) plus the accepted `Host` (the server's own origin), or a configured extra | 403 |
| `Authorization` | `Bearer <token>`, or `Basic` credentials whose password is the token (any user name); without `Authorization`, the offered subprotocol `jstream.token.<token>` (below); with neither, on a TLS listener, the device cookie (see *The pairing page and the device cookie*). The token is the control token or, on a loopback bind or a TLS listener, a paired device's (see *Pairing*). Matched by comparing BLAKE3 digests (below) | 401 with `WWW-Authenticate: Bearer` (`Basic` on the UI's paths) |

`Host` must appear exactly once, `Authorization` at most once (exactly once unless the
subprotocol carries the token) and `Origin` at most once, and
comparisons ignore ASCII case (except the token). The `Host` check defeats DNS
rebinding: a page on an attacker's domain that resolves to `127.0.0.1` still sends its
own name. The `Origin` check stops foreign pages, since browsers always send `Origin` on
a WebSocket upgrade, and an opaque `null` origin is refused like any other foreign one.
A request without `Origin` is a native client (the CLI, `websocat`, a script) and goes
on to the token check, which is the gate (open question 4). The token is never logged (see *Logging*), and `Token`'s `Debug` form hides it.

**`Host` without a port.** A `Host` (or `Origin`) without a port names the scheme's
default port, 80 for `http` and `ws` (443 for `https` and `wss`, with
`ServerConfig::tls`), and browsers leave it out. So when the server listens on port 80,
`localhost` is the same as `localhost:80` and is accepted (and `http://localhost` is its
own origin). On any other port, the default and ephemeral ones included, a `Host`
without a port names a different port and is refused.

**`Basic` credentials.** A browser can't put a bearer token on a page load or a
WebSocket upgrade, but it sends the HTTP `Basic` credentials it was given for an
origin with every request to it, upgrades included (checked in Chromium). So the gate
also accepts `Authorization: Basic base64(user:token)`: the password is the token and
the user name is ignored. Credentials without a `:`, or that aren't base64 with
padding, are refused like a wrong token. This is the same token presented differently,
not a new kind of client (ADR-0006).

**The token as a subprotocol.** `jstream-control`'s browser client can't rely on
cached credentials (a UI served from elsewhere, or a browser that doesn't resend them),
so it offers two subprotocols in `Sec-WebSocket-Protocol`: `jstream` and
`jstream.token.<token>`. A request without `Authorization` may carry its token that
way, on any `Sec-WebSocket-Protocol` line, in exactly one offered subprotocol; with
`Authorization` present, only that counts. The upgrade selects `jstream` (browsers
fail a connection whose offered subprotocols none were selected), never the token's,
so the answer doesn't echo it, and like every header it is never logged. Both names
are `jstream-control`'s constants, shared with the client
([ADR-0045](../../docs/adr/0045-leptos-web-ui.md)).

**Extra hosts.** Each configured extra host must be `name:port` with one port from 1
to 65535, and an IPv6 address must be in brackets (`[::1]:4455`), as it is in a
request's `Host`. Any other entry (no port, two ports as in `name:80:80`, or a bare
`fe80::1`) could never match, since the gate compares hosts with their port, so
`Server::bind` refuses it (`ServerError::InvalidHost`) before opening the socket rather
than ignoring it. A configured extra origin is trusted as the server's own: a page there
can use whatever credentials the browser holds for the server, a cached `Basic` password
included.

**Token compare.** `Token` keeps only the BLAKE3 digest of the token. The presented
value is hashed with BLAKE3 too, and the two 32-byte digests are compared with
`blake3::Hash`'s constant-time equality. So the compare reveals nothing about the
token: not where the two differ, and not its length.

**Connection limits.** Five bounds keep one client, or a crowd of them, from holding
the server's resources. Each is a `ServerConfig` field, and `ServerConfig::new` sets
the default:

| Limit | Default | When it is exceeded |
|---|---|---|
| `max_tcp_connections` | 128 open TCP connections of every kind: not yet authenticated, refused but still open, HTTP keep-alive ones to `/mcp` and the UI, and WebSocket connections | A connection accepted beyond it is closed at once, unanswered. Its place is taken when it is accepted and freed when it closes. |
| `max_connections` | 32 open WebSocket connections | An upgrade that passes the checks above gets 503 and no session. The slot is taken just before the upgrade and freed when the connection ends. |
| `header_read_timeout` | 10 s from accept to an accepted upgrade request | The connection is dropped, unanswered. |
| `handshake_timeout` | 10 s from accept to a successful `hello` | The connection is closed with close code 1008 (see *Sessions*). |
| `liveness_timeout` | 45 s without hearing from a WebSocket client, which is pinged every `ping_interval` (15 s) | The connection is closed with close code 1008 and its session dropped (see *Sessions*). |

`Server::bind` refuses a zero limit or timeout, a zero `ping_interval`, and a zero
`max_mcp_sessions` or `mcp_idle_timeout` (see *MCP over streamable HTTP*), with
`ServerError::InvalidLimit` before opening the socket, since the server would refuse or
drop everything. It refuses a `liveness_timeout` no longer than `ping_interval` with
`ServerError::LivenessTimeout`, since an idle client that answers every ping would
still be closed.

The TCP cap and the header-read timeout are kept on the socket, by the listener
(`listener.rs`). The cap counts every accepted stream until it is dropped, so it also
bounds the connections the header-read deadline no longer does: authenticated UI and
MCP keep-alive connections, and upgraded WebSocket ones. The first connection dropped
for it is logged at `warn` (`too many open connections`), and later ones are not, until
a connection is accepted again. The header-read timeout is there since hyper's own isn't
reachable through `axum::serve`: until the upgrade handler accepts a request, every read
and write fails once the deadline has passed. So it also drops a connection that sends
no request, asks for another path, or stays open after a refusal.
The WebSocket cap (`max_connections`) is checked after the `Host`, `Origin` and token
checks, so a refused upgrade learns nothing about how many sessions are open, and a
refusal costs no session. The TCP cap is checked at accept, before any request is read. The listener
stays on loopback by default; these bounds are what a non-loopback bind would rely on
against a misbehaving client on the network.

### The web UI

With `ServerConfig::ui_dir` set (`ServerConfig::new` leaves it unset; `Server::bind`
refuses a path that isn't a directory with `ServerError::UiDir`), every path the API
doesn't use (anything but `/api/v1/ws`, `/mcp`, `/pair` and other paths under `/api`,
`/mcp` or `/pair`)
serves a file from that directory: the UI's built bundle, `trunk build`'s `dist`
(`Server::ui_url` gives the address). Each request passes the gate above first,
so the bundle is served only to holders of the token
([ADR-0006](../../docs/adr/0006-mcp-full-access.md): every listener, the web UI
included, requires it). A refused token is answered with `WWW-Authenticate: Basic
realm="JStream"`, so the browser asks for a user name and password: the user pastes the
token from the token file as the password. The browser then sends it with the bundle's
requests and the UI's WebSocket upgrade, and the UI never handles the token. On a TLS
listener the `401`'s body is instead a short HTML page (under a policy that loads and
runs nothing) that links to the pairing page beside the challenge; without TLS it is
plain text with no link. A request
that passes disarms the header-read deadline, as on `/mcp`, so the bundle loads over a
kept-alive connection.

| Request | Answer |
|---|---|
| `GET` or `HEAD` of a file in the directory | 200, the file (at most 64 MiB) |
| A directory (`/` included) | its `index.html` |
| A missing path whose last segment has no `.` (the UI's own routes) | the root's `index.html` |
| Any other missing path, or one under `/api`, `/mcp` or `/pair` | 404 |
| A path with an empty, `.` or `..` segment, or one that holds `\`, `:` or NUL (percent-escapes decoded first) | 404 |
| A file that resolves outside the directory (a symbolic link) | never served: 404, or the root's `index.html` for a name without a `.` |
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
request's accepted `Host`, `wss://<Host>` on a TLS listener, because older browsers
don't match `ws:` with `'self'`), the
WebAssembly module may be compiled, and the page can't be framed, post forms or load
plugins. trunk's loader is an inline module script, so an HTML file's nonce
placeholder (`{{__TRUNK NONCE__}}`, which trunk writes with `create_nonce = true`) is
replaced on every response by a fresh nonce, 16 bytes from `getrandom` as 32 hex
digits, and the policy names it as `'nonce-N'`. Other files get the policy without a
nonce.

### Pairing

[ADR-0044](../../docs/adr/0044-pairing-remote-browsers.md) lets a device get a token of
its own from a short code instead of the control token.

- **The code.** `pairing.start`, from any admitted client, opens a code: 8 Crockford
  base32 characters (40 bits from `getrandom`), shown `XXXX-XXXX`. One is open at a time,
  and a new `pairing.start` replaces it. It lives 2 minutes, works once, and is void
  after 5 wrong attempts (logged at `warn`, without the code). Entry ignores case,
  dashes and spaces, and reads `O` as `0` and `I`/`L` as `1`. `pairing.start` is refused
  (`action_failed`) unless the server is bound to loopback or `ServerConfig::tls` is on:
  a device token is accepted only over loopback or TLS
  ([R-SEC-002](../../docs/RULES.md#R-SEC-002)).
- **Pairing off.** With `ServerConfig::pairing` off (on in `ServerConfig::new`), the
  server answers none of the three calls: they reach the session, which answers
  `unsupported`, so a host's UI hides its pairing panel. No code is ever drawn,
  `GET /pair` is 404, and the gate accepts no device token or cookie, not even one
  already in the device file, since nothing could list or revoke it. `jstream-desktop`
  turns it off.
- **The exchange.** `POST /api/v1/pair` (`PAIR_PATH`) with
  `{"code": "...", "name": "..."}`, and `"browser": true` from the pairing page, is
  answered without a token; it still passes the `Host` and `Origin` checks. The name is
  trimmed, 1 to 64 characters, with no control characters.

| Request | Answer |
|---|---|
| A method other than `POST` | 405 with `Allow: POST` |
| No code open | 401 |
| A body that isn't `Content-Type: application/json` | 415 |
| A body over 4 KiB, one that isn't `{"code", "name"}`, a name that isn't allowed, a `browser` that isn't a boolean, or `"browser": true` without TLS | 400 (no attempt used up) |
| A wrong code | 403 (its message says when that attempt voided the code) |
| The open code | 200, `Cache-Control: no-store`, `{"device_id": "dev_…", "token": "<64 hex digits>"}` |
| The open code, `"browser": true`, on a TLS listener | 200, `Cache-Control: no-store`, `{"device_id": "dev_…"}` and `Set-Cookie: __Host-jstream_device=<token>; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000` |
| The device file can't be written | 500, and nothing is paired |

- **Device tokens.** A device token is 32 bytes from `getrandom` as 64 hex digits,
  presented exactly like the control token (or, in a browser, as the device cookie, see
  below). The server keeps only its BLAKE3 digest,
  compared in constant time as above. A device's session says `hello` as
  `device:<name>`, so the engine's audit log names it, and it may do anything the
  control token may (ADR-0006). One unused for 30 days lapses; use is recorded at most
  hourly.
- **The device file.** With `ServerConfig::device_file` set (`jstream-daemon` sets it
  beside the token file), the devices (ID, name, digest, paired and last-used times) are
  kept there as JSON, written through a temporary file and renamed, user-only (mode
  `0600` on Unix; on Windows a protected DACL for SYSTEM and the user, from
  `jstream_platform_windows::create_private_file`, #344); `Server::bind` refuses a file
  others may access (`ServerError::DeviceFile`): on Windows, one
  `check_private_file` refuses. Without it,
  devices last as long as the server.
- **Listing and revoking.** `device.list` answers each device's ID, name, and when it was
  paired and last used. `device.revoke` forgets one and closes its open WebSocket
  connections (close code 1008, `the device was revoked`); its MCP sessions are dropped
  at the next MCP request.
- **Secrets.** Codes and device tokens are never logged, nor put in an event, the audit
  log or a snapshot; the code appears only in `pairing.start`'s answer and the token only
  in the exchange's ([R-SEC-001](../../docs/RULES.md#R-SEC-001)). The test
  `tests::pairing_codes_and_device_tokens_are_never_logged` checks this.

These three calls are answered by the server, not the engine
([ADR-0057](../../docs/adr/0057-server-answers-credential-calls.md)): see *Sessions*.

### UI settings

[ADR-0084](../../docs/adr/0084-ui-settings-kept-by-the-server.md) keeps the UI's own
settings on the engine's machine, so the desktop app and every browser of one engine
show the same workspace.

- **The calls.** `ui_settings.set {key, value}` keeps any JSON value under a key, and a
  `null` or missing value removes it; `ui_settings.get {key}` reads it back. The server
  checks only the key's form (1 to 64 of `a`-`z`, `0`-`9`, `.`, `_`, `-`) and the
  sizes (64 KiB of compact JSON per value, 64 keys), answering `invalid_argument`
  otherwise. They are answered with `ServerConfig::pairing` off too, so the desktop
  app's webview is served.
- **The settings file.** With `ServerConfig::ui_settings_file` set (`jstream-daemon` and
  `jstream-desktop` both set `ui-settings.json` in the data folder), the settings are
  read when the server binds and the whole file is rewritten on every change, through a
  temporary file and a rename, user-only like the device file; the file's folder is
  created if missing. A file that can't be read or isn't a version 1 settings file is
  logged at `warn` and replaced on the next change: the server still starts. A change
  that can't be written answers `action_failed` and is kept in memory until the server
  stops. Without a file, the settings last as long as the server.

### The pairing page and the device cookie

[ADR-0047](../../docs/adr/0047-browser-pairing-page-and-device-cookie.md) lets a
browser pair by itself and stay signed in. All of it exists only with
`ServerConfig::tls` and `ServerConfig::pairing` on; otherwise `GET /pair` is 404 and the UI's `401` links nowhere.

- **The page.** `GET /pair` (`PAIR_PAGE_PATH`; `HEAD` too, any other method 405) is
  answered without a token, after the `Host` and `Origin` checks. It is a fixed page
  built into the crate (`pair.html`), not part of the UI bundle: a code field, a
  device-name field and one inline script, under the UI's policy with a fresh nonce for
  its `<script>` and `<style>`, and the UI's other headers. It holds no engine data and
  no secret, and changes nothing. The route is always registered, so the UI's fallback
  never serves `/pair`, and the fallback refuses paths below it.
- **The browser exchange.** The script posts the code and name with `"browser": true`
  (see the exchange's table above). The token goes only into the `HttpOnly` cookie, so
  no script, the page's own included, ever reads it. Then the page opens `/`.
- **The cookie at the gate.** A fourth way to present a device token, never the
  control token. The gate reads only `__Host-jstream_device` (not a plain
  `jstream_device` another service on the host could plant), only on a TLS listener,
  and only when the request carries neither `Authorization` nor an offered
  `jstream.token.…` subprotocol (a wrong one of those is not rescued by the cookie).
  The cookie on more than one `Cookie` pair, on one line or several, is refused. A
  request the cookie admits must also carry `Sec-Fetch-Site: same-origin` or `none`,
  besides the `Host` check and the `Origin` check when `Origin` is present: a page on
  another port of the host or a sibling subdomain is same-site, so the browser sends it
  the cookie, but is refused even on a `GET` without `Origin`. A browser that sends no
  `Sec-Fetch-Site` falls back to the `Basic` prompt. Each refusal is a 401 like a wrong
  token.
- **Renewal.** A use the cookie admits that renews the device's 30-day lapse (recorded
  at most hourly, see *Pairing*) re-sets the cookie with a fresh `Max-Age` on that
  answer, a UI file or the WebSocket upgrade, at most once a day. When it was last set is
  kept in memory only; after a restart it is taken to be the device's last use. Revoking the
  device, or its lapse, makes the cookie worthless; the server never clears it.
- **Secrets.** Like every header the cookie is never logged
  ([R-SEC-001](../../docs/RULES.md#R-SEC-001)); the test
  `tests::a_browser_pairs_into_a_cookie_that_admits_it_until_revoked` checks this.

### Sessions

Each accepted connection gets a fresh session from the `Connector`
(`Arc<dyn Fn() -> Box<dyn Control>>`), dropped when the connection ends, wrapped in the
server's own `Control` (`admitted.rs`), which answers `pairing.start`, `device.list` and
`device.revoke` from the device store and `ui_settings.get` and `ui_settings.set` from
the UI settings (after a successful `hello`, like any call),
renames a device's `hello` to `device:<name>`, and passes everything else through. The connection
task speaks the envelope
([jstream-protocol › Design](../jstream-protocol/README.md#design)):

- `hello` is forwarded to `Control::hello`. On success the task subscribes to the
  session's events and answers `welcome`; from then on events are forwarded. A refused
  first handshake (`unsupported_version`) is answered and the connection closed (close
  code 1008), as the envelope says. A connection whose handshake hasn't succeeded within
  `handshake_timeout` of being accepted is closed with code 1008 too (`no handshake in
  time`), including one whose `hello` arrived in time but that the engine hasn't
  answered by then: the wait for the engine's answer races the same deadline.
- Each `request` is forwarded to `Control::call` and may stay in flight while others
  run, up to 64 per connection (see *Slow clients*); answers go back as they complete,
  carrying the client's request ID. A request
  before `hello` is answered by the session itself (`handshake_required`).
- A message that doesn't parse gets a `malformed` error, with the request ID when one is
  readable. A binary message from the client gets `malformed` too: binary frames only
  go to the client. Messages over 4 MiB close the connection.
- **Preview frames.** After a successful `hello` the task also opens the session's
  binary channel (`Control::binary`) and sends each frame it yields as a WebSocket
  binary message (`BinaryFrame::to_bytes`), so `preview.subscribe` works over the
  wire. The frames have their own queue of 8 (`FRAME_BUFFER`), apart from the outbox:
  the writer sends text before binary, so a picture never delays an answer, and when a
  frame finds the queue full that channel's frames are dropped until a keyframe finds
  room (`KeyframeGate`), so what the client receives decodes. The session's own queue
  is bounded the same way, so a slow client holds a few pictures and nothing more.
- **Request IDs.** A `Control` session assigns its own IDs, which appear in the
  `state.changed` and `studio.changed` events its calls cause. The task maps them back to the client's wire
  IDs, so a client matches its event to its response by the ID it chose, as in the
  protocol README's example. An event can arrive before the answer that reveals the
  mapping, so while calls are in flight it is held back, with every event after it to
  keep the order, until the answer arrives or nothing is left in flight (or 1024 events
  are held). The last 1024 pairs are remembered. Other clients' events pass unchanged:
  their IDs mean nothing to this client.
- **Slow clients.** The connection task never waits on the socket: what it sends goes
  into an outbox, and a writer that owns the socket's sending half sends it, one message
  at a time, while the task keeps reading the client and draining the session's events.
  So a client that reads slowly leaves nothing piling up in the engine's subscription
  queue, and never slows the engine or other clients. The outbox holds at most 1024
  events (`EVENT_BUFFER`): about 100 KB of today's events, or 34 seconds of one
  gesture's `live.changed`, which the engine sends at most 30 times a second. When one more arrives, every queued event is dropped, one `events.dropped
  {count}` notice is queued at the end, and the new event after it; while that notice
  is still unsent, later drops add to its count instead of queuing another. Replies
  (`welcome`, `response`, `error`) are never dropped, and what remains keeps its order,
  so a `state.changed` that survives still follows its call's response. The client
  re-reads the state with `state.get`. Each drop is logged at `warn` with its count
  (see *Logging*). Replies are bounded instead by reading: while 1024 of them wait
  unsent (`REPLY_BACKLOG`), the task reads no more requests, so a client that sends
  requests without reading the answers is held back by the socket. Calls the engine
  hasn't answered yet are bounded the same way: while 64 are in flight on a connection
  (`MAX_IN_FLIGHT`), the task reads nothing more until one completes, so a client that
  never waits for its answers holds at most 64 parsed requests, each of up to 4 MiB of
  message text.
- The connection ends when the client closes it, when the session's event stream ends
  (the engine went away; close code 1001), when the session reports `Disconnected`
  (close code 1011), when the server shuts down, or when the client goes silent (see
  *Liveness* below). Dropping the session expires the
  client's open live-value gestures
  ([ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)).
- **Liveness.** A connection that dies without a close (a laptop lid shut, a network
  gone) may not fail a read or write for a long time, so the task checks it, as
  ADR-0032 asks. It pings the client every `ping_interval` (15 s), and anything it
  reads from the client counts as hearing from it: a message, or the pong a live
  client's WebSocket layer answers with. A connection not heard from within
  `liveness_timeout` (45 s) is closed with code 1008 (`no answer to pings in time`) and
  its session dropped, which expires its gestures. While the task itself has stopped
  reading (a reply backlog or 64 calls in flight, see *Slow clients*), the silence
  doesn't count, since a pong would wait unread behind the requests; it counts again
  from when reading resumes. A ping goes before the next queued message, once the send
  in progress completes. The client's own pings are answered by the WebSocket layer.

`serve_with_shutdown(signal)` stops accepting when `signal` completes and closes every
open connection (close code 1001), so their clients' pending and later calls fail with
`Disconnected` and their event streams end. A connection handles nothing it reads once
the server is shutting down, so a call sent after the shutdown is never answered. It
returns once every WebSocket connection has finished closing (as below), so a host that
exits right after it doesn't cut a close short. Whenever the server closes a
connection, it first ends the session's event subscription, then sends what the outbox
still holds before the close frame, then reads and drops what the client still sends
until the client's own close frame or the end of the stream, and only then closes the
socket. Closing it with client data unread would make the kernel reset the connection,
and on macOS and Windows a reset discards what the client hasn't read yet, the close
frame and last replies included. A client that doesn't finish this within a short
timeout (`CLOSE_TIMEOUT`) is dropped, so a client that stopped reading can't keep its
connection or session open, nor hold up the shutdown.

### MCP over streamable HTTP

With `ServerConfig::mcp` on (`ServerConfig::new` leaves it off; `jstream-daemon` turns
it on), `/mcp` (`MCP_PATH`) serves MCP's
[streamable-HTTP transport](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports)
on the control port. Every request, whatever its method, first passes the same `Host`,
`Origin` and token checks as an upgrade, with the same refusals (403, 401), so a
request without `Origin` (a native agent) goes on to the token check, as on the
WebSocket API: browsers send `Origin` on a cross-origin `fetch`, and a `POST` needs
`Content-Type: application/json`, which forces a preflight. Each body is one JSON-RPC message, answered by a
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
session from the `Connector`, wrapped like a connection's (see *Sessions*): a client of
its own, with its own undo history and its own `events` buffer
([ADR-0013](../../docs/adr/0013-command-model.md)). Its ID is 16
bytes from the OS's random source (`getrandom`) as 32 hex digits. It is not a secret
that admits anyone: every request still needs the token. Requests that carry no session
share one stateless server per credential (the control token, or each paired device),
opened at the first of them, so those clients share a
session ([jstream-mcp › Open questions](../jstream-mcp/README.md#open-questions)).
Requests to one session may run concurrently.

**Credentials.** A session belongs to the credential that opened it: a request with
another token gets 404 for it. A session or stateless server a device opened ends at the
next MCP request once that device is revoked or has lapsed.

**Limits.** At most `max_mcp_sessions` (default 16) sessions are open. One unused for
`mcp_idle_timeout` (default 30 minutes) is ended, and its `Control` session dropped,
when the next MCP request arrives; until then it holds its place. These are separate
from `max_connections`, which counts WebSocket connections: an MCP request holds no
connection slot, though its TCP connection counts toward `max_tcp_connections`. A request that passes the checks disarms its connection's header-read
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
- **Stream keys** pass through on their way to the engine and are never logged or sent
  back ([R-SEC-001](../../docs/RULES.md#R-SEC-001)): not in `output.set_key`'s answer,
  the errors for a key of the wrong shape or a frame that doesn't parse, an event, or an
  MCP tool result. The test `tests::stream_keys_are_never_echoed_or_logged` checks this
  over a raw WebSocket and the MCP endpoint, with a capturing subscriber.
- **Failed credentials** (a missing or wrong token, or a refused device cookie) are
  logged that way only up to 10 in a minute, counted from the first, for every peer
  together, since on loopback a peer address tells no one apart. The 11th logs `too
  many failed credential checks: not logging the rest of this window`, later ones in the
  minute are only counted, and the first one after the minute logs their number
  (`failed credential checks were not logged`, `unlogged`) before its own line. The
  requests are still refused one by one and the right token still gets in: 256 bits
  can't be guessed, so delaying or locking out attempts would only let any local
  process shut out the real clients (#354). The state is a start time and a count.
  `gate::tests::a_flood_of_wrong_tokens_logs_a_bounded_summary` checks this.
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
- **Pairing.** A refused pairing request is logged as `connection refused` too (`pairing
  attempted while no pairing code is open`, `wrong pairing code`), and so is a refused
  device cookie (`device cookie repeated`, `device cookie without a same-origin
  Sec-Fetch-Site`). A started pairing,
  a paired device (ID and name), a revoked or lapsed device (ID), and a connection a
  device's token admitted (`device_id`) are logged at `info`; a voided code and a device
  file that can't be updated at `warn`. No code, device token or cookie is ever logged
  (see *Pairing*).
- A UI file that can't be read is logged at `warn` with the error (500 to the
  client); the path is not logged.
- A request that isn't a WebSocket upgrade is logged at `debug`, and so is a connection
  dropped by the header-read timeout. A connection dropped because `max_tcp_connections`
  are open is logged at `warn` (`too many open connections`) with the peer address, once
  per run of such drops. A connection closed by the handshake timeout logs
  `connection closed` at `info` with its code and reason, like any other close.

### Dependencies

axum (features `http1`, `tokio`, `ws`, no defaults) for HTTP and the upgrade, which
brings hyper, tower and `tokio-tungstenite` without TLS; tokio for the listener and the
connection tasks; `futures-util` to split the socket, run calls concurrently
(`FuturesUnordered`) and box the writer's send in progress; `blake3` (the workspace's `pure` build, already used by `xtask`)
for the token and device-token digests; `tracing` for the spans and events above; `jstream-mcp` for the
MCP server behind `/mcp`; `serde_json` for the pairing exchange, the device file and the UI settings file;
`getrandom` for MCP session IDs, pairing codes and device tokens, already in the tree through
tungstenite's `rand` and used by `jstream-daemon` for its token; on Windows only,
`jstream-platform-windows` for the device file's user-only DACL and its check (#344):
the server forbids `unsafe`, and the daemon links that crate anyway. No TLS crate is in the tree, so no `ring` or `rustls`.

### Open questions

1. **TLS.** Plain `ws://` on loopback is the first cut. Remote browsers need `wss://`
   with rustls and the ring provider ([R-DEP-006](../../docs/RULES.md#R-DEP-006)): a TLS
   feature and its wiring. `ring` is already on the lint-deps native-code allowlist, for
   `jstream-outputs`' RTMPS, so that needs no further governance change. What depends on
   TLS (pairing beyond loopback, the pairing page, the device cookie) is built behind
   `ServerConfig::tls`, which the listener will set.
2. **Browser token.** Settled and built (see *`Basic` credentials* and *The token as
   a subprotocol*): a page load presents the token as `Basic` credentials, and the
   UI's upgrade either carries the same credentials (checked in Chromium) or offers
   `jstream.token.<token>` ([ADR-0045](../../docs/adr/0045-leptos-web-ui.md)).
   Pairing a device with a short code instead of pasting the token is settled and built
   (see *Pairing*, [ADR-0044](../../docs/adr/0044-pairing-remote-browsers.md)), and so is
   how a browser that gets a 401 reaches a pairing form and then presents its device
   token on page loads (see *The pairing page and the device cookie*,
   [ADR-0047](../../docs/adr/0047-browser-pairing-page-and-device-cookie.md)).
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
6. **Logging volume.** Failed credentials are settled and built (see *Logging*): at
   most 10 lines a minute, then a count (#354). Every other refused request is still
   logged at `warn`, so a foreign page that keeps retrying a refused `Origin` can fill
   the log; whether those need the same limit is open.
7. **Backpressure.** Settled and built (see *Sessions*): each connection has a bounded
   event buffer, and on overflow the server drops the queued events and sends one
   `events.dropped` notice, so the engine is never slowed down by one client. What is
   left open is tuning: whether 1024 events suits `stats.tick` (about once a second,
   while a pipeline runs or, with the engine's heartbeat, while idle) and `meters.tick` (about ten a second while the mixer runs,
   so 1024 events are under two minutes of them), or wants a byte limit or
   per-event-kind limits, such as keeping only the newest meter reading as
   `jstream-mcp` does.
8. **Limit tuning.** The connection limits (see *Listening and admission*) are a
   conservative first cut. Whether a remote bind wants a per-peer cap as well, or the
   host app should expose the limits, is open. Settled: the handshake timeout counts
   from accept, so a client must say `hello` as soon as it connects. `jstream-cli mcp`
   connects when it starts, and `jstream-mcp` sends `hello` at the MCP `initialize` (or
   `server/discover`), not at the first tool call
   ([jstream-mcp › Design](../jstream-mcp/README.md#design)), so an idle agent keeps its
   session.
