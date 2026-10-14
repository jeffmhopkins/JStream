<!-- @doc
id = "CRATE-jstream-mcp"
title = "jstream-mcp"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-mcp

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-mcp/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map"]
verified = { hash = "b3:62f26e4467ed08a3", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Merged main's #96 events tool into the Purpose beside this PR's tracing; crate-map row unchanged; Purpose matches the crate" }
-->

The MCP server: it gives AI agents full control of JStream through the Model Context
Protocol ([ADR-0006](../../docs/adr/0006-mcp-full-access.md)). It is written against
`dyn Control` and `jstream-protocol` only, never the engine
([R-ARCH-003](../../docs/RULES.md#R-ARCH-003)): the host hands it a session, in process
or over the WebSocket API. Layer 3: it builds on `jstream-control` (layer 2) and needs no
async runtime.

Contents (a first cut; [Design](#design) explains the choices):
- **Server** (`server.rs`): `Server`, which answers one JSON-RPC message at a time
  (`Server::handle`), with `MODERN_VERSION` and `LEGACY_VERSIONS`, the MCP revisions it
  serves.
- **Tools** (`tools.rs`): the tool table, generated from `jstream_protocol::json_schema()`,
  and the decoding of tool arguments into protocol calls.
- **Events** (`events.rs`): the `events` tool's feed, one event subscription pumped into
  a bounded, numbered buffer.
- **JSON-RPC** (`jsonrpc.rs`): parsing one message and building replies.
- **stdio transport** (`stdio.rs`): `serve_stdio`, newline-delimited messages over any
  `BufRead`/`Write` pair.
- **Errors** (`error.rs`): `McpError`, a `thiserror` enum for a transport that stops.

Not here yet: the streamable-HTTP transport ([Design](#design)).

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-mcp/src/**", "crates/jstream-mcp/Cargo.toml"]
depends_on = ["docs/ARCHITECTURE.md#mcp", "docs/adr/0006-mcp-full-access.md", "docs/adr/0013-command-model.md", "docs/adr/0036-mcp-events-polling-tool.md"]
verified = { hash = "b3:4c9c4b9df6baa5bf", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Joined this PR's Logging subsection (mcp_session/mcp_request/tool_call spans, never arguments) with main's #96 events tool row, malformed cursor and Events subsection; the Not implemented line now names both the events tool and tracing; depends_on keeps ADR-0036; matches server.rs" }
-->

### Tools

Tools are grouped by noun ([ARCH › MCP](../../docs/ARCHITECTURE.md#mcp)). The table is
generated from the protocol's JSON Schema when a `Server` is built: every variant of
`Edit`, `Action` and `Query` is named `noun.verb`, and the tool named after its noun
takes it as its `op` argument. A command added to the protocol becomes an op with no
change here, and a new noun becomes a new tool. The generated list is in the snapshot
below; its shape is:

| Tool | Takes | Becomes |
|---|---|---|
| one per noun (`item`, `output`, `scene`, `source`, `state` today) | `op`: one edit, action or query of that noun | an edit: a batch of one; an action: an action; a query: a query |
| `batch` | `edits`, `expected_rev`, `idempotency_key`: the protocol's `Batch` | one atomic batch |
| `describe_api` | nothing | the whole protocol schema, without calling the engine |
| `events` | `after`: a cursor, optional | the buffered engine events after it ([Events](#design)), without calling the engine |

A group tool's arguments are `{"op": {...}, "expected_rev"?, "idempotency_key"?}`. `op`
is the protocol's own object, tagged by `type`, and its schema is the protocol's variant
schemas under `oneOf`. The two batch options apply to edits only: an action or query that
carries them is rejected. Unknown argument keys are rejected too, so a misspelt
`expected_rev` can't silently drop the concurrency check. Each tool's `inputSchema`
carries, under `$defs`, every protocol definition it refers to, so it stands alone. Tool
names follow [R-API-002](../../docs/RULES.md#R-API-002), checked by the crate-root test
`tool_names_are_valid`, and the whole list is snapshot-tested in
[`schema/tools.snap`](schema/tools.snap): a protocol change shows up there as the tool
diff agents will see. After an intended change, regenerate it with
`INSTA_UPDATE=always cargo test -p jstream-mcp` and review the diff.

### MCP revisions

The current MCP revision is
[2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28). It drops the
`initialize` handshake: every request carries its protocol version and client
capabilities in `_meta`, and servers answer each request on its own. Clients written
for earlier revisions open with the handshake of
[2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25) and have no way
to move forward, so the server is dual-era, which 2026-07-28 allows:

- A request whose `_meta` carries `io.modelcontextprotocol/protocolVersion` is served
  statelessly under 2026-07-28: `server/discover`, `tools/list` and `tools/call`. Results
  carry `resultType`, `serverInfo` in `_meta`, and caching hints on the two cacheable ones.
  Another version gets error `-32022` listing the supported ones, and a missing
  `clientCapabilities` gets `-32602`.
- `initialize` selects the handshake-based era for the rest of the connection, at
  2025-11-25 or 2025-06-18 (their tool subsets are the same), the client's choice if
  served, else 2025-11-25. Then `tools/list` and `tools/call` work without `_meta`, and
  so does `ping`, which that era also allows before `initialize`.
- Any other request with neither gets `-32602`, naming both ways in.

Not implemented: resources, prompts, subscriptions (`subscriptions/listen`, list-changed
notifications; engine events come through the `events` tool instead), progress, MCP logging
(`logging/setLevel` and log notifications; the server logs through `tracing` instead, see
*Logging*), cancellation (requests are answered in order, so there
is nothing in flight to cancel), pagination (one page), `outputSchema`, and JSON-RPC
batches (MCP removed them in 2025-06-18).

### Results and errors

| What happened | MCP answer |
|---|---|
| The engine answered | a result: `structuredContent` is the protocol `Response` (`{"id", "result"}`), repeated as JSON in a text block |
| The engine refused (`ControlError::Protocol`) | a result with `isError: true` and `{"error": <WireError>}`: the stable `code`, its details and the message |
| The arguments don't fit the tool, or an `events` cursor is past the newest event | the same, with the protocol's `malformed` code and the reason |
| The connection to the engine was lost during the call | the same, with code `disconnected` (an MCP-layer code, not a protocol one), saying whether the call took effect is unknown. The next call sends `hello` again ([Session and identity](#design)). |
| `hello` failed with a lost connection | the same `disconnected` code, saying the call was not sent and that the MCP server must be restarted for a new session, with its own undo history |
| Unknown tool, or a `tools/call` without `name` or with non-object `arguments` | JSON-RPC `-32602` |
| Unparseable JSON / not a valid request / unknown method | `-32700` / `-32600` / `-32601` |

MCP asks for errors the model can correct (bad input, refused operations) to come back
as tool results with `isError`, and for problems with the request itself as JSON-RPC
errors. Engine errors keep their protocol shape, so an agent matches on the same `code`
whether it talks MCP or the WebSocket API. Notifications, including malformed ones, get
no reply.

### Session and identity

A `Server` owns one `Control` session, so every tool call acts as one client of the
engine: one undo history and one name in the audit log
([ADR-0013](../../docs/adr/0013-command-model.md)). `hello` is sent before the first
tool call that needs the engine, with the MCP client's name (`clientInfo.name` from
`initialize`, or from the request's `_meta`; `jstream-mcp` if none). Later names don't
change the session. `describe_api`, argument errors and `events` reads never call the
engine ([Events](#design)).

After a call fails with a lost connection, the next call sends `hello` again on the same
`Control`. That never yields a new `client_id`, so it never splits the undo history
([ADR-0013](../../docs/adr/0013-command-model.md),
[ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)): the `Control` contract says a
second `hello` on an open session returns the same `client_id`, `InProcessControl` never
loses its connection, and `WsControl` never reconnects, so once its socket has closed
every `hello` fails with `Disconnected` too (tested in `jstream-server`, and end to end
through `jstream-cli mcp` in its `tests/mcp.rs`). The server then reports that the call
was not sent and that the MCP server must be restarted. The new session that gives is a
new client of the engine, with a new `client_id` and its own undo history, so edits
made through the old one can't be undone from it; the error says so. The contract
doesn't cover a `Control` that reconnects by itself (none exists): one that answered the
second `hello` with a new `client_id` would split the history without the agent
noticing, and this server doesn't check for that.

Authentication happens below this crate: the session the host passes in already holds
the control token ([ADR-0006](../../docs/adr/0006-mcp-full-access.md)). The stdio
transport is not a listener; the streamable-HTTP transport will be, so it must check the
token, `Origin` and `Host` ([R-SEC-002](../../docs/RULES.md#R-SEC-002)).

### Events

The `events` tool exposes the engine's events
([ADR-0036](../../docs/adr/0036-mcp-events-polling-tool.md)). Just before its first
`hello`, the server subscribes once to `Control::events()`. A thread drains that
subscription into a buffer of the newest `BUFFER_CAPACITY` events (in `events.rs`),
numbering them from 1 (`seq`), and parks until the subscription wakes it, so neither
the buffer nor the `Control`'s own stream grows while the agent is idle. A read drains
the subscription first, under the same lock, so it sees every event already delivered,
then returns at most `PAGE_SIZE` events after `after`:

| Field | Meaning |
|---|---|
| `events` | `{"seq", "event"}` pairs, oldest first; `event` is the protocol `Event` |
| `next` | the cursor to pass as `after`: the last `seq` returned, else `after` |
| `dropped` | events after `after` that the full buffer dropped before this read; not 0 means re-read the state |
| `more` | further events are buffered after `next` |
| `ended` | the subscription has ended with the session; no more events will come |

A read sends no call to the engine. Only when no tool call has opened the session yet
does it send `hello`, which starts the subscription, so that first read holds at most
what arrived since. Once the subscription exists it sends nothing, so after the session
is lost it still answers from the buffer. Every MCP client of one server reads the same
buffer, each with its own cursor (question 6). The tool returns at once; waiting for the
next event would hold up the in-order stdio loop. The tool's description states the
buffer and page sizes, so agents don't need this page.

### Async and transports

`Server::handle` is an `async fn` whose future is `Send` and only awaits the `Control`
futures, so any executor can run it, and `jstream-server` can mount it inside axum.
`serve_stdio` needs none: it reads a line, drives that message's future on the calling
thread with a small `block_on` (park until woken, built on `std::task::Wake`), writes the
reply line and flushes. That works for any `Control` whose futures are woken by their
own machinery (channels from a reader task, a reactor on another thread), as the
WebSocket client's design has it
([jstream-control › Design](../jstream-control/README.md#design)). Messages are answered
one at a time, in order. Blank lines are skipped, a line that isn't UTF-8 gets a parse
error, and end of input ends the transport. Nothing else may write to the stdout it is
given.

Streamable HTTP is deferred to `jstream-server`, which owns the HTTP stack and the token
and `Origin` checks: it will feed request bodies to `Server::handle`.

### Logging

The server logs through `tracing` ([R-RS-005](../../docs/RULES.md#R-RS-005)) and
installs no subscriber; the host decides where logs go. `jstream-cli mcp` installs none,
so nothing is logged there, and `tracing` never writes to the stdout that carries MCP.
Each `Server` has an `info` span `mcp_session`. Every request runs in a `debug` span
`mcp_request` under it with the JSON-RPC method, and a request answered with a JSON-RPC
error logs its code at `debug`. Each `tools/call` runs in an `info` span `tool_call`
with the tool's name, directly under `mcp_session` so it keeps its session when the
`debug` request span is off, and logs at `debug` whether it succeeded or, for a result with
`isError`, its error code. `initialize` (client name and settled revision) and the
engine session's `hello` (its `client_id`) log at `info`; a failed `hello` and a
connection lost during a call log at `warn`. Tool arguments and results are never
logged ([R-SEC-001](../../docs/RULES.md#R-SEC-001)).

### No MCP SDK

The server implements its JSON-RPC and MCP subset on `serde_json` instead of using the
official Rust SDK (`rmcp`):

- **Weight.** `rmcp` is actively maintained and already speaks 2026-07-28, but every
  build of it requires tokio (with its `rt`, `macros` and `time` features), `tokio-util`,
  `futures`, `tracing`, `indexmap` and `pin-project-lite`, and its default `server`
  feature adds `schemars` with `chrono`, `uuid` and proc-macro crates. That would force a
  runtime on every client of this crate. This crate needs only crates the workspace
  already has (`serde`, `serde_json`, `thiserror`, `tracing`) plus the two JStream
  crates.
- **Fit.** The SDK's main help is deriving tool schemas from Rust types with its macros.
  Here the tools are generated from the protocol's schema at runtime, so that help
  doesn't apply; the table-driven code is short.
- **Risk.** A hand-written subset can drift from the spec. It is small (four methods per
  era), every reply path is tested, and the MCP revisions it serves are named in the code
  (`MODERN_VERSION`, `LEGACY_VERSIONS`), so a new revision is a deliberate change.

<a id="open-questions"></a>
## Open questions
<!-- @section
-->

1. **Progressive discovery.** Each group tool carries its full op schemas, and `batch`
   carries every edit's (about 1,300 lines of JSON in total today). ARCH plans small
   schemas plus `describe_api` on demand. When the protocol grows, should group schemas
   shrink to the op names, with `describe_api` taking a group or op name?
2. **`disconnected`.** A lost engine connection is reported with an MCP-layer error code
   that the protocol doesn't define. Should the protocol define it, or should it be a
   JSON-RPC error instead?
3. **Events.** Settled by [ADR-0036](../../docs/adr/0036-mcp-events-polling-tool.md):
   the `events` polling tool with a server-local cursor and a bounded buffer that
   reports dropped events ([Design](#design)). MCP notifications may be added beside
   it once the server has an outgoing channel.
4. **Streamable HTTP.** Mounted by `jstream-server` (planned above), or a feature of this
   crate on a small HTTP stack?
5. **Legacy support.** How long to keep the handshake-based era once clients move to
   2026-07-28.
6. **One session per server.** Every MCP client of one server shares an undo history.
   That fits stdio (one client per process). Over HTTP, should each client identity get
   its own `Control` session?
