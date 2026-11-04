<!-- @doc
id = "ADR-0110"
title = "Shared values reach MCP through one values tool"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0110: Shared values reach MCP through one values tool

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0102](0102-page-api-for-browser-sources.md) decision 7 adds shared values, set and
listed over the protocol with `values.set` and `values.list`. It names their MCP tools
`values_set` and `values_list`. That contradicts
[R-API-002](../RULES.md#R-API-002): MCP tools are generated from the protocol and grouped
by noun, so each `noun.verb` command or query is an `op` of the tool named after its noun.
No per-command tools exist (#1128).

<a id="decision"></a>
## Decision
This amends ADR-0102 decision 7's MCP naming only. Shared values reach MCP through the one
`values` tool, with ops `values.set` and `values.list`, as R-API-002 decides. Everything
else in decision 7 stands: they are actions, not edits, and the store, names and limits are
unchanged.

<a id="consequences"></a>
## Consequences
- ADR-0102 keeps its text and status (R-DOC-009). A reader of decision 7 should take its
  MCP tool names from this ADR.
- No protocol, MCP or code change follows: the tool is generated from the protocol.
