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
verified = { hash = "b3:b18fad5b9420e0cd", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "ARCH#crate-map changed only in the jstream-sources row (ADR-0031 kinds by milestone; pure-Rust image and font crates) and the ADR-0003 to ADR-0031 citation; the jstream-mcp row and this Purpose are unchanged and still match the crate" }
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
- **JSON-RPC** (`jsonrpc.rs`): parsing one message and building replies.
- **stdio transport** (`stdio.rs`): `serve_stdio`, newline-delimited messages over any
  `BufRead`/`Write` pair.
- **Errors** (`error.rs`): `McpError`, a `thiserror` enum for a transport that stops.

Not here yet: the streamable-HTTP transport ([Design](#design)).

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-mcp/src/**", "crates/jstream-mcp/Cargo.toml"]
depends_on = ["docs/ARCHITECTURE.md#mcp", "docs/adr/0006-mcp-full-access.md", "docs/adr/0013-command-model.md"]
verified = { hash = "b3:49c0763d39419a32", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Edited: errors table splits lost-during-call from hello-failed (call not sent, restart for a new session with its own undo history); Session and identity explains why re-hello never splits undo (Control contract, InProcessControl never disconnects, WsControl never reconnects); tools, revisions, async/transports re-read against server.rs" }
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
notifications), progress, logging, cancellation (requests are answered in order, so there
is nothing in flight to cancel), pagination (one page), `outputSchema`, and JSON-RPC
batches (MCP removed them in 2025-06-18).

### Results and errors

| What happened | MCP answer |
|---|---|
| The engine answered | a result: `structuredContent` is the protocol `Response` (`{"id", "result"}`), repeated as JSON in a text block |
| The engine refused (`ControlError::Protocol`) | a result with `isError: true` and `{"error": <WireError>}`: the stable `code`, its details and the message |
| The arguments don't fit the tool | the same, with the protocol's `malformed` code and serde's reason |
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
change the session. `describe_api` and argument errors never reach the engine.

After a call fails with a lost connection, the next call sends `hello` again on the same
`Control`. That never splits the undo history: the `Control` contract says a second
`hello` on an open session returns the same `client_id`, `InProcessControl` never loses
its connection, and `WsControl` never reconnects, so once its socket has closed every
`hello` fails with `Disconnected` too (tested in `jstream-server`). The server then
reports that the call was not sent and that the MCP server must be restarted. The new
session that gives is a new client of the engine, so edits made through the old one
can't be undone from it. The contract doesn't cover a `Control` that reconnects by
itself (none exists): one that answered the second `hello` with a new `client_id` would
split the history without the agent noticing, and this server doesn't check for that.

Authentication happens below this crate: the session the host passes in already holds
the control token ([ADR-0006](../../docs/adr/0006-mcp-full-access.md)). The stdio
transport is not a listener; the streamable-HTTP transport will be, so it must check the
token, `Origin` and `Host` ([R-SEC-002](../../docs/RULES.md#R-SEC-002)).

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

### No MCP SDK

The server implements its JSON-RPC and MCP subset on `serde_json` instead of using the
official Rust SDK (`rmcp`):

- **Weight.** `rmcp` is actively maintained and already speaks 2026-07-28, but every
  build of it requires tokio (with its `rt`, `macros` and `time` features), `tokio-util`,
  `futures`, `tracing`, `indexmap` and `pin-project-lite`, and its default `server`
  feature adds `schemars` with `chrono`, `uuid` and proc-macro crates. That would force a
  runtime on every client of this crate. This crate needs only crates the workspace
  already has (`serde`, `serde_json`, `thiserror`) plus the two JStream crates.
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
3. **Events.** Engine events (`state.changed`, `output.state_changed`) don't reach agents,
   though [ADR-0006](../../docs/adr/0006-mcp-full-access.md) makes every protocol
   capability available over MCP. Options:
   - **A polling tool** (say `events`, taking an optional `after` cursor): the server
     opens one `Control::events()` subscription after `hello`, buffers what it yields,
     and the tool returns the events after the cursor, without waiting, plus the next
     cursor. It needs no protocol change and works in both eras and over stdio as it is.
     It still needs decisions: the buffer must be bounded, because an agent that never
     polls would grow it without limit, so what to drop and how to report the gap (the
     same question as [protocol open question 7](../jstream-protocol/README.md#open-questions)
     and [jstream-control open question 1](../jstream-control/README.md#design)); the
     cursor is a server-local sequence number, since only `state.changed` carries a
     `rev`; every MCP client of one server shares the buffer (question 6); and whether
     the tool may wait for the next event, which would hold up the in-order stdio loop.
   - **MCP notifications** (2026-07-28 subscriptions, or notifications on the
     handshake-based era): the server would write messages that answer no request.
     `Server::handle` returns at most one reply per message, and `serve_stdio` writes
     only replies, so both would need an outgoing channel. Many MCP clients also don't
     show notifications to the model.
   - **Nothing new:** agents poll `state.get` and `output.status` and compare `rev`.
     That covers the state, but not who changed it (the event's `client_id` and
     `request_id`), and misses an output that started and stopped between two polls.

   The polling tool is the smallest step, once the buffer policy is settled.
4. **Streamable HTTP.** Mounted by `jstream-server` (planned above), or a feature of this
   crate on a small HTTP stack?
5. **Legacy support.** How long to keep the handshake-based era once clients move to
   2026-07-28.
6. **One session per server.** Every MCP client of one server shares an undo history.
   That fits stdio (one client per process). Over HTTP, should each client identity get
   its own `Control` session?
