# 0006: MCP agents get full access

- Status: Accepted
- Date: 2026-10-13

## Context
Agents should be able to do anything a user can, including going live.

## Decision
Every protocol command is available as an MCP tool with no permission tiers. Agents can
fetch preview/program snapshots to see the output. Two defaults remain: listeners bind
to localhost, and every agent action is written to the audit log (with client name) and
is undoable where the command supports undo.

## Consequences
- Simple and powerful; suits a single-operator setup.
- Exposing MCP beyond localhost is a deliberate config change with an auth token.
- If multi-user or untrusted agents become a need, revisit with a tiered model.
