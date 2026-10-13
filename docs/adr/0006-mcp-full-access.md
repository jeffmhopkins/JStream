---
id: ADR-0006
title: "MCP agents get full access"
kind: adr
status: accepted
decided: 2026-10-13
rev: 1
review_every: 180d
depends_on: [ADR-0001, ext:mcp-spec]
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version, recorded from design Q&A with the maintainer"}
---

# 0006: MCP agents get full access


## Context
Agents should be able to do anything a user can, including going live.

## Decision
Every protocol command is available as an MCP tool with no permission tiers. Agents can
fetch preview/program snapshots to see the output. Two defaults remain: listeners bind
to localhost, and every agent action is written to the audit log (with client name) and
is undoable where the command supports undo.

## Consequences
- Simple and powerful; suits a single-operator setup.
- Exposing MCP beyond localhost is a deliberate config change with an auth token ([[RULES#R-SEC-002]]).
- If multi-user or untrusted agents become a need, revisit with a tiered model.
