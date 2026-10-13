---
id: ADR-0010
title: "Build headless engine and MCP before the GUI"
kind: adr
status: accepted
decided: 2026-10-13
rev: 1
review_every: 180d
depends_on: [ADR-0001, ADR-0006]
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version, recorded from design Q&A with the maintainer"}
---

# 0010: Build headless engine and MCP before the GUI


## Context
The GUI is the most change-prone layer. Agents can exercise the engine through MCP
from day one, and snapshots let them verify visual output.

## Decision
Milestone 1 delivers the engine, protocol, MCP server and CLI, including virtual camera
and YouTube streaming, with no GUI. The UI is M2 and is built on the proven API.

## Consequences
- The API is validated by real use before the UI depends on it.
- Early demos are agent/CLI-driven rather than visual.
