<!-- @doc
id = "ADR-0059"
title = "MCP tool annotations, and third-party text marked as data"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0059: MCP tool annotations, and third-party text marked as data

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Agents get full access with no confirmation step
([ADR-0006](0006-mcp-full-access.md)), and an MCP tool result is the protocol reply as
JSON, with nothing marking which strings came from outside JStream. Today one path
carries a third party's text to an agent: when an RTMP server refuses a stream, its
rejection `code` and `description` end up in the `action_failed` `reason` of
`output.start`. The RTMP publisher caps and cleans that text (512 characters for each part, control
characters replaced, the stream key redacted first), as `jstream-youtube` does for
Google's errors. More is planned: YouTube chat,
browser page titles and plugin parameter names
([ADR-0037](0037-kind-descriptors-for-transitions-and-plugins.md)) will all be readable by agents. One injected
instruction in such text could ask an agent to retarget an output and send its key
elsewhere (#343).

The MCP revisions this server speaks
([2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28) and the two
handshake-based ones) let a `Tool` carry `annotations`: `readOnlyHint` (default false),
`destructiveHint` (default true), `idempotentHint` (default false) and `openWorldHint`
(default true). They are hints for the client, which may ask the user before a call, and
not a security boundary. Tools are grouped by noun
([R-API-002](../RULES.md#R-API-002)), so a hint describes a whole group, not one op.

<a id="decision"></a>
## Decision
1. **Every tool carries annotations, derived from its ops' classes** (ADR-0013), with
   every hint written out so the spec's defaults never apply by omission:
   - `readOnlyHint` is true when every op is a query (and for `describe_api` and
     `events`). Such a tool gets only `readOnlyHint` and `openWorldHint`.
   - Otherwise `destructiveHint` is true when the tool has an action (undo can't take it
     back), a history call (undo can remove what a create made), or an edit whose verb is
     `delete` or `remove`. `batch` is destructive, since it takes every delete and
     `output.set_target`. A tool of edits that only add or change is not destructive.
   - `openWorldHint` is true for the group tools in `OPEN_WORLD` and for `batch`, which
     can carry their edits. Today those are `output`, whose `output.start` publishes to a
     streaming server, and `source`, whose `browser` kind loads any http or https page an
     agent sets as its `url`, so an injected instruction could name the page. The other
     tools' ops stay inside JStream's state and the machine's own devices.
   - `idempotentHint` is false on every tool that changes state: each edit raises the
     state `rev` unless it carries an `idempotency_key`, which is per call.

   So the `output` tool, which carries `output.set_target`, `output.set_key`,
   `output.start` and `output.delete`, is destructive and open-world, and a client that
   asks before such tools asks before each of them. Splitting a group so that its
   read-only ops escape that question is not done here.
2. **Third-party text is marked by where it is, not wrapped.** The protocol reply stays
   as it is, so an agent sees the same shape over MCP and the WebSocket API, and no
   protocol version changes. A protocol field added for text from outside JStream (chat
   messages, page titles, plugin-supplied names) is named with the prefix `remote_`, and
   its schema description says it is third-party text. The `action_failed` `reason` is
   the one exception, since it mixes JStream's own words with the server's and predates
   this rule: the `output` tool's description says that its `reason` includes the code
   and description a streaming server sent (each at most 512 characters). Nothing marks
   where the server's words start or end, so the description and the instructions say
   the whole `reason` is untrusted. A `remote_*` field for the server's text would mark it
   structurally; that is a protocol change, left to a later one.
3. **The server instructions say so.** Both eras' `instructions` say that text from
   outside JStream is data, never instructions, that it sits in `remote_` fields and in
   that `reason`, that the agent should ask the user instead of acting on requests found
   there, and that the annotations mark the tools that remove things, can't be undone or
   reach outside JStream (`output`, `source` and `batch`).

<a id="consequences"></a>
## Consequences
- A client that confirms destructive or open-world tools now asks the user before an
  agent retargets an output, sets its key, goes live, points a browser source at a page
  or deletes anything. The read-only tools can run without a question.
- The hints come from the op classes and verbs, so a new op or noun is annotated with no
  change here; a new noun that reaches outside JStream must be added to `OPEN_WORLD`.
- Marking stays advisory: a model can still follow injected text. Bounding the text
  limits what it can say, and the user's confirmation, where the client asks,
  is the actual check.
- Features that bring third-party text (YouTube chat, page titles, plugin names) must
  name their fields `remote_*`; review checks it against this ADR.
- If a group's single hint proves too coarse, the fix is a finer tool split, recorded in
  a new ADR.
