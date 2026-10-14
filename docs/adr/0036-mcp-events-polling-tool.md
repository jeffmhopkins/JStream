<!-- @doc
id = "ADR-0036"
title = "Engine events over MCP through a polling tool"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0036: Engine events over MCP through a polling tool

<a id="context"></a>
## Context
[ADR-0006](0006-mcp-full-access.md) makes every protocol capability available over MCP.
The protocol has events (`state.changed`, `output.state_changed`), and `Control::events()`
delivers them to any client, but `jstream-mcp` exposed only calls: an agent couldn't see
who changed the state, and missed an output that started and stopped between two
`output.status` reads.

MCP offers two ways to push data that answers no request: notifications on the
handshake-based revisions, and subscriptions in
[2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28). Both need a
channel that writes messages the client didn't ask for. `Server::handle` returns at
most one reply per message and the stdio loop answers one message at a time, so both
would need a second writer, and many MCP clients don't show notifications to the model
anyway. A tool works in both eras the crate serves, over stdio as it is, and every MCP
client shows tool results to the model.

A tool that hands out buffered events needs a buffer, and an agent that never reads
would grow it without limit. Engine-side subscriptions and backpressure are still open
([protocol open question 7](../../crates/jstream-protocol/README.md#open-questions),
[jstream-control › Design](../../crates/jstream-control/README.md#design), question 1);
this decision is about the MCP layer only.

<a id="decision"></a>
## Decision
- **An `events` tool**, beside `batch` and `describe_api`. It is not a protocol noun, so
  it takes no `op`; its name follows [R-API-002](../RULES.md#R-API-002)'s pattern. It
  takes one optional argument, `after`, a cursor, and returns at once, never waiting
  for the next event:
  `{"events": [{"seq", "event"}], "next", "dropped", "more", "ended"}`. `event` is the
  protocol `Event` unchanged, so the tool adds nothing beside the protocol
  ([R-ARCH-001](../RULES.md#R-ARCH-001)): only the cursor and the buffer belong to MCP.
- **The subscription** is opened once per `Server`, just before its first `hello`, so the
  agent sees every event from the moment its session exists: its own changes, the
  user's and other clients'.
- **The cursor is a sequence number local to the server**, from 1 up, because only
  `state.changed` carries a `rev`. `next` is the last `seq` returned (or `after` if none
  was); passing it back as `after` reads on. A cursor past the newest event (from an
  earlier server process) is refused with `malformed`, so the agent knows to start over.
- **The buffer is bounded**: it keeps a fixed number of the newest events and drops the
  oldest. A read reports in `dropped` how many events after its cursor were dropped
  before it read them, so a gap is never silent; the agent then re-reads the state. A
  read returns a bounded page; `more` says further events wait.
- **The buffer is drained continuously**, by a thread that parks until the
  subscription wakes it, so the `Control`'s own stream never grows while the agent is
  idle. A read drains it first, so it sees every event already delivered.
- **Reading sends no call to the engine**, and no `hello` once the subscription exists.
  After the session is lost it still returns what was buffered, with `ended` set once
  the subscription has ended.

<a id="consequences"></a>
## Consequences
- Agents can follow the engine, including other clients' edits and output progress,
  with no protocol change and no change to the transports.
- Events reach an agent only when it asks. Waiting for the next event (a long poll)
  would hold up the in-order stdio loop, so it isn't offered; MCP notifications can be
  added later beside the tool, once the server has an outgoing channel.
- Every MCP client of one server shares the buffer and its cursors, as they already
  share the session (`jstream-mcp` open question 6). Each reader keeps its own cursor,
  so sharing is safe; per-client feeds would come with per-client sessions.
- The buffer size and page size are constants of `jstream-mcp`, stated in the tool's
  description. When engine-side subscriptions land, the feed can subscribe to fewer
  events, and high-rate ones (`stats.tick`, meters) should not reach this buffer
  unasked.
